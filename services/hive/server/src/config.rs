// Owns typed HIVE configuration and its production and development defaults.
// main.rs applies CLI overrides; lib.rs consumes the resolved values during server startup.

// Server configuration. Everything that differs dev↔prod is config, never
// code (HIVE.md: "promotion is a redeploy, not a rewrite").
//
// Precedence: hive.toml (if --config given) → CLI overrides → --dev defaults.

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Listen address. Prod: 0.0.0.0:443 (CAP_NET_BIND_SERVICE via systemd).
    pub bind: SocketAddr,
    /// The whole universe lives here: hive.db, blobs/, server_key, TLS material.
    pub data_dir: PathBuf,
    /// Dev mode: self-signed TLS allowed, relaxed posture. Never set in prod.
    pub dev: bool,
    /// PEM cert chain path (prod, e.g. Let's Encrypt fullchain.pem).
    pub tls_cert: Option<PathBuf>,
    /// PEM private key path (prod).
    pub tls_key: Option<PathBuf>,
    /// Human name reported in /v1/info.
    pub name: String,
    /// Minimum client protocol version accepted (advertised in /v1/info).
    pub min_client: u32,
    /// Static website root (landing/signup/login/download pages). None = no
    /// website, API only.
    pub web_dir: Option<PathBuf>,
    /// Release APK served — session-gated — at /download/connect.apk.
    pub apk_path: Option<PathBuf>,
    /// Nexus Notify APK served to signed-in Connect clients.
    pub notifier_apk_path: Option<PathBuf>,
    /// Optional machine-readable release notes served with /v1/app/version.
    pub release_notes_path: Option<PathBuf>,
    /// Optional low-friction beta website gate. When set, static website files
    /// require either this code in `?access=...` once or the cookie it sets.
    /// This is not account auth; it just keeps the public beta site out of
    /// casual view while /v1 API routes remain reachable by installed apps.
    pub site_access_code: Option<String>,
    /// Optional first-account guard. When set on a prod/invite-gated server,
    /// the bootstrap founder registration must supply this as invite_code.
    /// This prevents a clean public server from being claimed by someone who
    /// bypasses the static website and calls /v1/identity/register directly.
    pub founder_access_code: Option<String>,
    /// Invite-only registration (beta posture). Flip to false for a public
    /// launch — registration opens, but codes that ARE supplied must still
    /// be valid and minting keeps working.
    pub invite_required: bool,
    /// Behind a trusted reverse proxy / Cloudflare tunnel on THIS machine:
    /// take the real client IP from CF-Connecting-IP (or X-Real-IP) when the
    /// direct peer is loopback. Without this, every audit row and evidence
    /// snapshot would record 127.0.0.1. Never enable when the port is
    /// directly reachable — clients could spoof the header.
    pub behind_proxy: bool,
    /// TURN URLs advertised to authenticated Connect clients.
    pub turn_urls: Vec<String>,
    /// coturn REST authentication secret used only to mint short-lived
    /// credentials. Never shipped to clients or returned by an API.
    pub turn_secret: Option<String>,
}

impl Default for Config {
    /// Production defaults for a Linux server deployment.
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:443".parse().unwrap(),
            data_dir: PathBuf::from("/var/lib/nexus-hive"),
            dev: false,
            tls_cert: None,
            tls_key: None,
            name: "hive".into(),
            min_client: 1,
            web_dir: None,
            apk_path: None,
            notifier_apk_path: None,
            release_notes_path: None,
            site_access_code: None,
            founder_access_code: None,
            invite_required: true,
            behind_proxy: false,
            turn_urls: Vec::new(),
            turn_secret: None,
        }
    }
}

impl Config {
    /// `--dev` defaults: localhost, high port, per-user data dir, self-signed TLS.
    pub fn dev_defaults() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        Self {
            bind: "127.0.0.1:8443".parse().unwrap(),
            data_dir: PathBuf::from(home).join(".local/share/nexus-hive-dev"),
            dev: true,
            ..Self::default()
        }
    }
}
