// SPDX-License-Identifier: Apache-2.0

//! D-AVATAR lifecycle integration test — drives the full
//! mould → incarnate → avatar → status → audit → grant flow
//! through cs-thin against an in-process rpp-adapter.

use std::sync::Arc;
use std::time::Duration;

use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_thin_cli::cli::{
    run_with, AvatarArgs, AvatarAuditArgs, AvatarGrantArgs, AvatarIncarnateArgs,
    AvatarMouldInfoArgs, AvatarStatusArgs, AvatarSub, Cli, Command,
};
use serde_json::Value;

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<(&str, &str, &str, &str)>,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let mut builder = HabilitationMap::builder();
    for (sub, nucleon, noyau, audience) in nucleons {
        builder = builder.insert(
            oidc.issuer(),
            sub,
            HabilitationId::new(nucleon),
            Noyau::new(noyau),
            audience,
        );
    }

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: cosmon_oidc_testkit::fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        artifact_root: security_dir.join("artifacts"),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(builder.build()),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
        auth_claude: None,
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
        events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
    }
}

async fn spawn_adapter(state: AppState) -> String {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

fn write_jwt_to_temp(jwt: &str) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("jwt.txt");
    std::fs::write(&p, jwt).unwrap();
    Box::leak(Box::new(dir));
    p
}

fn avatar_cli(base_url: &str, jwt_path: &std::path::Path, sub: AvatarSub) -> Cli {
    Cli {
        base_url: Some(base_url.to_owned()),
        jwt_from_env: None,
        jwt_file: Some(jwt_path.to_owned()),
        coverage_report: false,
        json: false,
        command: Some(Command::Avatar(AvatarArgs { sub })),
    }
}

#[tokio::test]
async fn mould_status_before_incarnation() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:world:observe"],
        lifetime_secs: Some(60),
        jti: Some("jti-avatar-status-mould"),
    });
    let jwt_path = write_jwt_to_temp(&jwt);

    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Status(AvatarStatusArgs {
            instance_id: "test-instance-001".to_owned(),
        }),
    );

    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("avatar status should succeed");
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["state"], "mould");
    assert_eq!(body["instance_id"], "test-instance-001");
}

#[tokio::test]
async fn mould_info_returns_ready() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:world:observe"],
        lifetime_secs: Some(60),
        jti: Some("jti-avatar-mould-info"),
    });
    let jwt_path = write_jwt_to_temp(&jwt);

    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::MouldInfo(AvatarMouldInfoArgs {
            instance_id: "test-instance-002".to_owned(),
        }),
    );

    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("avatar mould-info should succeed");
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["state"], "mould");
    assert_eq!(body["instance_id"], "test-instance-002");
    assert_eq!(body["ready_for_incarnation"], true);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn full_lifecycle_incarnate_then_status_then_audit_then_grant() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:pilote:converse", "cosmon:world:observe"],
        lifetime_secs: Some(60),
        jti: Some("jti-avatar-lifecycle"),
    });
    let jwt_path = write_jwt_to_temp(&jwt);
    let instance_id = "lifecycle-test-001";

    // 1. Status before incarnation → mould
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Status(AvatarStatusArgs {
            instance_id: instance_id.to_owned(),
        }),
    );
    let mut out = Vec::new();
    run_with(cli, &mut out).await.unwrap();
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["state"], "mould");

    // 2. Incarnate
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Incarnate(AvatarIncarnateArgs {
            instance_id: instance_id.to_owned(),
            pilote: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_owned(),
            tenant: "democorp-internal".to_owned(),
            juridiction: "FR".to_owned(),
        }),
    );
    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("incarnate should succeed");
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["instance_id"], instance_id);
    assert_eq!(
        body["pilote_id"],
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"
    );
    assert_eq!(body["juridiction"], "FR");
    assert_eq!(body["tenant_id"], "democorp-internal");
    assert!(body["cicatrice"].as_str().unwrap().len() == 64);
    assert!(body["incarnated_at"].is_string());

    // 3. Status after incarnation → avatar
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Status(AvatarStatusArgs {
            instance_id: instance_id.to_owned(),
        }),
    );
    let mut out = Vec::new();
    run_with(cli, &mut out).await.unwrap();
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["state"], "avatar");
    assert_eq!(body["instance_id"], instance_id);
    assert!(body["cicatrice"].as_str().unwrap().len() == 64);
    assert_eq!(
        body["pilote_id"],
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"
    );
    assert_eq!(body["juridiction"], "FR");

    // 4. Audit — should show incarnation event
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Audit(AvatarAuditArgs {
            instance_id: instance_id.to_owned(),
        }),
    );
    let mut out = Vec::new();
    run_with(cli, &mut out).await.expect("audit should succeed");
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["state"], "avatar");
    assert_eq!(body["instance_id"], instance_id);
    assert!(body["cicatrice"].as_str().unwrap().len() == 64);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "incarnation_at");

    // 5. Grant canal b
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Grant(AvatarGrantArgs {
            instance_id: instance_id.to_owned(),
            canal: "b".to_owned(),
            target: "did:key:z6MktargetPiloteXYZ".to_owned(),
        }),
    );
    let mut out = Vec::new();
    run_with(cli, &mut out).await.expect("grant should succeed");
    let body: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
    assert_eq!(body["instance_id"], instance_id);
    assert_eq!(body["canal"], "b");
    assert_eq!(body["target"], "did:key:z6MktargetPiloteXYZ");
    assert_eq!(body["granted"], true);

    // 6. Double incarnation should fail
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Incarnate(AvatarIncarnateArgs {
            instance_id: instance_id.to_owned(),
            pilote: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_owned(),
            tenant: "democorp-internal".to_owned(),
            juridiction: "FR".to_owned(),
        }),
    );
    let mut out = Vec::new();
    let result = run_with(cli, &mut out).await;
    assert!(result.is_err(), "double incarnation must fail");
}

#[tokio::test]
async fn grant_on_mould_fails() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:pilote:converse"],
        lifetime_secs: Some(60),
        jti: Some("jti-avatar-grant-mould"),
    });
    let jwt_path = write_jwt_to_temp(&jwt);

    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Grant(AvatarGrantArgs {
            instance_id: "nonexistent-instance".to_owned(),
            canal: "b".to_owned(),
            target: "did:key:z6Mksomeone".to_owned(),
        }),
    );
    let mut out = Vec::new();
    let result = run_with(cli, &mut out).await;
    assert!(result.is_err(), "grant on non-existent instance must fail");
}

#[tokio::test]
async fn invalid_canal_rejected() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:pilote:converse", "cosmon:world:observe"],
        lifetime_secs: Some(60),
        jti: Some("jti-avatar-invalid-canal"),
    });
    let jwt_path = write_jwt_to_temp(&jwt);
    let instance_id = "canal-test-001";

    // Incarnate first
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Incarnate(AvatarIncarnateArgs {
            instance_id: instance_id.to_owned(),
            pilote: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_owned(),
            tenant: "democorp-internal".to_owned(),
            juridiction: "FR".to_owned(),
        }),
    );
    let mut out = Vec::new();
    run_with(cli, &mut out).await.unwrap();

    // Try invalid canal "x"
    let cli = avatar_cli(
        &base_url,
        &jwt_path,
        AvatarSub::Grant(AvatarGrantArgs {
            instance_id: instance_id.to_owned(),
            canal: "x".to_owned(),
            target: "did:key:z6Mksomeone".to_owned(),
        }),
    );
    let mut out = Vec::new();
    let result = run_with(cli, &mut out).await;
    assert!(result.is_err(), "invalid canal must be rejected");
}
