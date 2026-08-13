// Owns ordered encrypted vault snapshots, history retention, pull status, and device enrollment.
// blob.rs stores opaque snapshot bytes; clients alone encrypt, decrypt, and merge vault contents.

// Vault sync (Hive_design_doc.md §4): opaque encrypted snapshots with
// sequence discipline. HIVE never understands the vault format — it stores
// ciphertext blobs and enforces ordering.
//
// - sync_commit: accepted only if seq == latest+1 for the account. A stale
//   device gets {err:"behind", latest_seq} and must pull/merge/retry.
// - sync_status / sync_pull: latest (or specific) snapshot for bootstrap,
//   catch-up, and point-in-time restore.
// - History: last SNAPSHOT_KEEP snapshots retained; older ones deref their
//   blobs (GC deletes the bytes).
// - sync_hint: pushed over /v1/stream to the account's OTHER devices on
//   every commit — no polling (ground rule #3).

use crate::api::AppState;
use crate::blob;
use crate::identity::{authenticate, now};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

/// Snapshots retained per account (§4.1): point-in-time restore window.
const SNAPSHOT_KEEP: i64 = 30;

fn err(msg: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": msg }))
}

// ------------------------------------------------------------ sync_commit

#[derive(Deserialize)]
pub struct CommitReq {
    /// Must be exactly latest+1 (1 for the first snapshot).
    pub seq: i64,
    /// Blob ID of the encrypted snapshot (uploaded via §3.2 first).
    pub blob_id: String,
}

pub async fn commit(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CommitReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, device_id) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    // The snapshot blob must exist and belong to this account.
    let owned: Option<(String,)> = sqlx::query_as("SELECT owner FROM blobs WHERE id = ?")
        .bind(&req.blob_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    match owned {
        Some((owner,)) if owner == account_id => {}
        _ => return err("blob not found or not yours (upload it first)"),
    }

    // Sequence discipline (§4.1). The UNIQUE(account_id, seq) primary key is
    // the arbiter under concurrency: two devices racing the same seq — one
    // insert wins, the loser gets "behind".
    let (latest,): (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(seq), 0) FROM vault_snapshots WHERE account_id = ?")
            .bind(&account_id)
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    if req.seq != latest + 1 {
        return Json(json!({ "ok": false, "err": "behind", "latest_seq": latest }));
    }
    let inserted = sqlx::query(
        "INSERT INTO vault_snapshots (account_id, seq, device_id, blob_id, created) \
         VALUES (?,?,?,?,?)",
    )
    .bind(&account_id)
    .bind(req.seq)
    .bind(&device_id)
    .bind(&req.blob_id)
    .bind(now())
    .execute(&s.db)
    .await;
    if inserted.is_err() {
        // Lost a race: someone else took this seq between MAX and INSERT.
        let (latest,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(MAX(seq), 0) FROM vault_snapshots WHERE account_id = ?",
        )
        .bind(&account_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or((0,));
        return Json(json!({ "ok": false, "err": "behind", "latest_seq": latest }));
    }
    blob::add_ref(&s, &req.blob_id, 1).await;

    // Retention: deref snapshots beyond the keep window (GC removes bytes).
    let expired: Vec<(i64, String)> = sqlx::query_as(
        "SELECT seq, blob_id FROM vault_snapshots WHERE account_id = ? AND seq <= ?",
    )
    .bind(&account_id)
    .bind(req.seq - SNAPSHOT_KEEP)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    for (seq, blob_id) in expired {
        let _ = sqlx::query("DELETE FROM vault_snapshots WHERE account_id = ? AND seq = ?")
            .bind(&account_id)
            .bind(seq)
            .execute(&s.db)
            .await;
        blob::add_ref(&s, &blob_id, -1).await;
    }

    // Push sync_hint to the account's OTHER devices (§4.1).
    s.push_to_account(
        &account_id,
        Some(&device_id),
        json!({ "type": "sync_hint", "seq": req.seq }),
    );

    Json(json!({ "ok": true, "seq": req.seq }))
}

// ------------------------------------------------- sync_status / sync_pull

pub async fn status(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let latest: Option<(i64, String, i64)> = sqlx::query_as(
        "SELECT seq, blob_id, created FROM vault_snapshots \
         WHERE account_id = ? ORDER BY seq DESC LIMIT 1",
    )
    .bind(&account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    match latest {
        Some((seq, blob_id, created)) => Json(json!({
            "ok": true, "seq": seq, "blob_id": blob_id, "created": created,
        })),
        None => Json(json!({ "ok": true, "seq": 0 })),
    }
}

#[derive(Deserialize)]
pub struct PullReq {
    /// Specific snapshot to pull; omit for latest (restore = pull older seq).
    #[serde(default)]
    pub seq: Option<i64>,
}

pub async fn pull(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PullReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let row: Option<(i64, String, i64)> = match req.seq {
        Some(seq) => sqlx::query_as(
            "SELECT seq, blob_id, created FROM vault_snapshots \
             WHERE account_id = ? AND seq = ?",
        )
        .bind(&account_id)
        .bind(seq)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None),
        None => sqlx::query_as(
            "SELECT seq, blob_id, created FROM vault_snapshots \
             WHERE account_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(&account_id)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None),
    };
    match row {
        Some((seq, blob_id, created)) => Json(json!({
            "ok": true, "seq": seq, "blob_id": blob_id, "created": created,
        })),
        None => err("no snapshot"),
    }
}

/// Snapshot history (drives the point-in-time restore UI).
pub async fn history(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT seq, blob_id, device_id, created FROM vault_snapshots \
         WHERE account_id = ? ORDER BY seq DESC",
    )
    .bind(&account_id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let list: Vec<Value> = rows
        .into_iter()
        .map(|(seq, blob_id, device_id, created)| {
            json!({ "seq": seq, "blob_id": blob_id, "device_id": device_id, "created": created })
        })
        .collect();
    Json(json!({ "ok": true, "snapshots": list }))
}

// ------------------------------------------------------------- device_add

#[derive(Deserialize)]
pub struct DeviceAddReq {
    pub device_pub: String,
    pub device_name: String,
    pub device_created: i64,
    /// Identity-key signature over the device-cert message (§1.3) — produced
    /// by an existing device that holds the vault (and thus the identity key).
    pub device_cert: String,
}

/// Enroll an additional device (§4.2 linking): an existing signed-in device
/// submits a cert for the new device. The new device then auths normally.
pub async fn device_add(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DeviceAddReq>,
) -> Json<Value> {
    use ed25519_dalek::Verifier;
    use sha2::{Digest, Sha256};

    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
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
    let key_bytes: Result<[u8; 32], _> = nexus_common::b64::decode(&identity_pub).try_into();
    let Ok(key_bytes) = key_bytes else {
        return err("corrupt identity key");
    };
    let Ok(identity) = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes) else {
        return err("corrupt identity key");
    };
    let sig_bytes: Result<[u8; 64], _> = nexus_common::b64::decode(&req.device_cert).try_into();
    let Ok(sig_bytes) = sig_bytes else {
        return err("bad device_cert");
    };
    let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    let msg = format!(
        "hive-device-cert:v1:{}:{}:{}",
        req.device_pub, req.device_name, req.device_created
    );
    if identity.verify(msg.as_bytes(), &sig).is_err() {
        return err("device_cert signature invalid");
    }

    let device_id = {
        let mut h = Sha256::new();
        h.update(nexus_common::b64::decode(&req.device_pub));
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let ok = sqlx::query(
        "INSERT INTO devices (id, account_id, device_pub, cert, name, created) \
         VALUES (?,?,?,?,?,?)",
    )
    .bind(&device_id)
    .bind(&account_id)
    .bind(&req.device_pub)
    .bind(&req.device_cert)
    .bind(&req.device_name)
    .bind(req.device_created)
    .execute(&s.db)
    .await;
    if ok.is_err() {
        return err("device already registered");
    }

    // Tell the account's devices (Devices page updates live).
    s.push_to_account(
        &account_id,
        None,
        json!({ "type": "device_event", "kind": "added", "device_id": device_id, "name": req.device_name }),
    );
    Json(json!({ "ok": true, "device_id": device_id }))
}
