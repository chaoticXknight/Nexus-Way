// Owns legal-document versioning and acceptance records (§16 of the beta ToS:
// material changes are announced in-app and the client asks the user to review
// and accept before continuing). api.rs owns transport; identity.rs records the
// initial acceptance at registration; admin.rs owns the announcement broadcast
// that usually accompanies a version bump.
//
// A document's "version" is the "Last updated:" date parsed from the served
// text at startup, so bumping the date in legal/*.txt IS the version bump —
// there is no second constant to forget.

use crate::api::AppState;
use crate::identity::authenticate;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::LazyLock;

fn err(msg: &str) -> Json<Value> {
    Json(json!({ "ok": false, "err": msg }))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

const TERMS_TEXT: &str = include_str!("../legal/terms.txt");
const PRIVACY_TEXT: &str = include_str!("../legal/privacy.txt");

/// Parse the "Last updated: YYYY-MM-DD" line out of a served legal document.
/// Panics at first use in debug if the marker is missing — the marker line is
/// part of the document contract.
fn parse_version(text: &str) -> String {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Last updated:"))
        .map(|rest| rest.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unversioned".to_string())
}

pub static TERMS_VERSION: LazyLock<String> = LazyLock::new(|| parse_version(TERMS_TEXT));
pub static PRIVACY_VERSION: LazyLock<String> = LazyLock::new(|| parse_version(PRIVACY_TEXT));

/// Record acceptance of the CURRENT versions of both documents for an
/// account. Used at registration (the sign-up UIs present both documents)
/// and by the explicit accept endpoint. Idempotent.
pub(crate) async fn record_current_acceptance(s: &AppState, account_id: &str, ip: &str) {
    let at = now();
    for (doc, version) in [
        ("terms", TERMS_VERSION.as_str()),
        ("privacy", PRIVACY_VERSION.as_str()),
    ] {
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO legal_acceptances (account_id, doc, version, accepted_at, ip) \
             VALUES (?,?,?,?,?)",
        )
        .bind(account_id)
        .bind(doc)
        .bind(version)
        .bind(at)
        .bind(ip)
        .execute(&s.db)
        .await;
    }
}

/// The acceptance state used by clients to drive the blocking review gate.
pub(crate) async fn acceptance_state(s: &AppState, account_id: &str) -> Value {
    let mut docs = Vec::with_capacity(2);
    let mut needs = false;
    for (doc, version) in [
        ("terms", TERMS_VERSION.as_str()),
        ("privacy", PRIVACY_VERSION.as_str()),
    ] {
        let accepted: Option<(i64,)> = sqlx::query_as(
            "SELECT accepted_at FROM legal_acceptances \
             WHERE account_id=? AND doc=? AND version=?",
        )
        .bind(account_id)
        .bind(doc)
        .bind(version)
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
        if accepted.is_none() {
            needs = true;
        }
        docs.push(json!({
            "doc": doc,
            "version": version,
            "accepted": accepted.is_some(),
            "accepted_at": accepted.map(|(at,)| at),
        }));
    }
    json!({ "docs": docs, "needs_acceptance": needs })
}

/// GET /v1/legal/status — current document versions + this account's
/// acceptance state. Drives the client-side blocking gate.
pub async fn status(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let state = acceptance_state(&s, &me).await;
    Json(json!({ "ok": true, "legal": state }))
}

#[derive(Deserialize)]
pub struct AcceptReq {
    /// Which documents the user reviewed. Must name both current docs —
    /// the review UI shows both, and partial acceptance has no meaning
    /// for continued use of the service.
    pub docs: Vec<String>,
}

/// POST /v1/legal/accept — record explicit acceptance of the current
/// document versions after the in-app review. Audited with IP.
pub async fn accept(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AcceptReq>,
) -> Json<Value> {
    let ip = peer.ip().to_string();
    let (me, _) = match authenticate(&s, &headers, &ip).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    for required in ["terms", "privacy"] {
        if !req.docs.iter().any(|d| d == required) {
            return err("both the terms and privacy policy must be accepted");
        }
    }
    record_current_acceptance(&s, &me, &ip).await;
    crate::admin::audit(
        &s,
        &me,
        "legal_accept",
        &format!(
            "terms={} privacy={}",
            TERMS_VERSION.as_str(),
            PRIVACY_VERSION.as_str()
        ),
        &ip,
    )
    .await;
    let state = acceptance_state(&s, &me).await;
    Json(json!({ "ok": true, "legal": state }))
}
