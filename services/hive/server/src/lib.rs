// Owns server assembly: data directories, keys, database, TLS, shared state, and listener lifetime.
// main.rs owns CLI/process signals; api.rs and its domain modules own request behavior.

// HIVE server library — the backbone server of the NEXUS-WAY ecosystem.
// See Hive_design_doc.md for the full design. This crate is both the
// `nexus-way-hive` binary and a library so the integration test harness
// (§8.4) can spin ephemeral in-process servers.

pub mod admin;
pub mod api;
pub mod blob;
pub mod config;
pub mod connect;
pub mod db;
pub mod identity;
pub mod keys;
pub mod legal;
pub mod sync;
pub mod tls;
pub mod web;
pub mod wire;

use anyhow::Result;
use std::fs;
use std::net::SocketAddr;

pub use config::Config;

/// Boot the server: data dir, server key, TLS, DB + migrations, then serve
/// until `handle.shutdown()` is called. `handle.listening()` yields the
/// bound address (useful with port 0 in tests).
pub async fn serve(cfg: Config, handle: axum_server::Handle) -> Result<()> {
    // rustls 0.23 needs an explicit process-level crypto provider when both
    // ring and aws-lc-rs are in the dependency graph. Idempotent.
    let _ = rustls::crypto::ring::default_provider().install_default();

    fs::create_dir_all(&cfg.data_dir)?;
    fs::create_dir_all(cfg.data_dir.join("blobs"))?;

    let key = keys::load_or_create(&cfg.data_dir)?;
    let server_pub = nexus_common::b64::encode(key.verifying_key().as_bytes());
    tracing::info!(server_pub, dev = cfg.dev, "HIVE server key ready");

    let db = db::open(&cfg.data_dir).await?;
    let tls = tls::rustls_config(&cfg).await?;
    let state = api::AppState {
        db,
        server_pub,
        name: cfg.name.clone(),
        min_client: cfg.min_client,
        dev: cfg.dev,
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        data_dir: cfg.data_dir.clone(),
        challenges: Default::default(),
        streams: Default::default(),
        notification_streams: Default::default(),
        rate: Default::default(),
        web_dir: cfg.web_dir.clone(),
        apk_path: cfg.apk_path.clone(),
        notifier_apk_path: cfg.notifier_apk_path.clone(),
        release_notes_path: cfg.release_notes_path.clone(),
        site_access_code: cfg.site_access_code.clone(),
        founder_access_code: cfg.founder_access_code.clone(),
        invite_required: cfg.invite_required,
        behind_proxy: cfg.behind_proxy,
        turn_urls: cfg.turn_urls.clone(),
        turn_secret: cfg.turn_secret.clone(),
        apk_hash: Default::default(),
        notifier_apk_hash: Default::default(),
        download_tickets: Default::default(),
        next_stream_generation: Default::default(),
    };
    let app = api::router(state.clone());

    // Data-minimization purge (state privacy laws): every 6 h, drop audit
    // rows and connection metadata older than 90 days, plus expired sessions.
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
        loop {
            tick.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let cutoff = now - 90 * 86_400;
            // Moderation + invite actions are exempt: they are the server's
            // legal record (§230 / DMCA repeat-infringer / vouching chain)
            // and must outlive the 90-day connection-metadata window.
            // Accounts under evidence_hold (ToS forfeiture) are exempt
            // entirely: their trail is frozen for law enforcement.
            let audit_rows = sqlx::query(
                "DELETE FROM audit WHERE at < ? AND action NOT IN \
                 ('report','report_resolve','suspend','unsuspend','mute','unmute', \
                  'takedown','quarantine','invite_create','invite_revoke', \
                  'community_role','membership_tier','community_invite_create', \
                  'community_invite_revoke','support_reply','support_status','support_delete', \
                  'account_delete') \
                 AND actor NOT IN \
                   (SELECT id FROM accounts WHERE evidence_hold IS NOT NULL)",
            )
            .bind(cutoff)
            .execute(&state.db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
            let _ = sqlx::query("DELETE FROM wire_history_sync WHERE expires<?")
                .bind(now)
                .execute(&state.db)
                .await;
            let sessions = sqlx::query(
                "DELETE FROM sessions WHERE expires < ? AND device_id NOT IN \
                 (SELECT d.id FROM devices d JOIN accounts a ON a.id = d.account_id \
                  WHERE a.evidence_hold IS NOT NULL)",
            )
            .bind(now)
            .execute(&state.db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
            let device_ips = sqlx::query(
                "UPDATE devices SET last_ip = NULL \
                 WHERE last_ip IS NOT NULL AND last_seen < ? AND account_id NOT IN \
                   (SELECT id FROM accounts WHERE evidence_hold IS NOT NULL)",
            )
            .bind(cutoff)
            .execute(&state.db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
            tracing::info!(
                audit_rows,
                sessions,
                device_ips,
                "90-day log purge complete"
            );
        }
    });

    tracing::info!(bind = %cfg.bind, data_dir = %cfg.data_dir.display(), "HIVE serving (HTTPS only)");
    axum_server::bind_rustls(cfg.bind, tls)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}
