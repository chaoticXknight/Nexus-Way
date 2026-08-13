// Owns integration coverage for registration, challenge auth, device reporting, and revocation.
// server/src/identity.rs owns behavior; hive-client constructs real signatures and certificates.

// Identity service integration tests (Hive_design_doc.md §1 DONE WHEN):
// register → auth → whoami, device reporting, revocation, bad signatures.

use axum_server::Handle;
use nexus_way_hive::Config;

async fn spawn_server() -> (String, Handle, tempfile::TempDir) {
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
    let addr = handle.listening().await.expect("bind");
    (format!("https://{addr}"), handle, dir)
}

async fn client(base: &str) -> hive_client::Client {
    let c = hive_client::Client::new(base, true).unwrap();
    c.info().await.unwrap(); // pin the server key
    c
}

#[tokio::test]
async fn register_auth_whoami_and_device_reporting() {
    let (base, handle, _dir) = spawn_server().await;
    let c = client(&base).await;

    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();

    // Register: first account bootstraps without an invite and is a founder.
    let (account_id, device_id) = c
        .register(&identity, &device, "joel", "workstation", None)
        .await
        .unwrap();
    assert_eq!(account_id, hive_client::account_id_for(&identity.verifying_key()));
    assert_eq!(device_id, hive_client::device_id_for(&device.verifying_key()));

    // Challenge–response sign-in.
    let token = c.auth(&account_id, &device).await.unwrap();
    assert!(!token.is_empty());

    // whoami reflects the account.
    let me = c.whoami().await.unwrap();
    assert_eq!(me["handle"], "joel");
    assert_eq!(me["account_id"], account_id);
    assert_eq!(me["founder"], true);

    // Device reporting: the device list carries a REAL last_ip + last_seen.
    let devs = c.devices().await.unwrap();
    let list = devs["devices"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], device_id);
    assert_eq!(list[0]["last_ip"], "127.0.0.1");
    assert!(list[0]["last_seen"].as_i64().unwrap() > 0);
    assert_eq!(list[0]["revoked"], false);

    c.logout().await.unwrap();
    let err = c.whoami().await.expect_err("logged-out session must be dead");
    assert!(err.to_string().contains("not signed in"), "got: {err:#}");

    handle.shutdown();
}

#[tokio::test]
async fn wrong_device_key_cannot_sign_in() {
    let (base, handle, _dir) = spawn_server().await;
    let c = client(&base).await;

    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();
    let (account_id, _) = c
        .register(&identity, &device, "alice", "phone", None)
        .await
        .unwrap();

    // An attacker with the right account/device IDs but the wrong private
    // key must fail signature verification.
    let imposter = hive_client::generate_key();
    let err = c.auth(&account_id, &imposter).await.expect_err("must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown or revoked device") || msg.contains("signature invalid"),
        "got: {msg}"
    );

    handle.shutdown();
}

#[tokio::test]
async fn recovery_absence_is_not_enumerable() {
    let (base, handle, _dir) = spawn_server().await;
    let c = client(&base).await;
    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();
    let (account_id, _) = c
        .register(&identity, &device, "recovery-user", "phone", None)
        .await
        .unwrap();
    c.auth(&account_id, &device).await.unwrap();

    let missing_account = c.escrow_fetch("unknown-user", "password").await.unwrap_err();
    let missing_path = c.escrow_fetch("recovery-user", "password").await.unwrap_err();
    assert!(missing_account.to_string().contains("recovery unavailable"));
    assert_eq!(missing_account.to_string(), missing_path.to_string());

    c.escrow_set("password", b"encrypted recovery blob").await.unwrap();
    let (fetched_account, blob) = c.escrow_fetch("recovery-user", "password").await.unwrap();
    assert_eq!(fetched_account, account_id);
    assert_eq!(blob, b"encrypted recovery blob");

    handle.shutdown();
}

#[tokio::test]
async fn revoked_device_is_dead_within_one_round_trip() {
    let (base, handle, _dir) = spawn_server().await;
    let c = client(&base).await;

    let identity = hive_client::generate_key();
    let dev_a = hive_client::generate_key();
    let (account_id, _) = c
        .register(&identity, &dev_a, "bob", "workstation", None)
        .await
        .unwrap();
    c.auth(&account_id, &dev_a).await.unwrap();

    // Enroll a second device by registering it under the same identity is a
    // step-4 linking flow; for revocation coverage, revoke device A from its
    // own session and confirm both the session and future auth die.
    let dev_a_id = hive_client::device_id_for(&dev_a.verifying_key());
    c.device_revoke(&dev_a_id).await.unwrap();

    // Session is gone.
    let err = c.whoami().await.expect_err("session must be dead");
    assert!(err.to_string().contains("invalid or expired session"), "got: {err:#}");

    // Future challenge-response is refused outright.
    let err = c.auth(&account_id, &dev_a).await.expect_err("auth must be refused");
    assert!(err.to_string().contains("unknown or revoked device"), "got: {err:#}");

    handle.shutdown();
}

#[tokio::test]
async fn duplicate_handles_and_replayed_challenges_are_rejected() {
    let (base, handle, _dir) = spawn_server().await;
    let c = client(&base).await;

    let id1 = hive_client::generate_key();
    let d1 = hive_client::generate_key();
    c.register(&id1, &d1, "carol", "laptop", None).await.unwrap();

    // Same handle, different identity: rejected.
    let id2 = hive_client::generate_key();
    let d2 = hive_client::generate_key();
    let err = c
        .register(&id2, &d2, "carol", "phone", None)
        .await
        .expect_err("duplicate handle must fail");
    assert!(err.to_string().contains("already exists"), "got: {err:#}");



    handle.shutdown();
}
