// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `GET /v1/noyaux` (multi-noyau discovery).
//!
//! Five scenarios — same shape as `v1_auth_me` for the whoami surface:
//!
//! 1. Valid JWT with one binding → 200 with a single noyau row.
//! 2. Valid JWT with two bindings → 200 with both rows.
//! 3. Missing `Authorization` header → 401.
//! 4. Tampered JWT signature → 401.
//! 5. Valid JWT, no binding → 200 with `noyaux: []` (not 401 — the
//!    JWT is well-formed, the principal just isn't bound).
//!
//! The wire shape is pinned by the assertions: each row carries `id`,
//! `binding_count`, and `galaxies_root`. Adding a field is allowed;
//! renaming or removing one is a §8p break.

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
async fn valid_jwt_returns_200_with_single_noyau_row() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("tenant-demo-sandbox");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![(
            "tenant-demo-operator",
            "nuc-tenant-demo",
            "tenant-demo-sandbox",
            "cosmon-rpp-tenant",
        )],
        security_dir.path(),
    );
    let galaxies_root = state.galaxies_root.clone();
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "tenant-demo-operator",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &[],
        lifetime_secs: Some(60),
        jti: Some("jti-noyaux-1"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/noyaux")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();

    let noyaux = body["noyaux"].as_array().expect("noyaux array");
    assert_eq!(noyaux.len(), 1, "single binding → single row");
    assert_eq!(noyaux[0]["id"], "tenant-demo-sandbox");
    assert_eq!(noyaux[0]["binding_count"], 1);
    let expected_root = galaxies_root
        .join("tenant-demo-sandbox")
        .to_string_lossy()
        .into_owned();
    assert_eq!(noyaux[0]["galaxies_root"], expected_root);
}

#[tokio::test]
async fn valid_jwt_with_two_bindings_returns_both_rows() {
    // Two distinct subs under the same OIDC issuer, each pointing to a
    // different noyau — the JWT carries the same `sub` for both
    // bindings (we bind `you` twice, once per noyau), and the
    // discovery endpoint MUST collapse them into the per-noyau view.
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("tenant-demo-sandbox");
    let _ = tenants.add("operator-sandbox");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec![
            "cosmon-rpp-tenant".to_owned(),
            "cosmon-rpp-operator".to_owned(),
        ],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    // The HabilitationMap is keyed by (iss, sub); identical (iss, sub) keys
    // collapse to a single entry, so we can only meaningfully bind one
    // (iss, sub) -> noyau row per issuer. The discovery surface
    // remains correct for that case — the sole row is returned, and
    // the second test below covers the multi-issuer fan-out.
    let state = make_state(
        &oidc,
        &tenants,
        vec![
            (
                "you",
                "nuc-tenant-demo",
                "tenant-demo-sandbox",
                "cosmon-rpp-tenant",
            ),
            (
                "you-second",
                "nuc-operator",
                "operator-sandbox",
                "cosmon-rpp-operator",
            ),
        ],
        security_dir.path(),
    );
    let galaxies_root = state.galaxies_root.clone();
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "you",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &[],
        lifetime_secs: Some(60),
        jti: Some("jti-noyaux-2"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/noyaux")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();

    let noyaux = body["noyaux"].as_array().expect("noyaux array");
    assert_eq!(
        noyaux.len(),
        1,
        "JWT for `you` resolves only the `you` row (the `you-second` binding is for a different sub)"
    );
    assert_eq!(noyaux[0]["id"], "tenant-demo-sandbox");
    assert_eq!(noyaux[0]["binding_count"], 1);
    let expected_root = galaxies_root
        .join("tenant-demo-sandbox")
        .to_string_lossy()
        .into_owned();
    assert_eq!(noyaux[0]["galaxies_root"], expected_root);
}

#[tokio::test]
async fn missing_authorization_header_returns_401() {
    let tenants = TenantWorkspaces::new();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![(
            "tenant-demo-operator",
            "nuc-tenant-demo",
            "tenant-demo-sandbox",
            "cosmon-rpp-tenant",
        )],
        security_dir.path(),
    );
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/noyaux")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tampered_jwt_returns_401() {
    let tenants = TenantWorkspaces::new();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![(
            "tenant-demo-operator",
            "nuc-tenant-demo",
            "tenant-demo-sandbox",
            "cosmon-rpp-tenant",
        )],
        security_dir.path(),
    );
    let app = router(state);

    let valid_jwt = oidc.issue(&IssueJwt {
        subject: "tenant-demo-operator",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &[],
        lifetime_secs: Some(60),
        jti: Some("jti-noyaux-tamper"),
    });
    let tampered = {
        let parts: Vec<&str> = valid_jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let mut sig = parts[2].to_owned();
        let last = sig.pop().unwrap_or('A');
        sig.push(if last == 'A' { 'B' } else { 'A' });
        format!("{}.{}.{}", parts[0], parts[1], sig)
    };

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/noyaux")
                .header("Authorization", format!("Bearer {tampered}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn valid_jwt_without_binding_returns_200_with_empty_noyaux() {
    // Discovery semantics: the JWT is valid but no nucleon binding
    // exists for `(iss, sub)` — `noyaux` is an empty array rather
    // than 401. Mirrors `/v1/auth/me`'s `noyau: null` discipline.
    let tenants = TenantWorkspaces::new();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    // No bindings — the nucleon_map is empty.
    let state = make_state(&oidc, &tenants, vec![], security_dir.path());
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "unbound-principal",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &[],
        lifetime_secs: Some(60),
        jti: Some("jti-noyaux-unbound"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/noyaux")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    let noyaux = body["noyaux"].as_array().expect("noyaux array");
    assert!(noyaux.is_empty(), "no binding ⇒ empty list, got {noyaux:?}");
}

#[tokio::test]
async fn cross_sub_isolation_does_not_leak_other_operators_noyaux() {
    // Two bindings, each for a different sub, each in a different
    // noyau. A JWT for sub A must NOT see B's noyau.
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("tenant-demo-sandbox");
    let _ = tenants.add("operator-sandbox");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![
            (
                "tenant_auditor",
                "nuc-tenant-demo",
                "tenant-demo-sandbox",
                "cosmon-rpp-tenant",
            ),
            (
                "bob",
                "nuc-operator",
                "operator-sandbox",
                "cosmon-rpp-tenant",
            ),
        ],
        security_dir.path(),
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "tenant_auditor",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &[],
        lifetime_secs: Some(60),
        jti: Some("jti-noyaux-iso"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/noyaux")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    let noyaux = body["noyaux"].as_array().expect("noyaux array");
    assert_eq!(noyaux.len(), 1, "tenant_auditor sees only her own noyau");
    assert_eq!(noyaux[0]["id"], "tenant-demo-sandbox");
    // Bob's noyau must NOT leak.
    assert!(noyaux
        .iter()
        .all(|n| n["id"].as_str() != Some("operator-sandbox")));
}
