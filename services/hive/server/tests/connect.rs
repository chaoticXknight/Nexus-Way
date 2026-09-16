// Owns integration coverage for Connect graph, content, moderation intake, and call signaling.
// server/src/connect.rs and wire.rs own behavior; hive-client supplies realistic callers.

// Connect integration tests (Hive_design_doc.md §6 DONE WHEN): two accounts
// befriend, post (public + followers + circle), comment, react, block,
// report; feed obeys the graph and blocks; circle key rotation locks out a
// removed member's new reads.

use axum_server::Handle;
use nexus_way_hive::Config;
use serde_json::json;

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

/// Register + sign in a fresh account; returns (client, account_id).
async fn user(base: &str, handle: &str) -> (hive_client::Client, String) {
    let c = hive_client::Client::new(base, true).unwrap();
    c.info().await.unwrap();
    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();
    let (account_id, _) = c
        .register(&identity, &device, handle, "test", None)
        .await
        .unwrap();
    c.auth(&account_id, &device).await.unwrap();
    (c, account_id)
}

fn post_bodies(feed: &serde_json::Value) -> Vec<String> {
    feed["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["body"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn mentions_resolve_longest_handle_with_spaces() {
    let (base, handle, _dir) = spawn_server().await;
    let (author, _) = user(&base, "mention-author").await;
    let (short, _) = user(&base, "Jane").await;
    let (spaced, _) = user(&base, "Jane Doe").await;

    author
        .post_create("text", "Hello @Jane Doe", &[], "public")
        .await
        .unwrap();

    let spaced_notifications = spaced.notifications(None, 50).await.unwrap();
    assert_eq!(
        spaced_notifications["notifications"][0]["kind"].as_str(),
        Some("mention"),
    );
    assert!(short.notifications(None, 50).await.unwrap()["notifications"]
        .as_array()
        .unwrap()
        .is_empty());

    handle.shutdown();
}

#[tokio::test]
async fn call_signaling_requires_an_accepted_unblocked_chat() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, _) = user(&base, "call-alice").await;
    let (bob, _) = user(&base, "call-bob").await;
    let mut bob_stream = bob.stream().await.unwrap();

    alice.follow_request("call-bob").await.unwrap();
    let _ = bob_stream.recv().await;
    bob.follow_accept("call-alice").await.unwrap();
    alice.wire_request("call-bob").await.unwrap();
    // A new message request pushes two frames: the wire_request itself and
    // the persisted message_request alert (connect_notif).
    let _ = bob_stream.recv().await;
    let _ = bob_stream.recv().await;
    bob.wire_respond("call-alice", true).await.unwrap();

    alice
        .wire_call_signal(
            "call-bob",
            "0123456789abcdef0123456789abcdef",
            "invite",
            "video",
            "e30=",
            "test-signature",
        )
        .await
        .unwrap();
    let signal = tokio::time::timeout(std::time::Duration::from_secs(5), bob_stream.recv())
        .await
        .expect("signal in time")
        .expect("stream open");
    assert_eq!(signal["type"].as_str(), Some("call_signal"));
    assert_eq!(signal["from"].as_str().is_some(), true);
    assert_eq!(signal["kind"].as_str(), Some("video"));

    // Active-call liveness actions use the same authenticated ephemeral relay.
    for action in ["heartbeat", "heartbeat_ack"] {
        alice
            .wire_call_signal(
                "call-bob",
                "0123456789abcdef0123456789abcdef",
                action,
                "video",
                "e30=",
                "test-signature",
            )
            .await
            .unwrap();
        let signal = tokio::time::timeout(std::time::Duration::from_secs(5), bob_stream.recv())
            .await
            .expect("heartbeat in time")
            .expect("stream open");
        assert_eq!(signal["action"].as_str(), Some(action));
    }

    let error = alice
        .wire_call_signal(
            "call-bob",
            "0123456789abcdef0123456789abcdef",
            "unknown",
            "video",
            "e30=",
            "test-signature",
        )
        .await
        .expect_err("unknown call actions must be rejected");
    assert!(error.to_string().contains("invalid call action"));

    bob.block("call-alice").await.unwrap();
    let error = alice
        .wire_call_signal(
            "call-bob",
            "0123456789abcdef0123456789abcdef",
            "hangup",
            "video",
            "e30=",
            "test-signature",
        )
        .await
        .expect_err("blocked calls must be rejected");
    assert!(error.to_string().contains("accepted message request"));
    handle.shutdown();
}

#[tokio::test]
async fn pending_calls_replay_without_resurrecting_cancelled_invites() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, _) = user(&base, "replay-alice").await;
    let (bob, _) = user(&base, "replay-bob").await;
    alice.follow_request("replay-bob").await.unwrap();
    bob.follow_accept("replay-alice").await.unwrap();
    alice.wire_request("replay-bob").await.unwrap();
    bob.wire_respond("replay-alice", true).await.unwrap();
    let call_id = "1234567890abcdef1234567890abcdef";
    alice.wire_call_signal("replay-bob", call_id, "invite", "voice", "e30=", "signature")
        .await.unwrap();
    let mut stream = bob.stream().await.unwrap();
    let invite = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
        .await.unwrap().unwrap();
    assert_eq!(invite["action"], "invite");
    assert_eq!(invite["call_id"], call_id);
    assert!(invite["expires_at"].as_i64().unwrap() > 0);

    let token = bob.notification_token().await.unwrap();
    let mut relay = bob.notification_stream(&token).await.unwrap();
    let replay = tokio::time::timeout(std::time::Duration::from_secs(5), relay.recv())
        .await.unwrap().unwrap();
    assert_eq!(replay["action"], "invite");
    alice.wire_call_signal("replay-bob", call_id, "hangup", "voice", "e30=", "signature")
        .await.unwrap();
    alice.wire_call_signal("replay-bob", call_id, "invite", "voice", "e30=", "signature")
        .await.unwrap();
    let mut reconnected = bob.notification_stream(&token).await.unwrap();
    alice.wire_call_signal("replay-bob", call_id, "heartbeat", "voice", "e30=", "signature")
        .await.unwrap();
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), reconnected.recv())
        .await.unwrap().unwrap();
    assert_eq!(next["action"], "heartbeat", "cancelled invite must not replay");
    handle.shutdown();
}

#[tokio::test]
async fn befriend_post_comment_react_block_flow() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, alice_id) = user(&base, "alice").await;
    let (bob, bob_id) = user(&base, "bob smith").await;

    // Profiles are public plaintext, addressable by handle.
    bob.profile_set("Bob Smith", "hello from the porch", None)
        .await
        .unwrap();
    let p = alice.profile_get("bob smith").await.unwrap();
    assert_eq!(p["display_name"].as_str(), Some("Bob Smith"));
    assert_eq!(p["account_id"].as_str(), Some(bob_id.as_str()));

    // Alice requests, Bob sees it pending and receives a live notif.
    let mut bob_stream = bob.stream().await.unwrap();
    alice.follow_request("bob smith").await.unwrap();
    let notif = tokio::time::timeout(std::time::Duration::from_secs(5), bob_stream.recv())
        .await
        .expect("notif in time")
        .expect("stream open");
    assert_eq!(notif["type"].as_str(), Some("connect_notif"));
    assert_eq!(notif["kind"].as_str(), Some("follow_request"));
    assert_eq!(notif["from"]["handle"].as_str(), Some("alice"));

    let g = bob.follows().await.unwrap();
    assert_eq!(
        g["pending_in"][0]["account_id"].as_str(),
        Some(alice_id.as_str())
    );
    bob.follow_accept("alice").await.unwrap();

    // Before Bob posts, Alice's feed is empty.
    assert!(post_bodies(&alice.feed(None, 30).await.unwrap()).is_empty());

    // Alice follows Bob (accepted) — Bob does NOT follow Alice. Bob's
    // followers-only post reaches Alice; Alice's posts do not reach Bob.
    let pub_post = bob
        .post_create("text", "public hello", &[], "public")
        .await
        .unwrap();
    let fol_post = bob
        .post_create("text", "followers only", &[], "followers")
        .await
        .unwrap();
    alice
        .post_create("text", "alice's post", &[], "followers")
        .await
        .unwrap();

    let bodies = post_bodies(&alice.feed(None, 30).await.unwrap());
    assert_eq!(
        bodies,
        vec![
            "alice's post".to_string(),
            "followers only".into(),
            "public hello".into()
        ]
    );
    // Bob does not follow Alice — his feed is only his own posts.
    let bob_feed = post_bodies(&bob.feed(None, 30).await.unwrap());
    assert_eq!(
        bob_feed,
        vec!["followers only".to_string(), "public hello".into()]
    );

    // Comment + react; Bob (author) gets pushed both.
    let comment_id = alice.comment_create(&fol_post, "nice porch").await.unwrap();
    alice.react(&fol_post, "like").await.unwrap();
    let mut kinds = Vec::new();
    for _ in 0..2 {
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), bob_stream.recv())
            .await
            .expect("notif in time")
            .expect("stream open");
        kinds.push(n["kind"].as_str().unwrap_or_default().to_string());
    }
    kinds.sort();
    assert_eq!(kinds, vec!["comment".to_string(), "reaction".to_string()]);

    let cs = bob.comments(&fol_post).await.unwrap();
    assert_eq!(cs["comments"][0]["body"].as_str(), Some("nice porch"));
    let feed = alice.feed(None, 30).await.unwrap();
    let fol = feed["posts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["post_id"].as_str() == Some(fol_post.as_str()))
        .expect("followers post in feed");
    assert_eq!(fol["reactions"].as_i64(), Some(1));
    assert_eq!(fol["my_reaction"].as_str(), Some("like"));

    // Deleting a post must also delete reactions attached to its comments.
    // Otherwise the comment FK blocks deletion after post reactions are gone.
    alice.comment_react(&comment_id, "like").await.unwrap();
    bob.post_delete(&fol_post).await.unwrap();
    assert!(alice.post_get(&fol_post).await.is_err());

    // Comment deletion removes reactions and detaches one-level replies.
    let disposable_comment = alice
        .comment_create(&pub_post, "temporary comment")
        .await
        .unwrap();
    bob.comment_react(&disposable_comment, "like")
        .await
        .unwrap();
    let detached_reply = bob
        .comment_reply(&pub_post, &disposable_comment, "surviving reply")
        .await
        .unwrap();
    alice.comment_delete(&disposable_comment).await.unwrap();
    let remaining_comments = bob.comments(&pub_post).await.unwrap();
    let remaining_comments = remaining_comments["comments"].as_array().unwrap();
    assert_eq!(remaining_comments.len(), 1);
    assert_eq!(remaining_comments[0]["comment_id"], detached_reply);
    assert!(remaining_comments[0]["parent_id"].is_null());
    bob.post_delete(&pub_post).await.unwrap();
    assert_eq!(
        post_bodies(&alice.feed(None, 30).await.unwrap()),
        vec!["alice's post".to_string()]
    );

    // Report intake.
    let report_target = bob
        .post_create("text", "report target", &[], "public")
        .await
        .unwrap();
    alice
        .report("post", &report_target, "testing the report lane")
        .await
        .unwrap();

    // Block: severs the graph and hides everything, silently. Alice keeps
    // only her own post.
    bob.block("alice").await.unwrap();
    assert_eq!(
        post_bodies(&alice.feed(None, 30).await.unwrap()),
        vec!["alice's post"]
    );
    let e = alice
        .profile_get("bob smith")
        .await
        .expect_err("blocked profile hidden");
    assert!(e.to_string().contains("no such account"), "got: {e:#}");
    let e = alice
        .follow_request("bob smith")
        .await
        .expect_err("cannot re-request");
    assert!(e.to_string().contains("no such account"), "got: {e:#}");

    handle.shutdown();
}

#[tokio::test]
async fn circle_posts_are_membership_gated_and_rotation_locks_out() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, alice_id) = user(&base, "alice").await;
    let (bob, bob_id) = user(&base, "bob").await;
    let (carol, carol_id) = user(&base, "carol").await;

    // Bob and Carol both follow Alice (accepted) so circle posts can even
    // reach their feeds — circles gate on top of the graph.
    bob.follow_request("alice").await.unwrap();
    carol.follow_request("alice").await.unwrap();
    alice.follow_accept("bob").await.unwrap();
    alice.follow_accept("carol").await.unwrap();

    // Alice creates a Fold with only herself active. Membership invitations
    // are pending until the recipient accepts.
    let circle_id = alice
        .circle_create(
            "family",
            json!({ alice_id.clone(): {"alice-device":"wrapped-for-alice"} }),
        )
        .await
        .unwrap();
    alice
        .fold_invite(&circle_id, &bob_id, json!({"bob-device":"wrapped-for-bob"}))
        .await
        .unwrap();
    alice
        .fold_invite(&circle_id, &carol_id, json!({"carol-device":"wrapped-for-carol"}))
        .await
        .unwrap();
    assert_eq!(bob.circles().await.unwrap()["invited"][0]["name"], "family");
    assert!(bob.fold_feed(&circle_id, None, 30).await.is_err());
    bob.fold_accept(&circle_id).await.unwrap();
    carol.fold_decline(&circle_id).await.unwrap();
    let bob_folds = bob.circles().await.unwrap();
    assert_eq!(bob_folds["member"][0]["wrapped_key"]["bob-device"], "wrapped-for-bob");
    assert_eq!(bob_folds["member"][0]["members"].as_array().unwrap().len(), 2);
    let alice_folds = alice.circles().await.unwrap();
    assert!(alice_folds["own"][0]["pending"].as_array().unwrap().is_empty());

    // Every active member may publish ciphertext. Fold posts stay out of Home
    // and are visible only through the Fold feed.
    let envelope = json!({"v":1,"epoch":1,"nonce":"nonce","ciphertext":"ciphertext"}).to_string();
    let fold_post = bob
        .fold_post_create(&envelope, &circle_id, 1)
        .await
        .unwrap();
    assert!(post_bodies(&bob.feed(None, 30).await.unwrap()).is_empty());
    let alice_fold_feed = alice.fold_feed(&circle_id, None, 30).await.unwrap();
    assert_eq!(alice_fold_feed["posts"][0]["body"], envelope);
    let alice_notifications = alice.notifications(None, 50).await.unwrap();
    let fold_post_alert = alice_notifications["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|notification| notification["kind"] == "fold_post")
        .expect("Fold post notification");
    assert_eq!(fold_post_alert["fold_id"], circle_id);
    bob.fold_comment_create(&fold_post, &envelope, 1).await.unwrap();
    assert_eq!(alice.comments(&fold_post).await.unwrap()["comments"][0]["body"], envelope);
    alice.fold_comment_create(&fold_post, &envelope, 1).await.unwrap();
    alice.react(&fold_post, "❤️").await.unwrap();
    let bob_notifications = bob.notifications(None, 50).await.unwrap();
    for kind in ["comment", "reaction"] {
        let alert = bob_notifications["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notification| notification["kind"] == kind)
            .unwrap_or_else(|| panic!("Fold {kind} notification"));
        assert_eq!(alert["fold_id"], circle_id);
    }
    assert!(alice.report("post", &fold_post, "review this").await.is_err());
    alice
        .report_with_copy("post", &fold_post, "review this", "decrypted reporter copy")
        .await
        .unwrap();
    let removable = bob
        .fold_post_create(&envelope, &circle_id, 1)
        .await
        .unwrap();
    alice.post_delete(&removable).await.unwrap();
    assert_eq!(alice.fold_feed(&circle_id, None, 30).await.unwrap()["posts"].as_array().unwrap().len(), 1);

    // A stranger outside the Fold sees nothing.
    let (dave, dave_id) = user(&base, "dave").await;
    dave.follow_request("alice").await.unwrap();
    alice.follow_accept("dave").await.unwrap();
    assert!(dave.fold_feed(&circle_id, None, 30).await.is_err());
    let _ = dave_id;

    // Leaving immediately removes access and marks the key stale, blocking new
    // content until the owner rotates to a new epoch.
    bob.fold_leave(&circle_id).await.unwrap();
    assert!(bob.fold_feed(&circle_id, None, 30).await.is_err());
    let epoch_two = json!({"v":1,"epoch":2,"nonce":"next","ciphertext":"next"}).to_string();
    assert!(alice.fold_post_create(&epoch_two, &circle_id, 1).await.is_err());
    alice
        .circle_set_keys(&circle_id, 1, json!({ alice_id.clone(): {"alice-device":"wrapped-for-alice-v2"} }))
        .await
        .unwrap();
    alice.fold_post_create(&epoch_two, &circle_id, 2).await.unwrap();

    // Only the owner can rotate.
    let e = bob
        .circle_set_keys(&circle_id, 2, json!({}))
        .await
        .expect_err("non-owner rotation must fail");
    assert!(e.to_string().contains("not yours"), "got: {e:#}");

    // Deleting the Fold deletes its posts (real deletion).
    alice.circle_delete(&circle_id).await.unwrap();
    assert!(post_bodies(&bob.feed(None, 30).await.unwrap()).is_empty());

    handle.shutdown();
}

#[tokio::test]
async fn blocked_fold_co_member_cannot_read_key_directory() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, alice_id) = user(&base, "key-alice").await;
    let (bob, bob_id) = user(&base, "key-bob").await;

    bob.follow_request("key-alice").await.unwrap();
    alice.follow_accept("key-bob").await.unwrap();
    let circle_id = alice
        .circle_create(
            "private",
            json!({ alice_id: {"alice-device":"wrapped-for-alice"} }),
        )
        .await
        .unwrap();
    alice
        .fold_invite(&circle_id, &bob_id, json!({"bob-device":"wrapped-for-bob"}))
        .await
        .unwrap();
    bob.fold_accept(&circle_id).await.unwrap();

    let directory = bob.fold_key_directory("key-alice").await.unwrap();
    assert!(directory["identity_pub"].as_str().is_some());

    alice.block("key-bob").await.unwrap();
    let error = bob
        .fold_key_directory("key-alice")
        .await
        .expect_err("a block must override active Fold co-membership");
    assert!(
        error.to_string().contains("account not found"),
        "got: {error:#}"
    );

    handle.shutdown();
}

#[tokio::test]
async fn feed_pagination_cursor_walks_history() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, _) = user(&base, "alice").await;

    for i in 0..7 {
        alice
            .post_create("text", &format!("post {i}"), &[], "public")
            .await
            .unwrap();
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = alice.feed(cursor.as_deref(), 3).await.unwrap();
        let bodies = post_bodies(&page);
        if bodies.is_empty() {
            break;
        }
        seen.extend(bodies);
        cursor = page["next_cursor"].as_str().map(String::from);
    }
    let want: Vec<String> = (0..7).rev().map(|i| format!("post {i}")).collect();
    assert_eq!(seen, want);

    handle.shutdown();
}

#[tokio::test]
async fn rich_posts_search_save_pin_and_revision_history() {
    let (base, handle, _dir) = spawn_server().await;
    let (alice, _) = user(&base, "rich-alice").await;
    let (bob, _) = user(&base, "rich-bob").await;
    let video_blob = alice.blob_upload(b"test mp4 payload", true).await.unwrap();
    let post_id = alice
        .post_create_rich(
            "video",
            "Garden clarity #weekend",
            std::slice::from_ref(&video_blob),
            &["video/mp4".to_string()],
            "public",
            "A clear garden video",
            "Flashing sunlight",
        )
        .await
        .unwrap();

    let search = bob.post_search("#weekend", 10).await.unwrap();
    let post = &search["posts"][0];
    assert_eq!(post["kind"], "video");
    assert_eq!(post["media_types"][0], "video/mp4");
    assert_eq!(post["alt_text"], "A clear garden video");
    assert_eq!(post["content_warning"], "Flashing sunlight");

    bob.post_save(&post_id).await.unwrap();
    assert_eq!(bob.saved_posts().await.unwrap()["posts"][0]["saved"], true);
    bob.post_unsave(&post_id).await.unwrap();
    assert!(bob.saved_posts().await.unwrap()["posts"]
        .as_array()
        .unwrap()
        .is_empty());

    alice.post_pin(&post_id, true).await.unwrap();
    assert_eq!(
        alice.author_posts("rich-alice", None, 10).await.unwrap()["posts"][0]["pinned"],
        true
    );
    assert!(bob.post_pin(&post_id, true).await.is_err());
    alice
        .post_edit(&post_id, "Updated garden clarity")
        .await
        .unwrap();
    let revisions = bob.post_revisions(&post_id).await.unwrap();
    assert_eq!(revisions["revisions"][0]["body"], "Garden clarity #weekend");

    alice
        .post_create("text", "private searchable phrase", &[], "followers")
        .await
        .unwrap();
    assert!(
        bob.post_search("searchable phrase", 10).await.unwrap()["posts"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    handle.shutdown();
}

#[tokio::test]
async fn search_notifications_media_and_device_link() {
    let (base, handle, _dir) = spawn_server().await;

    // Alice registered by hand — the link test needs her identity key.
    let alice = hive_client::Client::new(&base, true).unwrap();
    alice.info().await.unwrap();
    let alice_identity = hive_client::generate_key();
    let alice_dev1 = hive_client::generate_key();
    let (alice_id, _) = alice
        .register(&alice_identity, &alice_dev1, "alice", "desktop", None)
        .await
        .unwrap();
    alice.auth(&alice_id, &alice_dev1).await.unwrap();
    let (bob, _bob_id) = user(&base, "bob").await;
    let (carol, _carol_id) = user(&base, "carol").await;

    // --- search: substring on handle/display name, no self, no wildcards.
    alice.profile_set("Alice Q", "", None).await.unwrap();
    let hits = bob.search("ali").await.unwrap();
    let names: Vec<&str> = hits["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["handle"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alice"]);
    let hits = alice.search("ali").await.unwrap();
    assert!(
        hits["results"].as_array().unwrap().is_empty(),
        "no self in search"
    );
    let hits = bob.search("%").await.unwrap();
    assert!(
        hits["results"].as_array().unwrap().is_empty(),
        "wildcards are literal"
    );

    // --- notifications persist (not just live frames) and mark seen.
    bob.follow_request("alice").await.unwrap();
    let inbox = alice.notifications(None, 50).await.unwrap();
    assert_eq!(inbox["unseen"].as_i64(), Some(1));
    let first = &inbox["notifications"][0];
    assert_eq!(first["kind"].as_str(), Some("follow_request"));
    assert_eq!(first["from"]["handle"].as_str(), Some("bob"));
    alice.follow_accept("bob").await.unwrap();
    alice.notifications_seen().await.unwrap();
    let inbox = alice.notifications(None, 50).await.unwrap();
    assert_eq!(inbox["unseen"].as_i64(), Some(0));
    let inbox = bob.notifications(None, 50).await.unwrap();
    assert_eq!(
        inbox["notifications"][0]["kind"].as_str(),
        Some("follow_accepted")
    );

    // --- media: followers-audience media readable by followers only;
    // avatars are public like the profile that wears them.
    let photo = b"not really a jpeg".to_vec();
    let blob_id = alice.blob_upload(&photo, false).await.unwrap();
    alice
        .post_create("photo", "porch view", &[blob_id.clone()], "followers")
        .await
        .unwrap();
    assert_eq!(bob.media_fetch(&blob_id).await.unwrap(), photo);
    assert!(
        carol.media_fetch(&blob_id).await.is_err(),
        "stranger blocked from followers media"
    );
    let avatar = alice.blob_upload(b"avatar bytes", false).await.unwrap();
    alice
        .profile_set("Alice Q", "", Some(&avatar))
        .await
        .unwrap();
    assert_eq!(
        carol.media_fetch(&avatar).await.unwrap(),
        b"avatar bytes".to_vec()
    );

    // --- device link (§4.2): a NEW device joins alice's account without
    // her identity key ever moving.
    let phone = hive_client::Client::new(&base, true).unwrap();
    phone.info().await.unwrap();
    let phone_key = hive_client::generate_key();
    let code = phone.link_begin(&phone_key, "phone").await.unwrap();
    assert!(
        phone.link_status(&code).await.unwrap().is_none(),
        "not approved yet"
    );
    alice.link_approve(&alice_identity, &code).await.unwrap();
    let linked = phone.link_status(&code).await.unwrap().expect("approved");
    assert_eq!(linked, alice_id);
    phone.auth(&linked, &phone_key).await.unwrap();
    let who = phone.whoami().await.unwrap();
    assert_eq!(who["account_id"].as_str(), Some(alice_id.as_str()));

    // A bogus code goes nowhere.
    assert!(alice
        .link_approve(&alice_identity, "WRONGCOD")
        .await
        .is_err());

    handle.shutdown();
}
