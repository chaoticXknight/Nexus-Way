// Owns integration coverage for legal acceptance records and operator announcements.
// server/src/legal.rs owns acceptance state; admin.rs owns the announce broadcast.

// Prod posture so the founder gate and invite path are live.

use axum_server::Handle;
use nexus_way_hive::Config;

async fn spawn_prod_server() -> (String, Handle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
        .expect("self-signed cert");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();
    std::fs::write(&key_path, ck.key_pair.serialize_pem()).unwrap();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().to_path_buf(),
        dev: false,
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
        ..Config::default()
    };
    let handle = Handle::new();
    let h = handle.clone();
    tokio::spawn(async move {
        if let Err(e) = nexus_way_hive::serve(cfg, h).await {
            panic!("server died: {e:#}");
        }
    });
    let addr = handle.listening().await.expect("bind");
    (format!("https://{addr}"), handle, dir)
}

async fn client(base: &str) -> hive_client::Client {
    let c = hive_client::Client::new(base, true).unwrap();
    c.info().await.unwrap();
    c
}

#[tokio::test]
async fn registration_records_acceptance_and_gate_reopens_on_version_bump() {
    let (base, handle, dir) = spawn_prod_server().await;

    let founder = client(&base).await;
    let f_identity = hive_client::generate_key();
    let f_device = hive_client::generate_key();
    let (f_account, _) = founder
        .register(&f_identity, &f_device, "founder", "legal-test", None)
        .await
        .unwrap();
    founder.auth(&f_account, &f_device).await.unwrap();

    // Registration recorded acceptance of the current versions: no gate.
    let status = founder.legal_status().await.unwrap();
    assert_eq!(status["legal"]["needs_acceptance"], false);
    let docs = status["legal"]["docs"].as_array().unwrap();
    assert_eq!(docs.len(), 2);
    assert!(docs.iter().all(|d| d["accepted"] == true));
    let terms_version = docs
        .iter()
        .find(|d| d["doc"] == "terms")
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!terms_version.is_empty() && terms_version != "unversioned");

    // Simulate a version bump by retagging the stored acceptance rows as an
    // older version (the served text/version constants can't change inside
    // one process, but the gate logic only compares stored vs current).
    {
        let db_path = dir.path().join("hive.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}", db_path.display()))
            .await
            .expect("open test db");
        sqlx::query("UPDATE legal_acceptances SET version='2000-01-01'")
            .execute(&pool)
            .await
            .expect("retag acceptances");
    }

    // Gate is now closed until the account explicitly accepts again.
    let status = founder.legal_status().await.unwrap();
    assert_eq!(status["legal"]["needs_acceptance"], true);

    let after = founder.legal_accept().await.unwrap();
    assert_eq!(after["legal"]["needs_acceptance"], false);

    // whoami carries the same state for startup gating.
    let who = founder.whoami().await.unwrap();
    assert_eq!(who["legal"]["needs_acceptance"], false);

    handle.shutdown();
}

#[tokio::test]
async fn announce_broadcasts_system_alert_with_content() {
    let (base, handle, _dir) = spawn_prod_server().await;

    let founder = client(&base).await;
    let f_identity = hive_client::generate_key();
    let f_device = hive_client::generate_key();
    let (f_account, _) = founder
        .register(&f_identity, &f_device, "founder", "announce-test", None)
        .await
        .unwrap();
    founder.auth(&f_account, &f_device).await.unwrap();
    let codes = founder.admin_invite_create(1).await.unwrap();

    let guest = client(&base).await;
    let g_identity = hive_client::generate_key();
    let g_device = hive_client::generate_key();
    let (g_account, _) = guest
        .register(
            &g_identity,
            &g_device,
            "guest",
            "announce-guest",
            Some(&codes[0]),
        )
        .await
        .unwrap();
    guest.auth(&g_account, &g_device).await.unwrap();

    // Guests cannot announce.
    assert!(guest
        .admin_announce("system", "nope", "not allowed")
        .await
        .is_err());

    // Kind and length validation.
    assert!(founder.admin_announce("party", "t", "b").await.is_err());
    assert!(founder.admin_announce("system", "", "b").await.is_err());

    let announcement_id = founder
        .admin_announce(
            "maintenance",
            "Scheduled maintenance",
            "Connect will be briefly unavailable tonight at 21:00 CT.",
        )
        .await
        .unwrap();
    assert!(!announcement_id.is_empty());

    // Both accounts (including the announcing founder) got a persisted
    // system alert carrying the announcement content.
    for c in [&founder, &guest] {
        let inbox = c.notifications(None, 20).await.unwrap();
        let items = inbox["notifications"].as_array().unwrap();
        let system = items
            .iter()
            .find(|n| n["kind"] == "system")
            .expect("system alert present");
        assert_eq!(system["subject_id"], announcement_id.as_str());
        assert_eq!(system["announce_kind"], "maintenance");
        assert_eq!(system["title"], "Scheduled maintenance");
        assert!(system["body"]
            .as_str()
            .unwrap()
            .contains("briefly unavailable"));
    }

    handle.shutdown();
}
