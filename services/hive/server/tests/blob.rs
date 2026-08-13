// Owns end-to-end tests for blob upload, resume, deduplication, quota, access, and collection.
// server/src/blob.rs owns the behavior; hive-client drives the public protocol used here.

// Blob store integration tests (Hive_design_doc.md §3 DONE WHEN): round-trip
// with hash verification, resume after interruption, dedupe, quota rejection,
// GC of unreferenced blobs, private-blob access control.

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

/// Register + sign in a fresh account; returns the authed client.
async fn signed_in(base: &str, handle_name: &str) -> hive_client::Client {
    let c = hive_client::Client::new(base, true).unwrap();
    c.info().await.unwrap();
    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();
    let (account_id, _) = c
        .register(&identity, &device, handle_name, "test-device", None)
        .await
        .unwrap();
    c.auth(&account_id, &device).await.unwrap();
    c
}

#[tokio::test]
async fn upload_fetch_roundtrip_and_dedupe() {
    let (base, handle, _dir) = spawn_server().await;
    let c = signed_in(&base, "uploader").await;

    // 10 MB of patterned data → exercises multiple 4 MB chunks.
    let data: Vec<u8> = (0..10 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let blob_id = c.blob_upload(&data, false).await.unwrap();

    // Fetch verifies sha256(content) == id internally.
    let back = c.blob_fetch(&blob_id).await.unwrap();
    assert_eq!(back.len(), data.len());
    assert_eq!(back, data);

    // Re-upload of identical bytes is a no-op dedupe (same id, no error).
    let again = c.blob_upload(&data, false).await.unwrap();
    assert_eq!(again, blob_id);

    // Usage reflects one copy only.
    let (used, quota) = c.blob_usage().await.unwrap();
    assert_eq!(used, data.len() as i64);
    assert!(quota >= used);

    handle.shutdown();
}

#[tokio::test]
async fn quota_rejects_and_private_blobs_are_owner_only() {
    let (base, handle, _dir) = spawn_server().await;
    let c = signed_in(&base, "alice").await;

    // Over-quota upload rejected at begin (dev quota = 100 MB).
    let hash = "ab".repeat(32);
    let err = c
        .blob_upload_probe(200 * 1024 * 1024, &hash)
        .await
        .expect_err("over-quota must fail");
    assert!(err.to_string().contains("quota"), "got: {err:#}");

    // Private blob: owner reads it, a stranger gets forbidden.
    let secret = b"ciphertext pretend".to_vec();
    let blob_id = c.blob_upload(&secret, false).await.unwrap();
    assert_eq!(c.blob_fetch(&blob_id).await.unwrap(), secret);

    let stranger = signed_in(&base, "mallory").await;
    let err = stranger.blob_fetch(&blob_id).await.expect_err("must be forbidden");
    assert!(err.to_string().contains("403"), "got: {err:#}");

    // Public blob: readable by anyone.
    let pub_id = c.blob_upload(b"public bytes", true).await.unwrap();
    assert_eq!(stranger.blob_fetch(&pub_id).await.unwrap(), b"public bytes");

    handle.shutdown();
}

#[tokio::test]
async fn connect_media_has_a_bounded_quota_separate_from_general_storage() {
    let (base, handle, dir) = spawn_server().await;
    let c = signed_in(&base, "mediauser").await;

    let db = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        dir.path().join("hive.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("UPDATE accounts SET quota_bytes=0 WHERE handle='mediauser'")
        .execute(&db)
        .await
        .unwrap();

    let general_err = c
        .blob_upload(b"general private bytes", false)
        .await
        .expect_err("zero general quota must remain enforced");
    assert!(general_err.to_string().contains("quota exceeded"));

    let media = b"encrypted Fold photo";
    let media_id = c
        .blob_upload_with_purpose(media, false, "connect_media")
        .await
        .expect("bounded Connect media should not use general quota");
    assert_eq!(c.blob_fetch(&media_id).await.unwrap(), media);

    let oversized_err = c
        .blob_upload_probe_with_purpose(
            nexus_way_hive::blob::CONNECT_MEDIA_BLOB_MAX as u64 + 1,
            &"cd".repeat(32),
            "connect_media",
        )
        .await
        .expect_err("oversized encrypted media must be rejected");
    assert!(oversized_err.to_string().contains("36 MB"));

    for suffix in 1..=7 {
        c.blob_upload_probe_with_purpose(
            nexus_way_hive::blob::CONNECT_MEDIA_BLOB_MAX as u64,
            &format!("{suffix:064x}"),
            "connect_media",
        )
        .await
        .expect("valid pending media should fit before the account ceiling");
    }
    let allowance_err = c
        .blob_upload_probe_with_purpose(
            5 * 1024 * 1024,
            &"ef".repeat(32),
            "connect_media",
        )
        .await
        .expect_err("retained media allowance must include existing media");
    assert!(allowance_err.to_string().contains("256 MB"));

    handle.shutdown();
}

#[tokio::test]
async fn gc_removes_unreferenced_blobs() {
    let (base, handle, _dir) = spawn_server().await;
    let c = signed_in(&base, "gcuser").await;

    let blob_id = c.blob_upload(b"disposable", false).await.unwrap();
    assert!(c.blob_fetch(&blob_id).await.is_ok());

    // Nothing references it (refcount 0); dev GC has zero grace.
    let removed = c.gc().await.unwrap();
    assert!(removed >= 1, "expected at least one blob removed, got {removed}");

    let err = c.blob_fetch(&blob_id).await.expect_err("gone after GC");
    assert!(err.to_string().contains("404"), "got: {err:#}");

    handle.shutdown();
}
