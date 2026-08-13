// Owns static-site fallback, beta access cookies, release metadata, and gated APK downloads.
// api.rs owns route registration; web/site owns browser pages and clients own artifact verification.

// Static website + gated APK download.
//
// The HIVE server doubles as the web host for the beta landing site
// (web/site/ in the repo): a plain static-file fallback for everything that
// isn't /v1/*, plus /download/connect.apk which requires a valid session —
// the invite gate lives at registration, so "has an account" IS the gate.

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::RngCore;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Component, Path};

use crate::api::AppState;
use crate::identity::authenticate;

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

const SITE_ACCESS_COOKIE: &str = "nw_site_access";
const APK_TICKET_TTL_SECS: i64 = 120;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn cookie_value<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (k, v) = part.trim().split_once('=')?;
            (k == key).then_some(v)
        })
}

fn site_access_ok(s: &AppState, uri: &Uri, headers: &HeaderMap) -> bool {
    let Some(code) = &s.site_access_code else {
        return true;
    };
    if code.is_empty() {
        return true;
    }
    let query_match = uri
        .query()
        .and_then(|q| query_value(q, "access"))
        .is_some_and(|v| v == code);
    let cookie_match = cookie_value(headers, SITE_ACCESS_COOKIE).is_some_and(|v| v == code);
    query_match || cookie_match
}

fn site_access_response(s: &AppState, uri: &Uri, headers: &HeaderMap) -> Option<Response> {
    let code = s.site_access_code.as_ref()?;
    if code.is_empty() || site_access_ok(s, uri, headers) {
        return None;
    }
    Some((StatusCode::NOT_FOUND, "not found").into_response())
}

/// Fallback route: serve files from `web_dir`. `/` maps to index.html.
/// Traversal-safe: only plain path components are accepted.
pub async fn static_file(State(s): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    let Some(dir) = &s.web_dir else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if let Some(resp) = site_access_response(&s, &uri, &headers) {
        return resp;
    }
    let set_access_cookie = s
        .site_access_code
        .as_ref()
        .filter(|code| !code.is_empty())
        .filter(|code| {
            uri.query()
                .and_then(|q| query_value(q, "access"))
                .is_some_and(|v| v == code.as_str())
        })
        .cloned();
    let rel = uri.path().trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let path = Path::new(rel);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let full = dir.join(path);
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let ct = content_type(&full);
            let mut resp = ([(header::CONTENT_TYPE, ct)], bytes).into_response();
            if ct.starts_with("text/html") {
                // Self-contained site: no third-party origins; hash-wasm
                // needs wasm-unsafe-eval to instantiate its bundled module.
                resp.headers_mut().insert(
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
                     style-src 'self'; connect-src 'self'; img-src 'self' data:; \
                     frame-ancestors 'none'"
                        .parse()
                        .unwrap(),
                );
            }
            if let Some(code) = set_access_cookie {
                let cookie = format!(
                    "{SITE_ACCESS_COOKIE}={code}; Path=/; Max-Age=2592000; Secure; HttpOnly; SameSite=Lax"
                );
                resp.headers_mut()
                    .insert(header::SET_COOKIE, cookie.parse().unwrap());
            }
            resp
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// The Android release APK. Requires a signed-in session (Bearer token):
/// accounts only exist behind the invite gate, so this keeps the beta
/// build inside the invited circle without a second code system.
pub async fn apk(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let ip = peer.ip().to_string();
    if authenticate(&s, &headers, &ip).await.is_err() {
        let ticket_ok = uri
            .query()
            .and_then(|q| query_value(q, "ticket"))
            .is_some_and(|ticket| {
                let current = now();
                let mut tickets = s.download_tickets.lock().unwrap();
                tickets.retain(|_, expires| *expires >= current);
                tickets
                    .remove(ticket)
                    .is_some_and(|expires| expires >= current)
            });
        if !ticket_ok {
            return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
        }
    }
    let Some(path) = &s.apk_path else {
        return (StatusCode::NOT_FOUND, "no APK configured on this server").into_response();
    };
    match tokio::fs::read(path).await {
        Ok(bytes) => (
            [
                (
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive",
                ),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"nexus-connect.apk\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "APK not found on server").into_response(),
    }
}

/// Nexus Notify companion APK. This endpoint is intentionally app-only: a
/// signed-in Connect session downloads it and hands it to Android's installer.
pub async fn notifier_apk(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = peer.ip().to_string();
    if authenticate(&s, &headers, &ip).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    }
    let Some(path) = &s.notifier_apk_path else {
        return (StatusCode::NOT_FOUND, "no Nexus Notify APK configured").into_response();
    };
    match tokio::fs::read(path).await {
        Ok(bytes) => (
            [
                (
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive",
                ),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"nexus-notify.apk\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Nexus Notify APK not found").into_response(),
    }
}

/// Browser download helper. A fetch(blob) + object URL is unreliable on some
/// Android browsers, so signed-in web sessions exchange their bearer token for
/// a short-lived, one-use URL that the browser can navigate to directly.
pub async fn download_ticket(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    if authenticate(&s, &headers, &ip).await.is_err() {
        return Json(json!({ "ok": false, "err": "sign in required" }));
    }
    let Some(path) = &s.apk_path else {
        return Json(json!({ "ok": false, "err": "no APK configured on this server" }));
    };
    let size = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(_) => return Json(json!({ "ok": false, "err": "APK not found on server" })),
    };
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let ticket = crate::blob::hex(&raw);
    let expires = now() + APK_TICKET_TTL_SECS;
    {
        let mut tickets = s.download_tickets.lock().unwrap();
        let current = now();
        tickets.retain(|_, exp| *exp >= current);
        tickets.insert(ticket.clone(), expires);
    }
    Json(json!({
        "ok": true,
        "url": format!("/download/connect.apk?ticket={ticket}"),
        "size": size,
        "expires": expires,
    }))
}

async fn release_notes(s: &AppState) -> Value {
    let Some(path) = &s.release_notes_path else {
        return json!({});
    };
    let Ok(bytes) = tokio::fs::read(path).await else {
        return json!({});
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return json!({});
    };
    json!({
        "version": v.get("version").and_then(Value::as_str).unwrap_or(""),
        "severity": v.get("severity").and_then(Value::as_str).unwrap_or("normal"),
        "title": v.get("title").and_then(Value::as_str).unwrap_or("Update available"),
        "notes": v.get("notes").and_then(Value::as_array).cloned().unwrap_or_default(),
    })
}

async fn artifact_metadata(
    path: &Path,
    cache: &std::sync::Mutex<Option<(u64, u64, String)>>,
) -> Option<(String, u64)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let size = meta.len();
    if let Some(hash) = cache
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(cached_mtime, cached_size, _)| *cached_mtime == mtime && *cached_size == size)
        .map(|(_, _, hash)| hash.clone())
    {
        return Some((hash, size));
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    let hash = tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        crate::blob::hex(&hasher.finalize())
    })
    .await
    .ok()?;
    *cache.lock().unwrap() = Some((mtime, size, hash.clone()));
    Some((hash, size))
}

/// In-app update check. The Connect app hashes its own installed base.apk
/// and compares against this: a mismatch means a new build is on the server
/// (no version bookkeeping to forget — the artifact IS the version). The
/// sha256 is cached keyed on (mtime, size) so we don't rehash a multi-MB
/// APK on every poll.
pub async fn app_version(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    use serde_json::json;
    let ip = peer.ip().to_string();
    if authenticate(&s, &headers, &ip).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    }
    let Some(path) = &s.apk_path else {
        return axum::Json(json!({ "ok": false, "err": "no APK configured on this server" }))
            .into_response();
    };
    let Some((hash, size)) = artifact_metadata(path, &s.apk_hash).await else {
        return axum::Json(json!({ "ok": false, "err": "APK not found on server" }))
            .into_response();
    };
    let notifier = match &s.notifier_apk_path {
        Some(path) => artifact_metadata(path, &s.notifier_apk_hash)
            .await
            .map(|(sha256, size)| json!({ "sha256": sha256, "size": size })),
        None => None,
    };
    let release = release_notes(&s).await;
    axum::Json(json!({
        "ok": true,
        "sha256": hash,
        "size": size,
        "release": release,
        "notifier": notifier,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::{cookie_value, query_value, SITE_ACCESS_COOKIE};
    use axum::http::{header, HeaderMap};

    #[test]
    fn query_value_finds_access_code() {
        assert_eq!(query_value("access=letmein&x=1", "access"), Some("letmein"));
        assert_eq!(query_value("x=1&access=letmein", "access"), Some("letmein"));
        assert_eq!(query_value("x=1", "access"), None);
    }

    #[test]
    fn cookie_value_finds_site_access_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("theme=dark; {SITE_ACCESS_COOKIE}=letmein; other=1")
                .parse()
                .unwrap(),
        );
        assert_eq!(cookie_value(&headers, SITE_ACCESS_COOKIE), Some("letmein"));
        assert_eq!(cookie_value(&headers, "missing"), None);
    }
}
