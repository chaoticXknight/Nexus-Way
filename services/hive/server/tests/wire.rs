// Owns integration coverage for accepted chats, opaque envelopes, attachments, receipts, and blocks.
// server/src/wire.rs owns relay policy; blob.rs owns attachment ciphertext storage.

use axum_server::Handle;
use nexus_common::b64;
use nexus_way_hive::Config;
use sha2::{Digest, Sha256};

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

async fn spawn_server() -> (String, Handle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().to_path_buf(),
        dev: true,
        ..Config::default()
    };
    let handle = Handle::new();
    let task_handle = handle.clone();
    tokio::spawn(async move {
        if let Err(error) = nexus_way_hive::serve(cfg, task_handle).await {
            panic!("server died: {error:#}");
        }
    });
    let address = handle.listening().await.expect("bind");
    (format!("https://{address}"), handle, dir)
}

async fn user(
    base: &str,
    handle: &str,
) -> (hive_client::Client, String, String, ed25519_dalek::SigningKey) {
    let client = hive_client::Client::new(base, true).unwrap();
    client.info().await.unwrap();
    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();
    let (account_id, device_id) = client
        .register(&identity, &device, handle, "test", None)
        .await
        .unwrap();
    client.auth(&account_id, &device).await.unwrap();
    (client, account_id, device_id, device)
}

#[tokio::test]
async fn accepted_message_requests_relay_and_ack_opaque_envelopes() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, _alice_id, _alice_device_id, alice_device) = user(&base, "alice").await;
    let (bob, _bob_id, bob_device_id, bob_device) = user(&base, "bob").await;
    let (charlie, _charlie_id, _charlie_device_id, _charlie_device) = user(&base, "charlie").await;
    alice.wire_publish(&alice_device, &b64::encode(&[1; 32])).await.unwrap();
    bob.wire_publish(&bob_device, &b64::encode(&[2; 32])).await.unwrap();

    let error = alice.wire_directory("bob").await.expect_err("unaccepted directory must fail");
    assert!(error.to_string().contains("accepted message request"));

    alice.follow_request("bob").await.unwrap();
    bob.follow_accept("alice").await.unwrap();
    let error = alice.wire_directory("bob").await.expect_err("follow alone must not permit chat");
    assert!(error.to_string().contains("accepted message request"));

    assert_eq!(alice.wire_request("bob").await.unwrap(), "pending");
    assert_eq!(alice.wire_conversations().await.unwrap()["conversations"][0]["direction"], "outgoing");
    assert_eq!(bob.wire_conversations().await.unwrap()["conversations"][0]["direction"], "incoming");
    let error = alice.wire_directory("bob").await.expect_err("pending request must not permit chat");
    assert!(error.to_string().contains("accepted message request"));
    assert_eq!(bob.wire_respond("alice", true).await.unwrap(), "accepted");

    let mut bob_stream = bob.stream().await.unwrap();
    assert_eq!(alice.wire_request("bob").await.unwrap(), "accepted");
    let reopened = tokio::time::timeout(std::time::Duration::from_secs(5), bob_stream.recv())
        .await
        .expect("renewed request in time")
        .expect("stream remains open");
    assert_eq!(reopened["type"], "wire_request");
    assert_eq!(reopened["state"], "accepted");

    let directory = alice.wire_directory("bob").await.unwrap();
    assert_eq!(directory["devices"][0]["device_id"], bob_device_id);
    assert_eq!(directory["devices"][0]["wire_pub"], b64::encode(&[2; 32]));

    let message_id = "0123456789abcdef0123456789abcdef";
    alice.wire_send(&bob_device_id, message_id, "opaque-ciphertext").await.unwrap();
    assert!(alice.wire_inbox().await.unwrap()["messages"].as_array().unwrap().is_empty());
    let inbox = bob.wire_inbox().await.unwrap();
    assert_eq!(inbox["messages"][0]["msg_id"], message_id);
    assert_eq!(inbox["messages"][0]["envelope"], "opaque-ciphertext");
    let receipt = alice.wire_receipts(&[message_id]).await.unwrap();
    assert!(receipt["receipts"][0]["delivered_at"].is_number());
    assert!(receipt["receipts"][0]["received_at"].is_null());
    assert_eq!(bob.wire_retract(message_id).await.unwrap(), 0);

    bob.wire_ack(&[message_id]).await.unwrap();
    assert!(bob.wire_inbox().await.unwrap()["messages"].as_array().unwrap().is_empty());
    let receipt = alice.wire_receipts(&[message_id]).await.unwrap();
    assert!(receipt["receipts"][0]["received_at"].is_number());
    assert!(receipt["receipts"][0]["read_at"].is_null());

    bob.wire_read(&[message_id]).await.unwrap();
    let receipt = alice.wire_receipts(&[message_id]).await.unwrap();
    assert!(receipt["receipts"][0]["read_at"].is_number());

    let queued_id = "11111111111111111111111111111111";
    alice.wire_send(&bob_device_id, queued_id, "queued").await.unwrap();
    assert_eq!(alice.wire_retract(queued_id).await.unwrap(), 1);
    assert!(bob.wire_inbox().await.unwrap()["messages"].as_array().unwrap().is_empty());

    let attachment = b"opaque encrypted photo bytes";
    let blob_id = alice.blob_upload(attachment, false).await.unwrap();
    let attachment_id = "22222222222222222222222222222222";
    alice
        .wire_send_attachment(
            &bob_device_id,
            attachment_id,
            "envelope-with-encrypted-key",
            Some(&blob_id),
        )
        .await
        .unwrap();
    assert_eq!(bob.wire_attachment_fetch(&blob_id).await.unwrap(), attachment);
    assert!(charlie.wire_attachment_fetch(&blob_id).await.is_err());
    assert_eq!(alice.wire_retract(attachment_id).await.unwrap(), 1);
    assert!(bob.wire_attachment_fetch(&blob_id).await.is_err());

    bob.block("alice").await.unwrap();
    let error = alice
        .wire_send(&bob_device_id, "fedcba9876543210fedcba9876543210", "blocked")
        .await
        .expect_err("blocked sender must fail");
    assert!(error.to_string().contains("accepted message request"));
    handle.shutdown();
}

#[tokio::test]
async fn encrypted_history_sync_is_target_device_scoped_and_replaceable() {
    let (base, handle, _dir) = spawn_server().await;

    let alice = hive_client::Client::new(&base, true).unwrap();
    alice.info().await.unwrap();
    let alice_identity = hive_client::generate_key();
    let alice_device = hive_client::generate_key();
    let (alice_id, _) = alice
        .register(&alice_identity, &alice_device, "history-alice", "phone-a", None)
        .await
        .unwrap();
    alice.auth(&alice_id, &alice_device).await.unwrap();
    alice.wire_publish(&alice_device, &b64::encode(&[7; 32])).await.unwrap();

    let second_device = hive_client::generate_key();
    let second_device_id = alice
        .device_add(
            &alice_identity,
            &second_device.verifying_key(),
            "phone-b",
        )
        .await
        .unwrap();
    let second = hive_client::Client::new(&base, true).unwrap();
    second.info().await.unwrap();
    second.auth(&alice_id, &second_device).await.unwrap();
    second
        .wire_publish(&second_device, &b64::encode(&[8; 32]))
        .await
        .unwrap();

    let mut first_stream = alice.stream().await.unwrap();
    second.wire_history_sync_request().await.unwrap();
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), first_stream.recv())
        .await
        .expect("sync request in time")
        .expect("stream remains open");
    assert_eq!(request["type"], "wire_history_sync_request");
    assert_eq!(request["requesting_device"], second_device_id);

    let first_payload = "encrypted-history-one";
    let first_snapshot = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    alice
        .wire_history_sync_publish(
            first_snapshot,
            &second_device_id,
            &sha256(first_payload),
            first_payload,
            "encrypted-pointer-one",
            100,
        )
        .await
        .unwrap();
    assert!(alice.wire_history_sync_offers().await.unwrap()["offers"]
        .as_array()
        .unwrap()
        .is_empty());
    let offers = second.wire_history_sync_offers().await.unwrap();
    assert_eq!(offers["offers"][0]["snapshot_id"], first_snapshot);
    assert_eq!(offers["offers"][0]["envelope"], "encrypted-pointer-one");
    assert_eq!(offers["offers"][0]["snapshot"], first_payload);

    let replacement_payload = "encrypted-history-two";
    let replacement_snapshot = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    alice
        .wire_history_sync_publish(
            replacement_snapshot,
            &second_device_id,
            &sha256(replacement_payload),
            replacement_payload,
            "encrypted-pointer-two",
            200,
        )
        .await
        .unwrap();
    let offers = second.wire_history_sync_offers().await.unwrap();
    assert_eq!(offers["offers"].as_array().unwrap().len(), 1);
    assert_eq!(offers["offers"][0]["snapshot_id"], replacement_snapshot);

    let (bob, _, _, _) = user(&base, "history-bob").await;
    let outsider_payload = "outsider";
    assert!(bob
        .wire_history_sync_publish(
            "cccccccccccccccccccccccccccccccc",
            &second_device_id,
            &sha256(outsider_payload),
            outsider_payload,
            "outsider-pointer",
            1,
        )
        .await
        .is_err());

    second
        .wire_history_sync_consume(replacement_snapshot, 200)
        .await
        .unwrap();
    assert!(second.wire_history_sync_offers().await.unwrap()["offers"]
        .as_array()
        .unwrap()
        .is_empty());

    handle.shutdown();
}