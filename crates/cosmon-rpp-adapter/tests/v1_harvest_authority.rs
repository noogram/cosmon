// SPDX-License-Identifier: AGPL-3.0-only

//! Current-fact challenges and signed-grant transport through the real router.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::harvest_authorization::{DoneAuthorization, HarvestGrant, OperatorHarvestSeal};
use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};
use cosmon_minisign_testkit::Operator;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::{json, Value};
use tower::ServiceExt;

const MOLECULE: &str = "task-20260930-a1b2";

fn state(oidc: &OidcMock, tenants: &TenantWorkspaces, security: &std::path::Path) -> AppState {
    oidc.write_jwks_file(security).unwrap();
    let map = HabilitationMap::builder()
        .insert_with_scopes(
            oidc.issuer(),
            "tenant-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
            vec![
                "cosmon:molecule:read".into(),
                "cosmon:molecule:harvest".into(),
            ],
        )
        .build();
    let provisioner = Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert());
    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security.to_path_buf(),
        inbox_root: security.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(JwksStore::load(security).unwrap()),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(map),
        rate_limiter: Arc::new(IngressRateLimiter::new(
            security.join("oidc-rate-limit"),
            64.0,
            0.0,
        )),
        deny_list: Arc::new(DenyList::new(security.to_path_buf()).with_ttl(Duration::from_secs(0))),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: security.join("artifacts"),
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            security.join("dist"),
        )),
        install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
        events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: provisioner.clone(),
        portee_provisioner: Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::inert()),
    }
}

fn request(method: &str, path: &str, jwt: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {jwt}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 128 * 1024).await.unwrap()).unwrap()
}

fn signed(grant: HarvestGrant, operator: &Operator) -> DoneAuthorization {
    let signature = operator.sign(&grant.canonical_bytes());
    let mut lines = signature.lines();
    let attestation = OperatorAttestation {
        key_id: OperatorKeyId::parse(&operator.key_id_display()).unwrap(),
        untrusted_comment: lines.next().unwrap().replace("untrusted comment: ", ""),
        signature: lines.next().unwrap().to_owned(),
        trusted_comment: lines.next().unwrap().replace("trusted comment: ", ""),
        global_signature: lines.next().unwrap().to_owned(),
    };
    DoneAuthorization::Ratified(OperatorHarvestSeal::new(grant, attestation).unwrap())
}

#[tokio::test]
async fn challenge_uses_current_completed_molecule_facts_and_import_verifies_signature() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    std::fs::create_dir_all(tenant.root.join(".cosmon/state")).unwrap();
    std::fs::write(
        tenant.root.join(".cosmon/config.toml"),
        "[project]\nproject_id = \"harvest-test\"\n[harvest_authority]\nremote = \"sealed\"\n",
    )
    .unwrap();
    tenant
        .insert_molecule(MOLECULE, &json!({"status": "completed"}))
        .unwrap();
    let operator = Operator::from_seed(27);
    std::fs::write(
        tenant.root.join(".cosmon/harvest.pub"),
        operator.public_key_file(),
    )
    .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let app = router(state(&oidc, &tenants, security.path()));
    let jwt = oidc.issue(&IssueJwt {
        subject: "tenant-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some("harvest-issue"),
    });
    let challenge = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/harvest/challenge",
            &jwt,
            json!({"molecule": MOLECULE, "expires_at": "2100-01-01T00:00:00Z"}),
        ))
        .await
        .unwrap();
    let challenge_status = challenge.status();
    let challenge_body = response_json(challenge).await;
    assert_eq!(challenge_status, StatusCode::OK, "{challenge_body}");
    let expected = format!(
        "cosmon-harvest-grant-v1\ngalaxy=harvest-test\nscope=molecule:{MOLECULE}\nbase=main\naction=done\nreservations=none\nepoch=1\nexpires=2100-01-01T00:00:00+00:00\n"
    );
    assert_eq!(challenge_body["canonical"], expected);
    let grant: HarvestGrant = serde_json::from_value(challenge_body["grant"].clone()).unwrap();
    let authorization = signed(grant.clone(), &operator);

    let mut tampered = serde_json::to_value(&authorization).unwrap();
    tampered["grant"]["galaxy"] = json!("other-galaxy");
    let refused = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/harvest/grants",
            &jwt,
            json!({"molecule": MOLECULE, "authorization": tampered}),
        ))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let mut foreign = grant.clone();
    foreign.galaxy = "other-galaxy".to_owned();
    let foreign = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/harvest/grants",
            &jwt,
            json!({"molecule": MOLECULE, "authorization": signed(foreign, &operator)}),
        ))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let dir = tenant.root.join(".cosmon/state/harvest/grants");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = security.path().join("outside-grant.json");
        let outside_bytes = serde_json::to_vec(&authorization).unwrap();
        std::fs::write(&outside, &outside_bytes).unwrap();
        let destination = dir.join(format!("{}.json", grant.fingerprint()));
        symlink(&outside, &destination).unwrap();
        let blocked = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/harvest/grants",
                &jwt,
                json!({"molecule": MOLECULE, "authorization": authorization}),
            ))
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(std::fs::read(&outside).unwrap(), outside_bytes);
        std::fs::remove_file(destination).unwrap();
    }

    let installed = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/harvest/grants",
            &jwt,
            json!({"molecule": MOLECULE, "authorization": authorization}),
        ))
        .await
        .unwrap();
    let installed_status = installed.status();
    let installed_body = response_json(installed).await;
    assert_eq!(installed_status, StatusCode::CREATED, "{installed_body}");
    let fingerprint = installed_body["fingerprint"].as_str().unwrap();
    assert!(tenant
        .root
        .join(".cosmon/state/harvest/grants")
        .join(format!("{fingerprint}.json"))
        .is_file());
    let status = app
        .oneshot(request(
            "GET",
            &format!("/v1/harvest/status?molecule={MOLECULE}"),
            &jwt,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let status = response_json(status).await;
    assert_eq!(status["policy"], "sealed");
    assert_eq!(status["provenance"], "explicit");
    assert_eq!(status["grant"]["valid"], true);
}
