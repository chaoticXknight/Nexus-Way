// Owns production-posture coverage for founder Console account, dossier, and audit endpoints.
// server/src/admin.rs owns these operations; identity.rs owns founder authentication.

// Console read-endpoint tests: /v1/admin/accounts, /v1/admin/dossier,
// /v1/admin/audit. Prod posture so the founder gate is live.

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
async fn console_accounts_dossier_audit() {
    let (base, handle, _dir) = spawn_prod_server().await;

    // Founder bootstraps; invites a guest.
    let founder = client(&base).await;
    let f_identity = hive_client::generate_key();
    let f_device = hive_client::generate_key();
    let (f_account, _) = founder
        .register(&f_identity, &f_device, "founder", "console", None)
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
            "phone",
            Some(codes[0].as_str()),
        )
        .await
        .unwrap();
    guest.auth(&g_account, &g_device).await.unwrap();

    // Roster: both accounts, guest shows its inviter.
    let v = founder.admin_accounts(None, None).await.unwrap();
    let accounts = v["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    let g_row = accounts.iter().find(|a| a["handle"] == "guest").unwrap();
    assert_eq!(g_row["invited_by"], "founder");
    assert_eq!(g_row["status"], "active");
    assert_eq!(g_row["devices"].as_i64(), Some(1));

    // Handle filter narrows the roster.
    let v = founder.admin_accounts(Some("gue"), None).await.unwrap();
    assert_eq!(v["accounts"].as_array().unwrap().len(), 1);

    // Dossier: lineage in both directions + a report row.
    guest
        .report("account", "founder", "spam: test report")
        .await
        .unwrap();
    let reports = founder.admin_reports(false).await.unwrap();
    let report_id = reports["reports"][0]["id"].as_i64().unwrap();
    let error = founder
        .admin_report_resolve(report_id, "  ")
        .await
        .expect_err("empty resolution must fail");
    assert!(error.to_string().contains("resolution must be"));
    let d = founder.admin_dossier("guest").await.unwrap();
    assert_eq!(d["account"]["handle"], "guest");
    assert_eq!(d["invited_by"]["handle"], "founder");
    assert_eq!(d["reports_filed"].as_array().unwrap().len(), 1);
    let d = founder.admin_dossier("founder").await.unwrap();
    let redeemed = &d["invited"].as_array().unwrap()[0];
    assert_eq!(redeemed["handle"], "guest");
    assert_eq!(redeemed["account_id"], g_account);
    assert_eq!(redeemed["code"].as_str(), Some(codes[0].as_str()));
    assert!(redeemed["created"].as_i64().is_some());
    assert!(redeemed["used_at"].as_i64().is_some());
    assert_eq!(d["reports_against"].as_array().unwrap().len(), 1);

    // Community Steward is a narrow Connect role, visible in Console metadata
    // but not an administrator role.
    founder
        .admin_community_role("guest", "steward")
        .await
        .unwrap();
    let roster = founder.admin_accounts(Some("guest"), None).await.unwrap();
    assert_eq!(roster["accounts"][0]["community_role"], "steward");
    let dossier = founder.admin_dossier("guest").await.unwrap();
    assert_eq!(dossier["account"]["community_role"], "steward");
    assert!(guest.admin_accounts(None, None).await.is_err());

    // Membership tiers affect identity presentation only and do not grant
    // administrative access.
    founder
        .admin_membership_tier("guest", "paid")
        .await
        .unwrap();
    assert_eq!(guest.whoami().await.unwrap()["membership_tier"], "paid");
    assert_eq!(guest.profile_get("guest").await.unwrap()["membership_tier"], "paid");
    let dossier = founder.admin_dossier("guest").await.unwrap();
    assert_eq!(dossier["account"]["membership_tier"], "paid");
    assert!(guest.admin_membership_tier("guest", "beta").await.is_err());
    founder
        .admin_membership_tier("guest", "beta")
        .await
        .unwrap();
    founder
        .admin_community_role("guest", "member")
        .await
        .unwrap();

    // Support is a two-way account-owned thread. Console can reply and close;
    // another user or non-founder cannot inspect or administer it.
    let support_id = guest
        .support_open("bug", "Feed jumps", "The feed jumps after refresh")
        .await
        .unwrap();
    guest
        .support_send(&support_id, "It happens on every refresh")
        .await
        .unwrap();
    let user_threads = guest.support_threads().await.unwrap();
    assert_eq!(user_threads["threads"][0]["messages"].as_array().unwrap().len(), 2);
    let admin_threads = founder.admin_support_threads().await.unwrap();
    assert_eq!(admin_threads["threads"][0]["handle"], "guest");
    assert_eq!(admin_threads["threads"][0]["unread"], 2);
    assert!(guest.admin_support_threads().await.is_err());
    founder
        .admin_support_reply(&support_id, "Thanks, this is queued for a fix")
        .await
        .unwrap();
    let user_threads = guest.support_threads().await.unwrap();
    assert_eq!(user_threads["threads"][0]["messages"][2]["sender_role"], "admin");
    founder
        .admin_support_status(&support_id, "closed")
        .await
        .unwrap();
    assert!(guest.support_send(&support_id, "One more thing").await.is_err());
    assert!(guest.admin_support_delete(&support_id, "test").await.is_err());
    founder
        .admin_support_delete(&support_id, "test")
        .await
        .unwrap();
    assert!(guest.support_threads().await.unwrap()["threads"]
        .as_array()
        .unwrap()
        .is_empty());
    let audit = founder
        .admin_audit(None, Some("support_delete"), 10)
        .await
        .unwrap();
    assert_eq!(audit["audit"][0]["action"], "support_delete");
    assert!(audit["audit"][0]["subject"]
        .as_str()
        .unwrap()
        .contains("reason=test"));

    // A posting mute is graduated enforcement: the account stays signed in
    // and can read, but cannot create posts or comments until unmuted.
    let founder_post = founder
        .post_create("text", "moderation test host", &[], "public")
        .await
        .unwrap();
    let muted_until = founder.admin_mute("guest", 24, "cool-down").await.unwrap();
    assert!(muted_until > 0);
    assert_eq!(guest.whoami().await.unwrap()["handle"], "guest");
    let error = guest
        .post_create("text", "blocked post", &[], "public")
        .await
        .expect_err("muted account must not post");
    assert!(error.to_string().contains("muted from posting"));
    let error = guest
        .comment_create(&founder_post, "blocked comment")
        .await
        .expect_err("muted account must not comment");
    assert!(error.to_string().contains("muted from commenting"));
    let roster = founder.admin_accounts(Some("guest"), None).await.unwrap();
    assert_eq!(roster["accounts"][0]["muted_until"], muted_until);
    let dossier = founder.admin_dossier("guest").await.unwrap();
    assert_eq!(dossier["account"]["muted_until"], muted_until);
    assert!(founder
        .admin_mute("guest", 12, "bad duration")
        .await
        .is_err());
    assert!(founder
        .admin_mute("founder", 24, "not allowed")
        .await
        .is_err());
    founder.admin_unmute("guest").await.unwrap();
    guest
        .post_create("text", "posting restored", &[], "public")
        .await
        .unwrap();

    // Suspend flows through to the roster status filter.
    founder
        .admin_suspend("guest", "console test")
        .await
        .unwrap();
    let v = founder
        .admin_accounts(None, Some("suspended"))
        .await
        .unwrap();
    let suspended = v["accounts"].as_array().unwrap();
    assert_eq!(suspended.len(), 1);
    assert_eq!(suspended[0]["handle"], "guest");
    founder.admin_unsuspend("guest").await.unwrap();

    // Audit log recorded the moderation trail; action filter works.
    let v = founder
        .admin_audit(None, Some("suspend"), 100)
        .await
        .unwrap();
    let rows = v["audit"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["action"] == "suspend"));
    assert!(rows.iter().any(|r| r["action"] == "unsuspend"));
    assert!(rows.iter().all(|r| r["actor_handle"] == "founder"));
    let v = founder.admin_audit(None, Some("mute"), 100).await.unwrap();
    let rows = v["audit"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["action"] == "mute"));
    assert!(rows.iter().any(|r| r["action"] == "unmute"));

    // Non-founder is locked out of every console read endpoint.
    // (guest was unsuspended above, so its session may be gone — re-auth.)
    guest.auth(&g_account, &g_device).await.unwrap();
    assert!(guest.admin_accounts(None, None).await.is_err());
    assert!(guest.admin_dossier("founder").await.is_err());
    assert!(guest.admin_audit(None, None, 10).await.is_err());

    handle.shutdown();
}
