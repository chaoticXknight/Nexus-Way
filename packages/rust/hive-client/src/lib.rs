// hive-client — the one crate that talks to HIVE (Hive_design_doc.md §9).
// It is to HIVE what ipc.rs is to the local vault socket: VAULT, WIRE, and
// CONNECT all embed this, and the server's integration tests drive it.
//
// Step 0 scope: connection, /v1/info with TOFU server-key pinning, /v1/health.
// Auth, the /v1/stream demux, and blob chunking land with build-order
// steps 1–3.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use nexus_common::b64;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

/// Protocol version this client speaks (checked against the server's
/// `min_client` in /v1/info).
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub ok: bool,
    pub name: String,
    pub version: String,
    /// Base64 Ed25519 server public key — the thing clients pin.
    pub server_pub: String,
    pub min_client: u32,
    #[serde(default)]
    pub dev: bool,
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    accept_self_signed: bool,
    /// TOFU pin (§2.3): set on first contact (or restored by the app from
    /// persisted config); any later mismatch is a hard error, never silent.
    pin: Mutex<Option<String>>,
    /// Current session token (§1.5), set by auth() or set_token().
    token: Mutex<Option<String>>,
}

impl Client {
    /// `base_url` like "https://hive.example.com" or "https://127.0.0.1:8443".
    /// `accept_self_signed` must only be true against a --dev server.
    pub fn new(base_url: impl Into<String>, accept_self_signed: bool) -> Result<Self> {
        // rustls 0.23 needs an explicit process-level crypto provider when
        // both ring and aws-lc-rs are in the dependency graph. Idempotent.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut b = reqwest::Client::builder().use_rustls_tls();
        if accept_self_signed {
            b = b.danger_accept_invalid_certs(true);
        }
        Ok(Self {
            http: b.build()?,
            base: base_url.into().trim_end_matches('/').to_string(),
            accept_self_signed,
            pin: Mutex::new(None),
            token: Mutex::new(None),
        })
    }

    /// Restore a previously persisted server-key pin before first contact.
    pub fn set_pin(&self, server_pub: impl Into<String>) {
        *self.pin.lock().unwrap() = Some(server_pub.into());
    }

    /// The current pin, for the app to persist after first contact.
    pub fn pin(&self) -> Option<String> {
        self.pin.lock().unwrap().clone()
    }

    /// Fetch /v1/info, enforce TOFU pinning and protocol compatibility.
    pub async fn info(&self) -> Result<ServerInfo> {
        let info: ServerInfo = self
            .http
            .get(format!("{}/v1/info", self.base))
            .send()
            .await
            .context("connecting to HIVE")?
            .error_for_status()?
            .json()
            .await
            .context("parsing /v1/info")?;
        if !info.ok {
            bail!("HIVE /v1/info reported not-ok");
        }
        if info.min_client > PROTOCOL_VERSION {
            bail!(
                "HIVE requires client protocol {} but this client speaks {} — update the app",
                info.min_client,
                PROTOCOL_VERSION
            );
        }
        let mut pin = self.pin.lock().unwrap();
        match pin.as_deref() {
            Some(p) if p != info.server_pub => bail!(
                "HIVE server key CHANGED (pinned {p}, got {}) — refusing to continue. \
                 If this server was legitimately rebuilt, remove the stored pin explicitly.",
                info.server_pub
            ),
            Some(_) => {}
            None => *pin = Some(info.server_pub.clone()),
        }
        Ok(info)
    }

    /// /v1/health — true when the server reports itself fully healthy.
    pub async fn health(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct Health {
            ok: bool,
        }
        let resp = self
            .http
            .get(format!("{}/v1/health", self.base))
            .send()
            .await
            .context("connecting to HIVE")?;
        let h: Health = resp.json().await.context("parsing /v1/health")?;
        Ok(h.ok)
    }

    // ------------------------------------------------------------ identity

    /// Register an account (§1.4). The identity key is generated in and
    /// owned by the caller's vault; the device key is this machine's.
    /// Returns (account_id, device_id).
    pub async fn register(
        &self,
        identity: &SigningKey,
        device: &SigningKey,
        handle: &str,
        device_name: &str,
        invite_code: Option<&str>,
    ) -> Result<(String, String)> {
        let identity_pub = b64::encode(identity.verifying_key().as_bytes());
        let device_pub = b64::encode(device.verifying_key().as_bytes());
        let created = unix_now();
        let cert_msg = format!("hive-device-cert:v1:{device_pub}:{device_name}:{created}");
        let cert = b64::encode(&identity.sign(cert_msg.as_bytes()).to_bytes());

        let v = self
            .post(
                "/v1/identity/register",
                serde_json::json!({
                    "identity_pub": identity_pub,
                    "handle": handle,
                    "invite_code": invite_code,
                    "device_pub": device_pub,
                    "device_name": device_name,
                    "device_created": created,
                    "device_cert": cert,
                    // Age gate: callers MUST show a 16-or-older attestation in
                    // their sign-up UI before invoking register().
                    "age_confirmed": true,
                }),
            )
            .await?;
        Ok((
            v["account_id"].as_str().unwrap_or_default().to_string(),
            v["device_id"].as_str().unwrap_or_default().to_string(),
        ))
    }

    /// Challenge–response sign-in (§1.5). Stores the session token in the
    /// client for subsequent authed calls, and returns it for persistence.
    pub async fn auth(&self, account_id: &str, device: &SigningKey) -> Result<String> {
        let device_id = device_id_for(&device.verifying_key());
        let v = self
            .post(
                "/v1/identity/auth_begin",
                serde_json::json!({ "account_id": account_id, "device_id": device_id }),
            )
            .await?;
        let challenge = v["challenge"].as_str().context("no challenge")?.to_string();

        // The pinned server key is folded into the signature (§2.3) so a
        // MITM cannot replay this exchange against another server.
        let server_pub = self
            .pin()
            .context("call info() before auth() to pin the server")?;
        let ts = unix_now();
        let msg = format!("hive-auth:v1:{challenge}:{server_pub}:{ts}");
        let sig = b64::encode(&device.sign(msg.as_bytes()).to_bytes());

        let v = self
            .post(
                "/v1/identity/auth_finish",
                serde_json::json!({
                    "account_id": account_id,
                    "device_id": device_id,
                    "challenge": challenge,
                    "timestamp": ts,
                    "sig": sig,
                }),
            )
            .await?;
        let token = v["token"].as_str().context("no token")?.to_string();
        *self.token.lock().unwrap() = Some(token.clone());
        Ok(token)
    }

    /// Restore a persisted session token.
    pub fn set_token(&self, token: impl Into<String>) {
        *self.token.lock().unwrap() = Some(token.into());
    }

    /// GET /v1/identity/whoami with the current session.
    pub async fn whoami(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/identity/whoami").await
    }

    /// Invalidate the current server-side session token.
    pub async fn logout(&self) -> Result<()> {
        self.post_authed("/v1/identity/logout", serde_json::json!({}))
            .await?;
        *self.token.lock().unwrap() = None;
        Ok(())
    }

    /// GET /v1/identity/devices — the account's device list (Devices page).
    pub async fn devices(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/identity/devices").await
    }

    /// POST /v1/identity/device_revoke — kill a device and its sessions.
    pub async fn device_revoke(&self, device_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/identity/device_revoke",
            serde_json::json!({ "device_id": device_id }),
        )
        .await?;
        Ok(())
    }

    /// POST /v1/identity/account_delete — wipe the account (content under
    /// evidence hold is preserved server-side).
    pub async fn account_delete(&self) -> Result<()> {
        self.post_authed("/v1/identity/account_delete", serde_json::json!({}))
            .await?;
        Ok(())
    }

    // ---------------------------------------------------------------- blobs

    /// Upload data to the blob store (§3.2): content-addressed, chunked,
    /// resumable. Returns the blob ID (hex SHA-256 of the bytes). The caller
    /// encrypts private content BEFORE calling this — HIVE stores what it is
    /// given.
    pub async fn blob_upload(&self, data: &[u8], public: bool) -> Result<String> {
        self.blob_upload_with_purpose(data, public, "general").await
    }

    /// Upload under a server-enforced storage policy class.
    pub async fn blob_upload_with_purpose(
        &self,
        data: &[u8],
        public: bool,
        purpose: &str,
    ) -> Result<String> {
        let hash = hex(&Sha256::digest(data));
        let v = self
            .post_authed(
                "/v1/blob/begin",
                serde_json::json!({
                    "bytes": data.len(),
                    "hash": hash,
                    "public": public,
                    "purpose": purpose,
                }),
            )
            .await?;
        if v["complete"].as_bool() == Some(true) {
            return Ok(hash); // dedupe: already stored
        }
        let chunk_max = v["chunk_max"].as_u64().unwrap_or(4 * 1024 * 1024) as usize;
        let mut offset = v["offset"].as_u64().unwrap_or(0) as usize;

        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        while offset < data.len() {
            let end = (offset + chunk_max).min(data.len());
            let v: serde_json::Value = self
                .http
                .post(format!(
                    "{}/v1/blob/{}/chunk?offset={}",
                    self.base, hash, offset
                ))
                .bearer_auth(&token)
                .body(data[offset..end].to_vec())
                .send()
                .await
                .context("uploading chunk")?
                .json()
                .await
                .context("parsing chunk response")?;
            if v["ok"].as_bool() != Some(true) {
                // Offset mismatch → resume from where the server actually is.
                if let Some(server_offset) = v["offset"].as_u64() {
                    offset = server_offset as usize;
                    continue;
                }
                bail!("HIVE error: {}", v["err"].as_str().unwrap_or("unknown"));
            }
            offset = end;
        }

        let v = self
            .post_authed(&format!("/v1/blob/{hash}/commit"), serde_json::json!({}))
            .await?;
        Ok(v["blob_id"].as_str().unwrap_or(&hash).to_string())
    }

    /// Download a blob and verify its content hash matches its ID.
    pub async fn blob_fetch(&self, blob_id: &str) -> Result<Vec<u8>> {
        let token = self.token.lock().unwrap().clone();
        let mut req = self.http.get(format!("{}/v1/blob/{}", self.base, blob_id));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.context("fetching blob")?;
        anyhow::ensure!(
            resp.status().is_success(),
            "blob fetch failed: {}",
            resp.status()
        );
        let bytes = resp.bytes().await.context("reading blob")?.to_vec();
        let got = hex(&Sha256::digest(&bytes));
        anyhow::ensure!(
            got == blob_id,
            "blob content hash mismatch (corrupt transfer)"
        );
        Ok(bytes)
    }

    /// GET /v1/blob/usage — (used, quota) in bytes.
    pub async fn blob_usage(&self) -> Result<(i64, i64)> {
        let v = self.get_authed("/v1/blob/usage").await?;
        Ok((
            v["used"].as_i64().unwrap_or(0),
            v["quota"].as_i64().unwrap_or(0),
        ))
    }

    /// Probe blob_begin with a declared size without sending data (quota
    /// check, resume-offset query).
    pub async fn blob_upload_probe(&self, bytes: u64, hash: &str) -> Result<serde_json::Value> {
        self.blob_upload_probe_with_purpose(bytes, hash, "general")
            .await
    }

    pub async fn blob_upload_probe_with_purpose(
        &self,
        bytes: u64,
        hash: &str,
        purpose: &str,
    ) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/blob/begin",
            serde_json::json!({
                "bytes": bytes,
                "hash": hash,
                "public": false,
                "purpose": purpose,
            }),
        )
        .await
    }

    /// POST /v1/blob/gc (dev servers only) — returns blobs removed.
    pub async fn gc(&self) -> Result<u64> {
        let v = self
            .post_authed("/v1/blob/gc", serde_json::json!({}))
            .await?;
        Ok(v["blobs_removed"].as_u64().unwrap_or(0))
    }

    // ----------------------------------------------------------- vault sync

    /// Commit snapshot `seq` (must be latest+1) referencing an uploaded blob.
    pub async fn sync_commit(&self, seq: i64, blob_id: &str) -> Result<CommitOutcome> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        let v: serde_json::Value = self
            .http
            .post(format!("{}/v1/sync/commit", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "seq": seq, "blob_id": blob_id }))
            .send()
            .await
            .context("connecting to HIVE")?
            .json()
            .await
            .context("parsing response")?;
        if v["ok"].as_bool() == Some(true) {
            Ok(CommitOutcome::Committed(v["seq"].as_i64().unwrap_or(seq)))
        } else if v["err"].as_str() == Some("behind") {
            Ok(CommitOutcome::Behind(v["latest_seq"].as_i64().unwrap_or(0)))
        } else {
            bail!("HIVE error: {}", v["err"].as_str().unwrap_or("unknown"))
        }
    }

    /// Latest snapshot (seq, blob_id), or None if the account has none.
    pub async fn sync_status(&self) -> Result<Option<(i64, String)>> {
        let v = self.get_authed("/v1/sync/status").await?;
        let seq = v["seq"].as_i64().unwrap_or(0);
        if seq == 0 {
            return Ok(None);
        }
        Ok(Some((
            seq,
            v["blob_id"].as_str().unwrap_or_default().to_string(),
        )))
    }

    /// Pull a snapshot's ciphertext (latest, or a specific seq for restore).
    pub async fn sync_pull(&self, seq: Option<i64>) -> Result<(i64, Vec<u8>)> {
        let v = self
            .post_authed("/v1/sync/pull", serde_json::json!({ "seq": seq }))
            .await?;
        let seq = v["seq"].as_i64().context("no seq")?;
        let blob_id = v["blob_id"].as_str().context("no blob_id")?;
        Ok((seq, self.blob_fetch(blob_id).await?))
    }

    /// Snapshot history for the restore UI.
    pub async fn sync_history(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/sync/history").await
    }

    /// Enroll an additional device (§4.2 linking). This signed-in device
    /// holds the identity key (inside the vault) and certifies the new
    /// device's public key. Returns the new device_id.
    pub async fn device_add(
        &self,
        identity: &SigningKey,
        new_device_pub: &VerifyingKey,
        device_name: &str,
    ) -> Result<String> {
        let device_pub = b64::encode(new_device_pub.as_bytes());
        let created = unix_now();
        let msg = format!("hive-device-cert:v1:{device_pub}:{device_name}:{created}");
        let cert = b64::encode(&identity.sign(msg.as_bytes()).to_bytes());
        let v = self
            .post_authed(
                "/v1/identity/device_add",
                serde_json::json!({
                    "device_pub": device_pub,
                    "device_name": device_name,
                    "device_created": created,
                    "device_cert": cert,
                }),
            )
            .await?;
        Ok(v["device_id"].as_str().unwrap_or_default().to_string())
    }

    // ------------------------------------------------------------ streaming

    /// Open the live event stream (§2.1): authenticates with the current session
    /// token and returns a receiver of tagged frames (sync_hint,
    /// device_event, wire_msg, ...). The connection lives until the handle
    /// is dropped or the server closes.
    pub async fn stream(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        self.open_stream("/v1/stream", &token).await
    }

    pub async fn notification_token(&self) -> Result<String> {
        let value = self
            .post_authed("/v1/notification/token", serde_json::json!({}))
            .await?;
        value["token"]
            .as_str()
            .map(str::to_string)
            .context("missing notification token")
    }

    pub async fn notification_revoke(&self) -> Result<()> {
        self.post_authed("/v1/notification/revoke", serde_json::json!({}))
            .await?;
        Ok(())
    }

    pub async fn notification_stream(
        &self,
        token: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>> {
        self.open_stream("/v1/notification/stream", token).await
    }

    async fn open_stream(
        &self,
        path: &str,
        token: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let url = format!("{}{path}", self.base.replacen("https://", "wss://", 1));
        let connector = if self.accept_self_signed {
            let cfg = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(std::sync::Arc::new(NoVerify))
                .with_no_client_auth();
            Some(tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(
                cfg,
            )))
        } else {
            None
        };
        let (mut ws, _) =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, connector)
                .await
                .context("connecting stream")?;

        // hello → send token → expect authed.
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                    match v["type"].as_str() {
                        Some("hello") => {
                            ws.send(WsMessage::Text(
                                serde_json::json!({ "token": token }).to_string(),
                            ))
                            .await
                            .context("sending stream auth")?;
                        }
                        Some("authed") => break,
                        Some("error") => bail!(
                            "stream auth failed: {}",
                            v["err"].as_str().unwrap_or("unknown")
                        ),
                        _ => {}
                    }
                }
                Some(Ok(_)) => {}
                _ => bail!("stream closed during handshake"),
            }
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws.next().await {
                if let WsMessage::Text(t) = msg {
                    if let Ok(v) = serde_json::from_str(&t) {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(rx)
    }

    // ------------------------------------------------------------- connect

    /// §6.1 profile_set — public plaintext profile.
    pub async fn profile_set(
        &self,
        display_name: &str,
        bio: &str,
        avatar_blob: Option<&str>,
    ) -> Result<()> {
        self.post_authed(
            "/v1/connect/profile_set",
            serde_json::json!({
                "display_name": display_name,
                "bio": bio,
                "avatar_blob": avatar_blob,
            }),
        )
        .await?;
        Ok(())
    }

    /// §6.1 profile_get — target is an account id or a handle.
    pub async fn profile_get(&self, target: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/profile_get",
            serde_json::json!({ "target": target }),
        )
        .await
    }

    /// §6.2 graph ops — target is an account id or a handle.
    pub async fn follow_request(&self, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/follow_request",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    pub async fn follow_accept(&self, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/follow_accept",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    pub async fn follow_decline(&self, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/follow_decline",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    pub async fn unfollow(&self, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/unfollow",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    /// {following, followers, pending_in, pending_out} — each a list of
    /// {account_id, handle, display_name}.
    pub async fn follows(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/connect/follows").await
    }

    pub async fn block(&self, target: &str) -> Result<()> {
        self.post_authed("/v1/connect/block", serde_json::json!({ "target": target }))
            .await?;
        Ok(())
    }

    pub async fn unblock(&self, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/unblock",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    /// §6.3 post_create. `audience` = "public" | "followers" | "circle:<id>".
    /// For circle posts, body/media are ciphertext under the circle key.
    /// Returns the post id.
    pub async fn post_create(
        &self,
        kind: &str,
        body: &str,
        media: &[String],
        audience: &str,
    ) -> Result<String> {
        let v = self
            .post_authed(
                "/v1/connect/post_create",
                serde_json::json!({
                    "kind": kind, "body": body, "media": media, "audience": audience,
                }),
            )
            .await?;
        Ok(v["post_id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn post_create_rich(
        &self,
        kind: &str,
        body: &str,
        media: &[String],
        media_types: &[String],
        audience: &str,
        alt_text: &str,
        content_warning: &str,
    ) -> Result<String> {
        let v = self
            .post_authed(
                "/v1/connect/post_create",
                serde_json::json!({
                    "kind": kind,
                    "body": body,
                    "media": media,
                    "media_types": media_types,
                    "audience": audience,
                    "alt_text": alt_text,
                    "content_warning": content_warning,
                }),
            )
            .await?;
        Ok(v["post_id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn fold_post_create(
        &self,
        body_envelope: &str,
        circle_id: &str,
        fold_epoch: i64,
    ) -> Result<String> {
        let value = self
            .post_authed(
                "/v1/connect/post_create",
                serde_json::json!({
                    "kind": "text",
                    "body": body_envelope,
                    "media": [],
                    "audience": format!("circle:{circle_id}"),
                    "fold_epoch": fold_epoch,
                }),
            )
            .await?;
        Ok(value["post_id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn post_edit(&self, post_id: &str, body: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/post_edit",
            serde_json::json!({ "post_id": post_id, "body": body }),
        )
        .await?;
        Ok(())
    }

    pub async fn post_save(&self, post_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/post_save",
            serde_json::json!({ "post_id": post_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn post_unsave(&self, post_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/post_unsave",
            serde_json::json!({ "post_id": post_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn saved_posts(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/connect/saved_posts").await
    }

    pub async fn post_pin(&self, post_id: &str, pinned: bool) -> Result<()> {
        self.post_authed(
            "/v1/connect/post_pin",
            serde_json::json!({ "post_id": post_id, "pinned": pinned }),
        )
        .await?;
        Ok(())
    }

    pub async fn post_revisions(&self, post_id: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/post_revisions",
            serde_json::json!({ "post_id": post_id }),
        )
        .await
    }

    pub async fn post_search(&self, query: &str, limit: i64) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/post_search",
            serde_json::json!({ "q": query, "limit": limit }),
        )
        .await
    }

    pub async fn post_delete(&self, post_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/post_delete",
            serde_json::json!({ "post_id": post_id }),
        )
        .await?;
        Ok(())
    }

    /// §6.3 chronological feed. Pass the previous page's `next_cursor` to
    /// page backwards; None starts at newest.
    pub async fn feed(&self, before_cursor: Option<&str>, limit: i64) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/feed",
            serde_json::json!({ "before_cursor": before_cursor, "limit": limit }),
        )
        .await
    }

    pub async fn fold_feed(
        &self,
        circle_id: &str,
        before_cursor: Option<&str>,
        limit: i64,
    ) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/fold_feed",
            serde_json::json!({
                "circle_id": circle_id,
                "before_cursor": before_cursor,
                "limit": limit,
            }),
        )
        .await
    }

    /// One author's posts (profile page), audience-gated per post.
    pub async fn author_posts(
        &self,
        target: &str,
        before_cursor: Option<&str>,
        limit: i64,
    ) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/author_posts",
            serde_json::json!({ "target": target, "before_cursor": before_cursor, "limit": limit }),
        )
        .await
    }

    /// Returns the comment id.
    pub async fn comment_create(&self, post_id: &str, body: &str) -> Result<String> {
        let v = self
            .post_authed(
                "/v1/connect/comment_create",
                serde_json::json!({ "post_id": post_id, "body": body }),
            )
            .await?;
        Ok(v["comment_id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn fold_comment_create(
        &self,
        post_id: &str,
        body_envelope: &str,
        fold_epoch: i64,
    ) -> Result<String> {
        let value = self
            .post_authed(
                "/v1/connect/comment_create",
                serde_json::json!({
                    "post_id": post_id,
                    "body": body_envelope,
                    "fold_epoch": fold_epoch,
                }),
            )
            .await?;
        Ok(value["comment_id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn comment_delete(&self, comment_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/comment_delete",
            serde_json::json!({ "comment_id": comment_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn comments(&self, post_id: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/comments",
            serde_json::json!({ "post_id": post_id }),
        )
        .await
    }

    pub async fn react(&self, post_id: &str, kind: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/react",
            serde_json::json!({ "post_id": post_id, "kind": kind }),
        )
        .await?;
        Ok(())
    }

    pub async fn unreact(&self, post_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/unreact",
            serde_json::json!({ "post_id": post_id }),
        )
        .await?;
        Ok(())
    }

    /// Reply to a comment (one level deep). Returns the comment id.
    pub async fn comment_reply(
        &self,
        post_id: &str,
        parent_id: &str,
        body: &str,
    ) -> Result<String> {
        let v = self
            .post_authed(
                "/v1/connect/comment_create",
                serde_json::json!({ "post_id": post_id, "parent_id": parent_id, "body": body }),
            )
            .await?;
        Ok(v["comment_id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn comment_react(&self, comment_id: &str, kind: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/comment_react",
            serde_json::json!({ "comment_id": comment_id, "kind": kind }),
        )
        .await?;
        Ok(())
    }

    pub async fn comment_unreact(&self, comment_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/comment_unreact",
            serde_json::json!({ "comment_id": comment_id }),
        )
        .await?;
        Ok(())
    }

    /// {discoverable, auto_accept, comments_from}.
    pub async fn settings_get(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/connect/settings_get").await
    }

    pub async fn settings_set(
        &self,
        discoverable: bool,
        auto_accept: bool,
        comments_from: &str,
    ) -> Result<()> {
        self.post_authed(
            "/v1/connect/settings_set",
            serde_json::json!({
                "discoverable": discoverable,
                "auto_accept": auto_accept,
                "comments_from": comments_from,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn handle_set(&self, handle: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/handle_set",
            serde_json::json!({ "handle": handle }),
        )
        .await?;
        Ok(())
    }

    /// §6.4 circle_create. `wrapped_keys` = {member_account_id: wrapped key
    /// b64}, wrapped client-side — HIVE stores ciphertext only. Returns the
    /// circle id.
    pub async fn circle_create(
        &self,
        name: &str,
        wrapped_keys: serde_json::Value,
    ) -> Result<String> {
        let v = self
            .post_authed(
                "/v1/connect/circle_create",
                serde_json::json!({ "name": name, "wrapped_keys": wrapped_keys }),
            )
            .await?;
        Ok(v["circle_id"].as_str().unwrap_or_default().to_string())
    }

    /// §6.4 key rotation: replace the full wrapped-key set (member removal =
    /// new key wrapped to the remaining members).
    pub async fn circle_set_keys(
        &self,
        circle_id: &str,
        expected_epoch: i64,
        wrapped_keys: serde_json::Value,
    ) -> Result<()> {
        self.post_authed(
            "/v1/connect/circle_set_keys",
            serde_json::json!({
                "circle_id": circle_id,
                "expected_epoch": expected_epoch,
                "wrapped_keys": wrapped_keys,
            }),
        )
        .await?;
        Ok(())
    }

    /// {own: [...full key maps...], member: [...only my wrapped key...]}.
    pub async fn circles(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/connect/circles").await
    }

    pub async fn fold_invite(
        &self,
        circle_id: &str,
        target: &str,
        wrapped_key: serde_json::Value,
    ) -> Result<()> {
        self.post_authed(
            "/v1/connect/fold_invite",
            serde_json::json!({
                "circle_id": circle_id,
                "target": target,
                "wrapped_key": wrapped_key,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn fold_key_directory(&self, target: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/fold_key_directory",
            serde_json::json!({ "target": target }),
        )
        .await
    }

    pub async fn fold_accept(&self, circle_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/fold_accept",
            serde_json::json!({ "circle_id": circle_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn fold_decline(&self, circle_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/fold_decline",
            serde_json::json!({ "circle_id": circle_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn fold_remove(&self, circle_id: &str, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/fold_remove",
            serde_json::json!({ "circle_id": circle_id, "target": target }),
        )
        .await?;
        Ok(())
    }

    pub async fn fold_leave(&self, circle_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/fold_leave",
            serde_json::json!({ "circle_id": circle_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn circle_delete(&self, circle_id: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/circle_delete",
            serde_json::json!({ "circle_id": circle_id }),
        )
        .await?;
        Ok(())
    }

    /// §6.7 report — subject_kind = "account" | "post" | "comment".
    pub async fn report(&self, subject_kind: &str, subject_id: &str, reason: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/report",
            serde_json::json!({
                "subject_kind": subject_kind, "subject_id": subject_id, "reason": reason,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn report_with_copy(
        &self,
        subject_kind: &str,
        subject_id: &str,
        reason: &str,
        reporter_copy: &str,
    ) -> Result<()> {
        self.post_authed(
            "/v1/connect/report",
            serde_json::json!({
                "subject_kind": subject_kind,
                "subject_id": subject_id,
                "reason": reason,
                "reporter_copy": reporter_copy,
            }),
        )
        .await?;
        Ok(())
    }

    /// Fetch a single post by id (permalink); audience rules apply.
    pub async fn post_get(&self, post_id: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/post_get",
            serde_json::json!({ "post_id": post_id }),
        )
        .await
    }

    /// Founder-only: mint invite codes.
    pub async fn admin_invite_create(&self, count: u32) -> Result<Vec<String>> {
        let v = self
            .post_authed(
                "/v1/admin/invite_create",
                serde_json::json!({ "count": count }),
            )
            .await?;
        Ok(v["codes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Founder-only: broadcast an announcement (system | maintenance | legal)
    /// to every active account. Returns the announcement id.
    pub async fn admin_announce(&self, kind: &str, title: &str, body: &str) -> Result<String> {
        let v = self
            .post_authed(
                "/v1/admin/announce",
                serde_json::json!({ "kind": kind, "title": title, "body": body }),
            )
            .await?;
        Ok(v["announcement_id"].as_str().unwrap_or_default().to_string())
    }

    /// Current legal-document versions + this account's acceptance state.
    pub async fn legal_status(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/legal/status").await
    }

    /// Record explicit acceptance of the current terms + privacy policy.
    pub async fn legal_accept(&self) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/legal/accept",
            serde_json::json!({ "docs": ["terms", "privacy"] }),
        )
        .await
    }

    /// Founder-only: list invites with redemption status.
    pub async fn admin_invite_list(&self) -> Result<serde_json::Value> {
        self.post_authed("/v1/admin/invite_list", serde_json::json!({}))
            .await
    }

    /// Founder-only: delete an unused invite code.
    pub async fn admin_invite_revoke(&self, code: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/invite_revoke",
            serde_json::json!({ "code": code }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: report queue ({reports: [...]}).
    pub async fn admin_reports(&self, include_resolved: bool) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/admin/reports",
            serde_json::json!({ "include_resolved": include_resolved }),
        )
        .await
    }

    /// Founder-only: resolve an open report with an outcome note.
    pub async fn admin_report_resolve(&self, report_id: i64, resolution: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/report_resolve",
            serde_json::json!({ "report_id": report_id, "resolution": resolution }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: suspend an account (id or handle).
    pub async fn admin_suspend(&self, target: &str, reason: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/suspend",
            serde_json::json!({ "target": target, "reason": reason }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: reinstate a suspended account.
    pub async fn admin_unsuspend(&self, target: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/unsuspend",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: temporarily block an account from posting/commenting.
    pub async fn admin_mute(&self, target: &str, duration_hours: i64, reason: &str) -> Result<i64> {
        let value = self
            .post_authed(
                "/v1/admin/mute",
                serde_json::json!({
                    "target": target,
                    "duration_hours": duration_hours,
                    "reason": reason,
                }),
            )
            .await?;
        value["muted_until"].as_i64().context("missing mute expiry")
    }

    /// Founder-only: remove a temporary posting mute.
    pub async fn admin_unmute(&self, target: &str) -> Result<()> {
        self.post_authed("/v1/admin/unmute", serde_json::json!({ "target": target }))
            .await?;
        Ok(())
    }

    /// Founder-only: assign or remove a non-administrative Community Steward role.
    pub async fn admin_community_role(&self, target: &str, role: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/community_role",
            serde_json::json!({ "target": target, "role": role }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: assign a display-only beta or paid membership tier.
    pub async fn admin_membership_tier(&self, target: &str, tier: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/membership_tier",
            serde_json::json!({ "target": target, "tier": tier }),
        )
        .await?;
        Ok(())
    }

    /// Founder/Community Steward: mint scoped beta signup invitations.
    pub async fn community_invite_create(&self, count: u32) -> Result<Vec<String>> {
        let value = self
            .post_authed(
                "/v1/connect/invite_create",
                serde_json::json!({ "count": count }),
            )
            .await?;
        Ok(value["codes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|code| code.as_str().map(String::from))
            .collect())
    }

    pub async fn community_invite_list(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/connect/invite_list").await
    }

    pub async fn community_invite_revoke(&self, code: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/invite_revoke",
            serde_json::json!({ "code": code }),
        )
        .await?;
        Ok(())
    }

    pub async fn support_open(&self, category: &str, subject: &str, body: &str) -> Result<String> {
        let value = self
            .post_authed(
                "/v1/connect/support_open",
                serde_json::json!({
                    "category": category,
                    "subject": subject,
                    "body": body,
                }),
            )
            .await?;
        value["thread_id"]
            .as_str()
            .map(String::from)
            .context("missing support thread id")
    }

    pub async fn support_send(&self, thread_id: &str, body: &str) -> Result<()> {
        self.post_authed(
            "/v1/connect/support_send",
            serde_json::json!({ "thread_id": thread_id, "body": body }),
        )
        .await?;
        Ok(())
    }

    pub async fn support_threads(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/connect/support_threads").await
    }

    pub async fn admin_support_threads(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/admin/support_threads").await
    }

    pub async fn admin_support_reply(&self, thread_id: &str, body: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/support_reply",
            serde_json::json!({ "thread_id": thread_id, "body": body }),
        )
        .await?;
        Ok(())
    }

    pub async fn admin_support_status(&self, thread_id: &str, status: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/support_status",
            serde_json::json!({ "thread_id": thread_id, "status": status }),
        )
        .await?;
        Ok(())
    }

    pub async fn admin_support_delete(&self, thread_id: &str, reason: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/support_delete",
            serde_json::json!({ "thread_id": thread_id, "reason": reason }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: soft-delete reported content (kind = "post" | "comment").
    pub async fn admin_takedown(&self, kind: &str, id: &str, reason: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/takedown",
            serde_json::json!({ "kind": kind, "id": id, "reason": reason }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: account roster with moderation metadata.
    pub async fn admin_accounts(
        &self,
        q: Option<&str>,
        status: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/admin/accounts",
            serde_json::json!({ "q": q, "status": status }),
        )
        .await
    }

    /// Founder-only: full dossier for one account (id or handle).
    pub async fn admin_dossier(&self, target: &str) -> Result<serde_json::Value> {
        self.post_authed("/v1/admin/dossier", serde_json::json!({ "target": target }))
            .await
    }

    /// Founder-only: audit log rows, newest first.
    pub async fn admin_audit(
        &self,
        actor: Option<&str>,
        action: Option<&str>,
        limit: i64,
    ) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/admin/audit",
            serde_json::json!({ "actor": actor, "action": action, "limit": limit }),
        )
        .await
    }

    /// Founder-only: quarantine content — hidden from users, preserved as
    /// evidence with media blobs legal-held (never GC'd).
    pub async fn admin_quarantine(&self, kind: &str, id: &str, reason: &str) -> Result<()> {
        self.post_authed(
            "/v1/admin/quarantine",
            serde_json::json!({ "kind": kind, "id": id, "reason": reason }),
        )
        .await?;
        Ok(())
    }

    /// Founder-only: full evidence bundle for a report (report, snapshot,
    /// account records with device history, audit trail).
    pub async fn admin_evidence(&self, report_id: i64) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/admin/evidence",
            serde_json::json!({ "report_id": report_id }),
        )
        .await
    }

    /// Founder-only: fetch any blob's bytes for evidence review (bypasses
    /// audience checks; audited server-side).
    pub async fn admin_media(&self, blob_id: &str) -> Result<Vec<u8>> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        let resp = self
            .http
            .get(format!("{}/v1/admin/media/{}", self.base, blob_id))
            .bearer_auth(&token)
            .send()
            .await
            .context("connecting to HIVE")?;
        if !resp.status().is_success() {
            bail!("media fetch failed: {}", resp.status());
        }
        Ok(resp.bytes().await.context("reading media")?.to_vec())
    }

    /// Discover search — substring match on handle/display name.
    pub async fn search(&self, q: &str) -> Result<serde_json::Value> {
        self.post_authed("/v1/connect/search", serde_json::json!({ "q": q }))
            .await
    }

    /// The persisted Alerts inbox: {notifications, unseen, next_cursor}.
    pub async fn notifications(
        &self,
        before_cursor: Option<&str>,
        limit: i64,
    ) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/connect/notifications",
            serde_json::json!({ "before_cursor": before_cursor, "limit": limit }),
        )
        .await
    }

    pub async fn notifications_seen(&self) -> Result<()> {
        self.post_authed("/v1/connect/notifications_seen", serde_json::json!({}))
            .await?;
        Ok(())
    }

    /// Fetch Connect media (audience-gated server-side).
    pub async fn media_fetch(&self, blob_id: &str) -> Result<Vec<u8>> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        let resp = self
            .http
            .get(format!("{}/v1/connect/media/{}", self.base, blob_id))
            .bearer_auth(token)
            .send()
            .await
            .context("connecting to HIVE")?;
        anyhow::ensure!(
            resp.status().is_success(),
            "media fetch failed: {}",
            resp.status()
        );
        Ok(resp.bytes().await.context("reading media")?.to_vec())
    }

    // -------------------------------------------------------- device link

    /// §4.2 step 1 (run on the NEW device, unauthenticated): announce the
    /// device pubkey, get the short code to show the user.
    pub async fn link_begin(&self, device: &SigningKey, device_name: &str) -> Result<String> {
        let v = self
            .post(
                "/v1/identity/link_begin",
                serde_json::json!({
                    "device_pub": nexus_common::b64::encode(device.verifying_key().as_bytes()),
                    "device_name": device_name,
                }),
            )
            .await?;
        v["code"]
            .as_str()
            .map(str::to_string)
            .context("missing code")
    }

    /// §4.2 step 2 (enrolled device): look up a pending link request so the
    /// user can see what they're about to approve.
    pub async fn link_fetch(&self, code: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/identity/link_fetch",
            serde_json::json!({ "code": code }),
        )
        .await
    }

    /// §4.2 step 2/3 (run on an ENROLLED device holding the identity key):
    /// fetch the pending request, sign its device cert, approve.
    pub async fn link_approve(&self, identity: &SigningKey, code: &str) -> Result<String> {
        use ed25519_dalek::Signer;
        let pending = self
            .post_authed(
                "/v1/identity/link_fetch",
                serde_json::json!({ "code": code }),
            )
            .await?;
        let device_pub = pending["device_pub"]
            .as_str()
            .context("missing device_pub")?;
        let device_name = pending["device_name"]
            .as_str()
            .context("missing device_name")?;
        let created = unix_now();
        let msg = format!("hive-device-cert:v1:{device_pub}:{device_name}:{created}");
        let cert = nexus_common::b64::encode(&identity.sign(msg.as_bytes()).to_bytes());
        let v = self
            .post_authed(
                "/v1/identity/link_approve",
                serde_json::json!({
                    "code": code, "device_created": created, "device_cert": cert,
                }),
            )
            .await?;
        v["device_id"]
            .as_str()
            .map(str::to_string)
            .context("missing device_id")
    }

    /// §4.2 step 4 (new device): poll until approved; returns account_id.
    pub async fn link_status(&self, code: &str) -> Result<Option<String>> {
        let v = self
            .post(
                "/v1/identity/link_status",
                serde_json::json!({ "code": code }),
            )
            .await?;
        if v["approved"].as_bool().unwrap_or(false) {
            Ok(Some(
                v["account_id"]
                    .as_str()
                    .context("missing account_id")?
                    .to_string(),
            ))
        } else {
            Ok(None)
        }
    }

    // -------------------------------------------------------- recovery escrow

    /// Park a client-encrypted recovery bundle on the server (§1.7). HIVE
    /// only ever sees ciphertext; the caller seals it first.
    pub async fn escrow_set(&self, path: &str, blob: &[u8]) -> Result<()> {
        self.post_authed(
            "/v1/identity/escrow_set",
            serde_json::json!({ "path": path, "blob": b64::encode(blob) }),
        )
        .await?;
        Ok(())
    }

    /// Unauthed: fetch the ciphertext bundle for handle+path (rate-limited
    /// 5/day). Returns (account_id, blob).
    pub async fn escrow_fetch(&self, handle: &str, path: &str) -> Result<(String, Vec<u8>)> {
        let v = self
            .post(
                "/v1/identity/escrow_fetch",
                serde_json::json!({ "handle": handle, "path": path }),
            )
            .await?;
        let account_id = v["account_id"]
            .as_str()
            .context("missing account_id")?
            .to_string();
        let blob = b64::decode(v["blob"].as_str().context("missing blob")?);
        Ok((account_id, blob))
    }

    /// Unauthed: enroll this device using a recovered identity key (§1.7).
    /// Possession of the identity key IS the authorization. Returns device_id.
    pub async fn recover_device(
        &self,
        account_id: &str,
        identity: &SigningKey,
        device: &SigningKey,
        device_name: &str,
    ) -> Result<String> {
        let device_pub = b64::encode(device.verifying_key().as_bytes());
        let created = unix_now();
        let msg = format!("hive-device-cert:v1:{device_pub}:{device_name}:{created}");
        let cert = b64::encode(&identity.sign(msg.as_bytes()).to_bytes());
        let v = self
            .post(
                "/v1/identity/recover_device",
                serde_json::json!({
                    "account_id": account_id,
                    "device_pub": device_pub,
                    "device_name": device_name,
                    "device_created": created,
                    "device_cert": cert,
                }),
            )
            .await?;
        v["device_id"]
            .as_str()
            .map(str::to_string)
            .context("missing device_id")
    }

    // --------------------------------------------------------------- wire

    pub async fn wire_publish(&self, device: &SigningKey, wire_pub: &str) -> Result<()> {
        let device_id = device_id_for(&device.verifying_key());
        let signature = b64::encode(
            &device
                .sign(format!("hive-wire-key:v1:{device_id}:{wire_pub}").as_bytes())
                .to_bytes(),
        );
        self.post_authed(
            "/v1/wire/key",
            serde_json::json!({ "wire_pub": wire_pub, "signature": signature }),
        )
        .await?;
        Ok(())
    }

    pub async fn wire_request(&self, target: &str) -> Result<String> {
        let value = self
            .post_authed("/v1/wire/request", serde_json::json!({ "target": target }))
            .await?;
        value["state"]
            .as_str()
            .map(str::to_string)
            .context("missing request state")
    }

    pub async fn wire_respond(&self, target: &str, accept: bool) -> Result<String> {
        let value = self
            .post_authed(
                "/v1/wire/respond",
                serde_json::json!({ "target": target, "accept": accept }),
            )
            .await?;
        value["state"]
            .as_str()
            .map(str::to_string)
            .context("missing response state")
    }

    pub async fn wire_conversations(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/wire/conversations").await
    }

    pub async fn wire_directory(&self, target: &str) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/wire/directory",
            serde_json::json!({ "target": target }),
        )
        .await
    }

    pub async fn wire_send(
        &self,
        recipient_device: &str,
        msg_id: &str,
        envelope: &str,
    ) -> Result<()> {
        self.wire_send_attachment(recipient_device, msg_id, envelope, None)
            .await
    }

    pub async fn wire_send_attachment(
        &self,
        recipient_device: &str,
        msg_id: &str,
        envelope: &str,
        attachment_blob: Option<&str>,
    ) -> Result<()> {
        self.post_authed(
            "/v1/wire/send",
            serde_json::json!({
                "recipient_device": recipient_device,
                "msg_id": msg_id,
                "envelope": envelope,
                "attachment_blob": attachment_blob,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn wire_call_signal(
        &self,
        target: &str,
        call_id: &str,
        action: &str,
        kind: &str,
        payload: &str,
        signature: &str,
    ) -> Result<()> {
        self.post_authed(
            "/v1/wire/call_signal",
            serde_json::json!({
                "target": target,
                "call_id": call_id,
                "action": action,
                "kind": kind,
                "payload": payload,
                "signature": signature,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn wire_inbox(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/wire/inbox").await
    }

    pub async fn wire_attachment_fetch(&self, blob_id: &str) -> Result<Vec<u8>> {
        let token = self.token.lock().unwrap().clone();
        let mut request = self
            .http
            .get(format!("{}/v1/wire/attachment/{}", self.base, blob_id,));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.context("fetching WIRE attachment")?;
        anyhow::ensure!(
            response.status().is_success(),
            "WIRE attachment fetch failed: {}",
            response.status(),
        );
        Ok(response
            .bytes()
            .await
            .context("reading WIRE attachment")?
            .to_vec())
    }

    pub async fn wire_ack(&self, msg_ids: &[&str]) -> Result<()> {
        self.post_authed("/v1/wire/ack", serde_json::json!({ "msg_ids": msg_ids }))
            .await?;
        Ok(())
    }

    pub async fn wire_history_sync_request(&self) -> Result<()> {
        self.post_authed("/v1/wire/history_sync_request", serde_json::json!({}))
            .await?;
        Ok(())
    }

    pub async fn wire_history_sync_publish(
        &self,
        snapshot_id: &str,
        target_device: &str,
        snapshot_hash: &str,
        snapshot: &str,
        envelope: &str,
        synced_through: i64,
    ) -> Result<()> {
        self.post_authed(
            "/v1/wire/history_sync_publish",
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "target_device": target_device,
                "snapshot_hash": snapshot_hash,
                "snapshot": snapshot,
                "envelope": envelope,
                "synced_through": synced_through,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn wire_history_sync_offers(&self) -> Result<serde_json::Value> {
        self.get_authed("/v1/wire/history_sync_offers").await
    }

    pub async fn wire_history_sync_consume(
        &self,
        snapshot_id: &str,
        synced_through: i64,
    ) -> Result<()> {
        self.post_authed(
            "/v1/wire/history_sync_consume",
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "synced_through": synced_through,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn wire_receipts(&self, msg_ids: &[&str]) -> Result<serde_json::Value> {
        self.post_authed(
            "/v1/wire/receipts",
            serde_json::json!({ "msg_ids": msg_ids }),
        )
        .await
    }

    pub async fn wire_read(&self, msg_ids: &[&str]) -> Result<()> {
        self.post_authed("/v1/wire/read", serde_json::json!({ "msg_ids": msg_ids }))
            .await?;
        Ok(())
    }

    pub async fn wire_retract(&self, msg_id: &str) -> Result<u64> {
        Ok(self
            .post_authed("/v1/wire/retract", serde_json::json!({ "msg_id": msg_id }))
            .await?["retracted"]
            .as_u64()
            .unwrap_or(0))
    }

    // ------------------------------------------------------------- plumbing

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let v: serde_json::Value = self
            .http
            .post(format!("{}{}", self.base, path))
            .json(&body)
            .send()
            .await
            .context("connecting to HIVE")?
            .json()
            .await
            .context("parsing response")?;
        ensure_ok(v)
    }

    async fn get_authed(&self, path: &str) -> Result<serde_json::Value> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        let v: serde_json::Value = self
            .http
            .get(format!("{}{}", self.base, path))
            .bearer_auth(token)
            .send()
            .await
            .context("connecting to HIVE")?
            .json()
            .await
            .context("parsing response")?;
        ensure_ok(v)
    }

    async fn post_authed(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let token = self
            .token
            .lock()
            .unwrap()
            .clone()
            .context("not signed in")?;
        let v: serde_json::Value = self
            .http
            .post(format!("{}{}", self.base, path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .context("connecting to HIVE")?
            .json()
            .await
            .context("parsing response")?;
        ensure_ok(v)
    }
}

fn ensure_ok(v: serde_json::Value) -> Result<serde_json::Value> {
    if v["ok"].as_bool() == Some(true) {
        Ok(v)
    } else {
        bail!("HIVE error: {}", v["err"].as_str().unwrap_or("unknown"))
    }
}

/// Result of a sync commit (§4.1 sequence discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed(i64),
    /// The server has a newer snapshot; pull latest, merge, retry with
    /// latest+1.
    Behind(i64),
}

/// Dev-only TLS verifier for self-signed --dev servers. Trust is carried by
/// the TOFU key pin (§2.3), not the certificate, in dev.
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Generate a fresh Ed25519 keypair (identity or device).
pub fn generate_key() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

/// Self-certifying account ID (§1.2): hex(SHA-256(identity pubkey)).
pub fn account_id_for(key: &VerifyingKey) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

/// Deterministic device ID: hex(SHA-256(device pubkey)).
pub fn device_id_for(key: &VerifyingKey) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}
