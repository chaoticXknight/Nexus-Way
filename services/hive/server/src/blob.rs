// Owns content-addressed blob upload, fetch, quota, reference counting, and garbage collection.
// sync.rs, wire.rs, and connect.rs decide why a blob is retained; clients encrypt private bytes.

// Blob store (Hive_design_doc.md §3.2): the shared encrypted-storage
// primitive used by Vault sync, WIRE attachments, and Connect media.
//
// - Content-addressed: blob ID = hex(SHA-256(stored bytes)). Client-side
//   encryption for everything private; the server never inspects content.
// - Chunked, resumable upload: blob_begin → chunk (raw body, offset param,
//   4 MB max per chunk) → blob_commit (hash verified server-side).
// - Quotas per account (§3.3: free tier quota_bytes = 0).
// - Refcounted; GC deletes refcount-0 blobs older than 24 h (real deletion).
// - Layout: <data_dir>/blobs/ab/cd/<hash> two-level fanout.

use crate::api::AppState;
use crate::identity::{authenticate, now};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Max chunk size (§3.2): 4 MB.
pub const CHUNK_MAX: usize = 4 * 1024 * 1024;
/// Fold envelopes base64-expand the 25 MiB raw media limit to about 33.4 MiB.
pub const CONNECT_MEDIA_BLOB_MAX: i64 = 36 * 1024 * 1024;
/// Retained social media is bounded separately from Vault/general storage.
pub const CONNECT_MEDIA_QUOTA_BYTES: i64 = 256 * 1024 * 1024;
/// Uploads not committed within this window are GC'd.
const UPLOAD_TTL: i64 = 24 * 3600;
/// Refcount-0 blobs must be at least this old before GC removes them.
const GC_GRACE: i64 = 24 * 3600;

fn err(msg: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": msg }))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit())
}

/// blobs/ab/cd/<hash> fanout under the data dir.
pub fn blob_path(data_dir: &std::path::Path, hash: &str) -> PathBuf {
    data_dir
        .join("blobs")
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(hash)
}

fn upload_path(data_dir: &std::path::Path, upload_id: &str) -> PathBuf {
    data_dir.join("blobs").join("uploads").join(upload_id)
}

// ------------------------------------------------------------- blob_begin

#[derive(Deserialize)]
pub struct BeginReq {
    /// Declared total size in bytes.
    pub bytes: i64,
    /// Expected content hash (hex SHA-256) — the future blob ID.
    pub hash: String,
    /// Public plaintext class (Connect public media only).
    #[serde(default)]
    pub public: bool,
    /// Storage policy class. Older clients omit this and remain `general`.
    #[serde(default = "general_purpose")]
    pub purpose: String,
}

fn general_purpose() -> String {
    "general".into()
}

/// Start (or resume) an upload. Enforces quota up front. If the blob already
/// exists, dedupe: bump nothing, just report complete.
pub async fn begin(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<BeginReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !valid_hash(&req.hash) {
        return err("hash must be 64 hex chars");
    }
    if req.bytes <= 0 {
        return err("bytes must be positive");
    }
    if !matches!(req.purpose.as_str(), "general" | "connect_media") {
        return err("bad blob purpose");
    }
    if req.purpose == "connect_media" && req.bytes > CONNECT_MEDIA_BLOB_MAX {
        return err("Connect media must be 36 MB or smaller after encryption");
    }

    // Content-addressed dedupe: already stored → done.
    let existing: Option<(i64,)> = sqlx::query_as("SELECT bytes FROM blobs WHERE id = ?")
        .bind(&req.hash)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    if existing.is_some() {
        return Json(json!({ "ok": true, "complete": true, "blob_id": req.hash }));
    }

    if req.purpose == "connect_media" {
        let (used,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(bytes),0) FROM blobs WHERE owner=? AND purpose='connect_media'",
        )
        .bind(&account_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or((0,));
        let (pending,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(bytes),0) FROM blob_uploads \
             WHERE owner=? AND purpose='connect_media' AND hash<>?",
        )
        .bind(&account_id)
        .bind(&req.hash)
        .fetch_one(&s.db)
        .await
        .unwrap_or((0,));
        if used + pending + req.bytes > CONNECT_MEDIA_QUOTA_BYTES {
            return err("Connect media storage is full (256 MB)");
        }
    } else {
        // General quota (§3.3) remains unchanged for Vault and private storage.
        let (quota,): (i64,) =
            match sqlx::query_as("SELECT quota_bytes FROM accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(&s.db)
                .await
            {
                Ok(v) => v,
                Err(_) => return err("db error"),
            };
        let (used,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(bytes),0) FROM blobs WHERE owner=? AND purpose='general'",
        )
        .bind(&account_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or((0,));
        if used + req.bytes > quota {
            return err("quota exceeded");
        }
    }

    // Resume support: how much of this upload do we already have?
    let upath = upload_path(&s.data_dir, &req.hash);
    let have = fs::metadata(&upath).map(|m| m.len() as i64).unwrap_or(0);
    if fs::create_dir_all(upath.parent().unwrap()).is_err() {
        return err("storage error");
    }
    let _ = sqlx::query(
        "INSERT INTO blob_uploads (hash, owner, bytes, public, started, purpose) VALUES (?,?,?,?,?,?) \
         ON CONFLICT(hash) DO NOTHING",
    )
    .bind(&req.hash)
    .bind(&account_id)
    .bind(req.bytes)
    .bind(req.public as i64)
    .bind(now())
    .bind(&req.purpose)
    .execute(&s.db)
    .await;

    Json(json!({ "ok": true, "complete": false, "offset": have, "chunk_max": CHUNK_MAX }))
}

// ------------------------------------------------------------------ chunk

#[derive(Deserialize)]
pub struct ChunkQuery {
    pub offset: i64,
}

/// Append a chunk (raw body) at the given offset. Out-of-order writes are
/// rejected; the client asks blob_begin for the current offset to resume.
pub async fn chunk(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    UrlPath(hash): UrlPath<String>,
    Query(q): Query<ChunkQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if body.len() > CHUNK_MAX {
        return err("chunk too large");
    }
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT owner, bytes FROM blob_uploads WHERE hash = ?")
            .bind(&hash)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((owner, total)) = row else {
        return err("no such upload (call blob_begin first)");
    };
    if owner != account_id {
        return err("not your upload");
    }

    let upath = upload_path(&s.data_dir, &hash);
    let have = fs::metadata(&upath).map(|m| m.len() as i64).unwrap_or(0);
    if q.offset != have {
        return Json(json!({ "ok": false, "err": "offset mismatch", "offset": have }));
    }
    if have + body.len() as i64 > total {
        return err("upload exceeds declared size");
    }

    let write = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&upath)
        .and_then(|mut f| f.write_all(&body));
    if write.is_err() {
        return err("storage error");
    }
    Json(json!({ "ok": true, "offset": have + body.len() as i64 }))
}

// ----------------------------------------------------------------- commit

/// Verify the assembled upload hashes to its declared ID, move it into the
/// content-addressed store, and create the metadata row (refcount 0 — the
/// referencing service bumps it).
pub async fn commit(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    UrlPath(hash): UrlPath<String>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let row: Option<(String, i64, i64, String)> =
        sqlx::query_as("SELECT owner, bytes, public, purpose FROM blob_uploads WHERE hash = ?")
            .bind(&hash)
            .fetch_optional(&s.db)
            .await
            .unwrap_or(None);
    let Some((owner, total, public, purpose)) = row else {
        return err("no such upload");
    };
    if owner != account_id {
        return err("not your upload");
    }

    let upath = upload_path(&s.data_dir, &hash);
    let have = fs::metadata(&upath).map(|m| m.len() as i64).unwrap_or(0);
    if have != total {
        return Json(json!({ "ok": false, "err": "incomplete", "offset": have }));
    }

    // Verify content hash in 1 MB windows (files can exceed RAM).
    let mut hasher = Sha256::new();
    let hashed = fs::File::open(&upath).and_then(|mut f| {
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(())
    });
    if hashed.is_err() {
        return err("storage error");
    }
    if hex(&hasher.finalize()) != hash {
        let _ = fs::remove_file(&upath);
        let _ = sqlx::query("DELETE FROM blob_uploads WHERE hash = ?")
            .bind(&hash)
            .execute(&s.db)
            .await;
        return err("hash mismatch — upload discarded");
    }

    let dest = blob_path(&s.data_dir, &hash);
    if fs::create_dir_all(dest.parent().unwrap()).is_err() || fs::rename(&upath, &dest).is_err() {
        return err("storage error");
    }
    let rel = format!("blobs/{}/{}/{}", &hash[0..2], &hash[2..4], hash);
    let ok = sqlx::query(
        "INSERT INTO blobs (id, owner, bytes, created, refcount, public, storage_path, purpose) \
         VALUES (?,?,?,?,0,?,?,?)",
    )
    .bind(&hash)
    .bind(&account_id)
    .bind(total)
    .bind(now())
    .bind(public)
    .bind(&rel)
    .bind(&purpose)
    .execute(&s.db)
    .await;
    if ok.is_err() {
        return err("db error");
    }
    let _ = sqlx::query("DELETE FROM blob_uploads WHERE hash = ?")
        .bind(&hash)
        .execute(&s.db)
        .await;
    Json(json!({ "ok": true, "blob_id": hash }))
}

// ------------------------------------------------------------------ fetch

/// Download a blob. Private blobs are owner-only for now; service-level
/// grants (circle members, message recipients) arrive with those services.
pub async fn fetch(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    UrlPath(hash): UrlPath<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let ip = peer.ip().to_string();
    let row: Option<(String, i64)> = sqlx::query_as("SELECT owner, public FROM blobs WHERE id = ?")
        .bind(&hash)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
    let Some((owner, public)) = row else {
        return (axum::http::StatusCode::NOT_FOUND, "not found").into_response();
    };
    if public == 0 {
        match authenticate(&s, &headers, &ip).await {
            Ok((account_id, _)) if account_id == owner => {}
            _ => return (axum::http::StatusCode::FORBIDDEN, "forbidden").into_response(),
        }
    }
    // Range support for resumable fetch.
    let path = blob_path(&s.data_dir, &hash);
    let range = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, _)| a.parse::<u64>().ok());
    let data = match range {
        Some(start) => fs::File::open(&path).and_then(|mut f| {
            f.seek(SeekFrom::Start(start))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            Ok(buf)
        }),
        None => fs::read(&path),
    };
    match data {
        Ok(bytes) => bytes.into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "storage error",
        )
            .into_response(),
    }
}

// ------------------------------------------------------------------ usage

pub async fn usage(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (account_id, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (used,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(bytes),0) FROM blobs WHERE owner=? AND purpose='general'",
    )
            .bind(&account_id)
            .fetch_one(&s.db)
            .await
            .unwrap_or((0,));
    let (quota,): (i64,) = sqlx::query_as("SELECT quota_bytes FROM accounts WHERE id = ?")
        .bind(&account_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or((0,));
    let (connect_media_used,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(bytes),0) FROM blobs WHERE owner=? AND purpose='connect_media'",
    )
    .bind(&account_id)
    .fetch_one(&s.db)
    .await
    .unwrap_or((0,));
    Json(json!({
        "ok": true,
        "used": used,
        "quota": quota,
        "connect_media_used": connect_media_used,
        "connect_media_quota": CONNECT_MEDIA_QUOTA_BYTES,
    }))
}

// --------------------------------------------------------------------- gc

/// Real deletion (§3.2): refcount-0 blobs past the grace window and stale
/// uncommitted uploads are removed from disk and DB. Runs nightly in prod;
/// callable directly for tests/admin. Dev servers use a zero grace window
/// so wipe-and-verify cycles work immediately.
pub async fn run_gc(s: &AppState) -> (u64, u64) {
    let grace = if s.dev { 0 } else { GC_GRACE };
    let cutoff = now() - grace;
    // legal_hold: evidence blobs referenced by report snapshots are never
    // collected, whatever their refcount (18 U.S.C. §2258A preservation).
    // Same for anything owned by an account under evidence_hold (ToS
    // forfeiture): the whole footprint is frozen.
    let dead: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, storage_path FROM blobs \
         WHERE refcount <= 0 AND created <= ? AND legal_hold = 0 \
           AND owner NOT IN (SELECT id FROM accounts WHERE evidence_hold IS NOT NULL)",
    )
    .bind(cutoff)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();
    let mut blobs_removed = 0u64;
    for (id, rel) in dead {
        let _ = fs::remove_file(s.data_dir.join(&rel));
        if sqlx::query(
            "DELETE FROM blobs WHERE id = ? AND refcount <= 0 AND legal_hold = 0 \
             AND owner NOT IN (SELECT id FROM accounts WHERE evidence_hold IS NOT NULL)",
        )
        .bind(&id)
        .execute(&s.db)
        .await
        .map(|r| r.rows_affected() == 1)
        .unwrap_or(false)
        {
            blobs_removed += 1;
        }
    }

    let stale_cutoff = now() - if s.dev { 0 } else { UPLOAD_TTL };
    let stale: Vec<(String,)> = sqlx::query_as("SELECT hash FROM blob_uploads WHERE started <= ?")
        .bind(stale_cutoff)
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
    let mut uploads_removed = 0u64;
    for (hash,) in stale {
        let _ = fs::remove_file(upload_path(&s.data_dir, &hash));
        let _ = sqlx::query("DELETE FROM blob_uploads WHERE hash = ?")
            .bind(&hash)
            .execute(&s.db)
            .await;
        uploads_removed += 1;
    }
    (blobs_removed, uploads_removed)
}

/// Adjust a blob's refcount (used by services when rows reference/deref a
/// blob). Not an HTTP endpoint — services call it in-process.
pub async fn add_ref(s: &AppState, blob_id: &str, delta: i64) {
    let _ = sqlx::query("UPDATE blobs SET refcount = refcount + ? WHERE id = ?")
        .bind(delta)
        .bind(blob_id)
        .execute(&s.db)
        .await;
}
