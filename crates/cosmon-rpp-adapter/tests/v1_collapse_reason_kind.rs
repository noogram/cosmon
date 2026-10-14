// SPDX-License-Identifier: AGPL-3.0-only

//! `POST /v1/molecules/{id}/collapse` passes the request's `reason_kind`
//! through to the state operation (issue #207, prerequisite 3).
//!
//! The route used to build its `CollapseRequest` with `reason_kind: None`, so
//! an API-driven collapse could never carry the classification `cs errors`
//! aggregates on, whatever the caller sent.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantPath, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, JwksStore, Posture, SharedHabilitationMap,
};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    oidc: OidcMock,
    tenant: TenantPath,
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
    _artifact_root: tempfile::TempDir,
    app: axum::Router,
}

async fn fixture() -> Fixture {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let artifact_root = tempfile::tempdir().unwrap();
    let _ = oidc.write_jwks_file(security_dir.path()).unwrap();
    let jwks = JwksStore::load(security_dir.path()).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();
    let rate_limiter =
        IngressRateLimiter::new(security_dir.path().join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list =
        DenyList::new(security_dir.path().to_path_buf()).with_ttl(Duration::from_secs(0));

    let state = AppState {
        harvest_effect: std::sync::Arc::new(
            cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect,
        ),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.path().to_path_buf(),
        inbox_root: security_dir.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: artifact_root.path().to_path_buf(),
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: std::sync::Arc::new(
            cosmon_rpp_adapter::config::InstallTemplating::default(),
        ),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
    };
    let app = router(state);
    Fixture {
        oidc,
        tenant,
        _tenants: tenants,
        _security_dir: security_dir,
        _artifact_root: artifact_root,
        app,
    }
}

fn jwt_with_scopes(oidc: &OidcMock, scopes: &[&str], jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

/// Absolute path of the persistent molecule dir for a `default`-fleet
/// molecule under tenant `a`.
async fn post_collapse(fx: &Fixture, id: &str, jwt: &str, body: &Value) -> StatusCode {
    fx.app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/molecules/{id}/collapse"))
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn stored_state(fx: &Fixture, id: &str) -> Value {
    let path = fx
        .tenant
        .state_dir
        .join("fleets/default/molecules")
        .join(id)
        .join("state.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn collapse_route_records_the_requested_reason_kind() {
    let fx = fixture().await;
    let id = "task-20261014-aaaa";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "pending" }))
        .unwrap();
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "jti-rk");

    let status = post_collapse(
        &fx,
        id,
        &jwt,
        &serde_json::json!({ "reason": "gate red", "reason_kind": "gate_failed" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let state = stored_state(&fx, id);
    assert_eq!(state["status"], "collapsed");
    assert_eq!(
        state["collapse_reason_kind"],
        serde_json::json!("gate_failed"),
        "the request's reason_kind must reach the stored molecule: {state}",
    );
}

#[tokio::test]
async fn collapse_route_without_reason_kind_records_none() {
    let fx = fixture().await;
    let id = "task-20261014-bbbb";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "pending" }))
        .unwrap();
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "jti-nork");

    let status = post_collapse(&fx, id, &jwt, &serde_json::json!({ "reason": "plain" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(stored_state(&fx, id)["collapse_reason_kind"].is_null());
}
