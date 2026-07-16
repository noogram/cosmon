// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `GET /v1/quota` and the `X-RateLimit-*`
//! response-header layer.
//!
//! Scenarios:
//!
//! 1. Fresh tenant → `/v1/quota` returns 200 with full burst remaining
//!    minus 1 (the quota call itself consumes one token).
//! 2. Five sequential observe requests → `X-RateLimit-Remaining`
//!    decreases monotonically; the `/v1/quota` snapshot agrees with
//!    the last header.
//! 3. `/v1/quota` without `Authorization` → 401, no rate-limit headers.
//! 4. The headers also appear on `GET /v1/molecules/{id}` responses.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::Value;
use tower::ServiceExt;

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<(&str, &str, &str, &str)>,
    security_dir: &std::path::Path,
    capacity: f64,
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

    // No leak — keeps the test deterministic. The V0 production
    // defaults (capacity=30, leak=600/h) still flow through the same
    // code paths since the snapshot accessor multiplies leak_per_ms
    // by 0 = 0 for the static test wall-clock.
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), capacity, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(builder.build()),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
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
async fn quota_fresh_tenant_returns_full_burst_minus_one() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");

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
        30.0,
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-quota-fresh"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/quota")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Headers — capacity=30, the /v1/quota call itself consumed one
    // token via the admission boundary, so remaining=29.
    let headers = resp.headers().clone();
    assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "30");
    assert_eq!(headers.get("x-ratelimit-remaining").unwrap(), "29");
    assert!(headers.contains_key("x-ratelimit-reset"));

    // Body shape mirrors the headers (single source of truth: the
    // rate-limit snapshot).
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["limits"]["burst_capacity"], 30);
    assert_eq!(body["remaining"], 29);
    assert_eq!(body["current"]["bucket_level"], 1.0);
    assert!(body["request_id"].is_string());
    assert!(body["reset_at"].is_string());
}

#[tokio::test]
async fn rate_limit_remaining_decreases_across_observe_calls() {
    // Capacity=10 keeps the assertions tight without polluting the
    // disk bucket too quickly.
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260522-quota",
            &serde_json::json!({"variables": {"k": "v"}}),
        )
        .unwrap();

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
        10.0,
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-quota-decay"),
    });

    let mut last_remaining: Option<i64> = None;
    for i in 0..5 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/molecules/task-20260522-quota")
                    .header("Authorization", format!("Bearer {jwt}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "iteration {i}");
        let remaining: i64 = resp
            .headers()
            .get("x-ratelimit-remaining")
            .expect("missing X-RateLimit-Remaining header")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(remaining, 10 - (i + 1));
        last_remaining = Some(remaining);
    }
    assert_eq!(last_remaining, Some(5));

    // /v1/quota consumes one more token; its body must agree with the
    // header it carries.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/quota")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let header_remaining: i64 = resp
        .headers()
        .get("x-ratelimit-remaining")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(header_remaining, 4); // 10 - 6 observe-or-quota calls

    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["remaining"], header_remaining);
    assert_eq!(body["current"]["bucket_level_floor"], 6);
}

#[tokio::test]
async fn quota_without_jwt_returns_401_and_no_headers() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");

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
        30.0,
    );
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let headers = resp.headers();
    assert!(
        !headers.contains_key("x-ratelimit-limit"),
        "no JWT → middleware must NOT inject any rate-limit headers"
    );
    assert!(!headers.contains_key("x-ratelimit-remaining"));
    assert!(!headers.contains_key("x-ratelimit-reset"));
}

#[tokio::test]
async fn rate_limit_headers_appear_on_molecules_route() {
    // Pinning the contract: the middleware fires on every JWT-bearing
    // /v1/ route, not just /v1/quota.
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260522-hdrs", &serde_json::json!({"variables": {}}))
        .unwrap();

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
        30.0,
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-headers-mol"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260522-hdrs")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers();
    assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "30");
    assert_eq!(headers.get("x-ratelimit-remaining").unwrap(), "29");
    assert!(headers.contains_key("x-ratelimit-reset"));
}
