// Owns the Axum route map, shared application state, push streams, and cross-service limits.
// Domain modules own request behavior; lib.rs owns bootstrapping this state and serving it.

// HTTP API (Hive_design_doc.md §2): REST-ish JSON ops under /v1/, every
// response an {ok, err?, ...} envelope, plus ONE WebSocket (/v1/stream) that
// will carry all push types (the remote twin of the local push channel).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{blob, connect, identity, sync, wire};

/// 64 KB JSON body cap (§2.2); blob chunks get their own 4 MB limit.
const BODY_LIMIT: usize = 64 * 1024;

/// A connected /v1/stream client: account, generation, and frame sender.
pub type StreamPeer = (String, u64, tokio::sync::mpsc::UnboundedSender<String>);

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub server_pub: String,
    pub name: String,
    pub min_client: u32,
    pub dev: bool,
    pub started_at: i64,
    pub data_dir: PathBuf,
    /// Outstanding auth challenges (§1.5): 60 s TTL, single-use, in-memory.
    pub challenges: Arc<Mutex<HashMap<String, identity::PendingChallenge>>>,
    /// Live push channels keyed by device_id (§2.1: one WebSocket per client).
    pub streams: Arc<Mutex<HashMap<String, StreamPeer>>>,
    /// Wake-only channels owned by the separate Nexus Notify package.
    pub notification_streams: Arc<Mutex<HashMap<String, StreamPeer>>>,
    /// Fixed-window rate-limit buckets: key -> (window_start, count).
    pub rate: Arc<Mutex<HashMap<String, (i64, u32)>>>,
    /// Static website root (None = API only).
    pub web_dir: Option<PathBuf>,
    /// Release APK served at /download/connect.apk (session-gated).
    pub apk_path: Option<PathBuf>,
    /// Nexus Notify APK served at /download/nexus-notify.apk (session-gated).
    pub notifier_apk_path: Option<PathBuf>,
    /// Machine-readable release notes for update banners.
    pub release_notes_path: Option<PathBuf>,
    /// Optional beta access code for static website pages.
    pub site_access_code: Option<String>,
    /// Optional first-account guard for clean public servers.
    pub founder_access_code: Option<String>,
    /// Invite-only registration (config `invite_required`, beta posture).
    pub invite_required: bool,
    /// Trust CF-Connecting-IP / X-Real-IP from loopback peers (config
    /// `behind_proxy`; Cloudflare tunnel deployment).
    pub behind_proxy: bool,
    /// TURN URLs and coturn REST secret for short-lived call relay access.
    pub turn_urls: Vec<String>,
    pub turn_secret: Option<String>,
    /// Cached APK digest for /v1/app/version: (mtime_secs, size, sha256).
    pub apk_hash: Arc<Mutex<Option<(u64, u64, String)>>>,
    /// Cached Nexus Notify APK digest: (mtime_secs, size, sha256).
    pub notifier_apk_hash: Arc<Mutex<Option<(u64, u64, String)>>>,
    /// One-use APK download tickets for browser downloads: ticket -> expires.
    pub download_tickets: Arc<Mutex<HashMap<String, i64>>>,
    /// Monotonic owner tag for replacing a device's stream safely.
    pub next_stream_generation: Arc<AtomicU64>,
}

impl AppState {
    /// Fixed-window rate limiter. Returns false when the caller has exceeded
    /// `max` events per `window_secs` for this key. Disabled on --dev servers
    /// (the integration test harness hammers endpoints freely).
    pub fn rate_ok(&self, key: &str, max: u32, window_secs: i64) -> bool {
        if self.dev {
            return true;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut rate = self.rate.lock().unwrap();
        // Opportunistic cleanup so the map can't grow unbounded.
        if rate.len() > 10_000 {
            rate.retain(|_, (start, _)| now - *start < 3600);
        }
        let entry = rate.entry(key.to_string()).or_insert((now, 0));
        if now - entry.0 >= window_secs {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= max
    }

    /// Push a tagged frame to every connected device of an account,
    /// optionally excluding the originator. Dead channels are pruned.
    pub fn push_to_account(&self, account_id: &str, exclude_device: Option<&str>, frame: Value) {
        let text = frame.to_string();
        let mut streams = self.streams.lock().unwrap();
        streams.retain(|device_id, (acct, _, tx)| {
            if acct != account_id || Some(device_id.as_str()) == exclude_device {
                return true;
            }
            tx.send(text.clone()).is_ok()
        });
        drop(streams);

        if !matches!(
            frame["type"].as_str(),
            Some("connect_notif" | "wire_msg" | "wire_request" | "wire_accepted" | "call_signal")
        ) {
            return;
        }
        let mut notification_streams = self.notification_streams.lock().unwrap();
        notification_streams
            .retain(|_, (acct, _, tx)| acct != account_id || tx.send(text.clone()).is_ok());
    }
}

pub fn router(state: AppState) -> Router {
    // Blob chunk uploads carry raw bytes up to CHUNK_MAX; everything else
    // is small JSON.
    let chunks = Router::new()
        .route("/v1/blob/:hash/chunk", post(blob::chunk))
        .layer(DefaultBodyLimit::max(blob::CHUNK_MAX + 4096));
    let history_sync = Router::new()
        .route(
            "/v1/wire/history_sync_publish",
            post(wire::history_sync_publish),
        )
        .layer(DefaultBodyLimit::max(wire::HISTORY_SYNC_BODY_MAX + 128 * 1024));
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/health", get(health))
        .route("/v1/monitor", get(monitor))
        .route("/v1/stream", get(stream))
        .route("/v1/notification/token", post(notification_token))
        .route("/v1/notification/revoke", post(notification_revoke))
        .route("/v1/notification/stream", get(notification_stream))
        .route("/v1/identity/register", post(identity::register))
        .route("/v1/identity/auth_begin", post(identity::auth_begin))
        .route("/v1/identity/auth_finish", post(identity::auth_finish))
        .route("/v1/identity/logout", post(identity::logout))
        .route("/v1/identity/whoami", get(identity::whoami))
        .route("/v1/identity/devices", get(identity::devices))
        .route("/v1/identity/device_revoke", post(identity::device_revoke))
        .route("/v1/blob/begin", post(blob::begin))
        .route("/v1/blob/:hash/commit", post(blob::commit))
        .route("/v1/blob/:hash", get(blob::fetch))
        .route("/v1/blob/usage", get(blob::usage))
        .route("/v1/blob/gc", post(gc))
        .route("/v1/sync/commit", post(sync::commit))
        .route("/v1/sync/status", get(sync::status))
        .route("/v1/sync/pull", post(sync::pull))
        .route("/v1/sync/history", get(sync::history))
        .route("/v1/identity/device_add", post(sync::device_add))
        .route("/v1/connect/profile_set", post(connect::profile_set))
        .route("/v1/connect/profile_get", post(connect::profile_get))
        .route("/v1/connect/follow_request", post(connect::follow_request))
        .route("/v1/connect/follow_accept", post(connect::follow_accept))
        .route("/v1/connect/follow_decline", post(connect::follow_decline))
        .route("/v1/connect/unfollow", post(connect::unfollow))
        .route("/v1/connect/follows", get(connect::follows))
        .route("/v1/connect/block", post(connect::block))
        .route("/v1/connect/unblock", post(connect::unblock))
        .route("/v1/connect/post_create", post(connect::post_create))
        .route("/v1/connect/post_delete", post(connect::post_delete))
        .route("/v1/connect/feed", post(connect::feed))
        .route("/v1/connect/fold_feed", post(connect::fold_feed))
        .route("/v1/connect/author_posts", post(connect::author_posts))
        .route("/v1/connect/post_save", post(connect::post_save))
        .route("/v1/connect/post_unsave", post(connect::post_unsave))
        .route("/v1/connect/saved_posts", get(connect::saved_posts))
        .route("/v1/connect/post_pin", post(connect::post_pin))
        .route("/v1/connect/post_revisions", post(connect::post_revisions))
        .route("/v1/connect/post_search", post(connect::post_search))
        .route("/v1/connect/comment_create", post(connect::comment_create))
        .route("/v1/connect/comment_delete", post(connect::comment_delete))
        .route("/v1/connect/comments", post(connect::comments))
        .route("/v1/connect/comment_react", post(connect::comment_react))
        .route(
            "/v1/connect/comment_unreact",
            post(connect::comment_unreact),
        )
        .route("/v1/connect/settings_get", get(connect::settings_get))
        .route("/v1/connect/settings_set", post(connect::settings_set))
        .route("/v1/connect/handle_set", post(connect::handle_set))
        .route("/v1/connect/react", post(connect::react))
        .route("/v1/connect/unreact", post(connect::unreact))
        .route("/v1/connect/circle_create", post(connect::circle_create))
        .route(
            "/v1/connect/circle_set_keys",
            post(connect::circle_set_keys),
        )
        .route("/v1/connect/circles", get(connect::circles))
        .route("/v1/connect/fold_invite", post(connect::fold_invite))
        .route(
            "/v1/connect/fold_key_directory",
            post(connect::fold_key_directory),
        )
        .route("/v1/connect/fold_accept", post(connect::fold_accept))
        .route("/v1/connect/fold_decline", post(connect::fold_decline))
        .route("/v1/connect/fold_remove", post(connect::fold_remove))
        .route("/v1/connect/fold_leave", post(connect::fold_leave))
        .route("/v1/connect/circle_delete", post(connect::circle_delete))
        .route("/v1/connect/support_open", post(connect::support_open))
        .route("/v1/connect/support_send", post(connect::support_send))
        .route("/v1/connect/support_threads", get(connect::support_threads))
        .route("/v1/connect/report", post(connect::report))
        .route("/v1/connect/invite_create", post(connect::invite_create))
        .route("/v1/connect/invite_list", get(connect::invite_list))
        .route("/v1/connect/invite_revoke", post(connect::invite_revoke))
        .route("/v1/connect/search", post(connect::search))
        .route("/v1/connect/notifications", post(connect::notifications))
        .route(
            "/v1/connect/notifications_seen",
            post(connect::notifications_seen),
        )
        .route(
            "/v1/connect/notification_delete",
            post(connect::notification_delete),
        )
        .route("/v1/connect/media/:blob_id", get(connect::media))
        .route("/v1/identity/link_begin", post(identity::link_begin))
        .route("/v1/identity/link_fetch", post(identity::link_fetch))
        .route("/v1/identity/link_approve", post(identity::link_approve))
        .route("/v1/identity/link_status", post(identity::link_status))
        .route("/v1/identity/escrow_set", post(identity::escrow_set))
        .route("/v1/identity/escrow_fetch", post(identity::escrow_fetch))
        .route("/v1/legal/status", get(crate::legal::status))
        .route("/v1/legal/accept", post(crate::legal::accept))
        .route(
            "/v1/identity/recover_device",
            post(identity::recover_device),
        )
        .route(
            "/v1/identity/account_delete",
            post(identity::account_delete),
        )
        .route("/v1/wire/key", post(wire::publish_key))
        .route("/v1/wire/request", post(wire::request_chat))
        .route("/v1/wire/respond", post(wire::respond_chat))
        .route("/v1/wire/conversations", get(wire::conversations))
        .route("/v1/wire/directory", post(wire::directory))
        .route("/v1/wire/send", post(wire::send))
        .route("/v1/wire/attachment/:blob_id", get(wire::attachment))
        .route("/v1/wire/inbox", get(wire::inbox))
        .route(
            "/v1/wire/history_sync_request",
            post(wire::history_sync_request),
        )
        .route(
            "/v1/wire/history_sync_offers",
            get(wire::history_sync_offers),
        )
        .route(
            "/v1/wire/history_sync_consume",
            post(wire::history_sync_consume),
        )
        .route("/v1/wire/ack", post(wire::ack))
        .route("/v1/wire/receipts", post(wire::receipts))
        .route("/v1/wire/read", post(wire::mark_read))
        .route("/v1/wire/retract", post(wire::retract))
        .route("/v1/wire/call_signal", post(wire::call_signal))
        .route("/v1/wire/call_ice", get(wire::call_ice))
        .route("/v1/connect/blocked", get(connect::blocked))
        .route("/v1/connect/export", get(connect::export))
        .route("/v1/connect/post_edit", post(connect::post_edit))
        .route("/v1/connect/comment_edit", post(connect::comment_edit))
        .route("/v1/admin/reports", post(crate::admin::reports))
        .route(
            "/v1/admin/report_resolve",
            post(crate::admin::report_resolve),
        )
        .route("/v1/admin/suspend", post(crate::admin::suspend))
        .route("/v1/admin/unsuspend", post(crate::admin::unsuspend))
        .route("/v1/admin/mute", post(crate::admin::mute))
        .route("/v1/admin/unmute", post(crate::admin::unmute))
        .route(
            "/v1/admin/community_role",
            post(crate::admin::community_role),
        )
        .route(
            "/v1/admin/membership_tier",
            post(crate::admin::membership_tier),
        )
        .route("/v1/admin/takedown", post(crate::admin::takedown))
        .route("/v1/admin/invite_create", post(crate::admin::invite_create))
        .route("/v1/admin/invite_list", post(crate::admin::invite_list))
        .route("/v1/admin/invite_revoke", post(crate::admin::invite_revoke))
        .route("/v1/admin/accounts", post(crate::admin::accounts))
        .route("/v1/admin/dossier", post(crate::admin::dossier))
        .route("/v1/admin/audit", post(crate::admin::audit_log))
        .route("/v1/admin/support_threads", get(crate::admin::support_threads))
        .route("/v1/admin/support_reply", post(crate::admin::support_reply))
        .route("/v1/admin/support_status", post(crate::admin::support_status))
        .route("/v1/admin/support_delete", post(crate::admin::support_delete))
        .route("/v1/admin/announce", post(crate::admin::announce))
        .route("/v1/admin/quarantine", post(crate::admin::quarantine))
        .route("/v1/admin/evidence", post(crate::admin::evidence))
        .route("/v1/admin/media/:blob_id", get(crate::admin::media))
        .route("/v1/connect/post_get", post(connect::post_get))
        .route("/legal/terms", get(legal_terms))
        .route("/legal/privacy", get(legal_privacy))
        .route("/download/connect.apk", get(crate::web::apk))
        .route("/download/nexus-notify.apk", get(crate::web::notifier_apk))
        .route("/v1/app/version", get(crate::web::app_version))
        .route("/v1/app/download_ticket", post(crate::web::download_ticket))
        .fallback(get(crate::web::static_file))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .merge(chunks)
        .merge(history_sync)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            real_client_ip,
        ))
        .with_state(state)
}

/// Behind a Cloudflare tunnel every TCP peer is 127.0.0.1, which would gut
/// audit rows and evidence snapshots. When `behind_proxy` is set AND the
/// direct peer is loopback (the tunnel daemon on this box), rewrite the
/// request's ConnectInfo to the real client IP from CF-Connecting-IP (or
/// X-Real-IP). Handlers keep using `ConnectInfo<SocketAddr>` unchanged —
/// 75 call sites, zero edits. Non-loopback peers never get header trust:
/// a client hitting the port directly can't spoof its way into evidence.
async fn real_client_ip(
    State(s): State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let is_api = req.uri().path().starts_with("/v1/");
    if s.behind_proxy {
        let peer_is_loopback = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(false);
        if peer_is_loopback {
            let header_ip = req
                .headers()
                .get("cf-connecting-ip")
                .or_else(|| req.headers().get("x-real-ip"))
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<std::net::IpAddr>().ok());
            if let Some(ip) = header_ip {
                req.extensions_mut()
                    .insert(ConnectInfo(SocketAddr::new(ip, 0)));
            }
        }
    }
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if is_api {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if !s.dev {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}

/// Terms of service — plain text, embedded at compile time so the legal
/// posture ships with the binary and can't drift from the deployed server.
async fn legal_terms() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        include_str!("../legal/terms.txt"),
    )
}

async fn legal_privacy() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        include_str!("../legal/privacy.txt"),
    )
}

/// Manual GC trigger: dev/admin convenience (nightly cron in prod calls the
/// same logic). Dev-only over HTTP to keep the prod surface minimal.
async fn gc(State(s): State<AppState>) -> Json<Value> {
    if !s.dev {
        return Json(json!({ "ok": false, "err": "gc endpoint is dev-only" }));
    }
    let (blobs, uploads) = blob::run_gc(&s).await;
    Json(json!({ "ok": true, "blobs_removed": blobs, "uploads_removed": uploads }))
}

/// Public server descriptor: clients TOFU-pin `server_pub` (§2.3) and refuse
/// servers whose `min_client` exceeds their protocol version.
async fn info(State(s): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": s.name,
        "version": env!("CARGO_PKG_VERSION"),
        "server_pub": s.server_pub,
        "min_client": s.min_client,
        "dev": s.dev,
    }))
}

/// Generic public liveness. Detailed component health is local-only at /v1/monitor.
async fn health(State(s): State<AppState>) -> (StatusCode, Json<Value>) {
    let db_ok = sqlx::query("SELECT 1").execute(&s.db).await.is_ok();
    let blobs_ok = {
        let probe = s.data_dir.join("blobs").join(".health");
        std::fs::write(&probe, b"ok")
            .and_then(|_| std::fs::remove_file(&probe))
            .is_ok()
    };
    let ok = db_ok && blobs_ok;
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(json!({ "ok": ok })))
}

async fn scalar_i64(s: &AppState, sql: &str) -> i64 {
    sqlx::query_as::<_, (i64,)>(sql)
        .fetch_one(&s.db)
        .await
        .map(|row| row.0)
        .unwrap_or(0)
}

async fn monitor(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> (StatusCode, Json<Value>) {
    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "err": "monitor endpoint is local-only" })),
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let db_ok = sqlx::query("SELECT 1").execute(&s.db).await.is_ok();
    let blobs_ok = {
        let probe = s.data_dir.join("blobs").join(".monitor-health");
        std::fs::write(&probe, b"ok")
            .and_then(|_| std::fs::remove_file(&probe))
            .is_ok()
    };
    let blob_files_bytes = std::fs::read_dir(s.data_dir.join("blobs"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len())
                .sum::<u64>()
        })
        .unwrap_or(0);
    let live_streams = s.streams.lock().map(|streams| streams.len()).unwrap_or(0);
    let outstanding_challenges = s.challenges.lock().map(|c| c.len()).unwrap_or(0);
    let rate_buckets = s.rate.lock().map(|r| r.len()).unwrap_or(0);
    let download_tickets = s.download_tickets.lock().map(|t| t.len()).unwrap_or(0);

    let payload = json!({
        "ok": db_ok && blobs_ok,
        "now": now,
        "uptime_secs": (now - s.started_at).max(0),
        "health": { "db": db_ok, "blobs": blobs_ok },
        "runtime": {
            "live_streams": live_streams,
            "outstanding_challenges": outstanding_challenges,
            "rate_buckets": rate_buckets,
            "download_tickets": download_tickets,
        },
        "identity": {
            "accounts_active": scalar_i64(&s, "SELECT COUNT(*) FROM accounts WHERE status='active'").await,
            "accounts_created_24h": scalar_i64(&s, "SELECT COUNT(*) FROM accounts WHERE created > strftime('%s','now') - 86400").await,
            "accounts_seen_24h": scalar_i64(&s, "SELECT COUNT(*) FROM accounts WHERE status='active' AND last_seen > strftime('%s','now') - 86400").await,
            "accounts_suspended": scalar_i64(&s, "SELECT COUNT(*) FROM accounts WHERE status='suspended'").await,
            "accounts_deleted": scalar_i64(&s, "SELECT COUNT(*) FROM accounts WHERE status='deleted'").await,
            "devices_active": scalar_i64(&s, "SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL").await,
            "devices_revoked": scalar_i64(&s, "SELECT COUNT(*) FROM devices WHERE revoked_at IS NOT NULL").await,
            "sessions_active": scalar_i64(&s, "SELECT COUNT(*) FROM sessions WHERE expires > strftime('%s','now')").await,
            "sessions_app": scalar_i64(&s, "SELECT COUNT(*) FROM sessions s JOIN devices d ON d.id=s.device_id WHERE s.expires > strftime('%s','now') AND d.name NOT LIKE 'Web browser%' AND d.name NOT LIKE 'Web signup%'").await,
            "sessions_web_browser": scalar_i64(&s, "SELECT COUNT(*) FROM sessions s JOIN devices d ON d.id=s.device_id WHERE s.expires > strftime('%s','now') AND d.name LIKE 'Web browser%'").await,
            "sessions_web_signup": scalar_i64(&s, "SELECT COUNT(*) FROM sessions s JOIN devices d ON d.id=s.device_id WHERE s.expires > strftime('%s','now') AND d.name LIKE 'Web signup%'").await,
        },
        "connect": {
            "posts": scalar_i64(&s, "SELECT COUNT(*) FROM posts WHERE deleted_at IS NULL").await,
            "posts_24h": scalar_i64(&s, "SELECT COUNT(*) FROM posts WHERE deleted_at IS NULL AND created > strftime('%s','now') - 86400").await,
            "comments": scalar_i64(&s, "SELECT COUNT(*) FROM comments WHERE deleted_at IS NULL").await,
            "comments_24h": scalar_i64(&s, "SELECT COUNT(*) FROM comments WHERE deleted_at IS NULL AND created > strftime('%s','now') - 86400").await,
            "follow_edges": scalar_i64(&s, "SELECT COUNT(*) FROM follows WHERE state='accepted'").await,
            "follow_requests": scalar_i64(&s, "SELECT COUNT(*) FROM follows WHERE state='requested'").await,
            "notifications_unseen": scalar_i64(&s, "SELECT COUNT(*) FROM notifications WHERE seen=0").await,
            "notifications_24h": scalar_i64(&s, "SELECT COUNT(*) FROM notifications WHERE created > strftime('%s','now') - 86400").await,
            "blocks": scalar_i64(&s, "SELECT COUNT(*) FROM blocks").await,
        },
        "wire": {
            "device_keys": scalar_i64(&s, "SELECT COUNT(*) FROM wire_device_keys").await,
            "conversations_accepted": scalar_i64(&s, "SELECT COUNT(*) FROM wire_conversations WHERE state='accepted'").await,
            "conversations_pending": scalar_i64(&s, "SELECT COUNT(*) FROM wire_conversations WHERE state='pending'").await,
            "mailbox_queued": scalar_i64(&s, "SELECT COUNT(*) FROM wire_mailbox WHERE delivered_at IS NULL AND expires > strftime('%s','now')").await,
            "mailbox_delivered_unacked": scalar_i64(&s, "SELECT COUNT(*) FROM wire_mailbox WHERE delivered_at IS NOT NULL AND expires > strftime('%s','now')").await,
            "messages_24h": scalar_i64(&s, "SELECT COUNT(*) FROM wire_receipts WHERE sent_at > strftime('%s','now') - 86400").await,
        },
        "storage": {
            "blob_rows": scalar_i64(&s, "SELECT COUNT(*) FROM blobs").await,
            "blob_bytes_db": scalar_i64(&s, "SELECT COALESCE(SUM(bytes),0) FROM blobs").await,
            "blob_files_bytes_top_level": blob_files_bytes,
        },
        "safety": {
            "reports_open": scalar_i64(&s, "SELECT COUNT(*) FROM reports WHERE resolved_at IS NULL").await,
            "reports_resolved": scalar_i64(&s, "SELECT COUNT(*) FROM reports WHERE resolved_at IS NOT NULL").await,
            "audit_rows_24h": scalar_i64(&s, "SELECT COUNT(*) FROM audit WHERE at > strftime('%s','now') - 86400").await,
        },
    });
    let code = if db_ok && blobs_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(payload))
}

/// The single per-client push channel (§2.1). First frame from the client
/// must be `{"token": "<session token>"}`; after that the server pushes
/// tagged frames (sync_hint, device_event, wire_msg, ...) and answers pings.
async fn stream(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(s): State<AppState>,
) -> Response {
    tracing::debug!(%peer, "stream connect");
    let ip = peer.ip().to_string();
    ws.on_upgrade(move |sock| stream_loop(sock, s, ip))
}

async fn stream_loop(mut sock: WebSocket, s: AppState, ip: String) {
    let hello = json!({ "type": "hello", "server_pub": s.server_pub }).to_string();
    if sock.send(Message::Text(hello)).await.is_err() {
        return;
    }

    // Auth handshake: first text frame carries the session token.
    let (account_id, device_id) = loop {
        match sock.recv().await {
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or_default();
                let token = v["token"].as_str().unwrap_or_default();
                let mut headers = axum::http::HeaderMap::new();
                if let Ok(hv) = format!("Bearer {token}").parse() {
                    headers.insert("authorization", hv);
                }
                match identity::authenticate(&s, &headers, &ip).await {
                    Ok(ids) => {
                        let _ = sock
                            .send(Message::Text(json!({ "type": "authed" }).to_string()))
                            .await;
                        break ids;
                    }
                    Err(_) => {
                        let _ = sock
                            .send(Message::Text(
                                json!({ "type": "error", "err": "auth failed" }).to_string(),
                            ))
                            .await;
                        return;
                    }
                }
            }
            Some(Ok(Message::Ping(p))) => {
                let _ = sock.send(Message::Pong(p)).await;
            }
            Some(Ok(_)) => {}
            _ => return,
        }
    };

    // Register this device's push channel.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let generation = s.next_stream_generation.fetch_add(1, Ordering::Relaxed);
    s.streams
        .lock()
        .unwrap()
        .insert(device_id.clone(), (account_id.clone(), generation, tx));

    // Pump: outbound pushes + inbound pings until either side closes.
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(text) => {
                    if sock.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inbound = sock.recv() => match inbound {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(Message::Ping(p))) => {
                    let _ = sock.send(Message::Pong(p)).await;
                }
                Some(Ok(_)) => {}
            },
        }
    }
    let mut streams = s.streams.lock().unwrap();
    if streams.get(&device_id).map(|(_, owner, _)| *owner) == Some(generation) {
        streams.remove(&device_id);
    }
}

async fn notification_token(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (_, device_id) = match identity::authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(error) => return error,
    };
    let mut token_bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut token_bytes);
    let token = nexus_common::b64::encode(&token_bytes);
    let token_hash = Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let result = sqlx::query(
        "INSERT INTO notification_relays(device_id,token_hash,updated) VALUES(?,?,?) \
         ON CONFLICT(device_id) DO UPDATE SET token_hash=excluded.token_hash,updated=excluded.updated",
    )
    .bind(&device_id)
    .bind(token_hash)
    .bind(crate::identity::now())
    .execute(&s.db)
    .await;
    if result.is_err() {
        return Json(json!({ "ok": false, "err": "could not authorize notification relay" }));
    }
    Json(json!({ "ok": true, "token": token }))
}

async fn notification_revoke(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (_, device_id) = match identity::authenticate(&s, &headers, &ip).await {
        Ok(ids) => ids,
        Err(error) => return error,
    };
    if sqlx::query("DELETE FROM notification_relays WHERE device_id=?")
        .bind(&device_id)
        .execute(&s.db)
        .await
        .is_err()
    {
        return Json(json!({ "ok": false, "err": "could not revoke notification relay" }));
    }
    s.notification_streams.lock().unwrap().remove(&device_id);
    Json(json!({ "ok": true }))
}

async fn notification_stream(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(s): State<AppState>,
) -> Response {
    tracing::debug!(%peer, "notification stream connect");
    ws.on_upgrade(move |sock| notification_stream_loop(sock, s))
}

async fn notification_stream_loop(mut sock: WebSocket, s: AppState) {
    let hello = json!({ "type": "hello", "server_pub": s.server_pub }).to_string();
    if sock.send(Message::Text(hello)).await.is_err() {
        return;
    }

    let (account_id, device_id) = loop {
        match sock.recv().await {
            Some(Ok(Message::Text(text))) => {
                let value: Value = serde_json::from_str(&text).unwrap_or_default();
                let token = value["token"].as_str().unwrap_or_default();
                let token_hash = Sha256::digest(token.as_bytes())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                let row: Option<(String, String)> = sqlx::query_as(
                    "SELECT d.account_id,d.id FROM notification_relays r \
                     JOIN devices d ON d.id=r.device_id JOIN accounts a ON a.id=d.account_id \
                     WHERE r.token_hash=? AND d.revoked_at IS NULL AND a.status='active'",
                )
                .bind(token_hash)
                .fetch_optional(&s.db)
                .await
                .unwrap_or(None);
                if let Some(ids) = row {
                    let _ = sock
                        .send(Message::Text(json!({ "type": "authed" }).to_string()))
                        .await;
                    break ids;
                }
                let _ = sock
                    .send(Message::Text(
                        json!({ "type": "error", "err": "auth failed" }).to_string(),
                    ))
                    .await;
                return;
            }
            Some(Ok(Message::Ping(payload))) => {
                let _ = sock.send(Message::Pong(payload)).await;
            }
            Some(Ok(_)) => {}
            _ => return,
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let generation = s.next_stream_generation.fetch_add(1, Ordering::Relaxed);
    s.notification_streams
        .lock()
        .unwrap()
        .insert(device_id.clone(), (account_id, generation, tx));
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(text) => {
                    if sock.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inbound = sock.recv() => match inbound {
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sock.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let mut streams = s.notification_streams.lock().unwrap();
    if streams.get(&device_id).map(|(_, owner, _)| *owner) == Some(generation) {
        streams.remove(&device_id);
    }
}
