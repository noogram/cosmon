// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/logs` — SSE logs stream integration tests.
//!
//! Pinned scenarios:
//!
//! 1. Auth gate — missing bearer yields 401.
//! 2. Scope gate — a JWT without `cosmon:logs:subscribe` yields 403.
//! 3. Authorised request — yields a `text/event-stream` response.
//! 4. Path-segment validation — a `..` or shell-meta segment yields
//!    400 `invalid_path_segment` rather than reaching tmux.
//!
//! What is **not** covered here:
//!
//! - End-to-end tmux capture. The polling task shells out to `tmux`,
//!   which is not present in the test environment; under that
//!   condition the polling task closes the channel immediately and
//!   the stream returns with zero `log.line` chunks. The empty-but-
//!   well-formed stream IS exercised by the content-type assertion.
//! - On-wire `event: log.line` body shape. The handler unit-tests
//!   (in `routes::logs_stream::tests`) lock the projection from
//!   `TailLine` → `Event`; pinning bytes here is brittle because
//!   axum's `Event` Debug impl truncates payloads.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, EventBus, JwksStore, Posture};
use tower::ServiceExt;

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
        events: std::sync::Arc::new(EventBus::with_default_capacity()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
    }
}

fn issue_logs_jwt(oidc: &OidcMock, sub: &str, audience: &str, jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: sub,
        audience: Some(audience),
        scopes: &["cosmon:logs:subscribe"],
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

#[tokio::test]
async fn logs_rejects_missing_bearer_with_401() {
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
    );
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260523-ad25/logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logs_rejects_missing_scope_with_403() {
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
    );
    let app = router(state);

    // JWT carries molecule + events scopes but NOT logs:subscribe.
    // The doctrinal point: events subscribe must not lift to logs.
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &[
            "cosmon:molecule:read",
            "cosmon:molecule:write",
            "cosmon:events:subscribe",
        ],
        lifetime_secs: Some(60),
        jti: Some("jti-no-logs"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260523-ad25/logs")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn logs_returns_event_stream_content_type_for_authorised_call() {
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
    );
    let app = router(state);

    let jwt = issue_logs_jwt(&oidc, "sub-a", "cosmon-rpp-a", "jti-logs-1");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260523-ad25/logs?follow=false")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("Content-Type missing")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected SSE content-type, got {content_type}"
    );
}

#[tokio::test]
async fn logs_rejects_path_traversal_segment_with_400() {
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
    );
    let app = router(state);

    let jwt = issue_logs_jwt(&oidc, "sub-a", "cosmon-rpp-a", "jti-logs-traversal");

    // `a..b` is path-traversal-suspicious. axum normalises raw `..`
    // before it reaches the extractor, so we test the embedded
    // `..` form which makes it through.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/a..b/logs")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
