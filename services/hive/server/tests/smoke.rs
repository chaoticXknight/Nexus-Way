// Owns startup, persistent server identity, artifact delivery, and TURN credential smoke tests.
// server/src/lib.rs assembles the server; web.rs and wire.rs own the tested endpoints.

// Integration harness (Hive_design_doc.md §8.4): spin an ephemeral HIVE on a
// random port with a temp data dir, drive it with the same hive-client crate
// the real apps embed.

use axum_server::Handle;
use hmac::{Hmac, Mac};
use nexus_way_hive::Config;
use sha1::Sha1;

async fn spawn_server() -> (std::net::SocketAddr, Handle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().to_path_buf(),
        dev: true,
        ..Config::default()
    };
    let handle = Handle::new();
    let h = handle.clone();
    tokio::spawn(async move {
        if let Err(e) = nexus_way_hive::serve(cfg, h).await {
            panic!("server died: {e:#}");
        }
    });
    let addr = handle.listening().await.expect("server failed to bind");
    (addr, handle, dir)
}

#[tokio::test]
async fn boots_serves_info_and_health_with_pinning() {
    let (addr, handle, _dir) = spawn_server().await;
    let client = hive_client::Client::new(format!("https://{addr}"), true).unwrap();

    // /v1/info: sane descriptor, TOFU pin recorded.
    let info = client.info().await.unwrap();
    assert!(info.ok);
    assert!(info.dev);
    assert!(!info.server_pub.is_empty());
    assert!(info.min_client <= hive_client::PROTOCOL_VERSION);
    let pin = client.pin().expect("pin set after first contact");

    // Second contact with same key: fine.
    client.info().await.unwrap();
    assert_eq!(client.pin().unwrap(), pin);

    // /v1/health: DB + blob dir writable.
    assert!(client.health().await.unwrap());

    handle.shutdown();
}

#[tokio::test]
async fn local_monitor_exposes_recent_activity_counters() {
    let (addr, handle, _dir) = spawn_server().await;
    let response: serde_json::Value = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
        .get(format!("https://{addr}/v1/monitor"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(response["ok"], true);
    for path in [
        "/identity/accounts_created_24h",
        "/identity/accounts_seen_24h",
        "/connect/posts_24h",
        "/connect/comments_24h",
        "/connect/notifications_24h",
        "/wire/messages_24h",
    ] {
        assert!(response.pointer(path).is_some(), "missing monitor field {path}");
    }

    handle.shutdown();
}

#[tokio::test]
async fn swapped_server_key_is_loudly_rejected() {
    let (addr, handle, _dir) = spawn_server().await;
    let client = hive_client::Client::new(format!("https://{addr}"), true).unwrap();

    // Simulate a client that pinned a DIFFERENT server key earlier.
    client.set_pin("bogus-pinned-key");
    let err = client.info().await.expect_err("pin mismatch must fail");
    assert!(err.to_string().contains("CHANGED"), "got: {err:#}");

    handle.shutdown();
}

#[tokio::test]
async fn server_key_survives_restart() {
    // Same data dir across two boots → same server identity (§2.3: host
    // migration is a copy, not a re-trust).
    let dir = tempfile::tempdir().unwrap();
    let mut pins = Vec::new();
    for _ in 0..2 {
        let cfg = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().to_path_buf(),
            dev: true,
            ..Config::default()
        };
        let handle = Handle::new();
        let h = handle.clone();
        tokio::spawn(async move {
            let _ = nexus_way_hive::serve(cfg, h).await;
        });
        let addr = handle.listening().await.expect("bind");
        let client = hive_client::Client::new(format!("https://{addr}"), true).unwrap();
        pins.push(client.info().await.unwrap().server_pub);
        handle.shutdown();
    }
    assert_eq!(pins[0], pins[1], "server identity must survive restart");
}

#[tokio::test]
async fn authenticated_connect_can_download_advertised_notifier() {
    let dir = tempfile::tempdir().unwrap();
    let connect_apk = dir.path().join("connect.apk");
    let notifier_apk = dir.path().join("nexus-notify.apk");
    std::fs::write(&connect_apk, b"connect-test-apk").unwrap();
    std::fs::write(&notifier_apk, b"notifier-test-apk").unwrap();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().join("data"),
        dev: true,
        apk_path: Some(connect_apk),
        notifier_apk_path: Some(notifier_apk),
        turn_urls: vec!["turn:turn.example.test:3478?transport=udp".into()],
        turn_secret: Some("test-turn-secret".into()),
        ..Config::default()
    };
    let handle = Handle::new();
    let server_handle = handle.clone();
    tokio::spawn(async move {
        nexus_way_hive::serve(cfg, server_handle).await.unwrap();
    });
    let addr = handle.listening().await.expect("server failed to bind");
    let base = format!("https://{addr}");
    let client = hive_client::Client::new(&base, true).unwrap();
    client.info().await.unwrap();
    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();
    let (account_id, _) = client
        .register(&identity, &device, "notifier-test", "test", None)
        .await
        .unwrap();
    let token = client.auth(&account_id, &device).await.unwrap();
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let anonymous = http
        .get(format!("{base}/download/nexus-notify.apk"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    let version: serde_json::Value = http
        .get(format!("{base}/v1/app/version"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(version["notifier"]["size"], 17);
    assert_eq!(version["notifier"]["sha256"].as_str().unwrap().len(), 64);

    let anonymous_ice: serde_json::Value = http
        .get(format!("{base}/v1/wire/call_ice"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(anonymous_ice["ok"], false);

    let ice: serde_json::Value = http
        .get(format!("{base}/v1/wire/call_ice"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ice["ok"], true);
    let username = ice["ice_servers"][0]["username"].as_str().unwrap();
    let expires: i64 = username.split(':').next().unwrap().parse().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((now + 295..=now + 300).contains(&expires));
    let mut mac = Hmac::<Sha1>::new_from_slice(b"test-turn-secret").unwrap();
    mac.update(username.as_bytes());
    assert_eq!(
        ice["ice_servers"][0]["credential"].as_str().unwrap(),
        nexus_common::b64::encode(&mac.finalize().into_bytes()),
    );

    let downloaded = http
        .get(format!("{base}/download/nexus-notify.apk"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(downloaded.as_ref(), b"notifier-test-apk");
    handle.shutdown();
}
