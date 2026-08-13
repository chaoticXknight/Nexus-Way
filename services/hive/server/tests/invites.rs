// Owns non-development coverage for invite, founder-bootstrap, and rate-limit policy.
// server/src/admin.rs mints invites; identity.rs consumes them during registration.

// Prod-posture tests: invite gate + minting (§1.4/§6.6) and rate limiting.
// These boot a NON-dev server, unlike every other test file, so the invite
// requirement and rate limiter are live.

use axum_server::Handle;
use nexus_way_hive::Config;

async fn spawn_prod_server() -> (String, Handle, tempfile::TempDir) {
    spawn_prod_server_with(|_| {}).await
}

async fn spawn_prod_server_with(
    configure: impl FnOnce(&mut Config),
) -> (String, Handle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    // Prod refuses to boot without a TLS cert (no plaintext listener), so
    // mint a self-signed one — same as a real deployment's PEM pair.
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
        .expect("self-signed cert");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();
    std::fs::write(&key_path, ck.key_pair.serialize_pem()).unwrap();
    let mut cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().to_path_buf(),
        dev: false,
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
        site_access_code: Some("site-code".into()),
        ..Config::default()
    };
    configure(&mut cfg);
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
async fn static_html_prevents_cross_origin_framing() {
    let (base, handle, _dir) = spawn_prod_server_with(|cfg| {
        let web_dir = cfg.data_dir.join("website");
        std::fs::create_dir_all(&web_dir).unwrap();
        std::fs::write(
            web_dir.join("index.html"),
            "<!doctype html><title>HIVE</title>",
        )
        .unwrap();
        cfg.web_dir = Some(web_dir);
    })
    .await;
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let response = http
        .get(format!("{base}/?access=site-code"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let csp = response
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let x_frame_options = response
        .headers()
        .get("x-frame-options")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        csp.contains("frame-ancestors 'none'") || x_frame_options.eq_ignore_ascii_case("DENY"),
        "HTML must reject framing; CSP={csp:?}, X-Frame-Options={x_frame_options:?}"
    );

    handle.shutdown();
}

#[tokio::test]
async fn invite_gate_mint_and_redeem() {
    let (base, handle, _dir) = spawn_prod_server().await;

    // Founder bootstraps without an invite.
    let founder = client(&base).await;
    let f_identity = hive_client::generate_key();
    let f_device = hive_client::generate_key();
    let (f_account, _) = founder
        .register(&f_identity, &f_device, "founder", "workstation", None)
        .await
        .unwrap();
    founder.auth(&f_account, &f_device).await.unwrap();

    // Second registration without an invite is refused.
    let guest = client(&base).await;
    let g_identity = hive_client::generate_key();
    let g_device = hive_client::generate_key();
    // Founder mints codes; a non-founder cannot.
    let codes = founder.admin_invite_create(2).await.unwrap();
    assert_eq!(codes.len(), 2);

    let list = founder.admin_invite_list().await.unwrap();
    let first = list["invites"].as_array().unwrap()[0].clone();
    let link = first["link"].as_str().expect("invite link");
    assert!(link.contains("/signup.html?"), "got {link}");
    assert!(link.contains("access=site-code"), "got {link}");
    assert!(link.contains("invite="), "got {link}");

    // Redeem: registration with a fresh code succeeds.
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

    // Non-founder cannot mint or list.
    assert!(guest.admin_invite_create(1).await.is_err());
    assert!(guest.admin_invite_list().await.is_err());
    assert!(guest.community_invite_create(1).await.is_err());

    // Founder can grant narrow Community Steward access without granting any
    // Console/admin authority. Steward invites are issuer-scoped.
    founder
        .admin_community_role("guest", "steward")
        .await
        .unwrap();
    assert_eq!(guest.whoami().await.unwrap()["community_role"], "steward");
    let steward_codes = guest.community_invite_create(1).await.unwrap();
    assert_eq!(steward_codes.len(), 1);
    let steward_list = guest.community_invite_list().await.unwrap();
    let steward_invites = steward_list["invites"].as_array().unwrap();
    assert_eq!(steward_invites.len(), 1);
    assert_eq!(steward_invites[0]["code"], steward_codes[0]);
    assert!(guest.admin_accounts(None, None).await.is_err());
    assert!(guest.admin_reports(false).await.is_err());
    assert!(guest.community_invite_revoke(&codes[1]).await.is_err());

    let steward_friend = client(&base).await;
    let sf_identity = hive_client::generate_key();
    let sf_device = hive_client::generate_key();
    let (sf_account, _) = steward_friend
        .register(
            &sf_identity,
            &sf_device,
            "steward-friend",
            "phone",
            Some(steward_codes[0].as_str()),
        )
        .await
        .unwrap();
    steward_friend.auth(&sf_account, &sf_device).await.unwrap();
    let steward_list = guest.community_invite_list().await.unwrap();
    assert_eq!(steward_list["invites"][0]["used_handle"], "steward-friend");

    // Rejected registration cases run after valid onboarding so the production
    // fixed-window limiter does not mask the Steward redemption path.
    let third = client(&base).await;
    let t_identity = hive_client::generate_key();
    let t_device = hive_client::generate_key();
    let refused = third
        .register(&t_identity, &t_device, "third", "tablet", None)
        .await;
    assert!(refused.is_err(), "prod register without invite must fail");
    let refused = third
        .register(
            &t_identity,
            &t_device,
            "third",
            "tablet",
            Some("deadbeefdeadbeefdeadbeef"),
        )
        .await;
    assert!(refused.is_err(), "bogus invite must fail");
    let refused = third
        .register(
            &t_identity,
            &t_device,
            "third",
            "tablet",
            Some(codes[0].as_str()),
        )
        .await;
    assert!(refused.is_err(), "used invite must fail");

    founder
        .admin_community_role("guest", "member")
        .await
        .unwrap();
    assert!(guest.community_invite_create(1).await.is_err());

    // Invite list shows redemption status.
    let list = founder.admin_invite_list().await.unwrap();
    let invites = list["invites"].as_array().unwrap();
    assert_eq!(invites.len(), 3);
    let used: Vec<_> = invites.iter().filter(|i| !i["used_by"].is_null()).collect();
    assert_eq!(used.len(), 2);
    assert!(used.iter().any(|invite| invite["used_handle"] == "guest"));
    assert!(used
        .iter()
        .any(|invite| invite["used_handle"] == "steward-friend"));

    // Revoke the unused code; a revoked code cannot register.
    let unused = invites.iter().find(|i| i["used_by"].is_null()).unwrap();
    let unused_code = unused["code"].as_str().unwrap();
    founder.admin_invite_revoke(unused_code).await.unwrap();
    let refused = third
        .register(&t_identity, &t_device, "third", "tablet", Some(unused_code))
        .await;
    assert!(refused.is_err(), "revoked invite must fail");

    handle.shutdown();
}

#[tokio::test]
async fn founder_access_code_guards_clean_server_bootstrap() {
    let (base, handle, _dir) = spawn_prod_server_with(|cfg| {
        cfg.founder_access_code = Some("founder-only".into());
    })
    .await;

    let founder = client(&base).await;
    let identity = hive_client::generate_key();
    let device = hive_client::generate_key();

    let refused = founder
        .register(&identity, &device, "founder", "workstation", None)
        .await;
    assert!(refused.is_err(), "clean server must require founder code");

    let refused = founder
        .register(&identity, &device, "founder", "workstation", Some("wrong"))
        .await;
    assert!(refused.is_err(), "wrong founder code must fail");

    let (account, _) = founder
        .register(
            &identity,
            &device,
            "founder",
            "workstation",
            Some("founder-only"),
        )
        .await
        .unwrap();
    founder.auth(&account, &device).await.unwrap();

    handle.shutdown();
}

#[tokio::test]
async fn register_rate_limit_applies_in_prod() {
    let (base, handle, _dir) = spawn_prod_server().await;
    let c = client(&base).await;

    // 5 register attempts per hour per IP: burn the budget with invalid
    // (invite-less) attempts, then confirm the limiter takes over.
    for _ in 0..5 {
        let id = hive_client::generate_key();
        let dev = hive_client::generate_key();
        let _ = c.register(&id, &dev, "x", "d", None).await; // "invite required"
    }
    let id = hive_client::generate_key();
    let dev = hive_client::generate_key();
    let e = c.register(&id, &dev, "x", "d", None).await.unwrap_err();
    assert!(
        e.to_string().contains("rate limited"),
        "expected rate limit, got: {e}"
    );

    handle.shutdown();
}
