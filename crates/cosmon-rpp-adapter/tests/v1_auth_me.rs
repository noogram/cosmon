// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `GET /v1/auth/me`.
//!
//! Five scenarios — the same shape as `v0_smoke` for the molecule
//! routes, but adapted to the whoami surface:
//!
//! 1. Valid JWT → 200 with the documented payload shape.
//! 2. Missing `Authorization` header → 401.
//! 3. Tampered JWT signature → 401.
//! 4. Expired JWT → 401.
//! 5. Valid JWT, no nucleon binding → 200 with `noyau: null`.
//!
//! The third gap-report priority (j) acceptance is point 1 + the
//! payload schema match: the tenant must be able to read `sub`, `aud`,
//! `scopes`, `noyau`, `expires_at`, `issuer` from a single call.

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
async fn valid_jwt_returns_200_with_whoami_payload() {
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
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "tenant-demo-operator",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &["cosmon:molecule:read", "cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-auth-me-1"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/me")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(body["sub"], "tenant-demo-operator");
    assert_eq!(body["aud"], serde_json::json!(["cosmon-rpp-tenant"]));
    assert_eq!(
        body["scopes"],
        serde_json::json!(["cosmon:molecule:read", "cosmon:molecule:write"])
    );
    assert_eq!(body["noyau"], "tenant-demo-sandbox");
    assert_eq!(body["issuer"], oidc.issuer());

    // expires_at must be an ISO-8601 UTC stamp; we don't pin the value
    // because the OidcMock issues `iat = now`, but the format is fixed.
    let expires_at = body["expires_at"]
        .as_str()
        .expect("expires_at must be a string");
    assert_eq!(expires_at.len(), 20, "ISO-8601 UTC: YYYY-MM-DDTHH:MM:SSZ");
    assert!(expires_at.ends_with('Z'));
    assert_eq!(&expires_at[4..5], "-");
    assert_eq!(&expires_at[7..8], "-");
    assert_eq!(&expires_at[10..11], "T");
    assert_eq!(&expires_at[13..14], ":");
    assert_eq!(&expires_at[16..17], ":");

    // Version fields (delib-20260610-9a0c, tolnay) — additive, mirror
    // `/healthz`. `version` is the crate version (bake tag series);
    // `api_surface_version` is the event-fold length, derived rather
    // than pinned so appending a surface event keeps this test green.
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        body["api_surface_version"],
        serde_json::json!(cosmon_rpp_adapter::surface_events::SURFACE_EVENTS.len())
    );
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
                .uri("/v1/auth/me")
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
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-auth-me-tamper"),
    });
    // Flip the last char of the signature segment.
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
                .uri("/v1/auth/me")
                .header("Authorization", format!("Bearer {tampered}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_jwt_returns_401() {
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

    let jwt = oidc.issue(&IssueJwt {
        subject: "tenant-demo-operator",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(1),
        jti: Some("jti-auth-me-exp"),
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/me")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Mirrors the contract from `v0_smoke::expired_jwt_returns_401` —
    // the jsonwebtoken default leeway (60s) may swallow a 2s overshoot,
    // so we assert the negative: a stale token must NOT yield 200.
    assert_ne!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn valid_jwt_without_binding_returns_200_with_null_noyau() {
    // Whoami semantics: the JWT is valid but no nucleon binding exists
    // for `(iss, sub)` — `noyau` is `null` rather than 401. The
    // molecule routes would reject this request, but `/me` is the
    // whoami counterpart, not a state-mutating verb.
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
        jti: Some("jti-auth-me-unbound"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/me")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["sub"], "unbound-principal");
    assert!(
        body["noyau"].is_null(),
        "no binding ⇒ noyau is null, got {:?}",
        body["noyau"]
    );
    assert_eq!(body["scopes"], serde_json::json!([]));
}

// ── Worker-glasses signal (smithy C1 onboarding) ──────────────────
//
// `claude_credentials_present` is the server-side signal `cosmon-remote
// doctor` turns into « lance `auth login` » before the first tackle
// 503. Three states, each asserted: surface unconfigured → null;
// configured + file absent → false; configured + file present → true.
// The probe reads the credentials file itself — falsified below by
// simply (not) writing it.

async fn auth_me_body_with(state: AppState, jwt: &str) -> Value {
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/me")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    serde_json::from_slice(&body_bytes).unwrap()
}

fn issue_jwt(oidc: &OidcMock, jti: &'static str) -> String {
    oidc.issue(&IssueJwt {
        subject: "tenant-demo-operator",
        audience: Some("cosmon-rpp-tenant"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

#[tokio::test]
async fn claude_credentials_present_is_null_when_surface_unconfigured() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("tenant-demo-sandbox");
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    // make_state leaves auth_claude: None — the server cannot know.
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
    let jwt = issue_jwt(&oidc, "jti-glasses-null");
    let body = auth_me_body_with(state, &jwt).await;
    assert!(
        body.get("claude_credentials_present").is_some(),
        "field must be present on the wire"
    );
    assert!(body["claude_credentials_present"].is_null());
}

#[tokio::test]
async fn claude_credentials_present_reads_the_file_both_states() {
    use cosmon_rpp_adapter::auth_claude::{
        AuthClaudeConfig, AuthClaudeState, FilesystemSessionStore, SessionStore,
    };

    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("tenant-demo-sandbox");
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-tenant".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    let make_with_auth_claude = || {
        let config = AuthClaudeConfig::defaults_with_home(home.path());
        let store: Arc<dyn SessionStore> =
            Arc::new(FilesystemSessionStore::new(security_dir.path()).unwrap());
        let mut state = make_state(
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
        state.auth_claude = Some(Arc::new(AuthClaudeState::new(config, store)));
        state
    };

    // Red state: surface configured, no login yet → false.
    let jwt = issue_jwt(&oidc, "jti-glasses-false");
    let body = auth_me_body_with(make_with_auth_claude(), &jwt).await;
    assert_eq!(body["claude_credentials_present"], serde_json::json!(false));

    // Green state: write the credentials file where the PKCE confirm
    // handler would — the probe must flip to true.
    let creds = AuthClaudeConfig::defaults_with_home(home.path()).credentials_path;
    std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
    std::fs::write(&creds, b"{}").unwrap();
    let jwt = issue_jwt(&oidc, "jti-glasses-true");
    let body = auth_me_body_with(make_with_auth_claude(), &jwt).await;
    assert_eq!(body["claude_credentials_present"], serde_json::json!(true));
}
