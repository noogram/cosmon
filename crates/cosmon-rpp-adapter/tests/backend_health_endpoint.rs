// SPDX-License-Identifier: AGPL-3.0-only

//! Acceptance test for `GET /health/backends` (T-V1-IFBDD-METER).
//!
//! The endpoint sits outside `/v1/` (operational diagnostic), is
//! unauthenticated, and reflects the in-RAM `BackendHealthRegistry`
//! pre-populated by `RppConfig::backends`. Drives ADR-080 §4.2: any
//! addition to `/v1/...` must trip the freeze test, but `/health/...`
//! routes do not.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, OidcMock, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, BackendProbe, JwksStore, Posture,
};
use serde_json::Value;
use tower::ServiceExt;

async fn make_state(
    security_dir: &std::path::Path,
    registry: Arc<BackendHealthRegistry>,
) -> AppState {
    let oidc = OidcMock::start().await;
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let nucleon_map = HabilitationMap::builder().build();
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));
    let tenants = TenantWorkspaces::new();
    AppState {
        cs_path: fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(5),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: registry,
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
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
    }
}

#[tokio::test]
async fn empty_registry_returns_empty_array() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(BackendHealthRegistry::new());
    let app = router(make_state(tmp.path(), registry).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health/backends")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    let backends = body.get("backends").and_then(|v| v.as_array()).unwrap();
    assert!(backends.is_empty());
}

#[tokio::test]
async fn configured_backends_show_up_as_unused() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(BackendHealthRegistry::new());
    registry.register_configured(["anthropic".to_owned(), "ollama".to_owned()]);
    let app = router(make_state(tmp.path(), registry).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health/backends")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    let backends = body.get("backends").and_then(|v| v.as_array()).unwrap();
    assert_eq!(backends.len(), 2);
    let anthropic = &backends[0];
    assert_eq!(anthropic["name"], "anthropic");
    assert_eq!(anthropic["status"], "configured-but-unused");
    assert!(
        anthropic.get("last_check_ms").is_none() || anthropic["last_check_ms"].is_null(),
        "unused backend must omit last_check_ms"
    );
}

#[tokio::test]
async fn recorded_probe_promotes_status_to_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(BackendHealthRegistry::new());
    registry.record(
        "anthropic",
        BackendProbe {
            success: true,
            latency_ms: 200,
            at: chrono::Utc::now(),
        },
    );
    let app = router(make_state(tmp.path(), registry).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health/backends")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    let backends = body.get("backends").and_then(|v| v.as_array()).unwrap();
    assert_eq!(backends.len(), 1);
    assert_eq!(backends[0]["status"], "ok");
    assert!(backends[0]["last_check_ms"].is_i64());
}
