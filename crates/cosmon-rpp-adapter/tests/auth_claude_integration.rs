// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the auth-claude surface (ADR-0017 smithy,
//! protocol spec v1.1 PKCE manual-paste).
//!
//! Mock Anthropic is a small axum server running on an ephemeral port
//! that emulates the `POST /v1/oauth/token` endpoint with success and
//! failure modes. The cs-rpp-adapter router under test is configured
//! to point its token URL at the mock.

#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_lines)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use cosmon_oidc_testkit::TenantWorkspaces;
use cosmon_rpp_adapter::auth_claude::{
    AuthClaudeConfig, AuthClaudeState, FilesystemSessionStore, SessionStore,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::jwt::JwksStore;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, IngressRateLimiter, Posture};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt;

/// Outcome the mock returns on its single `/v1/oauth/token` endpoint.
#[derive(Clone)]
enum MockOutcome {
    /// Standard success envelope.
    Success {
        access_token: String,
        refresh_token: String,
        expires_in: Option<u64>,
        account_email: Option<String>,
        subscription_type: Option<String>,
    },
    /// OAuth 2.0 error envelope with the given status + error code.
    OAuthError { status: u16, error: String },
    /// Mock returns 503 (simulates unreachable upstream).
    ServerError,
}

#[derive(Clone)]
struct MockState {
    outcome: Arc<Mutex<MockOutcome>>,
    /// JSON bodies received, for assertion in the test. The official CLI
    /// POSTs `application/json` (claude-code v2.1.88), so the mock parses
    /// the body as JSON — a form-encoded body would fail to parse here,
    /// which is itself the contract under test (ADR-0017 §13).
    received: Arc<Mutex<Vec<Value>>>,
}

async fn mock_token_handler(
    State(state): State<MockState>,
    body: String,
) -> axum::response::Response {
    let parsed: Value = serde_json::from_str(&body).unwrap_or_else(|e| {
        panic!("token-exchange body must be valid JSON (not form-encoded): {e}; raw={body:?}")
    });
    state.received.lock().await.push(parsed);
    let outcome = state.outcome.lock().await.clone();
    match outcome {
        MockOutcome::Success {
            access_token,
            refresh_token,
            expires_in,
            account_email,
            subscription_type,
        } => {
            let mut body = json!({
                "access_token": access_token,
                "refresh_token": refresh_token,
                "scope": "user:profile user:inference user:sessions:claude_code",
            });
            if let Some(e) = expires_in {
                body["expires_in"] = json!(e);
            }
            if let Some(email) = account_email {
                body["account"] = json!({ "email_address": email });
            }
            if let Some(s) = subscription_type {
                body["subscription_type"] = json!(s);
            }
            (StatusCode::OK, axum::Json(body)).into_response()
        }
        MockOutcome::OAuthError { status, error } => {
            let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
            let body = json!({ "error": error, "error_description": format!("mock: {error}") });
            (status_code, axum::Json(body)).into_response()
        }
        MockOutcome::ServerError => {
            (StatusCode::SERVICE_UNAVAILABLE, "upstream gone").into_response()
        }
    }
}

/// Start the mock Anthropic server. Returns the bound address and a
/// handle to mutate the outcome / inspect received requests.
async fn start_mock_anthropic(initial: MockOutcome) -> (SocketAddr, MockState) {
    let state = MockState {
        outcome: Arc::new(Mutex::new(initial)),
        received: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/oauth/token", post(mock_token_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

/// Build an `AppState` whose auth-claude surface is wired to the mock
/// Anthropic at `token_url` and writes credentials to `home_dir`.
fn make_state(security_dir: &Path, home_dir: &Path, token_url: String) -> AppState {
    let nucleon_map = HabilitationMap::builder().build();
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));
    let tenants = TenantWorkspaces::new();
    let jwks_dir = security_dir.join("jwks");
    std::fs::create_dir_all(&jwks_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(FilesystemSessionStore::new(security_dir).unwrap());
    let config = AuthClaudeConfig::defaults_with_home(home_dir).with_token_url(token_url);
    let auth_claude = Some(Arc::new(AuthClaudeState::new(config, store)));

    AppState {
        cs_path: std::path::PathBuf::from("/bin/false"),
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
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude,
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

async fn get_json(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn happy_path_start_email_confirm_writes_credentials() {
    let (mock_addr, mock) = start_mock_anthropic(MockOutcome::Success {
        access_token: "sk-ant-oat01-test-access".to_owned(),
        refresh_token: "sk-ant-ort01-test-refresh".to_owned(),
        expires_in: Some(31_536_000),
        account_email: Some("operator@example.com".to_owned()),
        subscription_type: Some("max".to_owned()),
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    // 1. POST /start → session_id
    let (status, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "start should return 200; body={body}"
    );
    let session_id = body["session_id"].as_str().expect("session_id").to_owned();
    assert!(session_id.starts_with("auth-"));
    assert_eq!(body["state"], "AWAITING_EMAIL");
    // Contract net (replay-Dave D1, task-20260610-828e): the REAL
    // route bytes must deserialise into the cosmon-remote client struct.
    // The CLI shipped with a hand-mirrored `expires_at` field, validated
    // only against wiremock fixtures, and died on the first `/start` of
    // every `auth login`. This is the test that would have caught it.
    let start: cosmon_remote::client::AuthStartResponse =
        serde_json::from_value(body.clone()).expect("client must parse the real /start body");
    assert_eq!(start.session_id, session_id);
    assert!(!start.ttl_at.is_empty());

    // 2. POST /email → verification_url + oauth_state
    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "operator@example.com" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "email should return 200; body={body}"
    );
    assert_eq!(body["state"], "AWAITING_USER_APPROVAL");
    let verification_url = body["verification_url"].as_str().unwrap();
    assert!(verification_url.contains("code_challenge="));
    assert!(verification_url.contains("code_challenge_method=S256"));
    assert!(verification_url.contains("response_type=code"));
    let oauth_state = body["oauth_state"].as_str().unwrap().to_owned();
    assert!(!oauth_state.is_empty());
    // Same contract net for `/email` (this one carries `expires_at`,
    // the PKCE deadline — distinct from the session `ttl_at`).
    let email: cosmon_remote::client::AuthEmailResponse =
        serde_json::from_value(body.clone()).expect("client must parse the real /email body");
    assert_eq!(email.verification_url, verification_url);
    assert!(!email.expires_at.is_empty());

    // 3. GET — session view shows verification_url + state
    let (status, body) =
        get_json(&app, "GET", &format!("/v1/auth/claude/{session_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "AWAITING_USER_APPROVAL");
    assert_eq!(body["oauth_state"].as_str().unwrap(), oauth_state);

    // 4. POST /confirm with `authorizationCode#state` → token exchange →
    //    COMPLETED. The pasted code carries the CSRF state after a '#',
    //    exactly as Anthropic's manual-redirect page emits it.
    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/confirm",
        Some(json!({
            "session_id": session_id,
            "authorization_code": format!("AUTH_CODE_FROM_REDIRECT_PAGE#{oauth_state}")
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "confirm should return 200; body={body}"
    );
    assert_eq!(body["ok"], true);
    assert_eq!(body["state"], "COMPLETED");
    assert_eq!(body["account_email"], "operator@example.com");

    // 4b. The mock received a JSON token-exchange body carrying the split
    //     authorization_code AND the CSRF state — the claude-code v2.1.88
    //     contract (ADR-0017 §13). Form encoding would have panicked the
    //     mock's JSON parse above.
    let received = mock.received.lock().await;
    let token_req = received
        .last()
        .expect("mock should have received the token exchange");
    assert_eq!(token_req["grant_type"], "authorization_code");
    assert_eq!(token_req["code"], "AUTH_CODE_FROM_REDIRECT_PAGE");
    assert_eq!(
        token_req["state"], oauth_state,
        "token exchange body must echo the CSRF state"
    );
    assert!(
        token_req["code_verifier"].is_string(),
        "token exchange body must carry the PKCE code_verifier"
    );
    drop(received);

    // 5. Credentials file was written with correct shape
    let cred_path = home.path().join(".claude/.credentials.json");
    assert!(
        cred_path.exists(),
        "credentials file must exist at {cred_path:?}"
    );
    let cred: Value = serde_json::from_slice(&std::fs::read(&cred_path).unwrap()).unwrap();
    let oauth = cred.get("claudeAiOauth").unwrap();
    assert_eq!(oauth["accessToken"], "sk-ant-oat01-test-access");
    assert_eq!(oauth["refreshToken"], "sk-ant-ort01-test-refresh");
    assert_eq!(oauth["subscriptionType"], "max");
    assert!(oauth["expiresAt"].is_i64());

    // 6. GET — session is COMPLETED, no verification_url (state purged) — well,
    // we keep it in the on-disk record but the wire shape doesn't carry it
    // because the field is conditional on the wire state per the OpenAPI
    // SessionView schema. Our impl leaves it set for forensics; just verify
    // the state.
    let (status, body) =
        get_json(&app, "GET", &format!("/v1/auth/claude/{session_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "COMPLETED");

    // 7. DELETE — session removed
    let (status, _) = get_json(
        &app,
        "DELETE",
        &format!("/v1/auth/claude/{session_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get_json(&app, "GET", &format!("/v1/auth/claude/{session_id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn anthropic_invalid_grant_marks_session_failed() {
    let (mock_addr, _mock) = start_mock_anthropic(MockOutcome::OAuthError {
        status: 400,
        error: "invalid_grant".to_owned(),
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (_, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    let session_id = body["session_id"].as_str().unwrap().to_owned();
    let (_, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "x@y" })),
    )
    .await;
    let oauth_state = body["oauth_state"].as_str().unwrap().to_owned();

    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/confirm",
        Some(json!({ "session_id": session_id, "authorization_code": format!("bad-code#{oauth_state}") })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "token_exchange_failed");
    assert_eq!(body["error"]["current_state"], "FAILED");

    // Credentials file was NOT written
    assert!(
        !home.path().join(".claude/.credentials.json").exists(),
        "no credentials file should be created on token exchange failure",
    );
}

#[tokio::test]
async fn anthropic_unreachable_returns_500() {
    let (mock_addr, _mock) = start_mock_anthropic(MockOutcome::ServerError).await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (_, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    let session_id = body["session_id"].as_str().unwrap().to_owned();
    let (_, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "x@y" })),
    )
    .await;
    let oauth_state = body["oauth_state"].as_str().unwrap().to_owned();

    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/confirm",
        Some(json!({ "session_id": session_id, "authorization_code": format!("code#{oauth_state}") })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], "anthropic_unreachable");
}

#[tokio::test]
async fn email_on_session_in_wrong_state_returns_409() {
    let (mock_addr, _) = start_mock_anthropic(MockOutcome::Success {
        access_token: "a".into(),
        refresh_token: "b".into(),
        expires_in: None,
        account_email: None,
        subscription_type: None,
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (_, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    let session_id = body["session_id"].as_str().unwrap().to_owned();
    // First email — OK
    let (status, _) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "x@y" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Second email — 409 (already in AWAITING_USER_APPROVAL)
    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "z@y" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "session_state_mismatch");
    assert_eq!(body["error"]["current_state"], "AWAITING_USER_APPROVAL");
}

#[tokio::test]
async fn confirm_on_session_not_yet_awaiting_returns_409() {
    let (mock_addr, _) = start_mock_anthropic(MockOutcome::Success {
        access_token: "a".into(),
        refresh_token: "b".into(),
        expires_in: None,
        account_email: None,
        subscription_type: None,
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (_, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    let session_id = body["session_id"].as_str().unwrap().to_owned();
    // Confirm without email first — session is still in INIT (AWAITING_EMAIL)
    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/confirm",
        Some(json!({ "session_id": session_id, "authorization_code": "code" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["current_state"], "AWAITING_EMAIL");
}

#[tokio::test]
async fn confirm_with_code_missing_hash_separator_returns_400() {
    // The pasted code MUST be `authorizationCode#state`. A bare code with
    // no '#' is rejected with 400 invalid_request — matching the official
    // CLI's `split('#')` + both-halves-required contract (ADR-0017 §13).
    let (mock_addr, mock) = start_mock_anthropic(MockOutcome::Success {
        access_token: "a".into(),
        refresh_token: "b".into(),
        expires_in: None,
        account_email: None,
        subscription_type: None,
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (_, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    let session_id = body["session_id"].as_str().unwrap().to_owned();
    get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "x@y" })),
    )
    .await;

    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/confirm",
        Some(json!({ "session_id": session_id, "authorization_code": "code-without-hash" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    // Session stays callable — no transition to FAILED, no token exchange.
    assert_eq!(body["error"]["current_state"], "AWAITING_USER_APPROVAL");
    assert!(
        mock.received.lock().await.is_empty(),
        "no token exchange must be attempted on a malformed code"
    );
}

#[tokio::test]
async fn confirm_with_mismatched_state_returns_400() {
    // The state echoed after '#' must match the session's oauth_state.
    // A CSRF mismatch is rejected with 400 before any token exchange.
    let (mock_addr, mock) = start_mock_anthropic(MockOutcome::Success {
        access_token: "a".into(),
        refresh_token: "b".into(),
        expires_in: None,
        account_email: None,
        subscription_type: None,
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (_, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    let session_id = body["session_id"].as_str().unwrap().to_owned();
    let (_, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/email",
        Some(json!({ "session_id": session_id, "email": "x@y" })),
    )
    .await;
    let oauth_state = body["oauth_state"].as_str().unwrap().to_owned();

    let (status, body) = get_json(
        &app,
        "POST",
        "/v1/auth/claude/confirm",
        Some(json!({
            "session_id": session_id,
            "authorization_code": format!("AUTH_CODE#{oauth_state}-TAMPERED")
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(body["error"]["current_state"], "AWAITING_USER_APPROVAL");
    assert!(
        mock.received.lock().await.is_empty(),
        "no token exchange must be attempted on a state mismatch"
    );
}

#[tokio::test]
async fn missing_session_returns_404() {
    let (mock_addr, _) = start_mock_anthropic(MockOutcome::Success {
        access_token: "a".into(),
        refresh_token: "b".into(),
        expires_in: None,
        account_email: None,
        subscription_type: None,
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state = make_state(
        security.path(),
        home.path(),
        format!("http://{mock_addr}/v1/oauth/token"),
    );
    let app = router(state);

    let (status, body) = get_json(&app, "GET", "/v1/auth/claude/auth-20260519-zzzzzz", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "session_not_found");
}

#[tokio::test]
async fn auth_claude_disabled_returns_503() {
    // Build state with auth_claude = None
    let security = tempfile::tempdir().unwrap();
    let tenants = TenantWorkspaces::new();
    let nucleon_map = HabilitationMap::builder().build();
    let rate_limiter = IngressRateLimiter::new(security.path().join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security.path().to_path_buf()).with_ttl(Duration::from_secs(0));
    let jwks = JwksStore::load(security.path()).unwrap();
    let state = AppState {
        cs_path: std::path::PathBuf::from("/bin/false"),
        state_dir: security.path().to_path_buf(),
        inbox_root: security.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(5),
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
    };
    let app = router(state);

    let (status, body) = get_json(&app, "POST", "/v1/auth/claude/start", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "service_unavailable");
}

#[tokio::test]
async fn molecule_routes_unaffected_by_auth_claude_addition() {
    // Regression — adding auth-claude must not break the existing
    // routes. Start adapter with no auth_claude state and ensure
    // `GET /v1/molecules/<id>` still rejects with 401 (no JWT).
    let security = tempfile::tempdir().unwrap();
    let tenants = TenantWorkspaces::new();
    let nucleon_map = HabilitationMap::builder().build();
    let rate_limiter = IngressRateLimiter::new(security.path().join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security.path().to_path_buf()).with_ttl(Duration::from_secs(0));
    let jwks = JwksStore::load(security.path()).unwrap();
    let state = AppState {
        cs_path: std::path::PathBuf::from("/bin/false"),
        state_dir: security.path().to_path_buf(),
        inbox_root: security.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(5),
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
    };
    let app = router(state);

    let (status, _) = get_json(&app, "GET", "/v1/molecules/task-99999999-abcd", None).await;
    // Missing JWT → 401.
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
