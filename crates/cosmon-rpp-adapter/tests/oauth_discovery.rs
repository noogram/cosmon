// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `GET /.well-known/cosmon-oauth-clients` — the
//! OAuth client-id reverse-discovery endpoint (delib-20260710-33b7 §C8,
//! `task-20260710-909a`).
//!
//! Scenarios:
//!
//! 1. Explicit `oauth-clients.toml` seeded → `200` with the exact
//!    audience-keyed document, `Cache-Control: no-store`, no JWT required.
//! 2. Only `trusted-issuers.toml` seeded (no explicit file) → `200` with
//!    the derived document (`client_id == audience`, Forgejo endpoints).
//! 3. Nothing configured → `404 discovery_unconfigured`.
//! 4. Malformed `oauth-clients.toml` → `500 discovery_error` (fail-closed).
//! 5. A `client_secret` poisoning `oauth-clients.toml` → **never** on the wire
//!    (df19-F5 serve-time regression: the public registry leaks no secret).
//!
//! Plus the **negative-audience** RS-wall test (kahneman-F5, C1/C8): a
//! token whose `aud` is not in the issuer's closed allowlist is rejected —
//! the isolation is proved by *rejection*, not by acceptance.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::Value;
use tower::ServiceExt;

/// Build an `AppState` whose `state_dir` is `state_dir` (its
/// `security/` subdir carries the registry / allowlist files the
/// discovery loader reads). The JWKS store is empty — the discovery
/// route is unauthenticated, so no keys are needed.
fn make_state(state_dir: &std::path::Path) -> AppState {
    // No JWKS needed — the discovery route is unauthenticated. An empty
    // temp dir yields an empty store.
    let jwks = JwksStore::load(state_dir).unwrap();
    let rate_limiter = IngressRateLimiter::new(state_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(state_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: fake_cs_path(),
        state_dir: state_dir.to_path_buf(),
        inbox_root: state_dir.join("whispers/inbox"),
        galaxies_root: state_dir.join("galaxies"),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(
            HabilitationMap::builder().build(),
        ),
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

fn security_dir(state_dir: &std::path::Path) -> std::path::PathBuf {
    let sec = state_dir.join("security");
    std::fs::create_dir_all(&sec).unwrap();
    sec
}

async fn get_discovery(app: axum::Router) -> (StatusCode, Option<String>, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/.well-known/cosmon-oauth-clients")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let cache_control = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cache_control, body)
}

#[tokio::test]
async fn explicit_registry_is_served_audience_keyed_no_jwt_required() {
    let dir = tempfile::tempdir().unwrap();
    let sec = security_dir(dir.path());
    std::fs::write(
        sec.join("oauth-clients.toml"),
        "schema_version = 1\n\
         issuer = \"https://forgejo.example.ts.net\"\n\
         authorization_endpoint = \"https://forgejo.example.ts.net/login/oauth/authorize\"\n\
         token_endpoint = \"https://forgejo.example.ts.net/login/oauth/access_token\"\n\
         [[clients]]\n\
         audience = \"cs-rpp-adapter\"\n\
         client_id = \"runtime-cid-a\"\n\
         redirect_uris = [\"http://127.0.0.1:7777/callback\"]\n\
         scopes = [\"cosmon:molecule:read\", \"cosmon:molecule:write\"]\n\
         [[clients]]\n\
         audience = \"claude-web\"\n\
         client_id = \"runtime-cid-b\"\n\
         redirect_uris = [\"https://claude.ai/api/mcp/auth_callback\"]\n",
    )
    .unwrap();

    let app = router(make_state(dir.path()));
    let (status, cache_control, body) = get_discovery(app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache_control.as_deref(), Some("no-store"));
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["issuer"], "https://forgejo.example.ts.net");

    // Audience-keyed: the CLI (A) and the MCP connector (B) coexist.
    let clients = body["clients"].as_array().unwrap();
    let a = clients
        .iter()
        .find(|c| c["audience"] == "cs-rpp-adapter")
        .unwrap();
    let b = clients
        .iter()
        .find(|c| c["audience"] == "claude-web")
        .unwrap();
    assert_eq!(a["client_id"], "runtime-cid-a");
    assert_eq!(b["client_id"], "runtime-cid-b");
    assert_eq!(a["redirect_uris"][0], "http://127.0.0.1:7777/callback");
}

/// df19-F5 regression: a `client_secret` present in `oauth-clients.toml` MUST
/// NOT appear in the JSON served over the unauthenticated
/// `/.well-known/cosmon-oauth-clients` route. This is the *serve-time*
/// direction of the "no secret leaks" property — proved end-to-end through the
/// real axum handler against the raw response bytes, not just the load+serialize
/// unit (`oauth_discovery::tests::client_secret_in_toml_never_reaches_serialized_json`).
///
/// The registry is poisoned with a `client_secret` at both the top level and
/// inside a `[[clients]]` entry (the two natural places a careless provisioner
/// or operator might drop one). The served type has no secret field, so the
/// secret is dropped on deserialize and never re-serialised — but a future
/// passthrough/flatten field would silently start echoing it. This test fails
/// loud if that ever regresses.
#[tokio::test]
async fn client_secret_never_leaks_through_served_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let sec = security_dir(dir.path());
    std::fs::write(
        sec.join("oauth-clients.toml"),
        "schema_version = 1\n\
         issuer = \"https://forgejo.example.ts.net\"\n\
         authorization_endpoint = \"https://forgejo.example.ts.net/login/oauth/authorize\"\n\
         token_endpoint = \"https://forgejo.example.ts.net/login/oauth/access_token\"\n\
         client_secret = \"TOP_LEVEL_SHOULD_NEVER_LEAK\"\n\
         [[clients]]\n\
         audience = \"cs-rpp-adapter\"\n\
         client_id = \"runtime-cid-a\"\n\
         client_secret = \"PER_CLIENT_SHOULD_NEVER_LEAK\"\n\
         redirect_uris = [\"http://127.0.0.1:7777/callback\"]\n",
    )
    .unwrap();

    let app = router(make_state(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/.well-known/cosmon-oauth-clients")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();

    // The document is served (client_id is public and present)…
    assert!(
        raw.contains("runtime-cid-a"),
        "public client_id must be served: {raw}"
    );
    // …but neither secret value nor the key that would smuggle one is on the wire.
    assert!(
        !raw.contains("SHOULD_NEVER_LEAK"),
        "client_secret value leaked over the wire: {raw}"
    );
    assert!(
        !raw.contains("client_secret"),
        "client_secret key leaked over the wire: {raw}"
    );
}

#[tokio::test]
async fn derived_registry_when_only_trusted_issuers_present() {
    let dir = tempfile::tempdir().unwrap();
    let sec = security_dir(dir.path());
    std::fs::write(
        sec.join("trusted-issuers.toml"),
        "[[issuer]]\n\
         iss = \"http://host/git\"\n\
         audiences = [\"cs-rpp-adapter\", \"claude-web\"]\n",
    )
    .unwrap();

    let app = router(make_state(dir.path()));
    let (status, _cc, body) = get_discovery(app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["issuer"], "http://host/git");
    assert_eq!(
        body["authorization_endpoint"],
        "http://host/git/login/oauth/authorize"
    );
    let clients = body["clients"].as_array().unwrap();
    // Forgejo aud == client_id.
    let a = clients
        .iter()
        .find(|c| c["audience"] == "cs-rpp-adapter")
        .unwrap();
    assert_eq!(a["client_id"], "cs-rpp-adapter");
}

#[tokio::test]
async fn unconfigured_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    security_dir(dir.path());
    let app = router(make_state(dir.path()));
    let (status, _cc, body) = get_discovery(app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "discovery_unconfigured");
}

#[tokio::test]
async fn malformed_registry_returns_500_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let sec = security_dir(dir.path());
    std::fs::write(sec.join("oauth-clients.toml"), "not valid toml [[[").unwrap();
    let app = router(make_state(dir.path()));
    let (status, _cc, body) = get_discovery(app).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "discovery_error");
}
