// Owns integration coverage for evidence snapshots, legal holds, quarantine, and held media.
// server/src/admin.rs owns preservation policy; blob.rs and connect.rs own bytes and content.

// Evidence preservation tests (CONSOLE-SPEC.md §5.2, 18 U.S.C. §2258A):
// reports freeze a snapshot of the content at filing time, snapshot media is
// legal-held so GC can never collect it (even after the author deletes),
// quarantine hides content from users while preserving it, the evidence
// bundle carries everything the authorities need, and the founder-gated
// media endpoint serves held blobs. Dev posture: zero GC grace window makes
// the delete-then-GC race provable; the founder gate is DB-based and live.

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
async fn snapshot_survives_deletion_and_quarantine_preserves() {
    let (base, handle, _dir) = spawn_server().await;
    // First account is the founder; author + reporter are ordinary users.
    let founder = signed_in(&base, "founder").await;
    let author = signed_in(&base, "author").await;
    let reporter = signed_in(&base, "reporter").await;

    // Author posts a photo; a control blob proves GC actually runs.
    // Profile + a bystander comment give the 0009 context capture
    // something to freeze.
    author.profile_set("Evil Author", "bio at filing time", None).await.unwrap();
    let evil_bytes = b"fake-image-evidence-bytes".to_vec();
    let evil_blob = author.blob_upload(&evil_bytes, false).await.unwrap();
    let control_blob = author.blob_upload(b"unreported-bytes", false).await.unwrap();
    let post = author
        .post_create("photo", "incriminating caption", &[evil_blob.clone()], "public")
        .await
        .unwrap();
    let control_post = author
        .post_create("photo", "innocent", &[control_blob.clone()], "public")
        .await
        .unwrap();
    reporter.comment_create(&post, "context comment").await.unwrap();

    // Report freezes a snapshot and legal-holds the media.
    reporter
        .report("post", &post, "illegal content — evidence test")
        .await
        .unwrap();
    let v = founder.admin_reports(false).await.unwrap();
    let report = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["subject_id"] == post.as_str())
        .expect("report listed");
    let report_id = report["id"].as_i64().unwrap();
    let snap = &report["snapshot"];
    assert_eq!(snap["author_handle"], "author");
    assert_eq!(snap["body"], "incriminating caption");
    assert_eq!(snap["media"][0], evil_blob.as_str());
    assert!(snap["captured_at"].as_i64().unwrap() > 0);

    // Author panic-deletes both posts; GC (grace 0 in dev) collects the
    // control blob but the legal-held evidence blob survives.
    author.post_delete(&post).await.unwrap();
    author.post_delete(&control_post).await.unwrap();
    founder.gc().await.unwrap();
    assert!(
        founder.admin_media(&control_blob).await.is_err(),
        "control blob should have been collected"
    );
    let bytes = founder.admin_media(&evil_blob).await.unwrap();
    assert_eq!(bytes, evil_bytes, "legal-held evidence blob must survive GC");

    // Snapshot still tells the story even though the post is gone.
    let v = founder.admin_reports(false).await.unwrap();
    let report = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == report_id)
        .unwrap();
    assert_eq!(report["snapshot"]["body"], "incriminating caption");

    // Quarantine: a second reported post is hidden from users but preserved.
    let q_bytes = b"quarantine-me".to_vec();
    let q_blob = author.blob_upload(&q_bytes, false).await.unwrap();
    let q_post = author
        .post_create("photo", "quarantine target", &[q_blob.clone()], "public")
        .await
        .unwrap();
    reporter.report("post", &q_post, "csam test").await.unwrap();
    founder
        .admin_quarantine("post", &q_post, "preserved for NCMEC")
        .await
        .unwrap();
    assert!(reporter.post_get(&q_post).await.is_err(), "quarantined post hidden");
    // Double quarantine is rejected; bytes survive GC and stay fetchable.
    assert!(founder.admin_quarantine("post", &q_post, "again").await.is_err());
    founder.gc().await.unwrap();
    assert_eq!(founder.admin_media(&q_blob).await.unwrap(), q_bytes);

    // Evidence bundle: report + snapshot + account records + audit trail.
    let bundle = founder.admin_evidence(report_id).await.unwrap();
    assert_eq!(bundle["report"]["id"].as_i64(), Some(report_id));
    assert_eq!(bundle["snapshot"]["media"][0], evil_blob.as_str());
    assert_eq!(bundle["author_account"]["handle"], "author");
    assert_eq!(bundle["reporter_account"]["handle"], "reporter");
    assert!(!bundle["author_account"]["devices"].as_array().unwrap().is_empty());
    assert!(founder.admin_evidence(999_999).await.is_err());

    // Deep snapshot (0008): network trail + content timeline frozen at filing.
    let snap = &bundle["snapshot"];
    assert!(snap["content_created"].as_i64().unwrap() > 0);
    assert!(!snap["author_ips"].as_array().unwrap().is_empty(),
        "author device/session IPs must be frozen at filing time");
    assert!(!snap["reporter_ip"].as_str().unwrap().is_empty());

    // Social context (0009): profile + surrounding thread frozen at filing,
    // surviving the author's later edits/deletes.
    assert_eq!(snap["author_profile"]["display_name"], "Evil Author");
    assert_eq!(snap["author_profile"]["bio"], "bio at filing time");
    assert_eq!(snap["thread"]["post"]["body"], "incriminating caption");
    let thread_comments = snap["thread"]["comments"].as_array().unwrap();
    assert!(thread_comments.iter().any(|c| c["body"] == "context comment"),
        "comments on the reported post must be frozen in the thread");

    // ToS forfeiture: quarantine put the author under evidence_hold —
    // visible in the bundle and freezing their whole footprint.
    assert!(bundle["author_account"]["evidence_hold"].as_i64().is_some());
    let fp = &bundle["author_footprint"];
    // post (reported → tombstoned, row preserved), control_post (unreported
    // → hard-deleted, gone), q_post (quarantined), held_post below.
    assert!(fp["posts"].as_array().unwrap().len() >= 2, "reported posts survive deletion");
    assert!(fp["posts"].as_array().unwrap().iter().any(|p| p["deleted_at"].as_i64().is_some()));
    assert!(fp["posts"].as_array().unwrap().iter().any(|p| p["body"] == "incriminating caption"),
        "the reported post's row must survive the author's delete");
    assert!(!fp["blobs"].as_array().unwrap().is_empty());
    assert!(!fp["audit_trail"].as_array().unwrap().is_empty());
    assert!(fp["reports"].as_array().unwrap().len() >= 2);

    // Forfeiture also shields NEW blobs from GC even without per-blob holds:
    // author uploads + deletes; owner is held, so GC must leave it.
    let held_bytes = b"uploaded-after-hold".to_vec();
    let held_blob = author.blob_upload(&held_bytes, false).await.unwrap();
    let held_post = author
        .post_create("photo", "posted after hold", &[held_blob.clone()], "public")
        .await
        .unwrap();
    author.post_delete(&held_post).await.unwrap();
    founder.gc().await.unwrap();
    assert_eq!(
        founder.admin_media(&held_blob).await.unwrap(),
        held_bytes,
        "held account's blobs must survive GC"
    );

    // Even account self-destruction can't erase the record: the held
    // author deletes their whole account, but posts/blobs/devices survive.
    author.account_delete().await.unwrap();
    let bundle3 = founder.admin_evidence(report_id).await.unwrap();
    assert_eq!(bundle3["author_account"]["status"], "deleted");
    assert!(!bundle3["author_account"]["devices"].as_array().unwrap().is_empty(),
        "device/IP history survives account deletion under hold");
    assert!(bundle3["author_footprint"]["posts"].as_array().unwrap().iter()
        .any(|p| p["body"] == "incriminating caption"));
    assert_eq!(founder.admin_media(&evil_blob).await.unwrap(), evil_bytes);

    // Suspension also triggers the hold; reinstatement lifts it (but filed
    // evidence blobs stay legal-held forever).
    founder.admin_suspend("reporter", "test hold").await.unwrap();
    let d = founder.admin_dossier("reporter").await.unwrap();
    assert_eq!(d["account"]["status"], "suspended");
    founder.admin_unsuspend("reporter").await.unwrap();
    let bundle2 = founder.admin_evidence(report_id).await.unwrap();
    assert!(
        bundle2["reporter_account"]["evidence_hold"].as_i64().is_none(),
        "unsuspend lifts the account-level hold"
    );
    assert_eq!(founder.admin_media(&evil_blob).await.unwrap(), evil_bytes);

    // Every evidence access left an audit row.
    let v = founder.admin_audit(None, None, 200).await.unwrap();
    let actions: Vec<&str> = v["audit"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["action"].as_str())
        .collect();
    assert!(actions.contains(&"quarantine"));
    assert!(actions.contains(&"evidence_export"));
    assert!(actions.contains(&"evidence_media_view"));

    // Non-founders are locked out of the entire evidence lane.
    assert!(reporter.admin_quarantine("post", &q_post, "nope").await.is_err());
    assert!(reporter.admin_evidence(report_id).await.is_err());
    assert!(reporter.admin_media(&evil_blob).await.is_err());

    handle.shutdown();
}
