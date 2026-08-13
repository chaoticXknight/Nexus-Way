// Owns integration coverage for snapshot sequencing, linking, restore history, and push streams.
// server/src/sync.rs owns snapshot policy; api.rs owns normal and wake-only WebSockets.

// Vault sync integration tests (Hive_design_doc.md §4 DONE WHEN): two-device
// snapshot replication, sequence discipline under staleness, device linking,
// point-in-time restore, and live sync_hint pushes.

use axum_server::Handle;
use hive_client::CommitOutcome;
use nexus_way_hive::Config;
use sha2::{Digest, Sha256};

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

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Upload `data` as a blob and commit it as snapshot `seq`.
async fn push_snapshot(c: &hive_client::Client, seq: i64, data: &[u8]) -> CommitOutcome {
    let blob_id = c.blob_upload(data, false).await.unwrap();
    c.sync_commit(seq, &blob_id).await.unwrap()
}

#[tokio::test]
async fn two_devices_replicate_snapshots_with_sequence_discipline() {
    let (base, handle, _dir) = spawn_server().await;

    // Device A: register + sign in ("the phone", holds the identity key).
    let a = hive_client::Client::new(&base, true).unwrap();
    a.info().await.unwrap();
    let identity = hive_client::generate_key();
    let dev_a = hive_client::generate_key();
    let (account_id, _) = a
        .register(&identity, &dev_a, "joel", "phone", None)
        .await
        .unwrap();
    a.auth(&account_id, &dev_a).await.unwrap();

    // A commits snapshot 1 ("edit a login on machine A").
    let snap1 = b"vault-ciphertext-v1".to_vec();
    assert_eq!(push_snapshot(&a, 1, &snap1).await, CommitOutcome::Committed(1));

    // Link device B ("the desktop"): A signs B's cert (blessing, not
    // transfer), then B signs in with its own key.
    let dev_b = hive_client::generate_key();
    a.device_add(&identity, &dev_b.verifying_key(), "desktop")
        .await
        .unwrap();
    let b = hive_client::Client::new(&base, true).unwrap();
    b.info().await.unwrap();
    b.auth(&account_id, &dev_b).await.unwrap();

    // B bootstraps: status → seq 1 → pulls the exact ciphertext.
    let (seq, blob_id) = b.sync_status().await.unwrap().expect("has snapshot");
    assert_eq!(seq, 1);
    assert_eq!(blob_id, hex(&Sha256::digest(&snap1)));
    let (seq, data) = b.sync_pull(None).await.unwrap();
    assert_eq!((seq, data), (1, snap1.clone()));

    // B edits and commits 2.
    let snap2 = b"vault-ciphertext-v2".to_vec();
    assert_eq!(push_snapshot(&b, 2, &snap2).await, CommitOutcome::Committed(2));

    // A is stale and tries to commit 2 as well → behind, latest 2 →
    // pull/merge/retry with 3.
    let snap2a = b"vault-ciphertext-v2-from-a".to_vec();
    assert_eq!(push_snapshot(&a, 2, &snap2a).await, CommitOutcome::Behind(2));
    let (seq, data) = a.sync_pull(None).await.unwrap();
    assert_eq!((seq, data), (2, snap2.clone()));
    let merged = b"vault-ciphertext-v3-merged".to_vec();
    assert_eq!(push_snapshot(&a, 3, &merged).await, CommitOutcome::Committed(3));

    // Point-in-time restore: pull seq 1 explicitly.
    let (seq, data) = b.sync_pull(Some(1)).await.unwrap();
    assert_eq!((seq, data), (1, snap1));

    // History lists 3 snapshots, newest first.
    let h = b.sync_history().await.unwrap();
    let snaps = h["snapshots"].as_array().unwrap();
    assert_eq!(snaps.len(), 3);
    assert_eq!(snaps[0]["seq"], 3);

    handle.shutdown();
}

#[tokio::test]
async fn sync_hint_is_pushed_to_other_devices_only() {
    let (base, handle, _dir) = spawn_server().await;

    let a = hive_client::Client::new(&base, true).unwrap();
    a.info().await.unwrap();
    let identity = hive_client::generate_key();
    let dev_a = hive_client::generate_key();
    let (account_id, _) = a
        .register(&identity, &dev_a, "pusher", "phone", None)
        .await
        .unwrap();
    a.auth(&account_id, &dev_a).await.unwrap();

    let dev_b = hive_client::generate_key();
    a.device_add(&identity, &dev_b.verifying_key(), "desktop")
        .await
        .unwrap();
    let b = hive_client::Client::new(&base, true).unwrap();
    b.info().await.unwrap();
    b.auth(&account_id, &dev_b).await.unwrap();

    // Both devices listen.
    let mut stream_a = a.stream().await.unwrap();
    let mut stream_b = b.stream().await.unwrap();

    // A commits → B must receive the hint within seconds (no polling).
    push_snapshot(&a, 1, b"snap").await;
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream_b.recv())
        .await
        .expect("timed out waiting for sync_hint")
        .expect("stream closed");
    assert_eq!(frame["type"], "sync_hint");
    assert_eq!(frame["seq"], 1);

    // The originator does NOT get its own hint.
    let none = tokio::time::timeout(std::time::Duration::from_millis(300), stream_a.recv()).await;
    assert!(none.is_err(), "originator should not receive its own sync_hint");

    handle.shutdown();
}

#[tokio::test]
async fn stream_rejects_bad_tokens() {
    let (base, handle, _dir) = spawn_server().await;
    let c = hive_client::Client::new(&base, true).unwrap();
    c.info().await.unwrap();
    c.set_token("forged-token");
    let err = c.stream().await.expect_err("must fail auth");
    assert!(err.to_string().contains("auth failed"), "got: {err:#}");
    handle.shutdown();
}

#[tokio::test]
async fn notification_relay_is_wake_only_and_receives_notification_frames() {
    let (base, handle, _dir) = spawn_server().await;
    let alice = hive_client::Client::new(&base, true).unwrap();
    alice.info().await.unwrap();
    let alice_identity = hive_client::generate_key();
    let alice_device = hive_client::generate_key();
    let (alice_id, _) = alice
        .register(&alice_identity, &alice_device, "relay-alice", "phone", None)
        .await
        .unwrap();
    alice.auth(&alice_id, &alice_device).await.unwrap();

    let bob = hive_client::Client::new(&base, true).unwrap();
    bob.info().await.unwrap();
    let bob_identity = hive_client::generate_key();
    let bob_device = hive_client::generate_key();
    let (bob_id, _) = bob
        .register(&bob_identity, &bob_device, "relay-bob", "phone", None)
        .await
        .unwrap();
    bob.auth(&bob_id, &bob_device).await.unwrap();

    bob.follow_request("relay-alice").await.unwrap();
    alice.follow_accept("relay-bob").await.unwrap();
    let relay_token = alice.notification_token().await.unwrap();
    let mut relay = alice.notification_stream(&relay_token).await.unwrap();

    bob.wire_request("relay-alice").await.unwrap();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), relay.recv())
        .await
        .expect("relay frame in time")
        .expect("relay remains open");
    assert_eq!(frame["type"], "wire_request");
    // The persisted message_request alert follows as a connect_notif frame —
    // the wake relay must see it so backgrounded devices sync the alert.
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), relay.recv())
        .await
        .expect("alert frame in time")
        .expect("relay remains open");
    assert_eq!(frame["type"], "connect_notif");
    assert_eq!(frame["kind"], "message_request");

    let relay_client = hive_client::Client::new(&base, true).unwrap();
    relay_client.set_token(&relay_token);
    let error = relay_client.whoami().await.expect_err("relay token must not authorize REST");
    assert!(error.to_string().contains("invalid or expired session"));

    let error = alice
        .notification_stream("forged-relay-token")
        .await
        .expect_err("forged relay token must fail");
    assert!(error.to_string().contains("auth failed"));

    alice.notification_revoke().await.unwrap();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), relay.recv())
        .await
        .expect("revoked relay closes in time");
    assert!(closed.is_none(), "revoked relay must close");
    handle.shutdown();
}
