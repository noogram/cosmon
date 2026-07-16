// SPDX-License-Identifier: AGPL-3.0-only

//! T23 — admin nucleon binding-granted scopes.
//!
//! The upstream `IdP` (Forgejo `OAuth2`) cannot mint custom scopes like
//! `cosmon:molecule:*` on its bearer tokens — it issues `openid` only.
//! The §8j HTTPS+JWT admission boundary therefore consults the admin
//! nucleon binding (`oidc-identity.toml` under `[scopes].allowed`) and
//! unions its grants with the JWT's own scope set before deciding
//! Allow/Absent. Cross-tenant isolation is **not** relaxed by this
//! gate: the audience pin (`CrossTenantPivot`) still rejects pivot
//! attempts independently.
//!
//! Scenarios:
//!
//! 1. JWT carrying only `openid` + binding granting `molecule:write`
//!    → 201 Created on `POST /v1/molecules` (binding wins).
//! 2. Audit trail records `grant_source = "binding"` on the same
//!    request.
//! 3. JWT carrying `molecule:write` + binding empty → 201 + audit
//!    `grant_source = "jwt"` (backwards-compat fallback).
//! 4. Cross-tenant attempt — JWT for noyau A trying to use noyau B's
//!    audience — still 4xx, regardless of binding grants.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::instrumentation::{read_authz_ndjson, AuthzDecision, AUTHZ_NDJSON_RELATIVE_PATH};
use serde_json::json;
use tower::ServiceExt;

/// Binding entry: `(sub, nucleon, noyau, audience, allowed_scopes)`.
type Binding<'a> = (&'a str, &'a str, &'a str, &'a str, &'a [&'a str]);

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<Binding<'_>>,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let mut builder = HabilitationMap::builder();
    for (sub, nucleon, noyau, audience, scopes) in nucleons {
        let allowed: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        builder = builder.insert_with_scopes(
            oidc.issuer(),
            sub,
            HabilitationId::new(nucleon),
            Noyau::new(noyau),
            audience,
            allowed,
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
async fn binding_granted_scope_admits_request_with_openid_only_jwt() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .install_task_work_formula()
        .expect("install task-work formula");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    // The admin binding grants `cosmon:molecule:write` implicitly.
    let state = make_state(
        &oidc,
        &tenants,
        vec![(
            "admin-a",
            "nuc-admin-a",
            "a",
            "cosmon-rpp-a",
            &["cosmon:molecule:read", "cosmon:molecule:write"],
        )],
        security_dir.path(),
    );
    let app = router(state);

    // JWT carries only `openid` — exactly what Forgejo's OAuth2
    // ApplicationClient issues. Without the binding grant this would
    // collapse to a 403 forbidden (`Absent` decision).
    let jwt = oidc.issue(&IssueJwt {
        subject: "admin-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some("jti-binding-admit-1"),
    });

    let body = json!({"formula": "task-work"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "binding-granted scope must admit the request even when the JWT itself only carries `openid`"
    );
}

#[tokio::test]
async fn audit_trail_records_grant_source_binding_when_binding_admits() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .install_task_work_formula()
        .expect("install task-work formula");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![(
            "admin-a",
            "nuc-admin-a",
            "a",
            "cosmon-rpp-a",
            &["cosmon:molecule:write"],
        )],
        security_dir.path(),
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "admin-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some("jti-binding-audit-1"),
    });

    let body = json!({"formula": "task-work"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let authz_path = security_dir.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
    let events = read_authz_ndjson(&authz_path).expect("authz ndjson readable");
    let nucleate_event = events
        .iter()
        .find(|e| e.verb == "nucleate")
        .expect("nucleate audit event present");
    assert_eq!(nucleate_event.decision, AuthzDecision::Allow);
    assert_eq!(
        nucleate_event.grant_source.as_deref(),
        Some("binding"),
        "grant_source must record that the binding (not the JWT) carried the scope"
    );
}

#[tokio::test]
async fn audit_trail_records_grant_source_jwt_when_jwt_admits() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .install_task_work_formula()
        .expect("install task-work formula");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    // Binding carries no granted scopes — must fall back to JWT scopes.
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a", &[])],
        security_dir.path(),
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-jwt-audit-1"),
    });

    let body = json!({"formula": "task-work"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let authz_path = security_dir.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
    let events = read_authz_ndjson(&authz_path).expect("authz ndjson readable");
    let nucleate_event = events
        .iter()
        .find(|e| e.verb == "nucleate")
        .expect("nucleate audit event present");
    assert_eq!(nucleate_event.decision, AuthzDecision::Allow);
    assert_eq!(
        nucleate_event.grant_source.as_deref(),
        Some("jwt"),
        "grant_source must record that the JWT carried the scope"
    );
}

#[tokio::test]
async fn empty_binding_and_empty_jwt_scope_returns_403_with_absent() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .install_task_work_formula()
        .expect("install task-work formula");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a", &[])],
        security_dir.path(),
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some("jti-no-scope-1"),
    });

    let body = json!({"formula": "task-work"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let authz_path = security_dir.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
    let events = read_authz_ndjson(&authz_path).expect("authz ndjson readable");
    let event = events
        .iter()
        .find(|e| e.verb == "nucleate")
        .expect("nucleate audit event present");
    assert_eq!(event.decision, AuthzDecision::Absent);
    assert!(
        event.grant_source.is_none(),
        "grant_source must be absent when no source granted the scope"
    );
}

#[tokio::test]
async fn cross_tenant_attempt_with_binding_grant_still_rejected() {
    // The admin binding for noyau A grants write. The JWT presents
    // audience `cosmon-rpp-b` (a different noyau's audience). The
    // audience pin in admission MUST reject this independently of the
    // scope check — binding-granted scopes do NOT widen tenant
    // isolation (ADR-080 §8j).
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .install_task_work_formula()
        .expect("install task-work formula");
    let tenant_b = tenants.add("b");
    tenant_b
        .install_task_work_formula()
        .expect("install task-work formula in b");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned(), "cosmon-rpp-b".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![(
            "admin-a",
            "nuc-admin-a",
            "a",
            "cosmon-rpp-a",
            &["cosmon:molecule:write"],
        )],
        security_dir.path(),
    );
    let app = router(state);

    // JWT crafted with admin-a's `sub` but B's audience — the audience
    // pin rejects.
    let jwt = oidc.issue(&IssueJwt {
        subject: "admin-a",
        audience: Some("cosmon-rpp-b"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some("jti-cross-tenant-1"),
    });

    let body = json!({"formula": "task-work"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    // Either 401/403 (rejection family) — but never 201.
    assert!(
        resp.status().is_client_error(),
        "cross-tenant pivot must be rejected (got {})",
        resp.status()
    );
    assert_ne!(resp.status(), StatusCode::CREATED);
}
