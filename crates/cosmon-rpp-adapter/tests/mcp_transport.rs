// SPDX-License-Identifier: AGPL-3.0-only

//! `/mcp` transport nest — the bearer gate boundary.
//!
//! The MCP surface is nested on the adapter's single listener as a third
//! projection of the same core the REST routes project
//! (delib-20260709-943e). These tests pin the one property this milestone
//! (path A) is responsible for: **the Streamable-HTTP service is never
//! anonymously reachable**. A request with no / invalid bearer is refused
//! *before* the MCP layer sees it; a request with a valid JWT passes the
//! gate and is handled by the transport (whatever it answers, it is not our
//! 401).
//!
//! Per-tool scope enforcement (F3-1, task-20260712-0294) IS exercised here:
//! a `tools/call` for a mutating tool with only `cosmon:molecule:read` in the
//! JWT must be refused `403` before the transport sees it, matching the REST
//! twin (`POST /v1/molecules` requires `cosmon:molecule:write`). `cwd`
//! severance and RFC 9728 discovery remain path-B seams documented in
//! `src/routes/mcp.rs` — not exercised here.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use tower::ServiceExt;

/// Build an [`AppState`] over the testkit primitives (mirrors the helper in
/// `v0_smoke.rs`, kept local so this file is self-contained).
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
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
        events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::inert()),
    }
}

/// A minimal MCP `initialize` JSON-RPC request body.
fn initialize_body() -> Body {
    Body::from(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "0.0.0" }
            }
        })
        .to_string(),
    )
}

/// A minimal MCP `tools/call` JSON-RPC request body for `tool`.
fn tools_call_body(tool: &str) -> Body {
    Body::from(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": tool, "arguments": {} }
        })
        .to_string(),
    )
}

/// Drive one `/mcp` POST through the full gated router and return the status.
async fn mcp_post_status(state: AppState, jwt: &str, body: Body) -> StatusCode {
    router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn mcp_post_without_bearer_is_401() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &TenantWorkspaces::new(),
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(initialize_body())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "an anonymous /mcp POST must be refused by the bearer gate"
    );
}

#[tokio::test]
async fn mcp_post_with_invalid_bearer_is_401() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &TenantWorkspaces::new(),
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("Authorization", "Bearer not-a-real-jwt")
                .body(initialize_body())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a garbage bearer on /mcp must be refused by the gate"
    );
}

#[tokio::test]
async fn mcp_post_with_valid_bearer_passes_the_gate() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &TenantWorkspaces::new(),
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-1"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(initialize_body())
                .unwrap(),
        )
        .await
        .unwrap();

    // The gate passed the request through to the Streamable-HTTP service.
    // We don't couple to the exact transport status (200/202/…): the point
    // is only that a *valid* token is no longer rejected with our gate 401.
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a valid JWT must clear the /mcp gate and reach the MCP transport"
    );
}

// ---------------------------------------------------------------------------
// F3-1 (task-20260712-0294) — per-tool scope enforcement at the /mcp gate.
//
// These tests are the falsifier gate for the fix: reverting the scope check
// in `require_valid_bearer` must redden `mcp_mutating_tool_call_without_write_
// scope_is_403`.
// ---------------------------------------------------------------------------

/// The core F3-1 regression: a `tools/call` for a MUTATING tool with a JWT
/// that carries only `cosmon:molecule:read` must be refused **403** before
/// the transport sees it — exactly as `POST /v1/molecules` (which requires
/// `cosmon:molecule:write`) would refuse the same identity.
#[tokio::test]
async fn mcp_mutating_tool_call_without_write_scope_is_403() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let tenants = TenantWorkspaces::new();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );

    // Read scope only — no `:write`.
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-nucleate-noscope"),
    });

    let status = mcp_post_status(state, &jwt, tools_call_body("cosmon_nucleate")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a mutating MCP tool call without `cosmon:molecule:write` must be 403 \
         (REST parity), not dispatched to the transport"
    );
}

/// The same mutating call **with** `cosmon:molecule:write` clears the scope
/// gate (it may then fail downstream for transport reasons, but never with
/// our 401/403).
#[tokio::test]
async fn mcp_mutating_tool_call_with_write_scope_clears_scope_gate() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let tenants = TenantWorkspaces::new();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-nucleate-write"),
    });

    let status = mcp_post_status(state, &jwt, tools_call_body("cosmon_nucleate")).await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "write scope is authenticated"
    );
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "a mutating MCP tool call WITH `cosmon:molecule:write` must clear the scope gate"
    );
}

/// A read-only `tools/call` clears the gate with only `cosmon:molecule:read`.
#[tokio::test]
async fn mcp_read_tool_call_with_read_scope_clears_gate() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let tenants = TenantWorkspaces::new();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-observe-read"),
    });

    let status = mcp_post_status(state, &jwt, tools_call_body("cosmon_observe")).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "a read-only MCP tool call with read scope must clear the gate"
    );
}

/// The read floor: a valid, tenant-resolved JWT that carries *no* molecule
/// scope (only `openid`) cannot even open the connector — mirroring REST,
/// where every route requires at least `cosmon:molecule:read`.
#[tokio::test]
async fn mcp_openid_only_is_refused_at_read_floor() {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let tenants = TenantWorkspaces::new();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-openid-only"),
    });

    let status = mcp_post_status(state, &jwt, initialize_body()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an `openid`-only JWT must be refused at the read floor (REST parity)"
    );
}
