// Owns founder-only moderation, account review, invites, quarantine, and evidence operations.
// api.rs exposes these handlers; connect.rs and blob.rs own user content and stored bytes.

// Admin / moderation endpoints (§230 posture: review reports, take down
// content, suspend accounts). "Admin" = any active founder account — the
// first account created on the server. All actions are audited.

use crate::api::AppState;
use crate::identity::authenticate;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

fn err(msg: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": msg }))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn external_origin(headers: &HeaderMap) -> Option<String> {
    let host = headers.get("host")?.to_str().ok()?.trim();
    if host.is_empty() {
        return None;
    }
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("https");
    Some(format!("{scheme}://{host}"))
}

pub(crate) fn invite_link(s: &AppState, headers: &HeaderMap, code: &str) -> Option<String> {
    let origin = external_origin(headers)?;
    let mut query = String::new();
    if let Some(access) = s.site_access_code.as_deref().filter(|v| !v.is_empty()) {
        query.push_str("access=");
        query.push_str(&pct(access));
        query.push('&');
    }
    query.push_str("invite=");
    query.push_str(&pct(code));
    Some(format!("{origin}/signup.html?{query}"))
}

pub(crate) async fn audit(s: &AppState, actor: &str, action: &str, subject: &str, ip: &str) {
    let _ = sqlx::query("INSERT INTO audit (actor, action, subject, at, ip) VALUES (?,?,?,?,?)")
        .bind(actor)
        .bind(action)
        .bind(subject)
        .bind(now())
        .bind(ip)
        .execute(&s.db)
        .await;
}

pub async fn support_threads(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if let Err(error) = require_admin(&s, &headers, &ip).await {
        return error;
    }
    let threads: Vec<(String, String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT t.id,a.handle,t.category,t.subject,t.status,t.created,t.updated \
         FROM support_threads t JOIN accounts a ON a.id=t.account_id \
         ORDER BY CASE WHEN t.status='open' THEN 0 ELSE 1 END, t.updated DESC LIMIT 500",
    )
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(threads.len());
    for (id, handle, category, subject, status, created, updated) in threads {
        let messages: Vec<(String, String, String, i64, Option<i64>)> = sqlx::query_as(
            "SELECT id,sender_role,body,created,read_at FROM support_messages \
             WHERE thread_id=? ORDER BY created,id",
        )
        .bind(&id)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
        let _ = sqlx::query(
            "UPDATE support_messages SET read_at=COALESCE(read_at,?) \
             WHERE thread_id=? AND sender_role='user'",
        )
        .bind(now())
        .bind(&id)
        .execute(&s.db)
        .await;
        out.push(json!({
            "id": id, "handle": handle, "category": category, "subject": subject,
            "status": status, "created": created, "updated": updated,
            "unread": messages.iter().filter(|(_, role, _, _, read)| role == "user" && read.is_none()).count(),
            "messages": messages.into_iter().map(|(message_id, role, body, at, read_at)| json!({
                "id": message_id, "sender_role": role, "body": body,
                "created": at, "read_at": read_at,
            })).collect::<Vec<_>>(),
        }));
    }
    Json(json!({ "ok": true, "threads": out }))
}

#[derive(Deserialize)]
pub struct SupportReplyReq {
    pub thread_id: String,
    pub body: String,
}

pub async fn support_reply(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SupportReplyReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let body = req.body.trim();
    if body.is_empty() || body.chars().count() > 4_000 {
        return err("reply must be 1-4000 characters");
    }
    let thread: Option<(String, String)> = sqlx::query_as(
        "SELECT account_id,status FROM support_threads WHERE id=?",
    )
    .bind(&req.thread_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((account_id, status)) = thread else {
        return err("no such support thread");
    };
    if status != "open" {
        return err("support thread is closed");
    }
    let created = now();
    let inserted = sqlx::query(
        "INSERT INTO support_messages (id,thread_id,sender,sender_role,body,created) \
         VALUES (?,?,?,'admin',?,?)",
    )
    .bind(crate::connect::new_id())
    .bind(&req.thread_id)
    .bind(&me)
    .bind(body)
    .bind(created)
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("support reply could not be sent");
    }
    let _ = sqlx::query("UPDATE support_threads SET updated=? WHERE id=?")
        .bind(created)
        .bind(&req.thread_id)
        .execute(&s.db)
        .await;
    let _ = sqlx::query(
        "UPDATE support_messages SET read_at=COALESCE(read_at,?) \
         WHERE thread_id=? AND sender_role='user'",
    )
    .bind(created)
    .bind(&req.thread_id)
    .execute(&s.db)
    .await;
    audit(&s, &me, "support_reply", &req.thread_id, &ip).await;
    crate::connect::notify(&s, &account_id, "support_reply", &me, &req.thread_id).await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct SupportStatusReq {
    pub thread_id: String,
    pub status: String,
}

pub async fn support_status(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SupportStatusReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !matches!(req.status.as_str(), "open" | "closed") {
        return err("status must be open or closed");
    }
    let result = sqlx::query("UPDATE support_threads SET status=?,updated=? WHERE id=?")
        .bind(&req.status)
        .bind(now())
        .bind(&req.thread_id)
        .execute(&s.db)
        .await;
    match result {
        Ok(value) if value.rows_affected() == 1 => {
            audit(
                &s,
                &me,
                "support_status",
                &format!("{} status={}", req.thread_id, req.status),
                &ip,
            )
            .await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such support thread"),
    }
}

#[derive(Deserialize)]
pub struct AnnounceReq {
    /// system = general notice, maintenance = downtime window,
    /// legal = terms/privacy change notice (usually with a version bump).
    pub kind: String,
    pub title: String,
    pub body: String,
}

/// POST /v1/admin/announce — operator broadcast. Persists one announcement
/// row plus a "system" alert row per active account, then pushes the live
/// connect_notif frame (which also reaches Nexus Notify wake streams). This
/// is the §16/§10 in-app notice channel for ToS changes, maintenance
/// windows, and shutdown-grace notices.
pub async fn announce(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AnnounceReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !matches!(req.kind.as_str(), "system" | "maintenance" | "legal") {
        return err("kind must be system, maintenance, or legal");
    }
    let title = req.title.trim();
    let body = req.body.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return err("title must be 1-120 characters");
    }
    if body.is_empty() || body.chars().count() > 4_000 {
        return err("body must be 1-4000 characters");
    }
    let announcement_id = crate::connect::new_id();
    let created = now();
    let accounts: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM accounts WHERE status='active'")
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
    let saved = async {
        let mut tx = s.db.begin().await?;
        sqlx::query(
            "INSERT INTO announcements (id, kind, title, body, created, created_by) \
             VALUES (?,?,?,?,?,?)",
        )
        .bind(&announcement_id)
        .bind(&req.kind)
        .bind(title)
        .bind(body)
        .bind(created)
        .bind(&me)
        .execute(&mut *tx)
        .await?;
        for (account_id,) in &accounts {
            sqlx::query(
                "INSERT INTO notifications (id, account_id, kind, actor, subject_id, created) \
                 VALUES (?,?,'system',?,?,?)",
            )
            .bind(crate::connect::new_id())
            .bind(account_id)
            .bind(&me)
            .bind(&announcement_id)
            .bind(created)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
    .await;
    if saved.is_err() {
        return err("announcement could not be created");
    }
    for (account_id,) in &accounts {
        s.push_to_account(
            account_id,
            None,
            serde_json::json!({
                "type": "connect_notif", "kind": "system",
                "post_id": announcement_id, "from": Value::Null,
            }),
        );
    }
    audit(
        &s,
        &me,
        "announce",
        &format!("{} kind={} to {} accounts", announcement_id, req.kind, accounts.len()),
        &ip,
    )
    .await;
    Json(serde_json::json!({ "ok": true, "announcement_id": announcement_id, "recipients": accounts.len() }))
}

#[derive(Deserialize)]
pub struct SupportDeleteReq {
    pub thread_id: String,
    pub reason: String,
}

pub async fn support_delete(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SupportDeleteReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !matches!(req.reason.as_str(), "spam" | "test" | "nonsense") {
        return err("delete reason must be spam, test, or nonsense");
    }
    let thread: Option<(String, String)> = sqlx::query_as(
        "SELECT a.handle,t.subject FROM support_threads t \
         JOIN accounts a ON a.id=t.account_id WHERE t.id=?",
    )
    .bind(&req.thread_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((handle, subject)) = thread else {
        return err("no such support thread");
    };
    let deleted = sqlx::query("DELETE FROM support_threads WHERE id=?")
        .bind(&req.thread_id)
        .execute(&s.db)
        .await;
    match deleted {
        Ok(value) if value.rows_affected() == 1 => {
            audit(
                &s,
                &me,
                "support_delete",
                &format!(
                    "{} handle={} reason={} subject={}",
                    req.thread_id,
                    handle,
                    req.reason,
                    subject.chars().take(120).collect::<String>(),
                ),
                &ip,
            )
            .await;
            Json(json!({ "ok": true }))
        }
        _ => err("support thread could not be deleted"),
    }
}

#[derive(Deserialize)]
pub struct CommunityRoleReq {
    /// Account id or handle.
    pub target: String,
    /// `member` or `steward`.
    pub role: String,
}

/// Assign a non-administrative Connect community role. Community Stewards may
/// issue scoped signup invitations, but receive no Console or moderation access.
pub async fn community_role(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommunityRoleReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !matches!(req.role.as_str(), "member" | "steward") {
        return err("role must be member or steward");
    }
    let target: Option<(String, i64)> = sqlx::query_as(
        "SELECT id, founder FROM accounts WHERE (id=?1 OR handle=?1) AND status='active'",
    )
    .bind(&req.target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((target, founder)) = target else {
        return err("no such active account");
    };
    if founder != 0 {
        return err("founder role is managed separately");
    }
    let updated = sqlx::query("UPDATE accounts SET community_role=? WHERE id=?")
        .bind(&req.role)
        .bind(&target)
        .execute(&s.db)
        .await;
    match updated {
        Ok(result) if result.rows_affected() == 1 => {
            audit(
                &s,
                &me,
                "community_role",
                &format!("{target} role={}", req.role),
                &ip,
            )
            .await;
            Json(json!({ "ok": true, "account_id": target, "role": req.role }))
        }
        _ => err("could not update community role"),
    }
}

#[derive(Deserialize)]
pub struct MembershipTierReq {
    /// Account id or handle.
    pub target: String,
    /// `beta` or `paid`.
    pub tier: String,
}

/// Assign a display-only membership tier. Tiers never grant administrative,
/// moderation, invitation, or Console permissions.
pub async fn membership_tier(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<MembershipTierReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !matches!(req.tier.as_str(), "beta" | "paid") {
        return err("tier must be beta or paid");
    }
    let target: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM accounts WHERE (id=?1 OR handle=?1) AND status='active'",
    )
    .bind(&req.target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((target,)) = target else {
        return err("no such active account");
    };
    let updated = sqlx::query("UPDATE accounts SET membership_tier=? WHERE id=?")
        .bind(&req.tier)
        .bind(&target)
        .execute(&s.db)
        .await;
    match updated {
        Ok(result) if result.rows_affected() == 1 => {
            audit(
                &s,
                &me,
                "membership_tier",
                &format!("{target} tier={}", req.tier),
                &ip,
            )
            .await;
            Json(json!({ "ok": true, "account_id": target, "tier": req.tier }))
        }
        _ => err("could not update membership tier"),
    }
}

/// Authenticate and require the caller to be an active founder account.
async fn require_admin(s: &AppState, headers: &HeaderMap, ip: &str) -> Result<String, Json<Value>> {
    let (me, _) = authenticate(s, headers, ip).await?;
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT founder FROM accounts WHERE id = ? AND status = 'active'")
            .bind(&me)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    match row {
        Some((f,)) if f != 0 => Ok(me),
        _ => Err(err("admin only")),
    }
}

// ---------------------------------------------------------------- invites

#[derive(Deserialize)]
pub struct InviteCreateReq {
    /// How many codes to mint (default 1, max 20 per call).
    #[serde(default)]
    pub count: Option<u32>,
}

/// Mint invite codes (founder-only). Prod registration requires an unused
/// code, so this is how new users get onto the server.
pub async fn invite_create(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<InviteCreateReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let count = req.count.unwrap_or(1).clamp(1, 20);
    let mut codes = Vec::with_capacity(count as usize);
    for _ in 0..count {
        // 12 random bytes, hex-encoded (matches the invites schema comment).
        let mut raw = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
        let code: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let ok = sqlx::query("INSERT INTO invites (code, issuer, created) VALUES (?,?,?)")
            .bind(&code)
            .bind(&me)
            .bind(now())
            .execute(&s.db)
            .await;
        if ok.is_ok() {
            codes.push(code);
        }
    }
    if codes.is_empty() {
        return err("failed to mint invites");
    }
    audit(
        &s,
        &me,
        "invite_create",
        &format!("count={}", codes.len()),
        &ip,
    )
    .await;
    let invites: Vec<Value> = codes
        .iter()
        .map(|code| json!({ "code": code, "link": invite_link(&s, &headers, code) }))
        .collect();
    Json(json!({ "ok": true, "codes": codes, "invites": invites }))
}

/// List all invites with redemption status (founder-only).
pub async fn invite_list(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if let Err(e) = require_admin(&s, &headers, &ip).await {
        return e;
    }
    let rows: Vec<(String, String, i64, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT i.code, i.issuer, i.created, i.used_by, i.used_at \
         FROM invites i ORDER BY i.created DESC LIMIT 500",
    )
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (code, issuer, created, used_by, used_at) in rows {
        let used_handle: Option<(String,)> = match &used_by {
            Some(id) => sqlx::query_as("SELECT handle FROM accounts WHERE id = ?")
                .bind(id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None),
            None => None,
        };
        out.push(json!({
            "code": code,
            "link": invite_link(&s, &headers, &code),
            "issuer": issuer,
            "created": created,
            "used_by": used_by,
            "used_handle": used_handle.map(|(h,)| h),
            "used_at": used_at,
        }));
    }
    Json(json!({ "ok": true, "invites": out }))
}

#[derive(Deserialize)]
pub struct InviteRevokeReq {
    pub code: String,
}

/// Delete an unused invite code (founder-only). Used codes are kept as a
/// permanent record of who vouched for whom.
pub async fn invite_revoke(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<InviteRevokeReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let deleted = sqlx::query("DELETE FROM invites WHERE code = ? AND used_by IS NULL")
        .bind(&req.code)
        .execute(&s.db)
        .await;
    match deleted {
        Ok(r) if r.rows_affected() == 1 => {
            audit(&s, &me, "invite_revoke", &req.code, &ip).await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such unused invite"),
    }
}

#[derive(Deserialize)]
pub struct ReportsReq {
    /// Include already-resolved reports (default: open only).
    #[serde(default)]
    pub include_resolved: bool,
}

/// Moderation queue: reports with reporter handle and, where the subject is
/// a post/comment/account, enough context to review without extra queries.
pub async fn reports(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ReportsReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let _ = me;
    let rows: Vec<(
        i64,
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT r.id, r.reporter, r.subject_kind, r.subject_id, r.reason, r.created, \
                    r.resolved_at, r.resolution \
             FROM reports r \
             WHERE (?1 OR r.resolved_at IS NULL) \
             ORDER BY r.created DESC LIMIT 200",
    )
    .bind(req.include_resolved)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (id, reporter, subject_kind, subject_id, reason, created, resolved_at, resolution) in rows {
        let reporter_handle: Option<(String,)> =
            sqlx::query_as("SELECT handle FROM accounts WHERE id = ?")
                .bind(&reporter)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None);
        // Pull subject context so the queue is reviewable at a glance.
        let context: Value = match subject_kind.as_str() {
            "post" => {
                let row: Option<(String, String, Option<i64>)> =
                    sqlx::query_as("SELECT author, body, deleted_at FROM posts WHERE id = ?")
                        .bind(&subject_id)
                        .fetch_optional(&s.db)
                        .await
                        .unwrap_or(None);
                row.map(|(author, body, deleted)| {
                    json!({ "author": author, "body": body, "deleted": deleted.is_some() })
                })
                .unwrap_or_else(|| json!({ "missing": true }))
            }
            "comment" => {
                let row: Option<(String, String, Option<i64>)> =
                    sqlx::query_as("SELECT author, body, deleted_at FROM comments WHERE id = ?")
                        .bind(&subject_id)
                        .fetch_optional(&s.db)
                        .await
                        .unwrap_or(None);
                row.map(|(author, body, deleted)| {
                    json!({ "author": author, "body": body, "deleted": deleted.is_some() })
                })
                .unwrap_or_else(|| json!({ "missing": true }))
            }
            "account" => {
                let row: Option<(String, String)> = sqlx::query_as(
                    "SELECT handle, status FROM accounts WHERE id = ?1 OR handle = ?1",
                )
                .bind(&subject_id)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None);
                row.map(|(handle, status)| json!({ "handle": handle, "status": status }))
                    .unwrap_or_else(|| json!({ "missing": true }))
            }
            _ => json!(null),
        };
        // Evidence snapshot frozen at filing time (0007_evidence). Present
        // for post/comment reports filed after the migration.
        #[allow(clippy::type_complexity)]
        let snapshot: Option<(String, String, String, String, i64)> = sqlx::query_as(
            "SELECT author_handle, body, media, audience, captured_at \
             FROM report_snapshots WHERE report_id = ?",
        )
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
        let snapshot = snapshot.map(|(author_handle, body, media, audience, captured_at)| {
            json!({
                "author_handle": author_handle,
                "body": body,
                "media": serde_json::from_str::<Value>(&media).unwrap_or_else(|_| json!([])),
                "audience": audience,
                "captured_at": captured_at,
            })
        });
        out.push(json!({
            "id": id,
            "reporter": reporter,
            "reporter_handle": reporter_handle.map(|(h,)| h),
            "subject_kind": subject_kind,
            "subject_id": subject_id,
            "reason": reason,
            "created": created,
            "resolved_at": resolved_at,
            "resolution": resolution,
            "context": context,
            "snapshot": snapshot,
        }));
    }
    Json(json!({ "ok": true, "reports": out }))
}

#[derive(Deserialize)]
pub struct ResolveReq {
    pub report_id: i64,
    /// Free-text outcome, e.g. "no action", "content removed", "account suspended".
    pub resolution: String,
}

pub async fn report_resolve(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ResolveReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let resolution = req.resolution.trim();
    if resolution.is_empty() || resolution.len() > 2_000 {
        return err("resolution must be 1-2000 bytes");
    }
    let updated = sqlx::query(
        "UPDATE reports SET resolved_at = ?, resolution = ? WHERE id = ? AND resolved_at IS NULL",
    )
    .bind(now())
    .bind(resolution)
    .bind(req.report_id)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {
            audit(&s, &me, "report_resolve", &req.report_id.to_string(), &ip).await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such open report"),
    }
}

#[derive(Deserialize)]
pub struct SuspendReq {
    /// Account id or handle.
    pub target: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Deserialize)]
pub struct MuteReq {
    /// Account id or handle.
    pub target: String,
    pub duration_hours: i64,
    pub reason: String,
}

/// Temporarily block post/comment creation while preserving sign-in and read access.
pub async fn mute(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<MuteReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !matches!(req.duration_hours, 24 | 72 | 168) {
        return err("mute duration must be 24, 72, or 168 hours");
    }
    let reason = req.reason.trim();
    if reason.is_empty() || reason.len() > 2_000 {
        return err("reason must be 1-2000 bytes");
    }
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT id, founder FROM accounts WHERE (id = ?1 OR handle = ?1) AND status = 'active'",
    )
    .bind(&req.target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((target, founder)) = row else {
        return err("no such active account");
    };
    if founder != 0 {
        return err("cannot mute a founder account");
    }
    let muted_until = now() + req.duration_hours * 3_600;
    let updated = sqlx::query("UPDATE accounts SET muted_until = ? WHERE id = ?")
        .bind(muted_until)
        .bind(&target)
        .execute(&s.db)
        .await;
    match updated {
        Ok(result) if result.rows_affected() == 1 => {
            audit(
                &s,
                &me,
                "mute",
                &format!("{target} until={muted_until} reason={reason}"),
                &ip,
            )
            .await;
            Json(json!({ "ok": true, "account_id": target, "muted_until": muted_until }))
        }
        _ => err("could not mute account"),
    }
}

#[derive(Deserialize)]
pub struct UnmuteReq {
    pub target: String,
}

pub async fn unmute(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UnmuteReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let updated = sqlx::query(
        "UPDATE accounts SET muted_until = NULL \
         WHERE (id = ?1 OR handle = ?1) AND status = 'active' AND muted_until IS NOT NULL",
    )
    .bind(&req.target)
    .execute(&s.db)
    .await;
    match updated {
        Ok(result) if result.rows_affected() == 1 => {
            audit(&s, &me, "unmute", &req.target, &ip).await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such muted account"),
    }
}

/// Suspend an account: auth + all authed endpoints refuse immediately
/// (authenticate() requires status = 'active'). Reversible via unsuspend.
// ToS forfeiture (§5.2): enforcement action = the account forfeits
// data-minimization protections. evidence_hold freezes its entire
// footprint — audit rows, device/session IPs survive the 90-day purge,
// and every blob it owns is exempt from GC — until explicitly lifted.
async fn hold_account(s: &AppState, account_id: &str) {
    let _ =
        sqlx::query("UPDATE accounts SET evidence_hold = COALESCE(evidence_hold, ?) WHERE id = ?")
            .bind(now())
            .bind(account_id)
            .execute(&s.db)
            .await;
    let _ = sqlx::query("UPDATE blobs SET legal_hold = 1 WHERE owner = ?")
        .bind(account_id)
        .execute(&s.db)
        .await;
}

pub async fn suspend(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SuspendReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT id, founder FROM accounts WHERE (id = ?1 OR handle = ?1) AND status = 'active'",
    )
    .bind(&req.target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((target, founder)) = row else {
        return err("no such active account");
    };
    if founder != 0 {
        return err("cannot suspend a founder account");
    }
    let _ = sqlx::query("UPDATE accounts SET status = 'suspended' WHERE id = ?")
        .bind(&target)
        .execute(&s.db)
        .await;
    // Kill live sessions so open apps drop out immediately.
    let _ = sqlx::query(
        "DELETE FROM sessions WHERE device_id IN (SELECT id FROM devices WHERE account_id = ?)",
    )
    .bind(&target)
    .execute(&s.db)
    .await;
    // ToS violated → protections forfeited: freeze the whole footprint.
    hold_account(&s, &target).await;
    audit(
        &s,
        &me,
        "suspend",
        &format!("{} reason={}", target, req.reason),
        &ip,
    )
    .await;
    Json(json!({ "ok": true, "account_id": target }))
}

#[derive(Deserialize)]
pub struct UnsuspendReq {
    pub target: String,
}

pub async fn unsuspend(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<UnsuspendReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let updated = sqlx::query(
        "UPDATE accounts SET status = 'active' WHERE (id = ?1 OR handle = ?1) AND status = 'suspended'",
    )
    .bind(&req.target)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {
            // Reinstatement restores data-minimization protections going
            // forward. Blob legal_holds from report snapshots stay — filed
            // evidence is never un-preserved.
            let _ = sqlx::query(
                "UPDATE accounts SET evidence_hold = NULL WHERE id = ?1 OR handle = ?1",
            )
            .bind(&req.target)
            .execute(&s.db)
            .await;
            audit(&s, &me, "unsuspend", &req.target, &ip).await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such suspended account"),
    }
}

#[derive(Deserialize)]
pub struct TakedownReq {
    /// "post" | "comment".
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub reason: String,
}

/// Soft-delete reported content (DMCA takedowns, ToS violations). Uses the
/// same deleted_at tombstone the owner's delete uses, so feeds/comments
/// queries hide it immediately.
pub async fn takedown(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<TakedownReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let table = match req.kind.as_str() {
        "post" => "posts",
        "comment" => "comments",
        _ => return err("kind must be post or comment"),
    };
    let updated = sqlx::query(&format!(
        "UPDATE {table} SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL"
    ))
    .bind(now())
    .bind(&req.id)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {
            // The author committed a ToS violation — hold their footprint.
            let author: Option<(String,)> =
                sqlx::query_as(&format!("SELECT author FROM {table} WHERE id = ?"))
                    .bind(&req.id)
                    .fetch_optional(&s.db)
                    .await
                    .unwrap_or(None);
            if let Some((author,)) = author {
                hold_account(&s, &author).await;
            }
            audit(
                &s,
                &me,
                "takedown",
                &format!("{}:{} reason={}", req.kind, req.id, req.reason),
                &ip,
            )
            .await;
            Json(json!({ "ok": true }))
        }
        _ => err("no such content"),
    }
}

// ------------------------------------------------------ console read views
// Read endpoints backing the desktop Console (nexus-way-CONSOLE). Metadata
// only — no content beyond what moderation already exposes via reports.

#[derive(Deserialize)]
pub struct AccountsReq {
    /// Optional handle substring filter.
    #[serde(default)]
    pub q: Option<String>,
    /// Optional status filter: active | suspended | deleted.
    #[serde(default)]
    pub status: Option<String>,
}

/// Account roster with moderation-relevant metadata (founder-only).
pub async fn accounts(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AccountsReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if let Err(e) = require_admin(&s, &headers, &ip).await {
        return e;
    }
    let q = format!("%{}%", req.q.as_deref().unwrap_or("").trim());
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        String,
        String,
        i64,
        i64,
        Option<i64>,
        i64,
        i64,
        Option<String>,
        Option<i64>,
        String,
    )> = sqlx::query_as(
        "SELECT a.id, a.handle, a.status, a.created, a.founder, a.last_seen, \
                    (SELECT COUNT(*) FROM devices d \
                       WHERE d.account_id = a.id AND d.revoked_at IS NULL), \
                    (SELECT COUNT(*) FROM posts p \
                       WHERE p.author = a.id AND p.deleted_at IS NULL), \
                          (SELECT h.handle FROM invites i JOIN accounts h ON h.id = i.issuer \
                              WHERE i.used_by = a.id LIMIT 1), a.muted_until, a.community_role \
             FROM accounts a \
             WHERE a.handle LIKE ?1 AND (?2 IS NULL OR a.status = ?2) \
             ORDER BY a.created DESC LIMIT 500",
    )
    .bind(&q)
    .bind(&req.status)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let out: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                id,
                handle,
                status,
                created,
                founder,
                last_seen,
                devices,
                posts,
                invited_by,
                muted_until,
                community_role,
            )| {
                json!({
                    "id": id,
                    "handle": handle,
                    "status": status,
                    "created": created,
                    "founder": founder != 0,
                    "last_seen": last_seen,
                    "devices": devices,
                    "posts": posts,
                    "invited_by": invited_by,
                    "muted_until": muted_until.filter(|until| *until > now()),
                    "community_role": community_role,
                })
            },
        )
        .collect();
    Json(json!({ "ok": true, "accounts": out }))
}

#[derive(Deserialize)]
pub struct DossierReq {
    /// Account id or handle.
    pub target: String,
}

/// Full account dossier: profile, devices, invite lineage, report history
/// (founder-only). Spec §3.3 — no engagement analytics, counts only.
pub async fn dossier(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DossierReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if let Err(e) = require_admin(&s, &headers, &ip).await {
        return e;
    }
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        String,
        i64,
        i64,
        Option<i64>,
        String,
        String,
        Option<i64>,
        String,
        String,
    )> = sqlx::query_as(
        "SELECT a.id, a.handle, a.status, a.created, a.founder, a.last_seen, \
                    COALESCE(p.display_name, ''), COALESCE(p.bio, ''), a.muted_until, \
                    a.community_role, a.membership_tier \
             FROM accounts a LEFT JOIN profiles p ON p.account_id = a.id \
             WHERE a.id = ?1 OR a.handle = ?1",
    )
    .bind(&req.target)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((
        id,
        handle,
        status,
        created,
        founder,
        last_seen,
        display_name,
        bio,
        muted_until,
        community_role,
        membership_tier,
    )) = row
    else {
        return err("no such account");
    };

    let devices: Vec<(String, String, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT id, name, created, last_seen, revoked_at FROM devices \
         WHERE account_id = ? ORDER BY created",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();

    // Invite lineage: who brought this account in, and whom it brought in.
    let invited_by: Option<(String, String)> = sqlx::query_as(
        "SELECT i.code, h.handle FROM invites i JOIN accounts h ON h.id = i.issuer \
         WHERE i.used_by = ? LIMIT 1",
    )
    .bind(&id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let invited: Vec<(String, i64, String, String, i64)> = sqlx::query_as(
        "SELECT i.code, i.created, h.id, h.handle, i.used_at \
         FROM invites i JOIN accounts h ON h.id = i.used_by \
         WHERE i.issuer = ? AND i.used_by IS NOT NULL ORDER BY i.used_at",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();

    // Strike history: reports against this account's content or the account
    // itself, and reports it filed (weaponized-reporting signal, spec §3.3).
    #[allow(clippy::type_complexity)]
    let against: Vec<(
        i64,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT r.id, r.subject_kind, r.subject_id, r.reason, r.created, \
                    r.resolved_at, r.resolution \
             FROM reports r \
             WHERE (r.subject_kind = 'account' AND (r.subject_id = ?1 OR r.subject_id = ?2)) \
                OR (r.subject_kind = 'post' AND r.subject_id IN \
                        (SELECT id FROM posts WHERE author = ?1)) \
                OR (r.subject_kind = 'comment' AND r.subject_id IN \
                        (SELECT id FROM comments WHERE author = ?1)) \
             ORDER BY r.created DESC LIMIT 50",
    )
    .bind(&id)
    .bind(&handle)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    #[allow(clippy::type_complexity)]
    let filed: Vec<(
        i64,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, subject_kind, subject_id, reason, created, resolved_at, resolution \
             FROM reports WHERE reporter = ? ORDER BY created DESC LIMIT 50",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();

    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM posts WHERE author = ?1 AND deleted_at IS NULL), \
                (SELECT COUNT(*) FROM comments WHERE author = ?1 AND deleted_at IS NULL)",
    )
    .bind(&id)
    .fetch_one(&s.db)
    .await
    .unwrap_or((0, 0));

    let report_json = |(rid, kind, sid, reason, rcreated, resolved_at, resolution): (
        i64,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    )| {
        json!({
            "id": rid, "subject_kind": kind, "subject_id": sid, "reason": reason,
            "created": rcreated, "resolved_at": resolved_at, "resolution": resolution,
        })
    };
    Json(json!({
        "ok": true,
        "account": {
            "id": id, "handle": handle, "status": status, "created": created,
            "founder": founder != 0, "last_seen": last_seen,
            "display_name": display_name, "bio": bio,
            "muted_until": muted_until.filter(|until| *until > now()),
            "community_role": community_role,
            "membership_tier": membership_tier,
            "posts": counts.0, "comments": counts.1,
        },
        "devices": devices.into_iter().map(|(did, name, dcreated, dseen, revoked)| json!({
            "id": did, "name": name, "created": dcreated,
            "last_seen": dseen, "revoked_at": revoked,
        })).collect::<Vec<_>>(),
        "invited_by": invited_by.map(|(code, h)| json!({ "code": code, "handle": h })),
        "invited": invited.into_iter().map(|(code, created, account_id, handle, used_at)| json!({
            "code": code,
            "created": created,
            "account_id": account_id,
            "handle": handle,
            "used_at": used_at,
        })).collect::<Vec<_>>(),
        "reports_against": against.into_iter().map(report_json).collect::<Vec<_>>(),
        "reports_filed": filed.into_iter().map(report_json).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct AuditReq {
    /// Optional filter: only rows by this actor (account id or handle).
    #[serde(default)]
    pub actor: Option<String>,
    /// Optional filter: action substring ("suspend", "invite", …).
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Audit log viewer (founder-only, read-only). Spec §3.5.
pub async fn audit_log(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AuditReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if let Err(e) = require_admin(&s, &headers, &ip).await {
        return e;
    }
    // Accept a handle for the actor filter (audit stores account ids).
    let actor = match &req.actor {
        Some(a) => {
            let resolved: Option<(String,)> =
                sqlx::query_as("SELECT id FROM accounts WHERE id = ?1 OR handle = ?1")
                    .bind(a)
                    .fetch_optional(&s.db)
                    .await
                    .unwrap_or(None);
            Some(resolved.map(|(i,)| i).unwrap_or_else(|| a.clone()))
        }
        None => None,
    };
    let action = req.action.as_deref().map(|a| format!("%{a}%"));
    let limit = req.limit.unwrap_or(200).clamp(1, 1000);
    let rows: Vec<(i64, String, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT id, actor, action, subject, at, ip FROM audit \
         WHERE (?1 IS NULL OR actor = ?1) AND (?2 IS NULL OR action LIKE ?2) \
         ORDER BY at DESC LIMIT ?3",
    )
    .bind(&actor)
    .bind(&action)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (aid, actor, action, subject, at, aip) in rows {
        let handle: Option<(String,)> = sqlx::query_as("SELECT handle FROM accounts WHERE id = ?")
            .bind(&actor)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
        out.push(json!({
            "id": aid, "actor": actor, "actor_handle": handle.map(|(h,)| h),
            "action": action, "subject": subject, "at": at, "ip": aip,
        }));
    }
    Json(json!({ "ok": true, "audit": out }))
}

// ------------------------------------------------- evidence & quarantine
// CSAM / danger-to-life posture (spec §5.2, 18 U.S.C. §2258A): preserve,
// never delete. Quarantine hides from every user; legal_hold pins the bytes;
// the evidence bundle is what gets handed to NCMEC / law enforcement.

#[derive(Deserialize)]
pub struct QuarantineReq {
    /// "post" | "comment".
    pub kind: String,
    pub id: String,
    pub reason: String,
}

/// Quarantine content: hidden from all users like a takedown, but flagged
/// preserved and every media blob legal-held so nothing can erase it.
pub async fn quarantine(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<QuarantineReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if req.reason.trim().is_empty() {
        return err("a reason is required");
    }
    let table = match req.kind.as_str() {
        "post" => "posts",
        "comment" => "comments",
        _ => return err("kind must be post or comment"),
    };
    // deleted_at hides it everywhere existing queries already filter;
    // quarantined records that this is preserved evidence, not a deletion.
    let updated = sqlx::query(&format!(
        "UPDATE {table} SET deleted_at = COALESCE(deleted_at, ?1), quarantined = ?1 \
         WHERE id = ?2 AND quarantined IS NULL"
    ))
    .bind(now())
    .bind(&req.id)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {}
        _ => return err("no such content (or already quarantined)"),
    }
    if req.kind == "post" {
        let media: Option<(String,)> = sqlx::query_as("SELECT media FROM posts WHERE id = ?")
            .bind(&req.id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
        if let Some((media,)) = media {
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
    // The author committed a ToS violation — hold their entire footprint.
    let author: Option<(String,)> =
        sqlx::query_as(&format!("SELECT author FROM {table} WHERE id = ?"))
            .bind(&req.id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    if let Some((author,)) = author {
        hold_account(&s, &author).await;
    }
    audit(
        &s,
        &me,
        "quarantine",
        &format!("{}:{} reason={}", req.kind, req.id, req.reason),
        &ip,
    )
    .await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct EvidenceReq {
    pub report_id: i64,
}

/// Account record for an evidence bundle: identity, status, device history
/// (with last IPs). What a subpoena response needs.
async fn account_record(s: &AppState, id: &str) -> Option<Value> {
    #[allow(clippy::type_complexity)]
    let acct: Option<(String, String, String, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT id, handle, status, created, last_seen, evidence_hold FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let devices: Vec<(String, String, i64, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT id, name, created, last_ip, last_seen FROM devices WHERE account_id = ?",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    acct.map(|(id, handle, status, created, last_seen, evidence_hold)| {
        json!({
            "id": id, "handle": handle, "status": status,
            "created": created, "last_seen": last_seen,
            "evidence_hold": evidence_hold,
            "devices": devices.into_iter().map(|(did, name, dcreated, lip, lseen)| json!({
                "id": did, "name": name, "created": dcreated,
                "last_ip": lip, "last_seen": lseen,
            })).collect::<Vec<_>>(),
        })
    })
}

/// The violator's complete footprint (ToS forfeiture): every post and
/// comment including tombstoned/quarantined ones, owned blobs, follow
/// graph, all reports involving them, and their entire audit trail.
async fn account_footprint(s: &AppState, id: &str) -> Value {
    #[allow(clippy::type_complexity)]
    let posts: Vec<(
        String,
        i64,
        String,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT id, created, kind, body, media, audience, deleted_at, quarantined \
             FROM posts WHERE author = ? ORDER BY created",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    #[allow(clippy::type_complexity)]
    let comments: Vec<(String, String, i64, String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT id, post_id, created, body, deleted_at, quarantined \
         FROM comments WHERE author = ? ORDER BY created",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let blobs: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT id, bytes, created, legal_hold FROM blobs WHERE owner = ? ORDER BY created",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let follows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT f.follower, f.followee, f.state FROM follows f \
         WHERE f.follower = ?1 OR f.followee = ?1",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    #[allow(clippy::type_complexity)]
    let reports: Vec<(
        i64,
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT r.id, r.reporter, r.subject_kind, r.subject_id, r.reason, r.created, \
                    r.resolved_at, r.resolution FROM reports r \
             WHERE r.reporter = ?1 \
                OR r.subject_id = ?1 \
                OR r.subject_id IN (SELECT id FROM posts WHERE author = ?1) \
                OR r.subject_id IN (SELECT id FROM comments WHERE author = ?1) \
             ORDER BY r.created",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let audit_rows: Vec<(String, String, i64, Option<String>)> =
        sqlx::query_as("SELECT action, subject, at, ip FROM audit WHERE actor = ? ORDER BY at")
            .bind(id)
            .fetch_all(&s.db)
            .await
            .unwrap_or_default();
    json!({
        "posts": posts.into_iter().map(|(pid, created, kind, body, media, audience, deleted_at, quarantined)| json!({
            "id": pid, "created": created, "kind": kind, "body": body,
            "media": serde_json::from_str::<Value>(&media).unwrap_or_else(|_| json!([])),
            "audience": audience, "deleted_at": deleted_at, "quarantined": quarantined,
        })).collect::<Vec<_>>(),
        "comments": comments.into_iter().map(|(cid, post_id, created, body, deleted_at, quarantined)| json!({
            "id": cid, "post_id": post_id, "created": created, "body": body,
            "deleted_at": deleted_at, "quarantined": quarantined,
        })).collect::<Vec<_>>(),
        "blobs": blobs.into_iter().map(|(bid, bytes, created, hold)| json!({
            "id": bid, "bytes": bytes, "created": created, "legal_hold": hold != 0,
        })).collect::<Vec<_>>(),
        "follows": follows.into_iter().map(|(follower, followee, state)| json!({
            "follower": follower, "followee": followee, "state": state,
        })).collect::<Vec<_>>(),
        "reports": reports.into_iter().map(|(rid, reporter, kind, sid, reason, created, resolved_at, resolution)| json!({
            "id": rid, "reporter": reporter, "subject_kind": kind, "subject_id": sid,
            "reason": reason, "created": created, "resolved_at": resolved_at,
            "resolution": resolution,
        })).collect::<Vec<_>>(),
        "audit_trail": audit_rows.into_iter().map(|(action, subject, at, aip)| json!({
            "action": action, "subject": subject, "at": at, "ip": aip,
        })).collect::<Vec<_>>(),
    })
}

/// Full evidence bundle for one report: the report, the frozen snapshot,
/// reporter + author account records, and the related audit trail. The
/// Console writes this (plus the media bytes) to disk for the authorities.
pub async fn evidence(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<EvidenceReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    #[allow(clippy::type_complexity)]
    let report: Option<(
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT reporter, subject_kind, subject_id, reason, created, resolved_at, resolution \
             FROM reports WHERE id = ?",
    )
    .bind(req.report_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((reporter, subject_kind, subject_id, reason, created, resolved_at, resolution)) =
        report
    else {
        return err("no such report");
    };
    #[allow(clippy::type_complexity)]
    let snapshot: Option<(
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
        Option<i64>,
        String,
        String,
        String,
        String,
        String,
    )> = sqlx::query_as(
        "SELECT author, author_handle, body, media, audience, captured_at, \
                content_created, content_edited, parent_post_id, author_ips, reporter_ip, \
                author_profile, thread \
         FROM report_snapshots WHERE report_id = ?",
    )
    .bind(req.report_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);

    // Account records for reporter and (when snapshotted) the author —
    // identity, creation time, device history. What a subpoena response needs.
    let reporter_record = account_record(&s, &reporter).await;
    let author_id = snapshot
        .as_ref()
        .map(|(author, ..)| author.clone())
        .filter(|a| !a.is_empty());
    let author_record = match &author_id {
        Some(author) => account_record(&s, author).await,
        None => None,
    };
    // ToS forfeiture: the violator's FULL footprint — every post and
    // comment (deleted/quarantined included), every blob they own, their
    // social graph, and their complete audit trail.
    let author_footprint = match &author_id {
        Some(author) => Some(account_footprint(&s, author).await),
        None => None,
    };

    let audit_rows: Vec<(String, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT actor, action, subject, at, ip FROM audit \
         WHERE subject LIKE ?1 OR subject LIKE ?2 ORDER BY at",
    )
    .bind(format!("%{subject_id}%"))
    .bind(format!("%{}%", req.report_id))
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();

    audit(&s, &me, "evidence_export", &req.report_id.to_string(), &ip).await;
    Json(json!({
        "ok": true,
        "generated_at": now(),
        "report": {
            "id": req.report_id, "reporter": reporter, "subject_kind": subject_kind,
            "subject_id": subject_id, "reason": reason, "created": created,
            "resolved_at": resolved_at, "resolution": resolution,
        },
        "snapshot": snapshot.map(|(author, author_handle, body, media, audience, captured_at,
                                   content_created, content_edited, parent_post_id, author_ips,
                                   reporter_ip, author_profile, thread)| json!({
            "author": author, "author_handle": author_handle, "body": body,
            "media": serde_json::from_str::<Value>(&media).unwrap_or_else(|_| json!([])),
            "audience": audience, "captured_at": captured_at,
            "content_created": content_created, "content_edited": content_edited,
            "parent_post_id": parent_post_id,
            "author_ips": serde_json::from_str::<Value>(&author_ips)
                .unwrap_or_else(|_| json!([])),
            "reporter_ip": reporter_ip,
            "author_profile": serde_json::from_str::<Value>(&author_profile)
                .unwrap_or_else(|_| json!({})),
            "thread": serde_json::from_str::<Value>(&thread)
                .unwrap_or_else(|_| json!({})),
        })),
        "reporter_account": reporter_record,
        "author_account": author_record,
        "author_footprint": author_footprint,
        "audit_trail": audit_rows.into_iter().map(|(actor, action, subject, at, aip)| json!({
            "actor": actor, "action": action, "subject": subject, "at": at, "ip": aip,
        })).collect::<Vec<_>>(),
    }))
}

/// Founder-gated media fetch: serves the bytes of any blob (including
/// deleted/quarantined content) for evidence review. Bypasses audience
/// checks BY DESIGN — the audit row is the accountability.
pub async fn media(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    axum::extract::Path(blob_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let ip = peer.ip().to_string();
    let me = match require_admin(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(_) => {
            return (axum::http::StatusCode::FORBIDDEN, "admin only").into_response();
        }
    };
    let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM blobs WHERE id = ?")
        .bind(&blob_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    if exists.is_none() {
        return (axum::http::StatusCode::NOT_FOUND, "not found").into_response();
    }
    audit(&s, &me, "evidence_media_view", &blob_id, &ip).await;
    match std::fs::read(crate::blob::blob_path(&s.data_dir, &blob_id)) {
        Ok(bytes) => bytes.into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "storage error",
        )
            .into_response(),
    }
}
