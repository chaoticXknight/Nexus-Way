// Owns accepted-contact messaging relay, encrypted attachment grants, receipts,
// and call signaling. api.rs owns routing; clients own payload encryption.

use crate::api::AppState;
use crate::identity::{authenticate, now};
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::http::HeaderMap;
use axum::Json;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs;
use std::net::SocketAddr;

const MAX_ENVELOPE_B64: usize = 64 * 1024;
const MAX_CALL_SIGNAL_BYTES: usize = 64 * 1024;
const HISTORY_SYNC_TTL: i64 = 30 * 86_400;
pub const HISTORY_SYNC_BODY_MAX: usize = 1024 * 1024;

fn err(message: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": message }))
}

fn account_pair<'a>(left: &'a str, right: &'a str) -> (&'a str, &'a str) {
    if left < right {
        (left, right)
    } else {
        (right, left)
    }
}

async fn graph_connected(s: &AppState, me: &str, other: &str) -> bool {
    if me == other {
        return true;
    }
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 WHERE EXISTS (SELECT 1 FROM follows WHERE state='accepted' AND \
         ((follower=?1 AND followee=?2) OR (follower=?2 AND followee=?1))) \
         AND NOT EXISTS (SELECT 1 FROM blocks WHERE \
         (blocker=?1 AND blocked=?2) OR (blocker=?2 AND blocked=?1))",
    )
    .bind(me)
    .bind(other)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    row.is_some()
}

async fn chat_allowed(s: &AppState, me: &str, other: &str) -> bool {
    if me == other {
        return true;
    }
    if !graph_connected(s, me, other).await {
        return false;
    }
    let (account_a, account_b) = account_pair(me, other);
    sqlx::query_as::<_, (i64,)>(
        "SELECT 1 FROM wire_conversations WHERE account_a=? AND account_b=? AND state='accepted'",
    )
    .bind(account_a)
    .bind(account_b)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None)
    .is_some()
}

async fn resolve_account(s: &AppState, target: &str) -> Option<(String, String)> {
    sqlx::query_as("SELECT id,handle FROM accounts WHERE (id=? OR handle=?) AND status='active'")
        .bind(target)
        .bind(target.trim_start_matches('@'))
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None)
}

#[derive(Deserialize)]
pub struct ChatTargetReq {
    target: String,
}

pub async fn request_chat(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ChatTargetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let Some((other, _)) = resolve_account(&s, &req.target).await else {
        return err("account not found");
    };
    if me == other || !graph_connected(&s, &me, &other).await {
        return err("message requests are limited to followers and following");
    }
    let (account_a, account_b) = account_pair(&me, &other);
    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT state,requested_by FROM wire_conversations WHERE account_a=? AND account_b=?",
    )
    .bind(account_a)
    .bind(account_b)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if let Some((state, requested_by)) = existing {
        if state == "accepted" {
            s.push_to_account(
                &other,
                None,
                json!({ "type": "wire_request", "from": me, "state": "accepted" }),
            );
            return Json(json!({ "ok": true, "state": state }));
        }
        if requested_by == me {
            return Json(json!({ "ok": true, "state": state }));
        }
        return err("this contact already sent you a message request");
    }
    if sqlx::query(
        "INSERT INTO wire_conversations(account_a,account_b,requested_by,state,created,updated) \
         VALUES(?,?,?,'pending',?,?)",
    )
    .bind(account_a)
    .bind(account_b)
    .bind(&me)
    .bind(now())
    .bind(now())
    .execute(&s.db)
    .await
    .is_err()
    {
        return err("could not create message request");
    }
    s.push_to_account(&other, None, json!({ "type": "wire_request", "from": me }));
    // Persisted alert row so the request also surfaces in the Alerts tab
    // (the wire_request frame only reaches devices that are online now).
    notify_message(&s, &other, "message_request", &me).await;
    Json(json!({ "ok": true, "state": "pending" }))
}

#[derive(Deserialize)]
pub struct RespondReq {
    target: String,
    accept: bool,
}

pub async fn respond_chat(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RespondReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let Some((other, _)) = resolve_account(&s, &req.target).await else {
        return err("account not found");
    };
    if req.accept && !graph_connected(&s, &me, &other).await {
        return err("message requests are limited to followers and following");
    }
    let (account_a, account_b) = account_pair(&me, &other);
    let result = if req.accept {
        sqlx::query(
            "UPDATE wire_conversations SET state='accepted',updated=? \
             WHERE account_a=? AND account_b=? AND state='pending' AND requested_by<>?",
        )
        .bind(now())
        .bind(account_a)
        .bind(account_b)
        .bind(&me)
        .execute(&s.db)
        .await
    } else {
        sqlx::query(
            "DELETE FROM wire_conversations WHERE account_a=? AND account_b=? \
             AND state='pending' AND requested_by<>?",
        )
        .bind(account_a)
        .bind(account_b)
        .bind(&me)
        .execute(&s.db)
        .await
    };
    let Ok(result) = result else {
        return err("could not respond to message request");
    };
    if result.rows_affected() != 1 {
        return err("message request not found");
    }
    if req.accept {
        s.push_to_account(&other, None, json!({ "type": "wire_accepted", "by": me }));
    }
    Json(json!({ "ok": true, "state": if req.accept { "accepted" } else { "declined" } }))
}

pub async fn conversations(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let rows: Vec<(String, String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT a.id,a.handle,COALESCE(p.display_name,''),c.state,c.requested_by,p.avatar_blob \
         FROM wire_conversations c JOIN accounts a ON a.id=CASE WHEN c.account_a=? \
         THEN c.account_b ELSE c.account_a END LEFT JOIN profiles p ON p.account_id=a.id \
         WHERE (c.account_a=? OR c.account_b=?) AND a.status='active' \
         AND NOT EXISTS (SELECT 1 FROM blocks b WHERE (b.blocker=? AND b.blocked=a.id) \
         OR (b.blocker=a.id AND b.blocked=?)) ORDER BY c.updated DESC",
    )
    .bind(&me)
    .bind(&me)
    .bind(&me)
    .bind(&me)
    .bind(&me)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let states: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "account_id": row.0,
                "handle": row.1,
                "display_name": row.2,
                "avatar_blob": row.5,
                "state": row.3,
                "direction": if row.3 == "accepted" { "accepted" } else if row.4 == me {
                    "outgoing"
                } else {
                    "incoming"
                },
            })
        })
        .collect();
    Json(json!({ "ok": true, "conversations": states }))
}

#[derive(Deserialize)]
pub struct PublishReq {
    wire_pub: String,
    signature: String,
}

pub async fn publish_key(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PublishReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (_, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    if nexus_common::b64::decode(&req.wire_pub).len() != 32 {
        return err("wire_pub must be a 32-byte X25519 key");
    }
    let device_pub: Option<(String,)> =
        sqlx::query_as("SELECT device_pub FROM devices WHERE id=? AND revoked_at IS NULL")
            .bind(&device_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((device_pub,)) = device_pub else {
        return err("active device not found");
    };
    let public: Option<[u8; 32]> = nexus_common::b64::decode(&device_pub).try_into().ok();
    let signature: Option<[u8; 64]> = nexus_common::b64::decode(&req.signature).try_into().ok();
    let (Some(public), Some(signature)) = (public, signature) else {
        return err("invalid key signature");
    };
    let Ok(public) = VerifyingKey::from_bytes(&public) else {
        return err("invalid device key");
    };
    let message = format!("hive-wire-key:v1:{device_id}:{}", req.wire_pub);
    if public
        .verify(message.as_bytes(), &Signature::from_bytes(&signature))
        .is_err()
    {
        return err("invalid key signature");
    }
    let result = sqlx::query(
        "INSERT INTO wire_device_keys(device_id,wire_pub,signature,updated) VALUES(?,?,?,?) \
         ON CONFLICT(device_id) DO UPDATE SET wire_pub=excluded.wire_pub, \
         signature=excluded.signature, updated=excluded.updated",
    )
    .bind(&device_id)
    .bind(&req.wire_pub)
    .bind(&req.signature)
    .bind(now())
    .execute(&s.db)
    .await;
    match result {
        Ok(_) => Json(json!({ "ok": true })),
        Err(_) => err("could not publish messaging key"),
    }
}

#[derive(Deserialize)]
pub struct DirectoryReq {
    target: String,
}

pub async fn directory(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DirectoryReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let target: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id,handle,identity_pub FROM accounts WHERE (id=? OR handle=?) AND status='active'",
    )
    .bind(&req.target)
    .bind(req.target.trim_start_matches('@'))
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((account_id, handle, identity_pub)) = target else {
        return err("account not found");
    };
    if !chat_allowed(&s, &me, &account_id).await {
        return err("an accepted message request is required");
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
        "devices": devices.into_iter().map(|d| json!({
            "device_id": d.0, "device_pub": d.1, "cert": d.2,
            "name": d.3, "created": d.4, "wire_pub": d.5,
            "wire_signature": d.6,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct SendReq {
    recipient_device: String,
    msg_id: String,
    envelope: String,
    attachment_blob: Option<String>,
}

pub async fn send(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SendReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, sender_device) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    if req.msg_id.len() != 32 || !req.msg_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return err("invalid message id");
    }
    if req.envelope.is_empty() || req.envelope.len() > MAX_ENVELOPE_B64 {
        return err("encrypted message is too large");
    }
    if req.attachment_blob.as_ref().is_some_and(|blob_id| {
        blob_id.len() != 64 || !blob_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return err("invalid attachment blob");
    }
    let recipient: Option<(String,)> = sqlx::query_as(
        "SELECT d.account_id FROM devices d JOIN wire_device_keys k ON k.device_id=d.id \
         WHERE d.id=? AND d.revoked_at IS NULL",
    )
    .bind(&req.recipient_device)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((recipient_account,)) = recipient else {
        return err("recipient device not found");
    };
    if !chat_allowed(&s, &me, &recipient_account).await {
        return err("an accepted message request is required");
    }
    if !s.rate_ok(&format!("wire-send:{sender_device}"), 120, 60) {
        return err("message rate limit exceeded");
    }
    let mut transaction = match s.db.begin().await {
        Ok(transaction) => transaction,
        Err(_) => return err("could not store encrypted message"),
    };
    if let Some(blob_id) = &req.attachment_blob {
        let owned: Option<(String,)> =
            sqlx::query_as("SELECT id FROM blobs WHERE id=? AND owner=? AND public=0")
                .bind(blob_id)
                .bind(&me)
                .fetch_optional(&mut *transaction)
                .await
                .unwrap_or(None);
        if owned.is_none() {
            return err("attachment blob not found");
        }
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO wire_attachments(msg_id,blob_id,owner_account,created) \
             VALUES(?,?,?,?)",
        )
        .bind(&req.msg_id)
        .bind(blob_id)
        .bind(&me)
        .bind(now())
        .execute(&mut *transaction)
        .await;
        let Ok(inserted) = inserted else {
            return err("could not grant attachment");
        };
        if inserted.rows_affected() == 1
            && sqlx::query("UPDATE blobs SET refcount=refcount+1 WHERE id=?")
                .bind(blob_id)
                .execute(&mut *transaction)
                .await
                .is_err()
        {
            return err("could not retain attachment");
        }
        for account in [&me, &recipient_account] {
            if sqlx::query(
                "INSERT OR IGNORE INTO wire_attachment_grants(msg_id,blob_id,account_id) \
                 VALUES(?,?,?)",
            )
            .bind(&req.msg_id)
            .bind(blob_id)
            .bind(account)
            .execute(&mut *transaction)
            .await
            .is_err()
            {
                return err("could not grant attachment");
            }
        }
    }
    let result = sqlx::query(
        "INSERT OR IGNORE INTO wire_mailbox \
         (recipient,sender_hint,msg_id,envelope,created,expires) VALUES(?,?,?,?,?,?)",
    )
    .bind(&req.recipient_device)
    .bind(&me)
    .bind(&req.msg_id)
    .bind(req.envelope.as_bytes())
    .bind(now())
    .bind(now() + 30 * 86_400)
    .execute(&mut *transaction)
    .await;
    if result.is_err() {
        return err("could not store encrypted message");
    }
    if me != recipient_account {
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO wire_receipts \
               (msg_id,sender_account,recipient_account,sent_at,expires) VALUES(?,?,?,?,?)",
        )
        .bind(&req.msg_id)
        .bind(&me)
        .bind(&recipient_account)
        .bind(now())
        .bind(now() + 30 * 86_400)
        .execute(&mut *transaction)
        .await;
    }
    if transaction.commit().await.is_err() {
        return err("could not store encrypted message");
    }
    s.push_to_account(
        &recipient_account,
        None,
        json!({ "type": "wire_msg", "msg_id": req.msg_id }),
    );
    // Persisted alert row (deduped: at most one unseen "message" alert per
    // sender) so encrypted messages surface in the Alerts tab too. The alert
    // carries only sender + kind — never message content.
    if me != recipient_account {
        notify_message(&s, &recipient_account, "message", &me).await;
    }
    Json(json!({ "ok": true }))
}

/// Insert a content-free message alert row and push the live connect_notif
/// frame, deduplicated so a chatty sender creates at most one unseen alert.
/// wire relays stay metadata-only: kind + sender, no subject, no body.
async fn notify_message(s: &AppState, recipient: &str, kind: &str, actor: &str) {
    let inserted = sqlx::query(
        "INSERT INTO notifications (id, account_id, kind, actor, subject_id, created) \
         SELECT ?,?,?,?,'',? \
         WHERE NOT EXISTS (SELECT 1 FROM notifications \
                           WHERE account_id=?2 AND kind=?3 AND actor=?4 AND seen=0)",
    )
    .bind(crate::connect::new_id())
    .bind(recipient)
    .bind(kind)
    .bind(actor)
    .bind(now())
    .execute(&s.db)
    .await;
    if matches!(inserted, Ok(ref r) if r.rows_affected() == 1) {
        let from = crate::connect::author_card(s, actor).await;
        s.push_to_account(
            recipient,
            None,
            json!({ "type": "connect_notif", "kind": kind, "post_id": "", "from": from }),
        );
    }
}

pub async fn attachment(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    UrlPath(blob_id): UrlPath<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(_) => return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    };
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT b.storage_path FROM blobs b JOIN wire_attachment_grants g ON g.blob_id=b.id \
         WHERE b.id=? AND g.account_id=? LIMIT 1",
    )
    .bind(&blob_id)
    .bind(&account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((storage_path,)) = row else {
        return (axum::http::StatusCode::NOT_FOUND, "not found").into_response();
    };
    match fs::read(s.data_dir.join(storage_path)) {
        Ok(bytes) => bytes.into_response(),
        Err(_) => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[derive(Deserialize)]
pub struct CallSignalReq {
    target: String,
    call_id: String,
    action: String,
    kind: String,
    payload: String,
    signature: String,
}

/// Relay ephemeral WebRTC signaling to an accepted, unblocked chat peer.
pub async fn call_signal(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CallSignalReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, sender_device) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let Some((other, _)) = resolve_account(&s, &req.target).await else {
        return err("account not found");
    };
    if me == other || !chat_allowed(&s, &me, &other).await {
        return err("an accepted message request is required");
    }
    if req.call_id.len() != 32 || !req.call_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return err("invalid call id");
    }
    if !matches!(req.kind.as_str(), "voice" | "video") {
        return err("invalid call kind");
    }
    if !matches!(
        req.action.as_str(),
        "invite"
            | "accept"
            | "offer"
            | "answer"
            | "ice"
            | "heartbeat"
            | "heartbeat_ack"
            | "reject"
            | "hangup"
    ) {
        return err("invalid call action");
    }
    if req.payload.len() > MAX_CALL_SIGNAL_BYTES || req.signature.len() > 128 {
        return err("call signal is too large");
    }
    if !s.rate_ok(&format!("call-signal:{sender_device}"), 240, 60) {
        return err("call signaling rate limit exceeded");
    }
    let frame = json!({
            "type": "call_signal",
            "from": me,
            "sender_device": sender_device,
            "call_id": req.call_id,
            "action": req.action,
            "kind": req.kind,
            "payload": req.payload,
            "signature": req.signature,
            "expires_at": now() + 60,
        });
    if matches!(req.action.as_str(), "invite" | "accept" | "reject" | "hangup") {
        let (first, second) = if me < other { (&me, &other) } else { (&other, &me) };
        let ended = req.action != "invite";
        let saved: Result<(i64, i64), _> = sqlx::query_as(
            "INSERT INTO pending_calls(call_id,first_account,second_account,recipient,frame,expires,ended) \
             VALUES(?,?,?,?,?,?,?) ON CONFLICT(call_id,first_account,second_account) \
             DO UPDATE SET ended=MAX(pending_calls.ended,excluded.ended) RETURNING ended,expires",
        )
        .bind(&req.call_id).bind(first).bind(second).bind(&other)
        .bind(frame.to_string()).bind(now() + 60).bind(ended)
        .fetch_one(&s.db).await;
        let (finished, expires) = match saved {
            Ok(state) => state,
            Err(_) => return err("could not save call state"),
        };
        if req.action == "invite" && (finished != 0 || expires <= now()) {
            return Json(json!({ "ok": true }));
        }
        let _ = sqlx::query("DELETE FROM pending_calls WHERE expires<?")
            .bind(now() - 600).execute(&s.db).await;
    }
    s.push_to_account(&other, None, frame);
    Json(json!({ "ok": true }))
}

pub(crate) async fn pending_call_invites(s: &AppState, recipient: &str) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT p.frame FROM pending_calls p JOIN devices d \
         ON d.id=json_extract(p.frame,'$.sender_device') \
         WHERE p.recipient=? AND p.ended=0 AND p.expires>? AND d.revoked_at IS NULL \
         ORDER BY p.expires LIMIT 100",
    ).bind(recipient).bind(now()).fetch_all(&s.db).await.unwrap_or_default();
    let mut frames = Vec::new();
    for (text,) in rows {
        if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(sender) = frame["from"].as_str() {
                if chat_allowed(s, sender, recipient).await { frames.push(text); }
            }
        }
    }
    frames
}

/// Return STUN plus short-lived coturn REST credentials. The shared TURN
/// secret stays server-side; a leaked client credential expires quickly.
pub async fn call_ice(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let Some(secret) = s.turn_secret.as_deref().filter(|value| !value.is_empty()) else {
        return Json(json!({ "ok": true, "ice_servers": [] }));
    };
    let urls: Vec<&str> = s
        .turn_urls
        .iter()
        .map(String::as_str)
        .filter(|url| url.starts_with("turn:") || url.starts_with("turns:"))
        .collect();
    if urls.is_empty() {
        return Json(json!({ "ok": true, "ice_servers": [] }));
    }
    let expires = crate::identity::now() + 300;
    let username = format!("{expires}:{account_id}");
    let mut mac =
        Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any size");
    mac.update(username.as_bytes());
    let credential = nexus_common::b64::encode(&mac.finalize().into_bytes());
    Json(json!({
        "ok": true,
        "ice_servers": [{
            "urls": urls,
            "username": username,
            "credential": credential,
        }],
        "expires": expires,
    }))
}

pub async fn inbox(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    let messages: Vec<(String, String, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT msg_id,sender_hint,envelope,created FROM wire_mailbox \
         WHERE recipient=? AND expires>? ORDER BY created ASC LIMIT 500",
    )
    .bind(&device_id)
    .bind(now())
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let delivered = now();
    let _ = sqlx::query(
        "UPDATE wire_mailbox SET delivered_at=COALESCE(delivered_at,?) \
         WHERE recipient=? AND delivered_at IS NULL",
    )
    .bind(delivered)
    .bind(&device_id)
    .execute(&s.db)
    .await;
    let _ = sqlx::query(
        "UPDATE wire_receipts SET delivered_at=COALESCE(delivered_at,?) \
         WHERE recipient_account=? AND msg_id IN \
         (SELECT msg_id FROM wire_mailbox WHERE recipient=?)",
    )
    .bind(delivered)
    .bind(&account_id)
    .bind(&device_id)
    .execute(&s.db)
    .await;
    for sender in messages.iter().map(|message| &message.1) {
        if sender != &account_id {
            s.push_to_account(sender, None, json!({ "type": "wire_receipt" }));
        }
    }
    Json(json!({
        "ok": true,
        "messages": messages.into_iter().filter_map(|m| {
            String::from_utf8(m.2).ok().map(|envelope| json!({
                "msg_id": m.0, "sender_hint": m.1,
                "envelope": envelope, "created": m.3,
            }))
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct AckReq {
    msg_ids: Vec<String>,
}

pub async fn ack(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AckReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    if req.msg_ids.is_empty()
        || req.msg_ids.len() > 500
        || req
            .msg_ids
            .iter()
            .any(|id| id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return err("invalid message acknowledgement");
    }
    let mut transaction = match s.db.begin().await {
        Ok(transaction) => transaction,
        Err(_) => return err("could not acknowledge messages"),
    };
    let mut senders = Vec::new();
    for id in &req.msg_ids {
        if let Ok(Some((sender,))) = sqlx::query_as::<_, (String,)>(
            "SELECT sender_hint FROM wire_mailbox WHERE recipient=? AND msg_id=?",
        )
        .bind(&device_id)
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        {
            if sender != account_id {
                senders.push(sender);
            }
        }
        if sqlx::query(
            "UPDATE wire_receipts SET received_at=COALESCE(received_at,?) \
             WHERE msg_id=? AND recipient_account=?",
        )
        .bind(now())
        .bind(id)
        .bind(&account_id)
        .execute(&mut *transaction)
        .await
        .is_err()
        {
            return err("could not acknowledge messages");
        }
        if sqlx::query("DELETE FROM wire_mailbox WHERE recipient=? AND msg_id=?")
            .bind(&device_id)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .is_err()
        {
            return err("could not acknowledge messages");
        }
    }
    if transaction.commit().await.is_err() {
        return err("could not acknowledge messages");
    }
    senders.sort();
    senders.dedup();
    for sender in senders {
        s.push_to_account(&sender, None, json!({ "type": "wire_receipt" }));
    }
    Json(json!({ "ok": true }))
}

// --------------------------------------------------------- history sync

pub async fn history_sync_request(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(error) => return error,
    };
    if !s.rate_ok(&format!("wire-history-request:{device_id}"), 6, 60) {
        return err("message history sync request rate limit exceeded");
    }
    s.push_to_account(
        &account_id,
        Some(&device_id),
        json!({
            "type": "wire_history_sync_request",
            "requesting_device": device_id,
        }),
    );
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct HistorySyncPublishReq {
    snapshot_id: String,
    target_device: String,
    snapshot_hash: String,
    snapshot: String,
    envelope: String,
    synced_through: i64,
}

pub async fn history_sync_publish(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<HistorySyncPublishReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, publisher) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(error) => return error,
    };
    if req.snapshot_id.len() != 32
        || !req.snapshot_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || req.snapshot_hash.len() != 64
        || !req.snapshot_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        || req.snapshot.is_empty()
        || req.snapshot.len() > HISTORY_SYNC_BODY_MAX
        || req.envelope.is_empty()
        || req.envelope.len() > MAX_ENVELOPE_B64
        || req.synced_through < 0
    {
        return err("invalid message history snapshot");
    }
    let actual_hash = format!("{:x}", Sha256::digest(req.snapshot.as_bytes()));
    if actual_hash != req.snapshot_hash {
        return err("message history snapshot hash mismatch");
    }
    if req.target_device == publisher {
        return err("history snapshot target must be another device");
    }
    if !s.rate_ok(&format!("wire-history-publish:{publisher}"), 30, 60) {
        return err("message history sync publish rate limit exceeded");
    }
    let target: Option<(String,)> = sqlx::query_as(
        "SELECT d.account_id FROM devices d JOIN wire_device_keys k ON k.device_id=d.id \
         WHERE d.id=? AND d.revoked_at IS NULL",
    )
    .bind(&req.target_device)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if target.map(|(owner,)| owner) != Some(account_id.clone()) {
        return err("history snapshot target is not an active account device");
    }
    let saved = async {
        let mut transaction = s.db.begin().await?;
        sqlx::query(
            "DELETE FROM wire_history_sync \
             WHERE publisher=? AND target_device=? AND consumed_at IS NULL",
        )
        .bind(&publisher)
        .bind(&req.target_device)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO wire_history_sync \
             (snapshot_id,account_id,publisher,target_device,snapshot_hash,snapshot,envelope,created,expires) \
             VALUES (?,?,?,?,?,?,?,?,?)",
        )
        .bind(&req.snapshot_id)
        .bind(&account_id)
        .bind(&publisher)
        .bind(&req.target_device)
        .bind(&req.snapshot_hash)
        .bind(req.snapshot.as_bytes())
        .bind(req.envelope.as_bytes())
        .bind(now())
        .bind(now() + HISTORY_SYNC_TTL)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }
    .await;
    if saved.is_err() {
        return err("message history snapshot could not be published");
    }
    s.push_to_account(
        &account_id,
        Some(&publisher),
        json!({
            "type": "wire_history_sync_offer",
            "target_device": req.target_device,
        }),
    );
    Json(json!({ "ok": true }))
}

pub async fn history_sync_offers(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (_, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(error) => return error,
    };
    let rows: Vec<(String, String, String, Vec<u8>, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT snapshot_id,publisher,snapshot_hash,snapshot,envelope,created FROM wire_history_sync \
         WHERE target_device=? AND consumed_at IS NULL AND expires>? \
         ORDER BY created,snapshot_id LIMIT 20",
    )
    .bind(&device_id)
    .bind(now())
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    Json(json!({
        "ok": true,
        "offers": rows.into_iter().filter_map(|row| {
            let snapshot = String::from_utf8(row.3).ok()?;
            String::from_utf8(row.4).ok().map(|envelope| json!({
                "snapshot_id": row.0,
                "publisher": row.1,
                "snapshot_hash": row.2,
                "snapshot": snapshot,
                "envelope": envelope,
                "created": row.5,
            }))
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct HistorySyncConsumeReq {
    snapshot_id: String,
    synced_through: i64,
}

pub async fn history_sync_consume(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<HistorySyncConsumeReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(error) => return error,
    };
    let consumed = async {
        let mut transaction = s.db.begin().await?;
        let offer: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM wire_history_sync \
             WHERE snapshot_id=? AND target_device=? AND account_id=? AND consumed_at IS NULL",
        )
        .bind(&req.snapshot_id)
        .bind(&device_id)
        .bind(&account_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((_,)) = offer else {
            return Err(sqlx::Error::RowNotFound);
        };
        sqlx::query("DELETE FROM wire_history_sync WHERE snapshot_id=? AND target_device=?")
            .bind(&req.snapshot_id)
            .bind(&device_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO wire_history_sync_state(account_id,device_id,synced_through,updated) \
             VALUES (?,?,?,?) ON CONFLICT(account_id,device_id) DO UPDATE SET \
             synced_through=MAX(wire_history_sync_state.synced_through,excluded.synced_through), \
             updated=excluded.updated",
        )
        .bind(&account_id)
        .bind(&device_id)
        .bind(req.synced_through.max(0))
        .bind(now())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }
    .await;
    match consumed {
        Ok(()) => Json(json!({ "ok": true })),
        Err(_) => err("message history snapshot not found"),
    }
}

#[derive(Deserialize)]
pub struct MessageIdsReq {
    msg_ids: Vec<String>,
}

fn valid_message_ids(ids: &[String]) -> bool {
    !ids.is_empty()
        && ids.len() <= 500
        && ids
            .iter()
            .all(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub async fn receipts(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<MessageIdsReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    if !valid_message_ids(&req.msg_ids) {
        return err("invalid receipt request");
    }
    let mut rows = Vec::new();
    for id in &req.msg_ids {
        if let Ok(Some(row)) =
            sqlx::query_as::<_, (String, i64, Option<i64>, Option<i64>, Option<i64>)>(
                "SELECT msg_id,sent_at,delivered_at,received_at,read_at FROM wire_receipts \
             WHERE msg_id=? AND sender_account=? AND expires>?",
            )
            .bind(id)
            .bind(&account_id)
            .bind(now())
            .fetch_optional(&s.db)
            .await
        {
            rows.push(json!({
                "msg_id": row.0, "sent_at": row.1, "delivered_at": row.2,
                "received_at": row.3, "read_at": row.4,
            }));
        }
    }
    Json(json!({ "ok": true, "receipts": rows }))
}

pub async fn mark_read(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<MessageIdsReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    if !valid_message_ids(&req.msg_ids) {
        return err("invalid read receipt");
    }
    let mut senders = Vec::new();
    for id in &req.msg_ids {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT sender_account FROM wire_receipts WHERE msg_id=? \
             AND recipient_account=? AND received_at IS NOT NULL AND expires>?",
        )
        .bind(id)
        .bind(&account_id)
        .bind(now())
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
        if !rows.is_empty() {
            let _ = sqlx::query(
                "UPDATE wire_receipts SET read_at=COALESCE(read_at,?) \
                 WHERE msg_id=? AND recipient_account=? AND received_at IS NOT NULL",
            )
            .bind(now())
            .bind(id)
            .bind(&account_id)
            .execute(&s.db)
            .await;
            senders.extend(rows.into_iter().map(|row| row.0));
        }
    }
    senders.sort();
    senders.dedup();
    for sender in senders {
        s.push_to_account(&sender, None, json!({ "type": "wire_receipt" }));
    }
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct RetractReq {
    msg_id: String,
}

pub async fn retract(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RetractReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(e) => return e,
    };
    if !valid_message_ids(std::slice::from_ref(&req.msg_id)) {
        return err("invalid message retraction");
    }
    let mut transaction = match s.db.begin().await {
        Ok(transaction) => transaction,
        Err(_) => return err("could not retract message"),
    };
    let attachment_ids: Vec<(String,)> =
        sqlx::query_as("SELECT blob_id FROM wire_attachments WHERE msg_id=? AND owner_account=?")
            .bind(&req.msg_id)
            .bind(&account_id)
            .fetch_all(&mut *transaction)
            .await
            .unwrap_or_default();
    let result = sqlx::query("DELETE FROM wire_mailbox WHERE msg_id=? AND sender_hint=?")
        .bind(&req.msg_id)
        .bind(&account_id)
        .execute(&mut *transaction)
        .await;
    let Ok(result) = result else {
        return err("could not retract message");
    };
    if sqlx::query("DELETE FROM wire_attachments WHERE msg_id=? AND owner_account=?")
        .bind(&req.msg_id)
        .bind(&account_id)
        .execute(&mut *transaction)
        .await
        .is_err()
    {
        return err("could not retract attachment");
    }
    for (blob_id,) in attachment_ids {
        if sqlx::query("UPDATE blobs SET refcount=refcount-1 WHERE id=?")
            .bind(blob_id)
            .execute(&mut *transaction)
            .await
            .is_err()
        {
            return err("could not release attachment");
        }
    }
    if transaction.commit().await.is_err() {
        return err("could not retract message");
    }
    Json(json!({ "ok": true, "retracted": result.rows_affected() }))
}
