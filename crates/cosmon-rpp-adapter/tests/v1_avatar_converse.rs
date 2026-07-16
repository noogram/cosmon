// SPDX-License-Identifier: AGPL-3.0-only

//! `POST /v1/avatar/converse` — canal (b) smoke + L3 anti-cycle bound.
//!
//! The route became a tenant verb (top-level `converse` client-side)
//! but its gating is UNCHANGED: on-by-binding. These tests pin both
//! sides of the gate and the hop bound on synchronous `request`
//! chains:
//!
//! 1. On-binding: a binding file for the target avatar admits the
//!    message → 200, `accepted: true`.
//! 2. Off-binding: no binding file → stable refusal `503 no_binding`
//!    (not 404 — no existence oracle).
//! 3. Hop bound: a `request` at the bound is refused with the stable
//!    code `409 max_hops_exceeded`; one hop below passes; `announce`
//!    at the same depth is exempt (fire-and-forget, no mutual wait).
//! 4. The bound is read from the binding (`max_hops` key), never from
//!    the request.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::routes::avatar::DEFAULT_MAX_CONVERSE_HOPS;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::{json, Value};
use tower::ServiceExt;

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let builder = HabilitationMap::builder().insert_with_scopes(
        oidc.issuer(),
        "pilote-a",
        HabilitationId::new("nuc-pilote-a"),
        Noyau::new("a"),
        "cosmon-rpp-a",
        vec!["cosmon:pilote:converse".to_owned()],
    );

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

/// Write a binding file for `avatar_id` under tenant `a`'s state dir.
/// `extra` is appended verbatim (e.g. a `max_hops` override).
fn write_binding(tenants: &TenantWorkspaces, avatar_id: &str, extra: &str) {
    let binding_dir = tenants
        .galaxies_root()
        .join("a")
        .join(".cosmon")
        .join("state")
        .join("bindings");
    std::fs::create_dir_all(&binding_dir).unwrap();
    std::fs::write(
        binding_dir.join(format!("{avatar_id}.toml")),
        format!("target = \"{avatar_id}\"\ngranted_at = \"2026-06-11T00:00:00Z\"\n{extra}"),
    )
    .unwrap();
}

async fn post_converse(
    app: axum::Router,
    oidc: &OidcMock,
    jti: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let jwt = oidc.issue(&IssueJwt {
        subject: "pilote-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["openid"],
        lifetime_secs: Some(60),
        jti: Some(jti),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/avatar/converse")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn harness() -> (axum::Router, OidcMock, TenantWorkspaces) {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    // The tempdir must outlive the router: leak it for the test's
    // lifetime (same pattern cost as keeping the guard, fewer moving
    // parts across the helper boundary).
    let security_dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let state = make_state(&oidc, &tenants, security_dir.path());
    (router(state), oidc, tenants)
}

#[tokio::test]
async fn converse_on_binding_accepts_the_message() {
    let (app, oidc, tenants) = harness().await;
    write_binding(&tenants, "ava-bound", "");

    let body = json!({
        "avatar_id": "ava-bound",
        "message": "bonjour",
        "kind": "request",
    });
    let (status, value) = post_converse(app, &oidc, "jti-conv-on-1", &body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "on-binding converse must pass: {value}"
    );
    assert_eq!(value["converse"]["accepted"], json!(true));
    assert!(
        value["converse"]["message_id"].as_str().is_some(),
        "envelope must carry a message_id: {value}"
    );
}

#[tokio::test]
async fn converse_off_binding_refuses_with_stable_code() {
    let (app, oidc, _tenants) = harness().await;
    // No binding file written for this avatar.
    let body = json!({
        "avatar_id": "ava-unbound",
        "message": "bonjour",
        "kind": "request",
    });
    let (status, value) = post_converse(app, &oidc, "jti-conv-off-1", &body).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "off-binding converse must be refused: {value}"
    );
    assert_eq!(
        value["error"],
        json!("no_binding"),
        "the refusal code is part of the wire contract"
    );
}

#[tokio::test]
async fn request_chain_beyond_the_bound_is_refused_with_stable_code() {
    let (app, oidc, tenants) = harness().await;
    write_binding(&tenants, "ava-bound", "");

    let body = json!({
        "avatar_id": "ava-bound",
        "message": "relayed",
        "kind": "request",
        "hop": DEFAULT_MAX_CONVERSE_HOPS,
    });
    let (status, value) = post_converse(app, &oidc, "jti-conv-hop-1", &body).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a request at the hop bound must be refused: {value}"
    );
    assert_eq!(
        value["error"],
        json!("max_hops_exceeded"),
        "the refusal code is part of the wire contract"
    );
}

#[tokio::test]
async fn request_below_the_bound_passes() {
    let (app, oidc, tenants) = harness().await;
    write_binding(&tenants, "ava-bound", "");

    let body = json!({
        "avatar_id": "ava-bound",
        "message": "relayed",
        "kind": "request",
        "hop": DEFAULT_MAX_CONVERSE_HOPS - 1,
    });
    let (status, value) = post_converse(app, &oidc, "jti-conv-hop-2", &body).await;
    assert_eq!(status, StatusCode::OK, "one hop below the bound: {value}");
    assert_eq!(value["converse"]["accepted"], json!(true));
}

#[tokio::test]
async fn announce_is_exempt_from_the_hop_bound() {
    let (app, oidc, tenants) = harness().await;
    write_binding(&tenants, "ava-bound", "");

    // Same depth that refuses a `request` — announce is
    // fire-and-forget: no mutual wait, no cycle, no bound.
    let body = json!({
        "avatar_id": "ava-bound",
        "message": "broadcast",
        "kind": "announce",
        "hop": DEFAULT_MAX_CONVERSE_HOPS + 7,
    });
    let (status, value) = post_converse(app, &oidc, "jti-conv-ann-1", &body).await;
    assert_eq!(status, StatusCode::OK, "announce must be exempt: {value}");
    assert_eq!(value["converse"]["accepted"], json!(true));
}

#[tokio::test]
async fn binding_max_hops_override_tightens_the_bound() {
    let (app, oidc, tenants) = harness().await;
    // The binding — operator-written, client-readable-never-writable —
    // tightens the bound to 2.
    write_binding(&tenants, "ava-tight", "max_hops = 2\n");

    let refused = json!({
        "avatar_id": "ava-tight",
        "message": "relayed",
        "kind": "request",
        "hop": 2,
    });
    let (status, value) = post_converse(app.clone(), &oidc, "jti-conv-tight-1", &refused).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"], json!("max_hops_exceeded"));

    let passes = json!({
        "avatar_id": "ava-tight",
        "message": "relayed",
        "kind": "request",
        "hop": 1,
    });
    let (status, value) = post_converse(app, &oidc, "jti-conv-tight-2", &passes).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["converse"]["accepted"], json!(true));
}
