// Owns accounts, device certificates, challenge-response authentication, sessions, and recovery.
// api.rs owns transport and rate buckets; clients own private identity and device keys.

// Identity service (Hive_design_doc.md §1): registration, device
// certificates, challenge–response auth, sessions, revocation.
//
// Signed message formats (duplicated in hive-client — change both together):
//   device cert:  "hive-device-cert:v1:{device_pub_b64}:{name}:{created}"
//   auth finish:  "hive-auth:v1:{challenge_b64}:{server_pub_b64}:{timestamp}"

use crate::api::AppState;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::Json;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Challenge lifetime (§1.5): 60 s, single-use.
const CHALLENGE_TTL: i64 = 60;
/// auth_finish timestamp tolerance.
const TIMESTAMP_SKEW: i64 = 300;
/// Session TTL, sliding (§1.5): 30 days.
const SESSION_TTL: i64 = 30 * 24 * 3600;
/// Browser/onboarding sessions should not linger like installed app sessions.
const WEB_SESSION_TTL: i64 = 2 * 3600;

fn session_ttl(device_name: &str) -> i64 {
    if device_name.starts_with("Web browser") || device_name.starts_with("Web signup") {
        WEB_SESSION_TTL
    } else {
        SESSION_TTL
    }
}

#[derive(Clone)]
pub struct PendingChallenge {
    pub account_id: String,
    pub device_id: String,
    pub expires: i64,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn err(msg: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": msg }))
}

fn verifying_key(b64: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = nexus_common::b64::decode(b64).try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

fn signature(b64: &str) -> Option<Signature> {
    let bytes: [u8; 64] = nexus_common::b64::decode(b64).try_into().ok()?;
    Some(Signature::from_bytes(&bytes))
}

/// Handles are free-form — whatever the user wants, as long as it isn't
/// blank, absurdly long, or already taken. Uniqueness is enforced by the
/// UNIQUE constraint on accounts.handle at insert time.
fn valid_handle(h: &str) -> bool {
    !h.trim().is_empty() && h.chars().count() <= 64 && !h.chars().any(char::is_control)
}

async fn audit(s: &AppState, actor: &str, action: &str, subject: &str, ip: &str) {
    let _ = sqlx::query("INSERT INTO audit (actor, action, subject, at, ip) VALUES (?,?,?,?,?)")
        .bind(actor)
        .bind(action)
        .bind(subject)
        .bind(now())
        .bind(ip)
        .execute(&s.db)
        .await;
}

// ---------------------------------------------------------------- register

#[derive(Deserialize)]
pub struct RegisterReq {
    pub identity_pub: String,
    pub handle: String,
    #[serde(default)]
    pub invite_code: Option<String>,
    pub device_pub: String,
    pub device_name: String,
    pub device_created: i64,
    /// Identity-key signature over the device-cert message.
    pub device_cert: String,
    /// Age gate: the client must present an 18-or-older attestation
    /// collected from the user at sign-up. Registration is refused without it.
    #[serde(default)]
    pub age_confirmed: bool,
}

/// §1.4: account ID is self-certifying (SHA-256 of the identity pubkey); the
/// first account ever becomes a founder. Production deployments can require a
/// configured founder_access_code for that bootstrap; after that an unused
/// invite is required unless the server runs --dev.
pub async fn register(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if !s.rate_ok(&format!("register:{ip}"), 5, 3600) {
        return err("rate limited, try again later");
    }
    if !req.age_confirmed {
        return err("you must confirm you are 18 or older to create an account");
    }
    if !valid_handle(&req.handle) {
        return err("handle must be 1-64 characters");
    }
    let Some(identity) = verifying_key(&req.identity_pub) else {
        return err("bad identity_pub");
    };
    let Some(device_key) = verifying_key(&req.device_pub) else {
        return err("bad device_pub");
    };
    let _ = device_key; // existence check only; auth verifies possession
    let Some(cert_sig) = signature(&req.device_cert) else {
        return err("bad device_cert");
    };
    let msg = format!(
        "hive-device-cert:v1:{}:{}:{}",
        req.device_pub, req.device_name, req.device_created
    );
    if identity.verify(msg.as_bytes(), &cert_sig).is_err() {
        return err("device_cert signature invalid");
    }

    let account_id = hex(&Sha256::digest(nexus_common::b64::decode(
        &req.identity_pub,
    )));
    let device_id = hex(&Sha256::digest(nexus_common::b64::decode(&req.device_pub)));

    let (count,): (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM accounts")
        .fetch_one(&s.db)
        .await
    {
        Ok(c) => c,
        Err(_) => return err("db error"),
    };
    let bootstrap = count == 0;

    // Invite policy (§1.4 / §6.6). Beta posture (`invite_required = true`):
    // an unused code is required for every account after the founder —
    // except on --dev servers (test harness). A public launch flips the
    // config to false. Either way, a code that IS supplied must be valid.
    let supplied = req.invite_code.as_deref().filter(|c| !c.trim().is_empty());
    let gate = s.invite_required && !s.dev;
    if bootstrap && gate {
        if let Some(code) = s.founder_access_code.as_deref().filter(|c| !c.is_empty()) {
            if supplied != Some(code) {
                return err("founder access code required");
            }
        }
    } else if !bootstrap && (gate || supplied.is_some()) {
        let Some(code) = supplied else {
            return err("invite required");
        };
        let claimed = sqlx::query(
            "UPDATE invites SET used_by = ?, used_at = ? WHERE code = ? AND used_by IS NULL",
        )
        .bind(&account_id)
        .bind(now())
        .bind(code)
        .execute(&s.db)
        .await;
        match claimed {
            Ok(r) if r.rows_affected() == 1 => {}
            _ => return err("invite invalid or already used"),
        }
    }

    let inserted = sqlx::query(
        "INSERT INTO accounts (id, identity_pub, handle, kind, created, founder, quota_bytes, age_attested) \
         VALUES (?,?,?,'personal',?,?,?,1)",
    )
    .bind(&account_id)
    .bind(&req.identity_pub)
    .bind(&req.handle)
    .bind(now())
    .bind(bootstrap as i64)
    // §3.3: storage quota is tier-driven and 0 on free accounts. Dev servers
    // grant 100 MB so sync/blob work can be exercised end to end.
    .bind(if s.dev { 100 * 1024 * 1024_i64 } else { 0 })
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("handle or account already exists");
    }

    let dev_ok = sqlx::query(
        "INSERT INTO devices (id, account_id, device_pub, cert, name, created, last_ip, last_seen) \
         VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(&device_id)
    .bind(&account_id)
    .bind(&req.device_pub)
    .bind(&req.device_cert)
    .bind(&req.device_name)
    .bind(req.device_created)
    .bind(&ip)
    .bind(now())
    .execute(&s.db)
    .await;
    if dev_ok.is_err() {
        return err("device already registered");
    }

    audit(&s, &account_id, "register", &device_id, &ip).await;
    // The sign-up UIs present the current Terms and Privacy Policy; creating
    // the account records acceptance of those versions (§16 evidence trail).
    crate::legal::record_current_acceptance(&s, &account_id, &ip).await;
    Json(json!({
        "ok": true,
        "account_id": account_id,
        "device_id": device_id,
        "founder": bootstrap,
    }))
}

// ------------------------------------------------------------------- auth

#[derive(Deserialize)]
pub struct AuthBeginReq {
    pub account_id: String,
    pub device_id: String,
}

pub async fn auth_begin(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<AuthBeginReq>,
) -> Json<Value> {
    if !s.rate_ok(&format!("authb:{}", peer.ip()), 30, 300) {
        return err("rate limited, try again later");
    }
    let device: Option<(String,)> = sqlx::query_as(
        "SELECT d.device_pub FROM devices d JOIN accounts a ON a.id = d.account_id \
         WHERE d.id = ? AND d.account_id = ? AND d.revoked_at IS NULL AND a.status = 'active'",
    )
    .bind(&req.device_id)
    .bind(&req.account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if device.is_none() {
        return err("unknown or revoked device");
    }

    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let challenge = nexus_common::b64::encode(&bytes);
    let expires = now() + CHALLENGE_TTL;
    s.challenges.lock().unwrap().insert(
        challenge.clone(),
        PendingChallenge {
            account_id: req.account_id,
            device_id: req.device_id,
            expires,
        },
    );
    Json(json!({ "ok": true, "challenge": challenge, "expires": expires }))
}

#[derive(Deserialize)]
pub struct AuthFinishReq {
    pub account_id: String,
    pub device_id: String,
    pub challenge: String,
    pub timestamp: i64,
    /// Device-key signature over the auth message (see header comment).
    pub sig: String,
}

pub async fn auth_finish(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<AuthFinishReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();

    // Single-use: remove regardless of outcome.
    let pending = s.challenges.lock().unwrap().remove(&req.challenge);
    let Some(p) = pending else {
        return err("unknown challenge");
    };
    if p.expires < now() || p.account_id != req.account_id || p.device_id != req.device_id {
        return err("challenge expired or mismatched");
    }
    if (now() - req.timestamp).abs() > TIMESTAMP_SKEW {
        return err("timestamp out of range");
    }

    let device: Option<(String,)> = sqlx::query_as(
        "SELECT d.device_pub FROM devices d JOIN accounts a ON a.id = d.account_id \
         WHERE d.id = ? AND d.account_id = ? AND d.revoked_at IS NULL AND a.status = 'active'",
    )
    .bind(&req.device_id)
    .bind(&req.account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((device_pub,)) = device else {
        return err("unknown or revoked device");
    };
    let Some(key) = verifying_key(&device_pub) else {
        return err("corrupt device key");
    };
    let Some(sig) = signature(&req.sig) else {
        return err("bad sig");
    };
    let msg = format!(
        "hive-auth:v1:{}:{}:{}",
        req.challenge, s.server_pub, req.timestamp
    );
    if key.verify(msg.as_bytes(), &sig).is_err() {
        audit(&s, &req.account_id, "auth_fail", &req.device_id, &ip).await;
        return err("signature invalid");
    }

    let mut token_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut token_bytes);
    let token = nexus_common::b64::encode(&token_bytes);
    let token_hash = hex(&Sha256::digest(token.as_bytes()));
    let device_name: Option<(String,)> = sqlx::query_as("SELECT name FROM devices WHERE id = ?")
        .bind(&req.device_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    let expires = now() + session_ttl(device_name.as_ref().map(|row| row.0.as_str()).unwrap_or(""));

    let ok = sqlx::query(
        "INSERT INTO sessions (token_hash, device_id, created, expires, last_ip) VALUES (?,?,?,?,?)",
    )
    .bind(&token_hash)
    .bind(&req.device_id)
    .bind(now())
    .bind(expires)
    .bind(&ip)
    .execute(&s.db)
    .await;
    if ok.is_err() {
        return err("db error");
    }
    let _ = sqlx::query("DELETE FROM sessions WHERE device_id = ? AND token_hash <> ?")
        .bind(&req.device_id)
        .bind(&token_hash)
        .execute(&s.db)
        .await;

    // Device reporting (§1.5): REAL client IP + timestamp.
    let _ = sqlx::query("UPDATE devices SET last_ip = ?, last_seen = ? WHERE id = ?")
        .bind(&ip)
        .bind(now())
        .bind(&req.device_id)
        .execute(&s.db)
        .await;
    let _ = sqlx::query("UPDATE accounts SET last_seen = ? WHERE id = ?")
        .bind(now())
        .bind(&req.account_id)
        .execute(&s.db)
        .await;

    audit(&s, &req.account_id, "auth", &req.device_id, &ip).await;
    Json(json!({ "ok": true, "token": token, "expires": expires }))
}

// ---------------------------------------------------------------- sessions

/// Resolve a Bearer token to (account_id, device_id), sliding the TTL and
/// recording the caller's IP. Every authed endpoint goes through this.
pub async fn authenticate(
    s: &AppState,
    headers: &HeaderMap,
    ip: &str,
) -> Result<(String, String), Json<Value>> {
    let token_hash = bearer_token_hash(headers)?;
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT d.account_id, d.id, d.name FROM sessions s JOIN devices d ON d.id = s.device_id \
         JOIN accounts a ON a.id = d.account_id \
         WHERE s.token_hash = ? AND s.expires > ? AND d.revoked_at IS NULL \
           AND a.status = 'active'",
    )
    .bind(&token_hash)
    .bind(now())
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((account_id, device_id, device_name)) = row else {
        return Err(err("invalid or expired session"));
    };
    let _ = sqlx::query("UPDATE sessions SET expires = ?, last_ip = ? WHERE token_hash = ?")
        .bind(now() + session_ttl(&device_name))
        .bind(ip)
        .bind(&token_hash)
        .execute(&s.db)
        .await;
    Ok((account_id, device_id))
}

fn bearer_token_hash(headers: &HeaderMap) -> Result<String, Json<Value>> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| err("missing bearer token"))?;
    Ok(hex(&Sha256::digest(token.as_bytes())))
}

pub async fn logout(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if let Err(error) = authenticate(&s, &headers, &ip).await {
        return error;
    }
    let token_hash = match bearer_token_hash(&headers) {
        Ok(hash) => hash,
        Err(error) => return error,
    };
    match sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(token_hash)
        .execute(&s.db)
        .await
    {
        Ok(_) => Json(json!({ "ok": true })),
        Err(_) => err("db error"),
    }
}

pub async fn whoami(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let handle: Option<(String, i64, String, String)> = sqlx::query_as(
        "SELECT handle, founder, community_role, membership_tier FROM accounts WHERE id = ?",
    )
            .bind(&account_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let (handle, founder, community_role, membership_tier) = handle.unwrap_or_default();
    let legal = crate::legal::acceptance_state(&s, &account_id).await;
    Json(json!({
        "ok": true,
        "account_id": account_id,
        "device_id": device_id,
        "handle": handle,
        "founder": founder != 0,
        "community_role": community_role,
        "membership_tier": membership_tier,
        "legal": legal,
    }))
}

// -------------------------------------------------------------- revocation

#[derive(Deserialize)]
pub struct RevokeReq {
    pub device_id: String,
}

/// §1.3: revocation kills all sessions for the device immediately. Any
/// non-revoked device of the account may revoke (the Vault UI drives this).
pub async fn device_revoke(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RevokeReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let updated = sqlx::query(
        "UPDATE devices SET revoked_at = ? WHERE id = ? AND account_id = ? AND revoked_at IS NULL",
    )
    .bind(now())
    .bind(&req.device_id)
    .bind(&account_id)
    .execute(&s.db)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {}
        _ => return err("device not found"),
    }
    let _ = sqlx::query("DELETE FROM sessions WHERE device_id = ?")
        .bind(&req.device_id)
        .execute(&s.db)
        .await;
    audit(&s, &account_id, "device_revoke", &req.device_id, &ip).await;
    Json(json!({ "ok": true }))
}

/// List the account's devices (drives the Vault Devices page).
pub async fn devices(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let rows: Vec<(
        String,
        String,
        i64,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT id, name, created, last_ip, last_seen, revoked_at \
             FROM devices WHERE account_id = ? ORDER BY created",
    )
    .bind(&account_id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let list: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, created, last_ip, last_seen, revoked_at)| {
            json!({
                "id": id, "name": name, "created": created,
                "last_ip": last_ip, "last_seen": last_seen,
                "revoked": revoked_at.is_some(),
            })
        })
        .collect();
    Json(json!({ "ok": true, "devices": list }))
}

// ------------------------------------------------------- device linking

/// Link requests expire after this window.
const LINK_TTL: i64 = 10 * 60;

/// Short human code — unambiguous alphabet (no 0/O/1/I/L).
fn link_code() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rngs::OsRng;
    (0..8)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

async fn link_gc(s: &AppState) {
    let _ = sqlx::query("DELETE FROM link_requests WHERE created < ?")
        .bind(now() - LINK_TTL)
        .execute(&s.db)
        .await;
}

#[derive(Deserialize)]
pub struct LinkBeginReq {
    pub device_pub: String,
    pub device_name: String,
}

/// §4.2 step 1 — the NEW device (unauthenticated: it has no account yet)
/// announces its pubkey and gets a short code to show the user. An enrolled
/// device approves by signing a device cert; keys never move.
pub async fn link_begin(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<LinkBeginReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if !s.rate_ok(&format!("link-begin:{ip}"), 10, 10 * 60) {
        return err("rate limited, try again later");
    }
    if verifying_key(&req.device_pub).is_none() {
        return err("bad device_pub");
    }
    if req.device_name.trim().is_empty() || req.device_name.chars().count() > 64 {
        return err("device_name must be 1-64 chars");
    }
    link_gc(&s).await;
    // Cap outstanding requests — this is an unauthenticated endpoint.
    let (pending,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM link_requests WHERE account_id IS NULL")
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    if pending >= 100 {
        return err("too many pending link requests — try again shortly");
    }
    let code = link_code();
    let inserted = sqlx::query(
        "INSERT INTO link_requests (code, device_pub, device_name, created) VALUES (?,?,?,?)",
    )
    .bind(&code)
    .bind(&req.device_pub)
    .bind(req.device_name.trim())
    .bind(now())
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("try again"); // code collision — vanishingly rare
    }
    Json(json!({ "ok": true, "code": code, "expires_in": LINK_TTL }))
}

#[derive(Deserialize)]
pub struct LinkCodeReq {
    pub code: String,
}

/// §4.2 step 2 — an ENROLLED device fetches the pending request to sign.
pub async fn link_fetch(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LinkCodeReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if authenticate(&s, &headers, &ip).await.is_err() {
        return err("auth required");
    }
    link_gc(&s).await;
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT device_pub, device_name, created FROM link_requests \
         WHERE code = ? AND account_id IS NULL",
    )
    .bind(req.code.trim().to_uppercase())
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    match row {
        Some((device_pub, device_name, created)) => Json(json!({
            "ok": true, "device_pub": device_pub,
            "device_name": device_name, "created": created,
        })),
        None => err("no such link code (expired?)"),
    }
}

#[derive(Deserialize)]
pub struct LinkApproveReq {
    pub code: String,
    /// The timestamp the approver signed over.
    pub device_created: i64,
    /// Identity-key signature over the device-cert message (§1.3).
    pub device_cert: String,
}

/// §4.2 step 3 — the approver submits the signed cert; the new device joins
/// THEIR account. Verification is identical to device_add.
pub async fn link_approve(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LinkApproveReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    link_gc(&s).await;
    let code = req.code.trim().to_uppercase();
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT device_pub, device_name FROM link_requests \
         WHERE code = ? AND account_id IS NULL",
    )
    .bind(&code)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((device_pub, device_name)) = row else {
        return err("no such link code (expired?)");
    };

    let identity_pub: Option<(String,)> =
        sqlx::query_as("SELECT identity_pub FROM accounts WHERE id = ?")
            .bind(&account_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((identity_pub,)) = identity_pub else {
        return err("account missing");
    };
    let Some(identity) = verifying_key(&identity_pub) else {
        return err("corrupt identity key");
    };
    let Some(sig) = signature(&req.device_cert) else {
        return err("bad device_cert");
    };
    let msg = format!(
        "hive-device-cert:v1:{}:{}:{}",
        device_pub, device_name, req.device_created
    );
    if identity.verify(msg.as_bytes(), &sig).is_err() {
        return err("device_cert signature invalid");
    }

    let device_id = hex(&Sha256::digest(nexus_common::b64::decode(&device_pub)));
    let inserted = sqlx::query(
        "INSERT INTO devices (id, account_id, device_pub, cert, name, created) \
         VALUES (?,?,?,?,?,?)",
    )
    .bind(&device_id)
    .bind(&account_id)
    .bind(&device_pub)
    .bind(&req.device_cert)
    .bind(&device_name)
    .bind(req.device_created)
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("device already registered");
    }
    let _ = sqlx::query("UPDATE link_requests SET account_id = ?, device_id = ? WHERE code = ?")
        .bind(&account_id)
        .bind(&device_id)
        .bind(&code)
        .execute(&s.db)
        .await;
    audit(&s, &account_id, "device_link", &device_id, &ip).await;
    s.push_to_account(
        &account_id,
        None,
        json!({ "type": "device_event", "kind": "added", "device_id": device_id, "name": device_name }),
    );
    Json(json!({ "ok": true, "device_id": device_id }))
}

/// §4.2 step 4 — the new device polls until approval, learns its account.
pub async fn link_status(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<LinkCodeReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if !s.rate_ok(&format!("link-status:{ip}"), 120, 60) {
        return err("rate limited, try again later");
    }
    link_gc(&s).await;
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT account_id FROM link_requests WHERE code = ?")
            .bind(req.code.trim().to_uppercase())
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    match row {
        Some((Some(account_id),)) => {
            Json(json!({ "ok": true, "approved": true, "account_id": account_id }))
        }
        Some((None,)) => Json(json!({ "ok": true, "approved": false })),
        None => err("no such link code (expired?)"),
    }
}

// -------------------------------------------------------- recovery escrow
//
// §1.7: the client encrypts its identity seed with a key derived from the
// user's recovery password (Argon2id, client-side) and parks only the
// ciphertext here. Fetching is rate-limited to 5 attempts per day; the
// server can never open the blob.

/// Escrow blobs are small: [salt][nonce][ct(seed)] ≈ 100 bytes. Cap generously.
const ESCROW_BLOB_MAX: usize = 4096;
/// §1.7 rate limit: 5 fetches per path per day.
const ESCROW_ATTEMPTS_PER_DAY: i64 = 5;

#[derive(Deserialize)]
pub struct EscrowSetReq {
    pub path: String,
    /// b64 of the client-encrypted key bundle.
    pub blob: String,
    #[serde(default)]
    pub questions: Option<String>,
}

/// Authed: park (or replace) a recovery bundle for one path.
pub async fn escrow_set(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<EscrowSetReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !matches!(req.path.as_str(), "password" | "questions" | "phrase") {
        return err("path must be password|questions|phrase");
    }
    let blob = nexus_common::b64::decode(&req.blob);
    if blob.is_empty() || blob.len() > ESCROW_BLOB_MAX {
        return err("bad blob");
    }
    let ok = sqlx::query(
        "INSERT INTO recovery_escrow (account_id, path, blob, questions) VALUES (?,?,?,?) \
         ON CONFLICT (account_id, path) DO UPDATE SET blob = excluded.blob, \
         questions = excluded.questions, attempts_today = 0",
    )
    .bind(&account_id)
    .bind(&req.path)
    .bind(&blob)
    .bind(&req.questions)
    .execute(&s.db)
    .await;
    if ok.is_err() {
        return err("db error");
    }
    audit(&s, &account_id, "escrow_set", &req.path, &ip).await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct EscrowFetchReq {
    pub handle: String,
    pub path: String,
}

/// Unauthed: hand back the ciphertext bundle for handle+path. Rate-limited
/// (§1.7, 5/day) and audited; decryption only ever happens on the client.
pub async fn escrow_fetch(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<EscrowFetchReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if !s.rate_ok(&format!("escrow:{ip}"), 10, 3600) {
        return err("rate limited, try again later");
    }
    let row: Option<(
        String,
        Option<Vec<u8>>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT a.id, e.blob, e.questions, e.attempts_today, e.attempts_reset \
             FROM accounts a LEFT JOIN recovery_escrow e \
               ON e.account_id = a.id AND e.path = ? \
             WHERE a.handle = ? AND a.status = 'active'",
    )
    .bind(&req.path)
    .bind(req.handle.trim())
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    let Some((account_id, Some(blob), questions, attempts, reset)) = row else {
        return err("recovery unavailable");
    };
    let mut attempts = attempts.unwrap_or(0);
    let reset = reset.unwrap_or(0);
    // Day rollover.
    let today = now() / 86400;
    if reset != today {
        attempts = 0;
    }
    if attempts >= ESCROW_ATTEMPTS_PER_DAY {
        audit(&s, &account_id, "escrow_fetch_limited", &req.path, &ip).await;
        return err("too many attempts today — try again tomorrow");
    }
    let _ = sqlx::query(
        "UPDATE recovery_escrow SET attempts_today = ?, attempts_reset = ? \
         WHERE account_id = ? AND path = ?",
    )
    .bind(attempts + 1)
    .bind(today)
    .bind(&account_id)
    .bind(&req.path)
    .execute(&s.db)
    .await;
    audit(&s, &account_id, "escrow_fetch", &req.path, &ip).await;
    Json(json!({
        "ok": true,
        "account_id": account_id,
        "blob": nexus_common::b64::encode(&blob),
        "questions": questions,
    }))
}

#[derive(Deserialize)]
pub struct RecoverDeviceReq {
    pub account_id: String,
    pub device_pub: String,
    pub device_name: String,
    pub device_created: i64,
    /// Identity-key signature over the device-cert message — proving the
    /// caller decrypted the escrowed identity seed.
    pub device_cert: String,
}

/// Unauthed: enroll a fresh device using a recovered identity key. The cert
/// verification is identical to register/link_approve — possession of the
/// identity key IS the authorization.
pub async fn recover_device(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<RecoverDeviceReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let identity_pub: Option<(String,)> =
        sqlx::query_as("SELECT identity_pub FROM accounts WHERE id = ? AND status = 'active'")
            .bind(&req.account_id)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((identity_pub,)) = identity_pub else {
        return err("account missing");
    };
    let Some(identity) = verifying_key(&identity_pub) else {
        return err("corrupt identity key");
    };
    let Some(sig) = signature(&req.device_cert) else {
        return err("bad device_cert");
    };
    let msg = format!(
        "hive-device-cert:v1:{}:{}:{}",
        req.device_pub, req.device_name, req.device_created
    );
    if identity.verify(msg.as_bytes(), &sig).is_err() {
        return err("device_cert signature invalid");
    }
    if (now() - req.device_created).abs() > TIMESTAMP_SKEW {
        return err("stale device_created — check the clock");
    }

    let device_id = hex(&Sha256::digest(nexus_common::b64::decode(&req.device_pub)));
    let inserted = sqlx::query(
        "INSERT INTO devices (id, account_id, device_pub, cert, name, created, last_ip, last_seen) \
         VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(&device_id)
    .bind(&req.account_id)
    .bind(&req.device_pub)
    .bind(&req.device_cert)
    .bind(&req.device_name)
    .bind(req.device_created)
    .bind(&ip)
    .bind(now())
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        return err("device already registered");
    }
    audit(&s, &req.account_id, "device_recover", &device_id, &ip).await;
    s.push_to_account(
        &req.account_id,
        None,
        json!({ "type": "device_event", "kind": "added", "device_id": device_id, "name": req.device_name }),
    );
    Json(json!({ "ok": true, "device_id": device_id }))
}

/// Permanently delete the caller's account and every trace of it (GDPR
/// Art. 17). Deletes are real, matching Connect's §6.3 discipline: rows go
/// away, sessions die, the handle is freed. Only the audit trail keeps a
/// tombstone (legal-hold record of the deletion itself).
pub async fn account_delete(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    // ToS forfeiture: an account under evidence_hold may leave, but its
    // record stays frozen — posts/comments tombstone instead of deleting,
    // and blobs + devices (IP history) are preserved for law enforcement.
    let held: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM accounts WHERE id = ? AND evidence_hold IS NOT NULL")
            .bind(&me)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    if held.is_some() {
        let now_ts = now();
        for sql in [
            "UPDATE posts SET deleted_at = COALESCE(deleted_at, ?2) WHERE author = ?1",
            "UPDATE comments SET deleted_at = COALESCE(deleted_at, ?2) WHERE author = ?1",
        ] {
            let _ = sqlx::query(sql).bind(&me).bind(now_ts).execute(&s.db).await;
        }
        for sql in [
            "DELETE FROM follows WHERE follower = ?1 OR followee = ?1",
            "DELETE FROM blocks WHERE blocker = ?1 OR blocked = ?1",
            "UPDATE circles SET key_stale=1 WHERE id IN \
                (SELECT circle_id FROM fold_members WHERE account_id=?1 AND role='member' AND state='active')",
            "DELETE FROM fold_members WHERE account_id = ?1",
            "DELETE FROM circles WHERE owner = ?1",
            "DELETE FROM support_threads WHERE account_id = ?1",
            "DELETE FROM notifications WHERE account_id = ?1 OR actor = ?1",
            "DELETE FROM profiles WHERE account_id = ?1",
            "DELETE FROM account_settings WHERE account_id = ?1",
            "DELETE FROM recovery_escrow WHERE account_id = ?1",
            "DELETE FROM wire_history_sync WHERE account_id = ?1",
            "DELETE FROM wire_history_sync_state WHERE account_id = ?1",
            "DELETE FROM sessions WHERE device_id IN \
                (SELECT id FROM devices WHERE account_id = ?1)",
            "DELETE FROM vault_snapshots WHERE account_id = ?1",
        ] {
            let _ = sqlx::query(sql).bind(&me).execute(&s.db).await;
        }
        let _ = sqlx::query(
            "UPDATE accounts SET handle = 'deleted-' || substr(id, 1, 12), \
             status = 'deleted' WHERE id = ?",
        )
        .bind(&me)
        .execute(&s.db)
        .await;
        audit(
            &s,
            &me,
            "account_delete",
            "evidence_hold: content preserved",
            &ip,
        )
        .await;
        return Json(json!({ "ok": true }));
    }
    // Social layer first (posts cascade comments/reactions via app-level
    // deletes elsewhere; here we sweep everything owned by or pointing at us).
    for sql in [
        "DELETE FROM comment_reactions WHERE account_id = ?1 \
         OR comment_id IN (SELECT id FROM comments WHERE post_id IN \
            (SELECT id FROM posts WHERE author = ?1))",
        "DELETE FROM comments WHERE author = ?1 OR post_id IN \
            (SELECT id FROM posts WHERE author = ?1)",
        "DELETE FROM post_reactions WHERE account_id = ?1 OR post_id IN \
            (SELECT id FROM posts WHERE author = ?1)",
        "DELETE FROM posts WHERE author = ?1",
        "DELETE FROM follows WHERE follower = ?1 OR followee = ?1",
        "DELETE FROM blocks WHERE blocker = ?1 OR blocked = ?1",
        "UPDATE circles SET key_stale=1 WHERE id IN \
            (SELECT circle_id FROM fold_members WHERE account_id=?1 AND role='member' AND state='active')",
        "DELETE FROM fold_members WHERE account_id = ?1",
        "DELETE FROM circles WHERE owner = ?1",
        "DELETE FROM support_threads WHERE account_id = ?1",
        "DELETE FROM notifications WHERE account_id = ?1 OR actor = ?1",
        "DELETE FROM profiles WHERE account_id = ?1",
        "DELETE FROM account_settings WHERE account_id = ?1",
        // Identity layer.
        "DELETE FROM recovery_escrow WHERE account_id = ?1",
        "DELETE FROM wire_history_sync WHERE account_id = ?1",
        "DELETE FROM wire_history_sync_state WHERE account_id = ?1",
        "DELETE FROM sessions WHERE account_id = ?1",
        "DELETE FROM devices WHERE account_id = ?1",
        "DELETE FROM vault_snapshots WHERE account_id = ?1",
        // legal_hold blobs are report-snapshot evidence: they outlive the
        // account (their bytes are what gets handed to authorities).
        "DELETE FROM blobs WHERE owner = ?1 AND legal_hold = 0",
    ] {
        let _ = sqlx::query(sql).bind(&me).execute(&s.db).await;
    }
    // Free the handle and tombstone the account row.
    let _ = sqlx::query(
        "UPDATE accounts SET handle = 'deleted-' || substr(id, 1, 12), \
         status = 'deleted' WHERE id = ?",
    )
    .bind(&me)
    .execute(&s.db)
    .await;
    audit(&s, &me, "account_delete", "", &ip).await;
    Json(json!({ "ok": true }))
}
