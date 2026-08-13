// Owns Connect profiles, graph policy, posts, comments, reactions, notifications, and reports.
// api.rs owns routes; blob.rs stores media bytes and admin.rs owns founder-only enforcement.

// Connect — the social layer (Hive_design_doc.md §6).
//
// The only HIVE service holding some plaintext (public posts, profiles) — by
// design, because strangers must be able to read them. Private-audience
// content is either delivery-gated by the graph ('followers', pragmatic v1)
// or E2E-encrypted to a circle key HIVE never sees (§6.4).
//
// Ground rules implemented here:
// - Feed is chronological pull with a (created, post_id) cursor. No ranking.
// - A block is a WHERE clause everywhere, not a client filter (§6.2).
// - Deletes are real: rows go away and media blobs are deref'd (§6.3).
// - Fan-out is a push *hint* (connect_notif) to online followers — it drives
//   badges, never feed content.

use crate::api::AppState;
use crate::blob;
use crate::identity::{authenticate, now};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::Json;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

/// §7 body caps.
const POST_BODY_MAX: usize = 10_000;
const COMMENT_BODY_MAX: usize = 2_000;
const MEDIA_PER_POST_MAX: usize = 20;
const FEED_LIMIT_MAX: i64 = 100;
const REACTION_KINDS: &[&str] = &["like", "❤️", "👍", "😂", "😮", "😢", "😠"];

fn err(msg: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": msg }))
}

/// Time-prefixed random id (ULID-style): 12 hex chars of unix millis +
/// 20 random. `created` is unix seconds, so the id is the feed cursor's
/// tie-breaker — the prefix keeps same-second posts in creation order.
pub(crate) fn new_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mut b = [0u8; 10];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!(
        "{millis:012x}{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// Resolve a target given as either an account id or a handle.
async fn resolve(s: &AppState, target: &str) -> Option<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM accounts WHERE (id = ?1 OR handle = ?1) AND status = 'active'",
    )
    .bind(target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    row.map(|(id,)| id)
}

/// §6.2: a block in either direction hides everything both ways.
async fn blocked_between(s: &AppState, a: &str, b: &str) -> bool {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM blocks WHERE (blocker = ?1 AND blocked = ?2) \
                                 OR (blocker = ?2 AND blocked = ?1) LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    row.is_some()
}

async fn is_accepted_follower(s: &AppState, follower: &str, followee: &str) -> bool {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM follows WHERE follower = ? AND followee = ? AND state = 'accepted'",
    )
    .bind(follower)
    .bind(followee)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    row.is_some()
}

/// Pending Fold invitations never authorize content access.
async fn circle_member(s: &AppState, circle_id: &str, viewer: &str) -> bool {
    sqlx::query_as::<_, (i64,)>(
        "SELECT 1 FROM fold_members WHERE circle_id=? AND account_id=? AND state='active'",
    )
    .bind(circle_id)
    .bind(viewer)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None)
    .is_some()
}

async fn fold_epoch_for_member(s: &AppState, circle_id: &str, account: &str) -> Option<i64> {
    sqlx::query_as::<_, (i64,)>(
        "SELECT c.key_epoch FROM circles c JOIN fold_members m ON m.circle_id=c.id \
         WHERE c.id=? AND c.key_stale=0 AND m.account_id=? AND m.state='active'",
    )
    .bind(circle_id)
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None)
    .map(|(epoch,)| epoch)
}

fn valid_fold_envelope(body: &str, epoch: i64) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|value| {
            value["v"].as_i64() == Some(1)
                && value["epoch"].as_i64() == Some(epoch)
                && value["nonce"].as_str().is_some_and(|v| !v.is_empty())
                && value["ciphertext"].as_str().is_some_and(|v| !v.is_empty())
        })
}

/// Full §6.3 audience gate for one post.
async fn can_view(s: &AppState, viewer: &str, author: &str, audience: &str) -> bool {
    if viewer == author {
        return true;
    }
    if blocked_between(s, viewer, author).await {
        return false;
    }
    match audience {
        "public" => true,
        "followers" => is_accepted_follower(s, viewer, author).await,
        a => match a.strip_prefix("circle:") {
            Some(id) => circle_member(s, id, viewer).await,
            None => false,
        },
    }
}

/// Accepted followers of an account (push fan-out targets).
async fn follower_ids(s: &AppState, account: &str) -> Vec<String> {
    sqlx::query_as::<_, (String,)>(
        "SELECT follower FROM follows WHERE followee = ? AND state = 'accepted'",
    )
    .bind(account)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id,)| id)
    .collect()
}

#[derive(Deserialize)]
pub struct FoldKeyDirectoryReq {
    pub target: String,
}

pub async fn fold_key_directory(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldKeyDirectoryReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let target: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id,handle,identity_pub FROM accounts \
         WHERE (id=?1 OR handle=?1) AND status='active'",
    )
    .bind(req.target.trim_start_matches('@'))
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((account_id, handle, identity_pub)) = target else {
        return err("account not found");
    };
    if blocked_between(&s, &me, &account_id).await {
        return err("account not found");
    }
    let socially_related = is_accepted_follower(&s, &me, &account_id).await
        || is_accepted_follower(&s, &account_id, &me).await;
    let co_members: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM fold_members mine JOIN fold_members theirs \
         ON theirs.circle_id=mine.circle_id \
         WHERE mine.account_id=? AND theirs.account_id=? \
           AND mine.state='active' AND theirs.state='active' LIMIT 1",
    )
    .bind(&me)
    .bind(&account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if account_id != me && !socially_related && co_members.is_none() {
        return err("Fold key directory requires a social relationship or active co-membership");
    }
    let devices: Vec<(String, String, String, String, i64, String, String)> = sqlx::query_as(
        "SELECT d.id,d.device_pub,d.cert,d.name,d.created,k.wire_pub,k.signature \
         FROM devices d JOIN wire_device_keys k ON k.device_id=d.id \
         WHERE d.account_id=? AND d.revoked_at IS NULL ORDER BY d.created",
    )
    .bind(&account_id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    Json(json!({
        "ok": true,
        "account_id": account_id,
        "handle": handle,
        "identity_pub": identity_pub,
        "devices": devices.into_iter().map(|device| json!({
            "device_id": device.0, "device_pub": device.1, "cert": device.2,
            "name": device.3, "created": device.4, "wire_pub": device.5,
            "wire_signature": device.6,
        })).collect::<Vec<_>>(),
    }))
}

/// handle + display_name + avatar for rendering.
pub(crate) async fn author_card(s: &AppState, account: &str) -> Value {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT a.handle, COALESCE(p.display_name, ''), p.avatar_blob FROM accounts a \
         LEFT JOIN profiles p ON p.account_id = a.id WHERE a.id = ?",
    )
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let (handle, display_name, avatar_blob) = row.unwrap_or_default();
    json!({
        "account_id": account,
        "handle": handle,
        "display_name": display_name,
        "avatar_blob": avatar_blob,
    })
}

/// Persist a notification row AND push the live connect_notif frame. The
/// row is what the Alerts tab reads after a restart; the frame is the
/// real-time badge. Self-notifications are dropped.
async fn notification_fold_id(
    s: &AppState,
    kind: &str,
    subject_id: &str,
) -> Option<String> {
    if matches!(kind, "fold_invite" | "fold_joined") {
        return (!subject_id.is_empty()).then(|| subject_id.to_string());
    }
    sqlx::query_as::<_, (String,)>("SELECT audience FROM posts WHERE id = ?")
        .bind(subject_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None)
        .and_then(|(audience,)| audience.strip_prefix("circle:").map(str::to_string))
}

pub(crate) async fn notify(
    s: &AppState,
    recipient: &str,
    kind: &str,
    actor: &str,
    subject_id: &str,
) {
    if recipient == actor {
        return;
    }
    let _ = sqlx::query(
        "INSERT INTO notifications (id, account_id, kind, actor, subject_id, created) \
         VALUES (?,?,?,?,?,?)",
    )
    .bind(new_id())
    .bind(recipient)
    .bind(kind)
    .bind(actor)
    .bind(subject_id)
    .bind(now())
    .execute(&s.db)
    .await;
    let from = author_card(s, actor).await;
    let fold_id = notification_fold_id(s, kind, subject_id).await;
    s.push_to_account(
        recipient,
        None,
        json!({
            "type": "connect_notif",
            "kind": kind,
            "post_id": subject_id,
            "fold_id": fold_id,
            "from": from,
        }),
    );
}

// -------------------------------------------------------------- profiles

fn mention_boundary(remainder: &str, handle: &str) -> bool {
    remainder
        .strip_prefix(handle)
        .and_then(|rest| rest.chars().next())
        .is_none_or(|next| !next.is_alphanumeric() && !matches!(next, '_' | '.' | '-'))
}

/// Pull @handle mentions out of a body. Quoted mentions resolve exactly, while
/// unquoted mentions choose the longest active handle prefix with a token
/// boundary. The legacy no-space token remains as a fallback.
async fn mention_handles(s: &AppState, body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'@' && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let start = i + 1;
            if b.get(start) == Some(&b'"') {
                if let Some(relative_end) = body[start + 1..].find('"') {
                    let end = start + 1 + relative_end;
                    let handle = &body[start + 1..end];
                    if !handle.is_empty() && resolve(s, handle).await.is_some()
                        && !out.iter().any(|existing| existing == handle)
                    {
                        out.push(handle.to_string());
                    }
                    i = end + 1;
                    if out.len() >= 20 {
                        break;
                    }
                    continue;
                }
            }
            let remainder = &body[start..];
            let candidates: Vec<(String,)> = sqlx::query_as(
                "SELECT handle FROM accounts \
                 WHERE status='active' AND substr(?1,1,length(handle))=handle \
                 ORDER BY length(handle) DESC LIMIT 20",
            )
            .bind(remainder)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
            if let Some((handle,)) = candidates
                .into_iter()
                .find(|(handle,)| mention_boundary(remainder, handle))
            {
                if !out.iter().any(|existing| existing == &handle) {
                    out.push(handle.clone());
                }
                i = start + handle.len();
                if out.len() >= 20 {
                    break;
                }
                continue;
            }
            let mut end = start;
            while end < b.len()
                && (b[end].is_ascii_alphanumeric() || matches!(b[end], b'_' | b'.' | b'-'))
            {
                end += 1;
            }
            let mut tok = &body[start..end];
            while tok.ends_with('.') || tok.ends_with('-') {
                tok = &tok[..tok.len() - 1];
            }
            if !tok.is_empty() && !out.iter().any(|t| t == tok) {
                out.push(tok.to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out.truncate(20);
    out
}

/// Notify every @mentioned account that can actually view the post. Blocked
/// or out-of-audience accounts are silently skipped (§6.2 — mentions must
/// not leak hidden content).
async fn notify_mentions(
    s: &AppState,
    body: &str,
    me: &str,
    author: &str,
    audience: &str,
    post_id: &str,
) {
    for h in mention_handles(s, body).await {
        let Some(target) = resolve(s, &h).await else {
            continue;
        };
        if target == me {
            continue;
        }
        if can_view(s, &target, author, audience).await && !blocked_between(s, me, &target).await {
            notify(s, &target, "mention", me, post_id).await;
        }
    }
}

async fn community_inviter(s: &AppState, account_id: &str) -> Option<bool> {
    sqlx::query_as::<_, (i64, String)>(
        "SELECT founder, community_role FROM accounts WHERE id=? AND status='active'",
    )
    .bind(account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None)
    .and_then(|(founder, role)| (founder != 0 || role == "steward").then_some(founder != 0))
}

#[derive(Deserialize)]
pub struct CommunityInviteCreateReq {
    #[serde(default)]
    pub count: Option<u32>,
}

/// Community Stewards may issue bounded beta signup invitations without
/// receiving any `/v1/admin/*` access.
pub async fn invite_create(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommunityInviteCreateReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Some(founder) = community_inviter(&s, &me).await else {
        return err("community steward access required");
    };
    if !s.rate_ok(&format!("community-invite:{me}"), 20, 3_600) {
        return err("invite creation rate limit exceeded");
    }
    let count = req
        .count
        .unwrap_or(1)
        .clamp(1, if founder { 20 } else { 5 });
    if !founder {
        let (outstanding,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM invites WHERE issuer=? AND used_by IS NULL")
                .bind(&me)
                .fetch_one(&s.db)
                .await
                .unwrap_or((0,));
        if outstanding + i64::from(count) > 25 {
            return err("community stewards may have at most 25 unused invites");
        }
    }
    let mut codes = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut raw = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
        let code: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
        if sqlx::query("INSERT INTO invites(code,issuer,created) VALUES(?,?,?)")
            .bind(&code)
            .bind(&me)
            .bind(now())
            .execute(&s.db)
            .await
            .is_ok()
        {
            codes.push(code);
        }
    }
    if codes.is_empty() {
        return err("failed to create invites");
    }
    crate::admin::audit(
        &s,
        &me,
        "community_invite_create",
        &format!("count={}", codes.len()),
        &ip,
    )
    .await;
    let invites = codes
        .iter()
        .map(|code| {
            json!({
                "code": code,
                "link": crate::admin::invite_link(&s, &headers, code),
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "ok": true, "codes": codes, "invites": invites }))
}

pub async fn invite_list(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if community_inviter(&s, &me).await.is_none() {
        return err("community steward access required");
    }
    let rows: Vec<(String, i64, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT i.code,i.created,a.handle,i.used_at FROM invites i \
         LEFT JOIN accounts a ON a.id=i.used_by WHERE i.issuer=? \
         ORDER BY i.created DESC LIMIT 200",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    Json(json!({
        "ok": true,
        "invites": rows.into_iter().map(|(code, created, used_handle, used_at)| json!({
            "code": code,
            "link": crate::admin::invite_link(&s, &headers, &code),
            "created": created,
            "used_handle": used_handle,
            "used_at": used_at,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct CommunityInviteRevokeReq {
    pub code: String,
}

pub async fn invite_revoke(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommunityInviteRevokeReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if community_inviter(&s, &me).await.is_none() {
        return err("community steward access required");
    }
    let deleted = sqlx::query("DELETE FROM invites WHERE code=? AND issuer=? AND used_by IS NULL")
        .bind(&req.code)
        .bind(&me)
        .execute(&s.db)
        .await;
    match deleted {
        Ok(result) if result.rows_affected() == 1 => {
            crate::admin::audit(&s, &me, "community_invite_revoke", &req.code, &ip).await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such unused invite issued by this account"),
    }
}

#[derive(Deserialize)]
pub struct ProfileSetReq {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub avatar_blob: Option<String>,
}

/// §6.1 profile_set — public plaintext, upsert.
pub async fn profile_set(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ProfileSetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if req.display_name.chars().count() > 64 || req.bio.chars().count() > 500 {
        return err("display_name max 64 chars, bio max 500");
    }
    if let Some(avatar) = &req.avatar_blob {
        let owned: Option<(String,)> = sqlx::query_as("SELECT owner FROM blobs WHERE id = ?")
            .bind(avatar)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
        if owned.map(|(o,)| o) != Some(account_id.clone()) {
            return err("avatar blob not found or not yours");
        }
    }
    let _ = sqlx::query(
        "INSERT INTO profiles (account_id, display_name, bio, avatar_blob, updated) \
         VALUES (?1,?2,?3,?4,?5) \
         ON CONFLICT(account_id) DO UPDATE SET \
           display_name = ?2, bio = ?3, avatar_blob = ?4, updated = ?5",
    )
    .bind(&account_id)
    .bind(&req.display_name)
    .bind(&req.bio)
    .bind(&req.avatar_blob)
    .bind(now())
    .execute(&s.db)
    .await;
    Json(json!({ "ok": true }))
}

// -------------------------------------------------------------- settings

/// (discoverable, auto_accept, comments_from) with defaults for accounts
/// that never touched their settings.
async fn account_settings(s: &AppState, account: &str) -> (bool, bool, String) {
    let row: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT discoverable, auto_accept, comments_from FROM account_settings \
         WHERE account_id = ?",
    )
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    match row {
        Some((d, a, c)) => (d != 0, a != 0, c),
        None => (true, false, "viewers".into()),
    }
}

pub async fn settings_get(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (discoverable, auto_accept, comments_from) = account_settings(&s, &me).await;
    Json(json!({
        "ok": true,
        "discoverable": discoverable,
        "auto_accept": auto_accept,
        "comments_from": comments_from,
    }))
}

#[derive(Deserialize)]
pub struct SettingsSetReq {
    pub discoverable: bool,
    pub auto_accept: bool,
    /// "viewers" | "followers" | "off"
    pub comments_from: String,
}

pub async fn settings_set(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SettingsSetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !matches!(req.comments_from.as_str(), "viewers" | "followers" | "off") {
        return err("comments_from must be viewers|followers|off");
    }
    let _ = sqlx::query(
        "INSERT INTO account_settings (account_id, discoverable, auto_accept, comments_from, updated) \
         VALUES (?1,?2,?3,?4,?5) \
         ON CONFLICT(account_id) DO UPDATE SET \
           discoverable = ?2, auto_accept = ?3, comments_from = ?4, updated = ?5",
    )
    .bind(&me)
    .bind(req.discoverable as i64)
    .bind(req.auto_accept as i64)
    .bind(&req.comments_from)
    .bind(now())
    .execute(&s.db)
    .await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct HandleSetReq {
    pub handle: String,
}

/// Change your handle. Same free-form rules as registration; uniqueness
/// is the UNIQUE constraint on accounts.handle.
pub async fn handle_set(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<HandleSetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let h = req.handle.trim();
    if h.is_empty() || h.chars().count() > 64 || h.chars().any(char::is_control) {
        return err("handle must be 1-64 characters");
    }
    match sqlx::query("UPDATE accounts SET handle = ? WHERE id = ?")
        .bind(h)
        .bind(&me)
        .execute(&s.db)
        .await
    {
        Ok(_) => Json(json!({ "ok": true, "handle": h })),
        Err(_) => err("handle already taken"),
    }
}

#[derive(Deserialize)]
pub struct TargetReq {
    /// Account id or handle.
    pub target: String,
}

/// §6.1 profile_get — public (any signed-in account), block-gated.
pub async fn profile_get(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (viewer, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    if blocked_between(&s, &viewer, &target).await {
        return err("no such account"); // blocks are silent (§6.2)
    }
    let row: Option<(String, i64, String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT a.handle, a.founder, COALESCE(p.display_name,''), COALESCE(p.bio,''), \
                p.avatar_blob, a.community_role, a.membership_tier \
         FROM accounts a LEFT JOIN profiles p ON p.account_id = a.id WHERE a.id = ?",
    )
    .bind(&target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((handle, founder, display_name, bio, avatar_blob, community_role, membership_tier)) = row else {
        return err("no such account");
    };
    // Relationship from the viewer's side (drives the follow button).
    let rel: Option<(String,)> =
        sqlx::query_as("SELECT state FROM follows WHERE follower = ? AND followee = ?")
            .bind(&viewer)
            .bind(&target)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    // Profile-header counts (public posts counted for strangers is fine:
    // the number of *visible* posts is enforced by author_posts anyway).
    let (followers,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM follows WHERE followee = ? AND state = 'accepted'")
            .bind(&target)
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    let (following,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM follows WHERE follower = ? AND state = 'accepted'")
            .bind(&target)
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    let (posts,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM posts WHERE author = ? AND deleted_at IS NULL")
            .bind(&target)
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    Json(json!({
        "ok": true,
        "account_id": target,
        "handle": handle,
        "founder": founder != 0,
        "community_role": community_role,
        "membership_tier": membership_tier,
        "display_name": display_name,
        "bio": bio,
        "avatar_blob": avatar_blob,
        "follow_state": rel.map(|(st,)| st),
        "followers": followers,
        "following": following,
        "posts": posts,
    }))
}

// ----------------------------------------------------------------- graph

/// §6.2 follow_request — mutual-approval by default.
pub async fn follow_request(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    if target == me {
        return err("that's you");
    }
    if blocked_between(&s, &me, &target).await {
        return err("no such account");
    }
    // Public accounts (auto_accept) skip the approval queue entirely.
    let (_, auto_accept, _) = account_settings(&s, &target).await;
    let state = if auto_accept { "accepted" } else { "requested" };
    let inserted =
        sqlx::query("INSERT INTO follows (follower, followee, state, created) VALUES (?,?,?,?)")
            .bind(&me)
            .bind(&target)
            .bind(state)
            .bind(now())
            .execute(&s.db)
            .await;
    if inserted.is_err() {
        return err("already requested or following");
    }
    if auto_accept {
        notify(&s, &target, "follow", &me, "").await;
    } else {
        notify(&s, &target, "follow_request", &me, "").await;
    }
    Json(json!({ "ok": true, "state": state }))
}

/// §6.2 follow_accept / follow_decline — the followee decides.
pub async fn follow_accept(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(follower) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    let updated = sqlx::query(
        "UPDATE follows SET state = 'accepted' \
         WHERE follower = ? AND followee = ? AND state = 'requested'",
    )
    .bind(&follower)
    .bind(&me)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {
            notify(&s, &follower, "follow_accepted", &me, "").await;
            Json(json!({ "ok": true }))
        }
        _ => err("no pending request"),
    }
}

pub async fn follow_decline(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(follower) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    let _ = sqlx::query(
        "DELETE FROM follows WHERE follower = ? AND followee = ? AND state = 'requested'",
    )
    .bind(&follower)
    .bind(&me)
    .execute(&s.db)
    .await;
    Json(json!({ "ok": true }))
}

/// Unfollow (or cancel an outgoing request).
pub async fn unfollow(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    let _ = sqlx::query("DELETE FROM follows WHERE follower = ? AND followee = ?")
        .bind(&me)
        .bind(&target)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

/// The viewer's whole graph: following, followers, and both pending sides.
pub async fn follows(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    async fn list(s: &AppState, sql: &str, me: &str) -> Vec<Value> {
        let rows: Vec<(String,)> = sqlx::query_as(sql)
            .bind(me)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
        let mut out = Vec::with_capacity(rows.len());
        for (id,) in rows {
            out.push(author_card(s, &id).await);
        }
        out
    }
    let following = list(
        &s,
        "SELECT followee FROM follows WHERE follower = ? AND state = 'accepted'",
        &me,
    )
    .await;
    let followers = list(
        &s,
        "SELECT follower FROM follows WHERE followee = ? AND state = 'accepted'",
        &me,
    )
    .await;
    let pending_in = list(
        &s,
        "SELECT follower FROM follows WHERE followee = ? AND state = 'requested'",
        &me,
    )
    .await;
    let pending_out = list(
        &s,
        "SELECT followee FROM follows WHERE follower = ? AND state = 'requested'",
        &me,
    )
    .await;
    Json(json!({
        "ok": true,
        "following": following,
        "followers": followers,
        "pending_in": pending_in,
        "pending_out": pending_out,
    }))
}

/// §6.2 block — severs follows both ways, silently.
pub async fn block(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    if target == me {
        return err("that's you");
    }
    let _ = sqlx::query("INSERT OR IGNORE INTO blocks (blocker, blocked, created) VALUES (?,?,?)")
        .bind(&me)
        .bind(&target)
        .bind(now())
        .execute(&s.db)
        .await;
    let _ = sqlx::query(
        "DELETE FROM follows WHERE (follower = ?1 AND followee = ?2) \
                                OR (follower = ?2 AND followee = ?1)",
    )
    .bind(&me)
    .bind(&target)
    .execute(&s.db)
    .await;
    Json(json!({ "ok": true }))
}

pub async fn unblock(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    let _ = sqlx::query("DELETE FROM blocks WHERE blocker = ? AND blocked = ?")
        .bind(&me)
        .bind(&target)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

/// The caller's block list (settings → Blocked users).
pub async fn blocked(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT blocked FROM blocks WHERE blocker = ? ORDER BY created DESC")
            .bind(&me)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (id,) in rows {
        out.push(author_card(&s, &id).await);
    }
    Json(json!({ "ok": true, "blocked": out }))
}

/// Full machine-readable export of the caller's Connect data (GDPR Art. 20 /
/// CCPA). Everything the server holds about the account, in one JSON blob;
/// the client saves it wherever the user wants.
pub async fn export(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let account: Option<(String, i64)> =
        sqlx::query_as("SELECT handle, created FROM accounts WHERE id = ?")
            .bind(&me)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let (handle, created) = account.unwrap_or_default();
    let profile: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT COALESCE(display_name,''), COALESCE(bio,''), avatar_blob \
         FROM profiles WHERE account_id = ?",
    )
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let posts: Vec<(String, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, kind, COALESCE(body,''), created, audience FROM posts \
         WHERE author = ? ORDER BY created",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let comments: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT id, post_id, COALESCE(body,''), created FROM comments \
         WHERE author = ? ORDER BY created",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let following: Vec<(String, String)> = sqlx::query_as(
        "SELECT f.followee, a.handle FROM follows f JOIN accounts a ON a.id = f.followee \
         WHERE f.follower = ? AND f.state = 'accepted'",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let followers: Vec<(String, String)> = sqlx::query_as(
        "SELECT f.follower, a.handle FROM follows f JOIN accounts a ON a.id = f.follower \
         WHERE f.followee = ? AND f.state = 'accepted'",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let blocked: Vec<(String,)> = sqlx::query_as("SELECT blocked FROM blocks WHERE blocker = ?")
        .bind(&me)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
    let settings: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT discoverable, auto_accept, comments_from FROM account_settings \
         WHERE account_id = ?",
    )
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    Json(json!({
        "ok": true,
        "exported_at": now(),
        "account": { "account_id": me, "handle": handle, "created": created },
        "profile": profile.map(|(dn, bio, av)| json!({
            "display_name": dn, "bio": bio, "avatar_blob": av,
        })),
        "settings": settings.map(|(d, a, c)| json!({
            "discoverable": d != 0, "auto_accept": a != 0, "comments_from": c,
        })),
        "posts": posts.iter().map(|(id, kind, body, created, audience)| json!({
            "id": id, "kind": kind, "body": body, "created": created, "audience": audience,
        })).collect::<Vec<_>>(),
        "comments": comments.iter().map(|(id, post_id, body, created)| json!({
            "id": id, "post_id": post_id, "body": body, "created": created,
        })).collect::<Vec<_>>(),
        "following": following.iter().map(|(id, h)| json!({ "account_id": id, "handle": h })).collect::<Vec<_>>(),
        "followers": followers.iter().map(|(id, h)| json!({ "account_id": id, "handle": h })).collect::<Vec<_>>(),
        "blocked": blocked.iter().map(|(id,)| id).collect::<Vec<_>>(),
    }))
}

// ----------------------------------------------------------------- posts

#[derive(Deserialize)]
pub struct PostCreateReq {
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub body: String,
    /// Blob ids (already uploaded). Ciphertext for circle posts.
    #[serde(default)]
    pub media: Vec<String>,
    #[serde(default)]
    pub media_types: Vec<String>,
    #[serde(default)]
    pub alt_text: String,
    #[serde(default)]
    pub content_warning: String,
    /// 'public' | 'followers' | 'circle:<id>'.
    #[serde(default = "default_audience")]
    pub audience: String,
    #[serde(default)]
    pub fold_epoch: Option<i64>,
}

fn default_kind() -> String {
    "text".into()
}
fn default_audience() -> String {
    "followers".into()
}

async fn active_mute(s: &AppState, account_id: &str) -> Option<i64> {
    sqlx::query_as::<_, (i64,)>("SELECT muted_until FROM accounts WHERE id = ? AND muted_until > ?")
        .bind(account_id)
        .bind(now())
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None)
        .map(|(until,)| until)
}

/// §6.3 post_create.
pub async fn post_create(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostCreateReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Some(until) = active_mute(&s, &me).await {
        return err(&format!("account muted from posting until {until}"));
    }
    if !s.rate_ok(&format!("post:{me}"), 30, 600) {
        return err("rate limited, try again later");
    }
    if !matches!(req.kind.as_str(), "text" | "photo" | "video") {
        return err("kind must be text, photo, or video");
    }
    if req.body.len() > POST_BODY_MAX {
        return err("post body too long");
    }
    if req.media.len() > MEDIA_PER_POST_MAX {
        return err("too many media attachments");
    }
    let media_types = if req.media_types.is_empty() {
        vec!["image/jpeg".to_string(); req.media.len()]
    } else {
        req.media_types.clone()
    };
    if media_types.len() != req.media.len()
        || media_types.iter().any(|mime| {
            mime.len() > 100 || !(mime.starts_with("image/") || mime.starts_with("video/"))
        })
    {
        return err("media types must match image or video attachments");
    }
    if req.kind == "video" && !media_types.iter().any(|mime| mime.starts_with("video/")) {
        return err("video posts require video media");
    }
    if req.alt_text.chars().count() > 2_000 || req.content_warning.chars().count() > 200 {
        return err("post accessibility metadata is too long");
    }
    match req.audience.as_str() {
        "public" | "followers" => {}
        a => match a.strip_prefix("circle:") {
            Some(circle_id) => {
                let Some(epoch) = fold_epoch_for_member(&s, circle_id, &me).await else {
                    return err("not an active Fold member");
                };
                if req.fold_epoch != Some(epoch) || !valid_fold_envelope(&req.body, epoch) {
                    return err("Fold content must use the current encrypted key epoch");
                }
            }
            None => return err("bad audience"),
        },
    }
    // Media blobs must exist, be yours, and get a reference.
    for id in &req.media {
        let owned: Option<(String,)> = sqlx::query_as("SELECT owner FROM blobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
        if owned.map(|(o,)| o) != Some(me.clone()) {
            return err("media blob not found or not yours");
        }
    }
    let post_id = new_id();
    let created = now();
    let media_json = serde_json::to_string(&req.media).unwrap_or_else(|_| "[]".into());
    let media_types_json = serde_json::to_string(&media_types).unwrap_or_else(|_| "[]".into());
    let stored_kind = if req.kind == "video" {
        "photo"
    } else {
        req.kind.as_str()
    };
    let inserted = sqlx::query(
        "INSERT INTO posts \
         (id, author, created, kind, body, media, audience, media_types, alt_text, content_warning, fold_epoch) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&post_id)
    .bind(&me)
    .bind(created)
    .bind(stored_kind)
    .bind(&req.body)
    .bind(&media_json)
    .bind(&req.audience)
    .bind(&media_types_json)
    .bind(req.alt_text.trim())
    .bind(req.content_warning.trim())
    .bind(req.fold_epoch)
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("post failed");
    }
    for id in &req.media {
        blob::add_ref(&s, id, 1).await;
    }
    if let Some(circle_id) = req.audience.strip_prefix("circle:") {
        let members: Vec<(String,)> = sqlx::query_as(
            "SELECT account_id FROM fold_members \
             WHERE circle_id=? AND state='active' AND account_id<>?",
        )
        .bind(circle_id)
        .bind(&me)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
        for (member,) in members {
            notify(&s, &member, "fold_post", &me, &post_id).await;
        }
    } else {
        // §6.3 fan-out hint to followers who may view it (badges, not content).
        for follower in follower_ids(&s, &me).await {
            if can_view(&s, &follower, &me, &req.audience).await {
                notify(&s, &follower, "post", &me, &post_id).await;
            }
        }
        notify_mentions(&s, &req.body, &me, &me, &req.audience, &post_id).await;
    }
    Json(json!({ "ok": true, "post_id": post_id, "created": created }))
}

#[derive(Deserialize)]
pub struct PostIdReq {
    pub post_id: String,
}

#[derive(Deserialize)]
pub struct PostEditReq {
    pub post_id: String,
    pub body: String,
}

/// Right to correction: edit your own post's text in place. Media and
/// audience are fixed (delete + repost to change those). Mention
/// notifications are NOT re-sent on edit to prevent notification spam.
pub async fn post_edit(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostEditReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if req.body.len() > POST_BODY_MAX {
        return err("post body too long");
    }
    let audience: Option<(String,)> = sqlx::query_as(
        "SELECT audience FROM posts WHERE id = ? AND author = ? AND deleted_at IS NULL",
    )
    .bind(&req.post_id)
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if matches!(audience, Some((ref value,)) if value.starts_with("circle:")) {
        return err("fold editing unavailable until end-to-end encryption is enabled");
    }
    let replaced_at = now();
    let updated = async {
        let mut tx = s.db.begin().await?;
        let current: Option<(String,)> = sqlx::query_as(
            "SELECT body FROM posts WHERE id = ? AND author = ? AND deleted_at IS NULL",
        )
        .bind(&req.post_id)
        .bind(&me)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((current_body,)) = current else {
            return Err(sqlx::Error::RowNotFound);
        };
        sqlx::query("INSERT INTO post_revisions (post_id, body, replaced_at) VALUES (?,?,?)")
            .bind(&req.post_id)
            .bind(current_body)
            .bind(replaced_at)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE posts SET body = ?, edited = ? WHERE id = ?")
            .bind(&req.body)
            .bind(replaced_at)
            .bind(&req.post_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    match updated {
        Ok(()) => Json(json!({ "ok": true })),
        Err(_) => err("no such post or not yours"),
    }
}

#[derive(Deserialize)]
pub struct CommentEditReq {
    pub comment_id: String,
    pub body: String,
}

/// Right to correction for comments.
pub async fn comment_edit(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommentEditReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if req.body.is_empty() || req.body.len() > COMMENT_BODY_MAX {
        return err("comment must be 1-2000 bytes");
    }
    let fold_comment: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM comments c JOIN posts p ON p.id = c.post_id \
         WHERE c.id = ? AND c.author = ? AND c.deleted_at IS NULL \
           AND p.audience LIKE 'circle:%'",
    )
    .bind(&req.comment_id)
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if fold_comment.is_some() {
        return err("fold editing unavailable until end-to-end encryption is enabled");
    }
    let updated = sqlx::query(
        "UPDATE comments SET body = ?, edited = ? \
         WHERE id = ? AND author = ? AND deleted_at IS NULL",
    )
    .bind(&req.body)
    .bind(now())
    .bind(&req.comment_id)
    .bind(&me)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => Json(json!({ "ok": true })),
        _ => err("no such comment or not yours"),
    }
}

/// §6.3: deletes are real — row deletion + blob deref.
pub async fn post_delete(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT author, media, audience FROM posts WHERE id = ?")
            .bind(&req.post_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((author, media, audience)) = row else {
        return err("no such post");
    };
    let owns_fold = if let Some(circle_id) = audience.strip_prefix("circle:") {
        sqlx::query_as::<_, (i64,)>("SELECT 1 FROM circles WHERE id=? AND owner=?")
            .bind(circle_id)
            .bind(&me)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None)
            .is_some()
    } else {
        false
    };
    if author != me && !owns_fold {
        return err("not your post");
    }
    // Evidence preservation: reported content and anything by an account
    // under evidence_hold (ToS forfeiture) cannot be destroyed by its
    // author — the delete becomes a tombstone and the record survives.
    if must_preserve(&s, "post", &req.post_id, &author).await {
        let _ = sqlx::query("UPDATE posts SET deleted_at = COALESCE(deleted_at, ?) WHERE id = ?")
            .bind(now())
            .bind(&req.post_id)
            .execute(&s.db)
            .await;
        return Json(json!({ "ok": true }));
    }
    let deleted = async {
        let mut tx = s.db.begin().await?;
        sqlx::query(
            "DELETE FROM comment_reactions WHERE comment_id IN \
             (SELECT id FROM comments WHERE post_id = ?)",
        )
        .bind(&req.post_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM post_reactions WHERE post_id = ?")
            .bind(&req.post_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE comments SET parent_id = NULL WHERE post_id = ?")
            .bind(&req.post_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM comments WHERE post_id = ?")
            .bind(&req.post_id)
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM posts WHERE id = ?")
            .bind(&req.post_id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        tx.commit().await
    }
    .await;
    if deleted.is_err() {
        return err("could not delete post");
    }
    for id in serde_json::from_str::<Vec<String>>(&media).unwrap_or_default() {
        blob::add_ref(&s, &id, -1).await;
    }
    Json(json!({ "ok": true }))
}

#[derive(sqlx::FromRow)]
struct RenderPostRow {
    id: String,
    author: String,
    created: i64,
    kind: String,
    body: String,
    media: String,
    audience: String,
    edited: Option<i64>,
    media_types: String,
    alt_text: String,
    content_warning: String,
    pinned: i64,
    handle: String,
    display_name: String,
    avatar_blob: Option<String>,
    reactions: i64,
    comment_count: i64,
    my_reaction: Option<String>,
    saved: i64,
}

fn render_post(row: RenderPostRow) -> Value {
    json!({
        "post_id": row.id,
        "author": {
            "account_id": row.author,
            "handle": row.handle,
            "display_name": row.display_name,
            "avatar_blob": row.avatar_blob,
        },
        "created": row.created,
        "kind": if row.kind == "photo" && row.media_types.contains("video/") { "video" } else { &row.kind },
        "body": row.body,
        "edited": row.edited,
        "media": serde_json::from_str::<Value>(&row.media).unwrap_or_else(|_| json!([])),
        "media_types": serde_json::from_str::<Value>(&row.media_types).unwrap_or_else(|_| json!([])),
        "alt_text": row.alt_text,
        "content_warning": row.content_warning,
        "pinned": row.pinned != 0,
        "saved": row.saved != 0,
        "audience": row.audience,
        "reactions": row.reactions,
        "my_reaction": row.my_reaction,
        "comments": row.comment_count,
    })
}

#[derive(Deserialize)]
pub struct PostGetReq {
    pub post_id: String,
}

/// Fetch a single post (for permalinks / opening a post from an alert).
/// Same visibility rules as the feed: audience + block checks re-verified.
pub async fn post_get(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostGetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let row: Option<RenderPostRow> =
        sqlx::query_as(
                "SELECT p.id, p.author, p.created, p.kind, p.body, p.media, p.audience, p.edited, \
                    p.media_types, p.alt_text, p.content_warning, p.pinned, \
                    a.handle, COALESCE(pr.display_name, '') AS display_name, pr.avatar_blob, \
                    (SELECT COUNT(*) FROM post_reactions r WHERE r.post_id=p.id) AS reactions, \
                    (SELECT COUNT(*) FROM comments c WHERE c.post_id=p.id AND c.deleted_at IS NULL) AS comment_count, \
                    (SELECT kind FROM post_reactions r WHERE r.post_id=p.id AND r.account_id=?3) AS my_reaction, \
                    EXISTS(SELECT 1 FROM saved_posts sp WHERE sp.post_id=p.id AND sp.account_id=?3) AS saved \
             FROM posts p JOIN accounts a ON a.id=p.author \
             LEFT JOIN profiles pr ON pr.account_id=p.author \
             WHERE p.id = ?1 AND p.deleted_at IS NULL \
               AND p.author NOT IN (SELECT blocked FROM blocks WHERE blocker = ?2) \
               AND p.author NOT IN (SELECT blocker FROM blocks WHERE blocked = ?2)",
        )
        .bind(&req.post_id)
        .bind(&me)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    let Some(row) = row else {
        return err("no such post");
    };
    if !can_view(&s, &me, &row.author, &row.audience).await {
        return err("no such post");
    }
    let post = render_post(row);
    Json(json!({ "ok": true, "post": post }))
}

#[derive(Deserialize)]
pub struct FeedReq {
    /// "{created}:{post_id}" from a previous page; omit for newest.
    #[serde(default)]
    pub before_cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// §6.3 feed — chronological pull from accepted-follow authors + self.
pub async fn feed(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FeedReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let limit = req.limit.unwrap_or(30).clamp(1, FEED_LIMIT_MAX);
    let (cur_created, cur_id) = match &req.before_cursor {
        Some(c) => match c.split_once(':') {
            Some((t, id)) => (t.parse::<i64>().unwrap_or(i64::MAX), id.to_string()),
            None => return err("bad cursor"),
        },
        None => (i64::MAX, String::new()),
    };
    // Authors = me + accepted followees; blocks re-checked in SQL (§6.2).
    // Circle-audience visibility is a JSON membership check, done in Rust —
    // we over-fetch a page and filter.
    let rows: Vec<RenderPostRow> = match sqlx::query_as(
        "SELECT p.id, p.author, p.created, p.kind, p.body, p.media, p.audience, p.edited, \
            p.media_types, p.alt_text, p.content_warning, p.pinned, \
                a.handle, COALESCE(pr.display_name, '') AS display_name, pr.avatar_blob, \
                (SELECT COUNT(*) FROM post_reactions r WHERE r.post_id=p.id) AS reactions, \
                (SELECT COUNT(*) FROM comments c WHERE c.post_id=p.id AND c.deleted_at IS NULL) AS comment_count, \
                (SELECT kind FROM post_reactions r WHERE r.post_id=p.id AND r.account_id=?5) AS my_reaction, \
                EXISTS(SELECT 1 FROM saved_posts sp WHERE sp.post_id=p.id AND sp.account_id=?5) AS saved \
         FROM posts p JOIN accounts a ON a.id=p.author \
         LEFT JOIN profiles pr ON pr.account_id=p.author \
         WHERE p.deleted_at IS NULL \
                     AND p.audience NOT LIKE 'circle:%' \
           AND (p.author = ?1 OR p.author IN \
                (SELECT followee FROM follows WHERE follower = ?1 AND state = 'accepted')) \
           AND p.author NOT IN (SELECT blocked FROM blocks WHERE blocker = ?1) \
           AND p.author NOT IN (SELECT blocker FROM blocks WHERE blocked = ?1) \
           AND (p.created < ?2 OR (p.created = ?2 AND p.id < ?3)) \
         ORDER BY p.created DESC, p.id DESC LIMIT ?4",
    )
    .bind(&me)
    .bind(cur_created)
    .bind(&cur_id)
    .bind(limit * 2)
    .bind(&me)
    .fetch_all(&s.db)
    .await {
        Ok(rows) => rows,
        Err(error) => return err(&format!("Fold feed failed: {error}")),
    };

    let mut out = Vec::new();
    let mut cursor = None;
    for row in rows {
        cursor = Some(format!("{}:{}", row.created, row.id));
        if !can_view(&s, &me, &row.author, &row.audience).await {
            continue;
        }
        out.push(render_post(row));
        if out.len() as i64 >= limit {
            break;
        }
    }
    Json(json!({ "ok": true, "posts": out, "next_cursor": cursor }))
}

#[derive(Deserialize)]
pub struct FoldFeedReq {
    pub circle_id: String,
    #[serde(default)]
    pub before_cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn fold_feed(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldFeedReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let membership: Option<(i64,)> = sqlx::query_as(
        "SELECT joined_at FROM fold_members \
         WHERE circle_id=? AND account_id=? AND state='active'",
    )
    .bind(&req.circle_id)
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((joined_at,)) = membership else {
        return err("not an active Fold member");
    };
    let limit = req.limit.unwrap_or(30).clamp(1, FEED_LIMIT_MAX);
    let (cur_created, cur_id) = match &req.before_cursor {
        Some(cursor) => match cursor.split_once(':') {
            Some((created, id)) => (
                created.parse::<i64>().unwrap_or(i64::MAX),
                id.to_string(),
            ),
            None => return err("bad cursor"),
        },
        None => (i64::MAX, String::new()),
    };
    let audience = format!("circle:{}", req.circle_id);
    let rows: Vec<RenderPostRow> = sqlx::query_as(
        "SELECT p.id,p.author,p.created,p.kind,p.body,p.media,p.audience,p.edited, \
                p.media_types,p.alt_text,p.content_warning,p.pinned, \
                a.handle,COALESCE(pr.display_name,'') AS display_name,pr.avatar_blob, \
                (SELECT COUNT(*) FROM post_reactions r WHERE r.post_id=p.id) AS reactions, \
                (SELECT COUNT(*) FROM comments c WHERE c.post_id=p.id AND c.deleted_at IS NULL) AS comment_count, \
                (SELECT kind FROM post_reactions r WHERE r.post_id=p.id AND r.account_id=?) AS my_reaction, \
                EXISTS(SELECT 1 FROM saved_posts sp WHERE sp.post_id=p.id AND sp.account_id=?) AS saved \
         FROM posts p JOIN accounts a ON a.id=p.author \
         LEFT JOIN profiles pr ON pr.account_id=p.author \
                 WHERE p.audience=? AND p.deleted_at IS NULL AND p.created>=? \
                     AND p.author NOT IN (SELECT blocked FROM blocks WHERE blocker=?) \
                     AND p.author NOT IN (SELECT blocker FROM blocks WHERE blocked=?) \
                     AND (p.created<? OR (p.created=? AND p.id<?)) \
                 ORDER BY p.created DESC,p.id DESC LIMIT ?",
    )
        .bind(&me)
        .bind(&me)
    .bind(&audience)
    .bind(joined_at)
        .bind(&me)
        .bind(&me)
    .bind(cur_created)
        .bind(cur_created)
    .bind(&cur_id)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let next_cursor = rows
        .last()
        .map(|row| format!("{}:{}", row.created, row.id));
    Json(json!({
        "ok": true,
        "posts": rows.into_iter().map(render_post).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    }))
}

#[derive(Deserialize)]
pub struct AuthorPostsReq {
    /// Account id or handle.
    pub target: String,
    #[serde(default)]
    pub before_cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Profile page: one author's posts, audience-gated per post.
pub async fn author_posts(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AuthorPostsReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    if blocked_between(&s, &me, &target).await {
        return err("no such account");
    }
    let limit = req.limit.unwrap_or(30).clamp(1, FEED_LIMIT_MAX);
    let (cur_created, cur_id) = match &req.before_cursor {
        Some(c) => match c.split_once(':') {
            Some((t, id)) => (t.parse::<i64>().unwrap_or(i64::MAX), id.to_string()),
            None => return err("bad cursor"),
        },
        None => (i64::MAX, String::new()),
    };
    let rows: Vec<RenderPostRow> = sqlx::query_as(
                "SELECT p.id, p.author, p.created, p.kind, p.body, p.media, p.audience, p.edited, \
                    p.media_types, p.alt_text, p.content_warning, p.pinned, \
                                a.handle, COALESCE(pr.display_name, '') AS display_name, pr.avatar_blob, \
                                (SELECT COUNT(*) FROM post_reactions r WHERE r.post_id=p.id) AS reactions, \
                                (SELECT COUNT(*) FROM comments c WHERE c.post_id=p.id AND c.deleted_at IS NULL) AS comment_count, \
                                (SELECT kind FROM post_reactions r WHERE r.post_id=p.id AND r.account_id=?5) AS my_reaction, \
                                EXISTS(SELECT 1 FROM saved_posts sp WHERE sp.post_id=p.id AND sp.account_id=?5) AS saved \
                 FROM posts p JOIN accounts a ON a.id=p.author \
                 LEFT JOIN profiles pr ON pr.account_id=p.author \
                 WHERE p.author = ?1 AND p.deleted_at IS NULL \
                     AND p.audience NOT LIKE 'circle:%' \
                     AND (p.created < ?2 OR (p.created = ?2 AND p.id < ?3)) \
                 ORDER BY p.created DESC, p.id DESC LIMIT ?4",
    )
    .bind(&target)
    .bind(cur_created)
    .bind(&cur_id)
    .bind(limit * 2)
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    let mut cursor = None;
    for row in rows {
        cursor = Some(format!("{}:{}", row.created, row.id));
        if !can_view(&s, &me, &row.author, &row.audience).await {
            continue;
        }
        out.push(render_post(row));
        if out.len() as i64 >= limit {
            break;
        }
    }
    Json(json!({ "ok": true, "posts": out, "next_cursor": cursor }))
}

pub async fn post_save(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = post_gate(&s, &me, &req.post_id).await {
        return e;
    }
    match sqlx::query(
        "INSERT INTO saved_posts (account_id, post_id, created) VALUES (?,?,?) \
         ON CONFLICT(account_id, post_id) DO UPDATE SET created=excluded.created",
    )
    .bind(&me)
    .bind(&req.post_id)
    .bind(now())
    .execute(&s.db)
    .await
    {
        Ok(_) => Json(json!({ "ok": true })),
        Err(_) => err("post could not be saved"),
    }
}

pub async fn post_unsave(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    match sqlx::query("DELETE FROM saved_posts WHERE account_id=? AND post_id=?")
        .bind(&me)
        .bind(&req.post_id)
        .execute(&s.db)
        .await
    {
        Ok(_) => Json(json!({ "ok": true })),
        Err(_) => err("saved post could not be removed"),
    }
}

pub async fn saved_posts(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let rows: Vec<RenderPostRow> = sqlx::query_as(
        "SELECT p.id, p.author, p.created, p.kind, p.body, p.media, p.audience, p.edited, \
                p.media_types, p.alt_text, p.content_warning, p.pinned, \
                a.handle, COALESCE(pr.display_name, '') AS display_name, pr.avatar_blob, \
                (SELECT COUNT(*) FROM post_reactions r WHERE r.post_id=p.id) AS reactions, \
                (SELECT COUNT(*) FROM comments c WHERE c.post_id=p.id AND c.deleted_at IS NULL) AS comment_count, \
                (SELECT kind FROM post_reactions r WHERE r.post_id=p.id AND r.account_id=?1) AS my_reaction, \
                1 AS saved \
         FROM saved_posts sp JOIN posts p ON p.id=sp.post_id \
         JOIN accounts a ON a.id=p.author LEFT JOIN profiles pr ON pr.account_id=p.author \
         WHERE sp.account_id=?1 AND p.deleted_at IS NULL \
                     AND p.audience NOT LIKE 'circle:%' \
           AND p.author NOT IN (SELECT blocked FROM blocks WHERE blocker=?1) \
           AND p.author NOT IN (SELECT blocker FROM blocks WHERE blocked=?1) \
         ORDER BY sp.created DESC LIMIT 100",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    for row in rows {
        if can_view(&s, &me, &row.author, &row.audience).await {
            out.push(render_post(row));
        }
    }
    Json(json!({ "ok": true, "posts": out }))
}

#[derive(Deserialize)]
pub struct PostPinReq {
    pub post_id: String,
    #[serde(default = "default_true")]
    pub pinned: bool,
}

fn default_true() -> bool {
    true
}

pub async fn post_pin(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostPinReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let result = async {
        let mut tx = s.db.begin().await?;
        if req.pinned {
            sqlx::query("UPDATE posts SET pinned=0 WHERE author=?")
                .bind(&me)
                .execute(&mut *tx)
                .await?;
        }
        let updated =
            sqlx::query("UPDATE posts SET pinned=? WHERE id=? AND author=? AND deleted_at IS NULL")
                .bind(if req.pinned { 1 } else { 0 })
                .bind(&req.post_id)
                .bind(&me)
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        tx.commit().await
    }
    .await;
    match result {
        Ok(()) => Json(json!({ "ok": true })),
        Err(_) => err("no such post or not yours"),
    }
}

pub async fn post_revisions(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = post_gate(&s, &me, &req.post_id).await {
        return e;
    }
    let revisions: Vec<(String, i64)> = sqlx::query_as(
        "SELECT body, replaced_at FROM post_revisions WHERE post_id=? ORDER BY replaced_at DESC",
    )
    .bind(&req.post_id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    Json(json!({
        "ok": true,
        "revisions": revisions.into_iter().map(|(body, replaced_at)| {
            json!({ "body": body, "replaced_at": replaced_at })
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct PostSearchReq {
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn post_search(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostSearchReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !s.rate_ok(&format!("post-search:{me}"), 60, 60) {
        return err("rate limited, try again later");
    }
    let q = req.q.trim().trim_start_matches('#');
    if q.is_empty() || q.chars().count() > 100 {
        return err("query must be 1-100 chars");
    }
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let limit = req.limit.unwrap_or(30).clamp(1, 50);
    let rows: Vec<RenderPostRow> = sqlx::query_as(
        "SELECT p.id, p.author, p.created, p.kind, p.body, p.media, p.audience, p.edited, \
                p.media_types, p.alt_text, p.content_warning, p.pinned, \
                a.handle, COALESCE(pr.display_name, '') AS display_name, pr.avatar_blob, \
                (SELECT COUNT(*) FROM post_reactions r WHERE r.post_id=p.id) AS reactions, \
                (SELECT COUNT(*) FROM comments c WHERE c.post_id=p.id AND c.deleted_at IS NULL) AS comment_count, \
                (SELECT kind FROM post_reactions r WHERE r.post_id=p.id AND r.account_id=?1) AS my_reaction, \
                EXISTS(SELECT 1 FROM saved_posts sp WHERE sp.post_id=p.id AND sp.account_id=?1) AS saved \
         FROM posts p JOIN accounts a ON a.id=p.author \
         LEFT JOIN profiles pr ON pr.account_id=p.author \
                 WHERE p.deleted_at IS NULL AND p.audience NOT LIKE 'circle:%' \
                     AND p.body LIKE ?2 ESCAPE '\\' \
           AND p.author NOT IN (SELECT blocked FROM blocks WHERE blocker=?1) \
           AND p.author NOT IN (SELECT blocker FROM blocks WHERE blocked=?1) \
         ORDER BY p.created DESC LIMIT ?3",
    )
    .bind(&me)
    .bind(&pattern)
    .bind(limit * 3)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    for row in rows {
        if can_view(&s, &me, &row.author, &row.audience).await {
            out.push(render_post(row));
            if out.len() as i64 >= limit {
                break;
            }
        }
    }
    Json(json!({ "ok": true, "posts": out }))
}

// -------------------------------------------- comments & reactions (§6.3)

#[derive(Deserialize)]
pub struct CommentCreateReq {
    pub post_id: String,
    pub body: String,
    /// Reply to another comment on the same post (one level deep).
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub fold_epoch: Option<i64>,
}

/// Comment visibility = the post's audience; a comment is a row with the
/// same gate as its post.
async fn post_gate(
    s: &AppState,
    viewer: &str,
    post_id: &str,
) -> Result<(String, String), Json<Value>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT author, audience FROM posts WHERE id = ? AND deleted_at IS NULL")
            .bind(post_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((author, audience)) = row else {
        return Err(err("no such post"));
    };
    if !can_view(s, viewer, &author, &audience).await {
        return Err(err("no such post")); // invisible, not forbidden
    }
    Ok((author, audience))
}

pub async fn comment_create(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommentCreateReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Some(until) = active_mute(&s, &me).await {
        return err(&format!("account muted from commenting until {until}"));
    }
    if !s.rate_ok(&format!("comment:{me}"), 60, 600) {
        return err("rate limited, try again later");
    }
    if req.body.is_empty() || req.body.len() > COMMENT_BODY_MAX {
        return err("comment must be 1-2000 bytes");
    }
    let (author, audience) = match post_gate(&s, &me, &req.post_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let fold_comment = audience.starts_with("circle:");
    if let Some(circle_id) = audience.strip_prefix("circle:") {
        let Some(epoch) = fold_epoch_for_member(&s, circle_id, &me).await else {
            return err("not an active Fold member");
        };
        if req.fold_epoch != Some(epoch) || !valid_fold_envelope(&req.body, epoch) {
            return err("Fold comment must use the current encrypted key epoch");
        }
    }
    if !fold_comment {
        // Public/profile post author's comment policy (§6.1 settings).
        let (_, _, comments_from) = account_settings(&s, &author).await;
        match comments_from.as_str() {
            "off" if me != author => return err("comments are off on this account's posts"),
            "followers" if me != author && !is_accepted_follower(&s, &me, &author).await => {
                return err("only followers can comment on this account's posts")
            }
            _ => {}
        }
    }
    // Replies stay one level deep and on the same post.
    let mut parent_author: Option<String> = None;
    if let Some(pid) = &req.parent_id {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT post_id, author, parent_id FROM comments \
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(pid)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
        match row {
            Some((post, ca, None)) if post == req.post_id => parent_author = Some(ca),
            Some((_, _, Some(_))) => return err("replies go one level deep"),
            _ => return err("no such comment"),
        }
    }
    let comment_id = new_id();
    let inserted = sqlx::query(
        "INSERT INTO comments (id, post_id, author, body, created, parent_id, fold_epoch) \
         VALUES (?,?,?,?,?,?,?)",
    )
    .bind(&comment_id)
    .bind(&req.post_id)
    .bind(&me)
    .bind(&req.body)
    .bind(now())
    .bind(&req.parent_id)
    .bind(req.fold_epoch)
    .execute(&s.db)
    .await;
    if !matches!(inserted, Ok(ref result) if result.rows_affected() == 1) {
        return err("comment could not be saved");
    }
    notify(&s, &author, "comment", &me, &req.post_id).await;
    // Tell the parent commenter too (dedup: skip when they own the post).
    if let Some(pa) = parent_author {
        if pa != author {
            notify(&s, &pa, "comment", &me, &req.post_id).await;
        }
    }
    // @mentions in the comment — audience-checked against the host post.
    let audience: String =
        sqlx::query_as::<_, (String,)>("SELECT audience FROM posts WHERE id = ?")
            .bind(&req.post_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None)
            .map(|(a,)| a)
            .unwrap_or_else(|| "followers".into());
    if !fold_comment {
        notify_mentions(&s, &req.body, &me, &author, &audience, &req.post_id).await;
    }
    Json(json!({ "ok": true, "comment_id": comment_id }))
}

#[derive(Deserialize)]
pub struct CommentDeleteReq {
    pub comment_id: String,
}

/// Delete your own comment, or any comment on your own post.
pub async fn comment_delete(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommentDeleteReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT c.author, p.author, p.audience FROM comments c JOIN posts p ON p.id = c.post_id WHERE c.id = ?",
    )
    .bind(&req.comment_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((comment_author, post_author, audience)) = row else {
        return err("no such comment");
    };
    let owns_fold = if let Some(circle_id) = audience.strip_prefix("circle:") {
        sqlx::query_as::<_, (i64,)>("SELECT 1 FROM circles WHERE id=? AND owner=?")
            .bind(circle_id)
            .bind(&me)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None)
            .is_some()
    } else {
        false
    };
    if comment_author != me && post_author != me && !owns_fold {
        return err("not yours to delete");
    }
    // Evidence preservation: see post_delete.
    if must_preserve(&s, "comment", &req.comment_id, &comment_author).await {
        let preserved =
            sqlx::query("UPDATE comments SET deleted_at = COALESCE(deleted_at, ?) WHERE id = ?")
                .bind(now())
                .bind(&req.comment_id)
                .execute(&s.db)
                .await;
        return match preserved {
            Ok(result) if result.rows_affected() == 1 => Json(json!({ "ok": true })),
            _ => err("could not preserve deleted comment"),
        };
    }
    let deleted = async {
        let mut tx = s.db.begin().await?;
        sqlx::query("DELETE FROM comment_reactions WHERE comment_id = ?")
            .bind(&req.comment_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE comments SET parent_id = NULL WHERE parent_id = ?")
            .bind(&req.comment_id)
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM comments WHERE id = ?")
            .bind(&req.comment_id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        tx.commit().await
    }
    .await;
    match deleted {
        Ok(()) => Json(json!({ "ok": true })),
        Err(_) => err("could not delete comment"),
    }
}

/// True when content must survive its author's delete: it is the subject
/// of a report (snapshot evidence) or the author is under evidence_hold.
async fn must_preserve(s: &AppState, kind: &str, id: &str, author: &str) -> bool {
    let reported: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM reports WHERE subject_kind = ? AND subject_id = ? LIMIT 1")
            .bind(kind)
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    if reported.is_some() {
        return true;
    }
    let held: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM accounts WHERE id = ? AND evidence_hold IS NOT NULL")
            .bind(author)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    held.is_some()
}

pub async fn comments(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = post_gate(&s, &me, &req.post_id).await {
        return e;
    }
    let rows: Vec<(String, String, String, i64, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT id, author, body, created, parent_id, edited FROM comments \
         WHERE post_id = ? AND deleted_at IS NULL \
           AND author NOT IN (SELECT blocked FROM blocks WHERE blocker = ?2) \
           AND author NOT IN (SELECT blocker FROM blocks WHERE blocked = ?2) \
         ORDER BY created ASC",
    )
    .bind(&req.post_id)
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (id, author, body, created, parent_id, edited) in rows {
        let (reactions,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM comment_reactions WHERE comment_id = ?")
                .bind(&id)
                .fetch_one(&s.db)
                .await
                .unwrap_or((0,));
        let mine: Option<(String,)> = sqlx::query_as(
            "SELECT kind FROM comment_reactions WHERE comment_id = ? AND account_id = ?",
        )
        .bind(&id)
        .bind(&me)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
        out.push(json!({
            "comment_id": id,
            "author": author_card(&s, &author).await,
            "body": body,
            "created": created,
            "parent_id": parent_id,
            "edited": edited,
            "reactions": reactions,
            "my_reaction": mine.map(|(k,)| k),
        }));
    }
    Json(json!({ "ok": true, "comments": out }))
}

#[derive(Deserialize)]
pub struct CommentReactReq {
    pub comment_id: String,
    #[serde(default = "default_react")]
    pub kind: String,
}

/// Comment gate: the comment exists and its post is visible to the viewer.
/// Returns (comment_author, post_id).
async fn comment_gate(
    s: &AppState,
    viewer: &str,
    comment_id: &str,
) -> Result<(String, String), Json<Value>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT author, post_id FROM comments WHERE id = ? AND deleted_at IS NULL")
            .bind(comment_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((author, post_id)) = row else {
        return Err(err("no such comment"));
    };
    post_gate(s, viewer, &post_id).await?;
    Ok((author, post_id))
}

pub async fn comment_react(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommentReactReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !REACTION_KINDS.contains(&req.kind.as_str()) {
        return err("unsupported reaction kind");
    }
    if !s.rate_ok(&format!("reaction:{me}"), 180, 60) {
        return err("reaction rate limit exceeded");
    }
    let (author, post_id) = match comment_gate(&s, &me, &req.comment_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let updated = sqlx::query(
        "INSERT INTO comment_reactions (comment_id, account_id, kind, created) \
         VALUES (?1,?2,?3,?4) \
         ON CONFLICT(comment_id, account_id) DO UPDATE SET kind = ?3, created = ?4",
    )
    .bind(&req.comment_id)
    .bind(&me)
    .bind(&req.kind)
    .bind(now())
    .execute(&s.db)
    .await;
    if updated.is_err() {
        return err("reaction could not be saved");
    }
    notify(&s, &author, "reaction", &me, &post_id).await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct CommentIdReq {
    pub comment_id: String,
}

pub async fn comment_unreact(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommentIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let deleted =
        sqlx::query("DELETE FROM comment_reactions WHERE comment_id = ? AND account_id = ?")
            .bind(&req.comment_id)
            .bind(&me)
            .execute(&s.db)
            .await;
    match deleted {
        Ok(_) => Json(json!({ "ok": true })),
        Err(_) => err("reaction could not be removed"),
    }
}

#[derive(Deserialize)]
pub struct ReactReq {
    pub post_id: String,
    #[serde(default = "default_react")]
    pub kind: String,
}

fn default_react() -> String {
    "like".into()
}

pub async fn react(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ReactReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !REACTION_KINDS.contains(&req.kind.as_str()) {
        return err("unsupported reaction kind");
    }
    if !s.rate_ok(&format!("reaction:{me}"), 180, 60) {
        return err("reaction rate limit exceeded");
    }
    let (author, _) = match post_gate(&s, &me, &req.post_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let updated = sqlx::query(
        "INSERT INTO post_reactions (post_id, account_id, kind, created) VALUES (?1,?2,?3,?4) \
         ON CONFLICT(post_id, account_id) DO UPDATE SET kind = ?3",
    )
    .bind(&req.post_id)
    .bind(&me)
    .bind(&req.kind)
    .bind(now())
    .execute(&s.db)
    .await;
    if updated.is_err() {
        return err("reaction could not be saved");
    }
    notify(&s, &author, "reaction", &me, &req.post_id).await;
    Json(json!({ "ok": true }))
}

pub async fn unreact(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PostIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let deleted = sqlx::query("DELETE FROM post_reactions WHERE post_id = ? AND account_id = ?")
        .bind(&req.post_id)
        .bind(&me)
        .execute(&s.db)
        .await;
    match deleted {
        Ok(_) => Json(json!({ "ok": true })),
        Err(_) => err("reaction could not be removed"),
    }
}

// --------------------------------------------------------- circles (§6.4)

#[derive(Deserialize)]
pub struct CircleCreateReq {
    pub name: String,
    /// {member_account_id: wrapped_circle_key_b64} — wrapped client-side in
    /// the owner's vault; HIVE stores ciphertext only.
    #[serde(default)]
    pub wrapped_keys: Value,
}

pub async fn circle_create(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CircleCreateReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if req.name.trim().is_empty() || req.name.chars().count() > 64 {
        return err("circle name must be 1-64 chars");
    }
    if !req.wrapped_keys.is_object() && !req.wrapped_keys.is_null() {
        return err("wrapped_keys must be an object");
    }
    let circle_id = new_id();
    let keys = if req.wrapped_keys.is_null() {
        json!({})
    } else {
        req.wrapped_keys
    };
    let _ = sqlx::query(
        "INSERT INTO circles (id, owner, name, wrapped_keys, created) VALUES (?,?,?,?,?)",
    )
    .bind(&circle_id)
    .bind(&me)
    .bind(req.name.trim())
    .bind(keys.to_string())
    .bind(now())
    .execute(&s.db)
    .await;
    let created = now();
    let owner_key = keys.get(&me).map(Value::to_string);
    let _ = sqlx::query(
        "INSERT INTO fold_members \
         (circle_id,account_id,role,state,invited_by,invited_at,joined_at,wrapped_key,key_epoch) \
         VALUES (?,?,'owner','active',?,?,?,?,1)",
    )
    .bind(&circle_id)
    .bind(&me)
    .bind(&me)
    .bind(created)
    .bind(created)
    .bind(owner_key)
    .execute(&s.db)
    .await;
    if let Some(member_keys) = keys.as_object() {
        for (account_id, wrapped_key) in member_keys {
            if account_id == &me {
                continue;
            }
            let _ = sqlx::query(
                "INSERT OR REPLACE INTO fold_members \
                 (circle_id,account_id,role,state,invited_by,invited_at,joined_at,wrapped_key,key_epoch) \
                 VALUES (?,?,'member','active',?,?,?,?,1)",
            )
            .bind(&circle_id)
            .bind(account_id)
            .bind(&me)
            .bind(created)
            .bind(created)
            .bind(wrapped_key.to_string())
            .execute(&s.db)
            .await;
        }
    }
    Json(json!({ "ok": true, "circle_id": circle_id }))
}

#[derive(Deserialize)]
pub struct CircleKeysReq {
    pub circle_id: String,
    pub expected_epoch: i64,
    /// Complete replacement set — key rotation on member removal (§6.4)
    /// is "replace with the new key wrapped to the remaining members".
    pub wrapped_keys: Value,
}

pub async fn circle_set_keys(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CircleKeysReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !req.wrapped_keys.is_object() {
        return err("wrapped_keys must be an object");
    }
    if req.wrapped_keys.as_object().is_some_and(|keys| {
        keys.values().any(|value| value.as_object().is_none_or(|devices| devices.is_empty()))
    }) {
        return err("each active member requires a non-empty per-device wrapped-key map");
    }
    let owned: Option<(i64,)> = sqlx::query_as(
        "SELECT key_epoch FROM circles WHERE id=? AND owner=?",
    )
    .bind(&req.circle_id)
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((old_epoch,)) = owned else {
        return err("no such circle or not yours");
    };
    if old_epoch != req.expected_epoch {
        return err("Fold changed on another device; refresh before rotating keys");
    }
    let new_epoch = old_epoch + 1;
    let keys = req.wrapped_keys.as_object().cloned().unwrap_or_default();
    let updated = async {
        let mut tx = s.db.begin().await?;
        let changed = sqlx::query(
            "UPDATE circles SET wrapped_keys=?,key_epoch=?,key_stale=0 \
             WHERE id=? AND owner=? AND key_epoch=?",
        )
            .bind(req.wrapped_keys.to_string())
            .bind(new_epoch)
            .bind(&req.circle_id)
            .bind(&me)
            .bind(old_epoch)
            .execute(&mut *tx)
            .await?;
        if changed.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        sqlx::query(
            "DELETE FROM fold_members WHERE circle_id=? AND role='member' AND state='active'",
        )
        .bind(&req.circle_id)
        .execute(&mut *tx)
        .await?;
        if let Some(owner_key) = keys.get(&me) {
            sqlx::query(
                "UPDATE fold_members SET wrapped_key=?,key_epoch=? \
                 WHERE circle_id=? AND account_id=?",
            )
            .bind(owner_key.to_string())
            .bind(new_epoch)
            .bind(&req.circle_id)
            .bind(&me)
            .execute(&mut *tx)
            .await?;
        }
        for (account_id, wrapped_key) in &keys {
            if account_id == &me {
                continue;
            }
            sqlx::query(
                "INSERT INTO fold_members \
                 (circle_id,account_id,role,state,invited_by,invited_at,joined_at,wrapped_key,key_epoch) \
                 VALUES (?,?,'member','active',?,?,?,?,?)",
            )
            .bind(&req.circle_id)
            .bind(account_id)
            .bind(&me)
            .bind(now())
            .bind(now())
            .bind(wrapped_key.to_string())
            .bind(new_epoch)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
    .await;
    match updated {
        Ok(()) => Json(json!({ "ok": true, "key_epoch": new_epoch })),
        Err(_) => err("Fold key rotation failed"),
    }
}

fn wrapped_value(value: Option<String>) -> Value {
    value
        .map(|raw| serde_json::from_str(&raw).unwrap_or(Value::String(raw)))
        .unwrap_or(Value::Null)
}

/// Folds I own, Folds I actively joined, and pending invitations. Active
/// rosters are visible to every member; pending invitations remain owner-only.
pub async fn circles(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut own = Vec::new();
    let mut member = Vec::new();
    let mut invited = Vec::new();
    let rows: Vec<(String, String, String, i64, i64, String, Option<i64>, Option<String>)> =
        sqlx::query_as(
            "SELECT c.id,c.owner,c.name,c.created,c.key_epoch,m.state,m.joined_at,m.wrapped_key \
             FROM circles c JOIN fold_members m ON m.circle_id=c.id \
             WHERE m.account_id=? ORDER BY c.created DESC",
        )
        .bind(&me)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
    for (id, owner, name, created, key_epoch, state, joined_at, my_wrapped_key) in rows {
        let active_rows: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT account_id,role,joined_at FROM fold_members \
             WHERE circle_id=? AND state='active' ORDER BY joined_at,account_id",
        )
        .bind(&id)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
        let mut roster = Vec::with_capacity(active_rows.len());
        for (account_id, role, member_joined_at) in active_rows {
            roster.push(json!({
                "account": author_card(&s, &account_id).await,
                "role": role,
                "joined_at": member_joined_at,
            }));
        }
        if state == "invited" {
            invited.push(json!({
                "circle_id": id, "name": name, "created": created,
                "owner": author_card(&s, &owner).await,
                "member_count": roster.len(),
            }));
            continue;
        }
        let mine = wrapped_value(my_wrapped_key);
        let common = json!({
            "circle_id": id, "name": name, "created": created,
            "key_epoch": key_epoch, "wrapped_key": mine,
            "device_id": device_id, "members": roster,
        });
        if owner == me {
            let wrapped_rows: Vec<(String, Option<String>)> = sqlx::query_as(
                "SELECT account_id,wrapped_key FROM fold_members \
                 WHERE circle_id=? AND state='active'",
            )
            .bind(&id)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
            let mut wrapped_keys = serde_json::Map::new();
            for (account_id, wrapped_key) in wrapped_rows {
                wrapped_keys.insert(account_id, wrapped_value(wrapped_key));
            }
            let pending_rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT account_id,invited_at FROM fold_members \
                 WHERE circle_id=? AND state='invited' ORDER BY invited_at",
            )
            .bind(&id)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
            let mut pending = Vec::with_capacity(pending_rows.len());
            for (account_id, invited_at) in pending_rows {
                pending.push(json!({
                    "account": author_card(&s, &account_id).await,
                    "invited_at": invited_at,
                }));
            }
            let mut value = common;
            value["pending"] = Value::Array(pending);
            value["wrapped_keys"] = Value::Object(wrapped_keys);
            own.push(value);
        } else {
            let mut value = common;
            value["owner"] = author_card(&s, &owner).await;
            value["joined_at"] = json!(joined_at);
            member.push(value);
        }
    }
    Json(json!({ "ok": true, "own": own, "member": member, "invited": invited }))
}

#[derive(Deserialize)]
pub struct FoldInviteReq {
    pub circle_id: String,
    pub target: String,
    pub wrapped_key: Value,
}

pub async fn fold_invite(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldInviteReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    if target == me {
        return err("owner is already a member");
    }
    if blocked_between(&s, &me, &target).await
        || !(is_accepted_follower(&s, &me, &target).await
            || is_accepted_follower(&s, &target, &me).await)
    {
        return err("Fold invitations require an accepted social relationship");
    }
    if req
        .wrapped_key
        .as_object()
        .is_none_or(|devices| devices.is_empty())
    {
        return err("Fold invitation requires per-device wrapped keys");
    }
    let fold: Option<(i64,)> = sqlx::query_as(
        "SELECT key_epoch FROM circles WHERE id=? AND owner=? AND key_stale=0",
    )
    .bind(&req.circle_id)
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((key_epoch,)) = fold else {
        return err("no such Fold, not yours, or key rotation required");
    };
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT state FROM fold_members WHERE circle_id=? AND account_id=?",
    )
    .bind(&req.circle_id)
    .bind(&target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if matches!(existing.as_ref(), Some((state,)) if state == "active") {
        return err("account is already a member");
    }
    let created = now();
    let saved = sqlx::query(
        "INSERT INTO fold_members \
         (circle_id,account_id,role,state,invited_by,invited_at,joined_at,wrapped_key,key_epoch) \
         VALUES (?,?,'member','invited',?,?,NULL,?,?) \
         ON CONFLICT(circle_id,account_id) DO UPDATE SET state='invited',invited_by=excluded.invited_by, \
            invited_at=excluded.invited_at,joined_at=NULL,wrapped_key=excluded.wrapped_key,key_epoch=excluded.key_epoch", // gitleaks:allow
    )
    .bind(&req.circle_id)
    .bind(&target)
    .bind(&me)
    .bind(created)
    .bind(req.wrapped_key.to_string())
    .bind(key_epoch)
    .execute(&s.db)
    .await;
    match saved {
        Ok(_) => {
            notify(&s, &target, "fold_invite", &me, &req.circle_id).await;
            Json(json!({ "ok": true }))
        }
        Err(_) => err("Fold invitation could not be sent"),
    }
}

#[derive(Deserialize)]
pub struct FoldIdReq {
    pub circle_id: String,
}

pub async fn fold_accept(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let joined = now();
    let result = sqlx::query(
        "UPDATE fold_members SET state='active',joined_at=? \
         WHERE circle_id=? AND account_id=? AND state='invited'",
    )
    .bind(joined)
    .bind(&req.circle_id)
    .bind(&me)
    .execute(&s.db)
    .await;
    match result {
        Ok(value) if value.rows_affected() == 1 => {
            let owner: Option<(String,)> = sqlx::query_as("SELECT owner FROM circles WHERE id=?")
                .bind(&req.circle_id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None);
            if let Some((owner,)) = owner {
                notify(&s, &owner, "fold_joined", &me, &req.circle_id).await;
            }
            Json(json!({ "ok": true, "joined_at": joined }))
        }
        _ => err("no pending Fold invitation"),
    }
}

pub async fn fold_decline(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let result = sqlx::query(
        "DELETE FROM fold_members WHERE circle_id=? AND account_id=? AND state='invited'",
    )
    .bind(&req.circle_id)
    .bind(&me)
    .execute(&s.db)
    .await;
    match result {
        Ok(value) if value.rows_affected() == 1 => Json(json!({ "ok": true })),
        _ => err("no pending Fold invitation"),
    }
}

#[derive(Deserialize)]
pub struct FoldTargetReq {
    pub circle_id: String,
    pub target: String,
}

pub async fn fold_remove(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldTargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Some(target) = resolve(&s, &req.target).await else {
        return err("no such account");
    };
    let owned: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM circles WHERE id=? AND owner=?")
        .bind(&req.circle_id)
        .bind(&me)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    if owned.is_none() || target == me {
        return err("no such Fold, not yours, or cannot remove owner");
    }
    let state: Option<(String,)> = sqlx::query_as(
        "SELECT state FROM fold_members WHERE circle_id=? AND account_id=? AND role='member'",
    )
    .bind(&req.circle_id)
    .bind(&target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((state,)) = state else {
        return err("account is not in this Fold");
    };
    let _ = sqlx::query("DELETE FROM fold_members WHERE circle_id=? AND account_id=?")
        .bind(&req.circle_id)
        .bind(&target)
        .execute(&s.db)
        .await;
    if state == "active" {
        let _ = sqlx::query("UPDATE circles SET key_stale=1 WHERE id=?")
            .bind(&req.circle_id)
            .execute(&s.db)
            .await;
    }
    Json(json!({ "ok": true, "rotation_required": state == "active" }))
}

pub async fn fold_leave(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<FoldIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let result = sqlx::query(
        "DELETE FROM fold_members WHERE circle_id=? AND account_id=? AND role='member' AND state='active'",
    )
    .bind(&req.circle_id)
    .bind(&me)
    .execute(&s.db)
    .await;
    match result {
        Ok(value) if value.rows_affected() == 1 => {
            let _ = sqlx::query("UPDATE circles SET key_stale=1 WHERE id=?")
                .bind(&req.circle_id)
                .execute(&s.db)
                .await;
            Json(json!({ "ok": true, "rotation_required": true }))
        }
        _ => err("not an active member or owner cannot leave"),
    }
}

#[derive(Deserialize)]
pub struct CircleIdReq {
    pub circle_id: String,
}

/// Delete a circle and its posts (real deletion, §6.3 rules).
pub async fn circle_delete(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CircleIdReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let owned: Option<(String,)> = sqlx::query_as("SELECT owner FROM circles WHERE id = ?")
        .bind(&req.circle_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    if owned.map(|(o,)| o) != Some(me.clone()) {
        return err("no such circle or not yours");
    }
    let audience = format!("circle:{}", req.circle_id);
    let posts: Vec<(String, String)> =
        sqlx::query_as("SELECT id, media FROM posts WHERE audience = ?")
            .bind(&audience)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
    for (post_id, media) in posts {
        let _ = sqlx::query("DELETE FROM post_reactions WHERE post_id = ?")
            .bind(&post_id)
            .execute(&s.db)
            .await;
        let _ = sqlx::query("DELETE FROM comments WHERE post_id = ?")
            .bind(&post_id)
            .execute(&s.db)
            .await;
        let _ = sqlx::query("DELETE FROM posts WHERE id = ?")
            .bind(&post_id)
            .execute(&s.db)
            .await;
        for id in serde_json::from_str::<Vec<String>>(&media).unwrap_or_default() {
            blob::add_ref(&s, &id, -1).await;
        }
    }
    let _ = sqlx::query("DELETE FROM circles WHERE id = ?")
        .bind(&req.circle_id)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

// --------------------------------------------------------- support threads

#[derive(Deserialize)]
pub struct SupportOpenReq {
    pub category: String,
    pub subject: String,
    pub body: String,
}

pub async fn support_open(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SupportOpenReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !matches!(req.category.as_str(), "bug" | "feature" | "help" | "other") {
        return err("category must be bug, feature, help, or other");
    }
    let subject = req.subject.trim();
    let body = req.body.trim();
    if subject.is_empty() || subject.chars().count() > 120 {
        return err("subject must be 1-120 characters");
    }
    if body.is_empty() || body.chars().count() > 4_000 {
        return err("message must be 1-4000 characters");
    }
    if !s.rate_ok(&format!("support-open:{me}"), 5, 86_400) {
        return err("support thread limit reached, try again tomorrow");
    }
    let thread_id = new_id();
    let message_id = new_id();
    let created = now();
    let saved = async {
        let mut tx = s.db.begin().await?;
        sqlx::query(
            "INSERT INTO support_threads (id,account_id,category,subject,status,created,updated) \
             VALUES (?,?,?,?, 'open',?,?)",
        )
        .bind(&thread_id)
        .bind(&me)
        .bind(&req.category)
        .bind(subject)
        .bind(created)
        .bind(created)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO support_messages (id,thread_id,sender,sender_role,body,created) \
             VALUES (?,?,?,'user',?,?)",
        )
        .bind(&message_id)
        .bind(&thread_id)
        .bind(&me)
        .bind(body)
        .bind(created)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    match saved {
        Ok(()) => Json(json!({ "ok": true, "thread_id": thread_id })),
        Err(_) => err("support thread could not be created"),
    }
}

#[derive(Deserialize)]
pub struct SupportSendReq {
    pub thread_id: String,
    pub body: String,
}

pub async fn support_send(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SupportSendReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let body = req.body.trim();
    if body.is_empty() || body.chars().count() > 4_000 {
        return err("message must be 1-4000 characters");
    }
    if !s.rate_ok(&format!("support-send:{me}"), 30, 86_400) {
        return err("support message limit reached, try again tomorrow");
    }
    let owned: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM support_threads WHERE id=? AND account_id=?",
    )
    .bind(&req.thread_id)
    .bind(&me)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    match owned {
        None => return err("no such support thread"),
        Some((status,)) if status != "open" => return err("support thread is closed"),
        _ => {}
    }
    let created = now();
    let inserted = sqlx::query(
        "INSERT INTO support_messages (id,thread_id,sender,sender_role,body,created) \
         VALUES (?,?,?,'user',?,?)",
    )
    .bind(new_id())
    .bind(&req.thread_id)
    .bind(&me)
    .bind(body)
    .bind(created)
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("support message could not be sent");
    }
    let _ = sqlx::query("UPDATE support_threads SET updated=? WHERE id=?")
        .bind(created)
        .bind(&req.thread_id)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

pub async fn support_threads(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let threads: Vec<(String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id,category,subject,status,created,updated FROM support_threads \
         WHERE account_id=? ORDER BY updated DESC LIMIT 100",
    )
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(threads.len());
    for (id, category, subject, status, created, updated) in threads {
        let messages: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT id,sender_role,body,created FROM support_messages \
             WHERE thread_id=? ORDER BY created,id",
        )
        .bind(&id)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
        let _ = sqlx::query(
            "UPDATE support_messages SET read_at=COALESCE(read_at,?) \
             WHERE thread_id=? AND sender_role='admin'",
        )
        .bind(now())
        .bind(&id)
        .execute(&s.db)
        .await;
        out.push(json!({
            "id": id, "category": category, "subject": subject, "status": status,
            "created": created, "updated": updated,
            "messages": messages.into_iter().map(|(message_id, role, body, at)| json!({
                "id": message_id, "sender_role": role, "body": body, "created": at,
            })).collect::<Vec<_>>(),
        }));
    }
    Json(json!({ "ok": true, "threads": out }))
}

// --------------------------------------------------------- reports (§6.7)

#[derive(Deserialize)]
pub struct ReportReq {
    pub subject_kind: String,
    pub subject_id: String,
    pub reason: String,
    #[serde(default)]
    pub reporter_copy: Option<String>,
}

pub async fn report(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ReportReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !s.rate_ok(&format!("report:{me}"), 20, 3600) {
        return err("rate limited, try again later");
    }
    if !matches!(req.subject_kind.as_str(), "account" | "post" | "comment") {
        return err("subject_kind must be account, post, or comment");
    }
    if req.reason.trim().is_empty() || req.reason.len() > 2000 {
        return err("reason must be 1-2000 bytes");
    }
    if req.reporter_copy.as_ref().is_some_and(|copy| copy.chars().count() > 10_000) {
        return err("reporter evidence copy is too long");
    }
    let reported_audience: Option<(String,)> = match req.subject_kind.as_str() {
        "post" => sqlx::query_as("SELECT audience FROM posts WHERE id=?")
            .bind(&req.subject_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None),
        "comment" => sqlx::query_as(
            "SELECT p.audience FROM comments c JOIN posts p ON p.id=c.post_id WHERE c.id=?",
        )
        .bind(&req.subject_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None),
        _ => None,
    };
    if matches!(reported_audience.as_ref(), Some((audience,)) if audience.starts_with("circle:"))
        && req
            .reporter_copy
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return err("encrypted Fold reports require the reporter's decrypted copy");
    }
    let created = now();
    let inserted = sqlx::query(
        "INSERT INTO reports (reporter, subject_kind, subject_id, reason, created) \
         VALUES (?,?,?,?,?)",
    )
    .bind(&me)
    .bind(&req.subject_kind)
    .bind(&req.subject_id)
    .bind(req.reason.trim())
    .bind(created)
    .execute(&s.db)
    .await;
    // Evidence snapshot (§2258A posture): freeze the subject as it exists
    // RIGHT NOW, so a later delete/edit can't destroy what was reported.
    // Media blobs in the snapshot get legal_hold so GC never removes them.
    if let Ok(r) = inserted {
        let report_id = r.last_insert_rowid();
        #[allow(clippy::type_complexity)]
        let snap: Option<(String, String, String, String, i64, Option<i64>, String)> =
            match req.subject_kind.as_str() {
                "post" => sqlx::query_as(
                    "SELECT author, body, media, audience, created, edited, '' \
                     FROM posts WHERE id = ?",
                )
                .bind(&req.subject_id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None),
                "comment" => sqlx::query_as(
                    "SELECT author, body, '[]', '', created, edited, post_id \
                     FROM comments WHERE id = ?",
                )
                .bind(&req.subject_id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None),
                _ => None,
            };
        if let Some((author, body, media, audience, content_created, content_edited, parent)) = snap
        {
            let evidence_body = if audience.starts_with("circle:") {
                req.reporter_copy.as_deref().unwrap_or("").trim().to_string()
            } else {
                body.clone()
            };
            let handle: Option<(String,)> =
                sqlx::query_as("SELECT handle FROM accounts WHERE id = ?")
                    .bind(&author)
                    .fetch_optional(&s.db)
                    .await
                    .unwrap_or(None);
            // Freeze the author's network trail NOW: device + session IPs
            // are purged after 90 days (data minimization), so this snapshot
            // is the only place they survive for law enforcement.
            let device_ips: Vec<(String, String, Option<String>, Option<i64>)> = sqlx::query_as(
                "SELECT id, name, last_ip, last_seen FROM devices WHERE account_id = ?",
            )
            .bind(&author)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
            let session_ips: Vec<(String, String, i64)> = sqlx::query_as(
                "SELECT se.device_id, se.last_ip, se.created FROM sessions se \
                 JOIN devices d ON d.id = se.device_id \
                 WHERE d.account_id = ? AND se.last_ip IS NOT NULL",
            )
            .bind(&author)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
            let mut ips: Vec<Value> = device_ips
                .into_iter()
                .map(|(id, name, ip, seen)| {
                    json!({ "kind": "device", "device": id, "name": name,
                            "ip": ip, "last_seen": seen })
                })
                .collect();
            ips.extend(session_ips.into_iter().map(|(device, ip, at)| {
                json!({ "kind": "session", "device": device, "ip": ip, "created": at })
            }));
            // Freeze social context too (0009): the profile can be rewritten
            // and thread comments deleted after filing.
            let profile: Option<(String, Option<String>, String)> = sqlx::query_as(
                "SELECT display_name, avatar_blob, bio FROM profiles WHERE account_id = ?",
            )
            .bind(&author)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
            let avatar_blob = profile.as_ref().and_then(|(_, a, _)| a.clone());
            let author_profile = profile
                .map(|(display_name, avatar_blob, bio)| {
                    json!({ "display_name": display_name, "avatar_blob": avatar_blob,
                            "bio": bio })
                })
                .unwrap_or_else(|| json!({}));
            // Thread: for comments, the post they were made on; for both,
            // the surrounding comment thread as it exists right now.
            let thread_post_id = if req.subject_kind == "comment" {
                parent.clone()
            } else {
                req.subject_id.clone()
            };
            #[allow(clippy::type_complexity)]
            let thread_post: Option<(String, String, i64, String, String)> = sqlx::query_as(
                "SELECT p.author, COALESCE(a.handle,''), p.created, p.body, p.media \
                 FROM posts p LEFT JOIN accounts a ON a.id = p.author WHERE p.id = ?",
            )
            .bind(&thread_post_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
            #[allow(clippy::type_complexity)]
            let thread_comments: Vec<(String, String, String, i64, String)> = sqlx::query_as(
                "SELECT c.id, c.author, COALESCE(a.handle,''), c.created, c.body \
                 FROM comments c LEFT JOIN accounts a ON a.id = c.author \
                 WHERE c.post_id = ? AND c.deleted_at IS NULL \
                 ORDER BY c.created LIMIT 200",
            )
            .bind(&thread_post_id)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
            let thread = json!({
                "post": thread_post.map(|(pauthor, phandle, pcreated, pbody, pmedia)| json!({
                    "id": thread_post_id, "author": pauthor, "author_handle": phandle,
                    "created": pcreated, "body": pbody,
                    "media": serde_json::from_str::<Value>(&pmedia)
                        .unwrap_or_else(|_| json!([])),
                })),
                "comments": thread_comments.into_iter()
                    .map(|(cid, cauthor, chandle, ccreated, cbody)| json!({
                        "id": cid, "author": cauthor, "author_handle": chandle,
                        "created": ccreated, "body": cbody,
                    })).collect::<Vec<_>>(),
            });
            let _ = sqlx::query(
                "INSERT OR IGNORE INTO report_snapshots \
                 (report_id, subject_kind, subject_id, author, author_handle, body, media, \
                  audience, captured_at, content_created, content_edited, parent_post_id, \
                  author_ips, reporter_ip, author_profile, thread) \
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(report_id)
            .bind(&req.subject_kind)
            .bind(&req.subject_id)
            .bind(&author)
            .bind(handle.map(|(h,)| h).unwrap_or_default())
            .bind(&evidence_body)
            .bind(&media)
            .bind(&audience)
            .bind(created)
            .bind(content_created)
            .bind(content_edited)
            .bind(&parent)
            .bind(serde_json::to_string(&ips).unwrap_or_else(|_| "[]".into()))
            .bind(&ip)
            .bind(author_profile.to_string())
            .bind(thread.to_string())
            .execute(&s.db)
            .await;
            // Legal-hold the avatar alongside the post media: it may itself
            // be the violation, and it identifies the account visually.
            if let Some(avatar) = avatar_blob {
                let _ = sqlx::query("UPDATE blobs SET legal_hold = 1 WHERE id = ?")
                    .bind(&avatar)
                    .execute(&s.db)
                    .await;
            }
            if let Ok(ids) = serde_json::from_str::<Vec<String>>(&media) {
                for blob_id in ids {
                    let _ = sqlx::query("UPDATE blobs SET legal_hold = 1 WHERE id = ?")
                        .bind(&blob_id)
                        .execute(&s.db)
                        .await;
                }
            }
        }
    }
    // §6.7: every report is also an audit row.
    let _ = sqlx::query("INSERT INTO audit (actor, action, subject, at, ip) VALUES (?,?,?,?,?)")
        .bind(&me)
        .bind("report")
        .bind(format!("{}:{}", req.subject_kind, req.subject_id))
        .bind(now())
        .bind(&ip)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

// ---------------------------------------------------------------- search

#[derive(Deserialize)]
pub struct SearchReq {
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Discover: substring match on handle or display name. Blocked accounts
/// (either direction) and self are invisible — no algorithmic suggestions,
/// you find people you already know.
pub async fn search(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SearchReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !s.rate_ok(&format!("search:{me}"), 60, 60) {
        return err("rate limited, try again later");
    }
    let q = req.q.trim();
    if q.is_empty() || q.chars().count() > 64 {
        return err("query must be 1-64 chars");
    }
    let limit = req.limit.unwrap_or(20).clamp(1, 50);
    // Escape LIKE wildcards in user input.
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT a.id FROM accounts a LEFT JOIN profiles p ON p.account_id = a.id \
         LEFT JOIN account_settings st ON st.account_id = a.id \
         WHERE a.status = 'active' AND a.id != ?1 \
           AND COALESCE(st.discoverable, 1) = 1 \
           AND (a.handle LIKE ?2 ESCAPE '\\' OR p.display_name LIKE ?2 ESCAPE '\\') \
           AND a.id NOT IN (SELECT blocked FROM blocks WHERE blocker = ?1) \
           AND a.id NOT IN (SELECT blocker FROM blocks WHERE blocked = ?1) \
         ORDER BY a.handle LIMIT ?3",
    )
    .bind(&me)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (id,) in rows {
        let mut card = author_card(&s, &id).await;
        let rel: Option<(String,)> =
            sqlx::query_as("SELECT state FROM follows WHERE follower = ? AND followee = ?")
                .bind(&me)
                .bind(&id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None);
        card["follow_state"] = json!(rel.map(|(st,)| st));
        out.push(card);
    }
    Json(json!({ "ok": true, "results": out }))
}

// --------------------------------------------------------- notifications

#[derive(Deserialize)]
pub struct NotificationsReq {
    #[serde(default)]
    pub before_cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// The Alerts inbox — persisted history of what the live frames announced.
pub async fn notifications(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<NotificationsReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let limit = req.limit.unwrap_or(50).clamp(1, 100);
    let (cur_created, cur_id) = match &req.before_cursor {
        Some(c) => match c.split_once(':') {
            Some((t, id)) => (t.parse::<i64>().unwrap_or(i64::MAX), id.to_string()),
            None => return err("bad cursor"),
        },
        None => (i64::MAX, String::new()),
    };
    let rows: Vec<(String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, kind, actor, subject_id, created, seen FROM notifications \
         WHERE account_id = ?1 \
           AND actor NOT IN (SELECT blocked FROM blocks WHERE blocker = ?1) \
           AND actor NOT IN (SELECT blocker FROM blocks WHERE blocked = ?1) \
           AND (created < ?2 OR (created = ?2 AND id < ?3)) \
         ORDER BY created DESC, id DESC LIMIT ?4",
    )
    .bind(&me)
    .bind(cur_created)
    .bind(&cur_id)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let (unseen,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM notifications WHERE account_id = ? AND seen = 0")
            .bind(&me)
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    let mut out = Vec::with_capacity(rows.len());
    let mut cursor = None;
    for (id, kind, actor, subject_id, created, seen) in rows {
        cursor = Some(format!("{created}:{id}"));
        let mut item = json!({
            "id": id,
            "kind": kind,
            "from": author_card(&s, &actor).await,
            "subject_id": subject_id,
            "created": created,
            "seen": seen != 0,
        });
        if let Some(fold_id) = notification_fold_id(&s, &kind, &subject_id).await {
            item["fold_id"] = json!(fold_id);
        }
        // System alerts reference an announcement row; inline its content so
        // the Alerts tab can render the notice without another round trip.
        if kind == "system" && !subject_id.is_empty() {
            let ann: Option<(String, String, String)> = sqlx::query_as(
                "SELECT kind, title, body FROM announcements WHERE id=?",
            )
            .bind(&subject_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
            if let Some((ann_kind, title, body)) = ann {
                item["announce_kind"] = json!(ann_kind);
                item["title"] = json!(title);
                item["body"] = json!(body);
            }
        }
        out.push(item);
    }
    Json(json!({ "ok": true, "notifications": out, "unseen": unseen, "next_cursor": cursor }))
}

/// Mark the whole inbox seen (opening the Alerts tab).
pub async fn notifications_seen(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let _ = sqlx::query("UPDATE notifications SET seen = 1 WHERE account_id = ? AND seen = 0")
        .bind(&me)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct NotificationDeleteReq {
    id: String,
}

/// Delete one alert from the caller's persisted inbox.
pub async fn notification_delete(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<NotificationDeleteReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let _ = sqlx::query("DELETE FROM notifications WHERE account_id = ? AND id = ?")
        .bind(&me)
        .bind(req.id)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true }))
}

// ------------------------------------------------------------------ media

/// Fetch Connect media with the post's audience gate (blob::fetch only
/// serves public blobs or the owner — followers/circle media flows through
/// here). Avatars referenced by any profile are public like the profile.
pub async fn media(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    axum::extract::Path(blob_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(_) => return (axum::http::StatusCode::UNAUTHORIZED, "auth required").into_response(),
    };
    let exists: Option<(String,)> = sqlx::query_as("SELECT owner FROM blobs WHERE id = ?")
        .bind(&blob_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    let Some((owner,)) = exists else {
        return (axum::http::StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mut allowed = owner == me;
    if !allowed {
        // Avatar of any profile ⇒ public.
        let avatar: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM profiles WHERE avatar_blob = ? LIMIT 1")
                .bind(&blob_id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None);
        allowed = avatar.is_some();
    }
    if !allowed {
        // Referenced by a post the viewer can see?
        let needle = format!("%\"{blob_id}\"%");
        let posts: Vec<(String, String)> = sqlx::query_as(
            "SELECT author, audience FROM posts \
             WHERE deleted_at IS NULL AND media LIKE ?1 LIMIT 20",
        )
        .bind(&needle)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
        for (author, audience) in posts {
            if can_view(&s, &me, &author, &audience).await {
                allowed = true;
                break;
            }
        }
    }
    if !allowed {
        return (axum::http::StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    match std::fs::read(blob::blob_path(&s.data_dir, &blob_id)) {
        Ok(bytes) => bytes.into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "storage error",
        )
            .into_response(),
    }
}
