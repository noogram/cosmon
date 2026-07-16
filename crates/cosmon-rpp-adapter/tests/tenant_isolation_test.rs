// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical tenant isolation test — clause (e) of §8j.
//!
//! The innocuité concern turns on a single invariant: a JWT issued for
//! `noyau=A` cannot read state owned by `noyau=B`. The GET read path is
//! library-direct, so the structural defence is the
//! per-tenant `FileStore`: the route resolves the store root from the
//! admitted `noyau` (`<galaxies_root>/<noyau>/.cosmon/state`), so a
//! JWT for noyau A reads only tenant A's tree and can never name a
//! molecule that lives under `<galaxies_root>/b/.cosmon/state/`. This
//! integration test plants identical molecule ids in two parallel
//! tenants, then proves a JWT for noyau A *only* reads tenant A's
//! content — never tenant B's.
//!
//! The fixture is the [`cosmon-oidc-testkit`] crate. `OidcMock`
//! produces JWTs and a JWKS file the adapter loads at boot;
//! [`TenantPath::insert_molecule`] plants the canonical `MoleculeData`
//! envelope the library-direct `cosmon_state::ops::observe` resolves.
//! Molecule ids therefore obey the `MoleculeId` grammar
//! (`PREFIX-YYYYMMDD-XXXX`) — a malformed id collapses to 404 at the
//! route boundary (turing §8.2.3, no existence oracle) before the
//! store is ever touched — and the distinguishing per-tenant marker
//! rides in `variables`, the only free-form field the canonical
//! `ObserveJson` wire shape echoes back.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use cosmon_oidc_testkit::{fake_cs_path, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::Value;
use tower::ServiceExt;

/// Build an [`AppState`] wired to the testkit primitives. JWKS is
/// loaded from the on-disk projection so the production loader code
/// runs unchanged.
fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<(&str, &str, &str, &str)>,
    jwks_state_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(jwks_state_dir).unwrap();
    let jwks = JwksStore::load(jwks_state_dir).unwrap();

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
    let nucleon_map = builder.build();

    let rate_limiter = IngressRateLimiter::new(jwks_state_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(jwks_state_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: fake_cs_path(),
        state_dir: jwks_state_dir.to_path_buf(),
        inbox_root: jwks_state_dir.join("whispers/inbox"),
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
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // canonical narrative — keep linear.
async fn jwt_for_noyau_a_cannot_read_noyau_b() {
    // 1. Tenant fixture — two noyaus, identical molecule ids.
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    let tenant_b = tenants.add("b");
    // Identical ids in both tenants — the marker that distinguishes
    // them rides in `variables` (the only free-form field the
    // canonical `ObserveJson` wire shape echoes; a top-level `owner`
    // key is dropped when the envelope deserializes into the typed
    // `MoleculeData`). Ids obey `PREFIX-YYYYMMDD-XXXX`.
    tenant_a
        .insert_molecule(
            "task-20260520-shrd",
            &serde_json::json!({"variables": {"owner": "noyau-a"}}),
        )
        .unwrap();
    tenant_b
        .insert_molecule(
            "task-20260520-shrd",
            &serde_json::json!({"variables": {"owner": "noyau-b"}}),
        )
        .unwrap();
    // task-20260520-onlb is the smoking gun: present under galaxies/b/
    // and absent from galaxies/a/. A leak would surface its body;
    // isolation makes it 404.
    tenant_b
        .insert_molecule(
            "task-20260520-onlb",
            &serde_json::json!({"variables": {"secret": "true"}}),
        )
        .unwrap();

    // 2. OIDC mock — one issuer, two tenant audiences.
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned(), "cosmon-rpp-b".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    // 3. Adapter state — security/jwks lives in a separate TempDir so
    //    the JWKS loader's directory enumeration does not collide
    //    with the tenant trees.
    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![
            ("sub-a", "nuc-a", "a", "cosmon-rpp-a"),
            ("sub-b", "nuc-b", "b", "cosmon-rpp-b"),
        ],
        security_dir.path(),
    );
    let app = router(state);

    // 4. JWT for sub-a → admitted as noyau-a. The adapter's
    //    subprocess invoker pins cwd to galaxies_root/a/.
    let jwt_a = oidc.issue(&cosmon_oidc_testkit::IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-a-1"),
    });

    // 5a. Reading task-shared with jwt_a returns tenant A's body —
    //     not tenant B's, despite both having the same id.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260520-shrd")
                .header("Authorization", format!("Bearer {jwt_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body_text = String::from_utf8_lossy(&body_bytes).to_string();
    assert_eq!(status, StatusCode::OK, "body was: {body_text}");
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        body["molecule"]["variables"]["owner"], "noyau-a",
        "tenant A's JWT must surface tenant A's molecule body — leak detected if this fails"
    );

    // 5b. The smoking gun: task-only-in-b is invisible to jwt_a
    //     because the subprocess `cwd` is rooted at galaxies/a/.
    let jwt_a2 = oidc.issue(&cosmon_oidc_testkit::IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-a-2"),
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260520-onlb")
                .header("Authorization", format!("Bearer {jwt_a2}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "tenant A's JWT must NOT see tenant B's molecule — clause (e) breach if 200"
    );

    // 5c. Symmetric proof — sub-b → noyau-b reads tenant B's body.
    let jwt_b = oidc.issue(&cosmon_oidc_testkit::IssueJwt {
        subject: "sub-b",
        audience: Some("cosmon-rpp-b"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-b-1"),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260520-shrd")
                .header("Authorization", format!("Bearer {jwt_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body_text = String::from_utf8_lossy(&body_bytes).to_string();
    assert_eq!(status, StatusCode::OK, "body was: {body_text}");
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["molecule"]["variables"]["owner"], "noyau-b");
}

#[tokio::test]
async fn unknown_audience_collapses_to_401() {
    // Defence-in-depth: even with a valid signature, an audience the
    // JWKS file does not pin must reject at JWT validation.
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260520-shrd",
            &serde_json::json!({"variables": {"owner": "noyau-a"}}),
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
    );
    let app = router(state);

    // Mint a token for an audience the JWKS file did not pin.
    let jwt_unknown = oidc.issue(&cosmon_oidc_testkit::IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-evil"),
        scopes: &[],
        lifetime_secs: Some(60),
        jti: Some("jti-unknown"),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-20260520-shrd")
                .header("Authorization", format!("Bearer {jwt_unknown}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// MCP surface (`/mcp`) — the M2 tenant-isolation seam.
//
// The `/mcp` Streamable-HTTP surface exposes the same molecule store through
// MCP tools that carry a `cwd` parameter (the stdio walk-up hook). On the
// multi-tenant HTTP path that parameter is a tenant-spoofing vector: a client
// could name another noyau's filesystem path. These tests prove the seam
// closes the door — the state directory is resolved ONLY from the validated
// JWT's noyau, and a client-supplied `cwd` is inert.
// ---------------------------------------------------------------------------

/// Both MIME types the Streamable-HTTP transport requires on POST.
const MCP_ACCEPT: &str = "application/json, text/event-stream";

/// Extract the first JSON-RPC message from an SSE (`text/event-stream`) body,
/// falling back to a plain-JSON parse for stateless-mode responses.
fn parse_mcp_body(bytes: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("data:") {
            let rest = rest.trim();
            if !rest.is_empty() {
                if let Ok(v) = serde_json::from_str::<Value>(rest) {
                    return Some(v);
                }
            }
        }
    }
    serde_json::from_slice(bytes).ok()
}

/// Open an MCP session over the in-process router: send `initialize`, capture
/// the `Mcp-Session-Id`, then send the `notifications/initialized` handshake.
/// Returns the session id every subsequent request must echo.
async fn mcp_open_session(app: &Router, jwt: &str) -> String {
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "tenant-isolation-test", "version": "0.0.0" }
        }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::from(init.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a valid JWT must clear the /mcp gate"
    );
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .expect("initialize must return an Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_owned();
    // Drain the initialize SSE body so the stream closes.
    let _ = to_bytes(resp.into_body(), 1 << 20).await.unwrap();

    // Complete the handshake: notifications/initialized.
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("Authorization", format!("Bearer {jwt}"))
                .header("mcp-session-id", &session_id)
                .body(Body::from(initialized.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = to_bytes(resp.into_body(), 1 << 20).await.unwrap();

    session_id
}

/// Invoke one MCP tool and return the decoded JSON-RPC message.
async fn mcp_call_tool(
    app: &Router,
    jwt: &str,
    session_id: &str,
    tool: &str,
    arguments: Value,
) -> Value {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("Authorization", format!("Bearer {jwt}"))
                .header("mcp-session-id", session_id)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "tool call with a valid JWT must clear the gate"
    );
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    parse_mcp_body(&bytes).unwrap_or_else(|| {
        panic!(
            "could not decode MCP body: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// Pull the flattened text of a successful `tools/call` result, if any.
///
/// A cosmon read tool answers with `result.content[].text` carrying the
/// molecule JSON; a not-found / cross-tenant miss answers with a JSON-RPC
/// `error` (no `result`). Returns `None` in the error case so the caller can
/// assert the leak did not happen.
fn tool_result_text(msg: &Value) -> Option<String> {
    let content = msg.get("result")?.get("content")?.as_array()?;
    let mut out = String::new();
    for item in content {
        if let Some(t) = item.get("text").and_then(Value::as_str) {
            out.push_str(t);
        }
    }
    Some(out)
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // canonical narrative — keep linear.
async fn mcp_jwt_for_noyau_a_cannot_read_noyau_b() {
    // 1. Two tenants, identical + exclusive molecule ids (mirrors the REST
    //    canonical test above).
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    let tenant_b = tenants.add("b");
    tenant_a
        .insert_molecule(
            "task-20260520-shrd",
            &serde_json::json!({"variables": {"owner": "noyau-a"}}),
        )
        .unwrap();
    tenant_b
        .insert_molecule(
            "task-20260520-shrd",
            &serde_json::json!({"variables": {"owner": "noyau-b"}}),
        )
        .unwrap();
    tenant_b
        .insert_molecule(
            "task-20260520-onlb",
            &serde_json::json!({"variables": {"secret": "top-secret-b"}}),
        )
        .unwrap();
    // The client-supplied `cwd` the spoofing attempt will pass: tenant B's
    // real root. Walk-up from here WOULD resolve B's state — the pin must
    // override it.
    let tenant_b_root = tenant_b.root.to_string_lossy().to_string();

    // 2. OIDC + adapter state — same shape as the REST test.
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned(), "cosmon-rpp-b".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![
            ("sub-a", "nuc-a", "a", "cosmon-rpp-a"),
            ("sub-b", "nuc-b", "b", "cosmon-rpp-b"),
        ],
        security_dir.path(),
    );
    let app = router(state);

    // 3. Session for noyau A.
    let jwt_a = oidc.issue(&cosmon_oidc_testkit::IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-a"),
    });
    let session_a = mcp_open_session(&app, &jwt_a).await;

    // 4a. Read the SHARED id while spoofing `cwd` at tenant B. The pin wins:
    //     A's body is returned, never B's — proving the cwd is inert.
    let msg = mcp_call_tool(
        &app,
        &jwt_a,
        &session_a,
        "cosmon_get",
        serde_json::json!({ "id": "task-20260520-shrd", "cwd": tenant_b_root }),
    )
    .await;
    let text = tool_result_text(&msg)
        .unwrap_or_else(|| panic!("expected a molecule body for A's own id, got: {msg}"));
    assert!(
        text.contains("noyau-a"),
        "tenant A's session must surface tenant A's molecule — got: {text}"
    );
    assert!(
        !text.contains("noyau-b"),
        "client-supplied cwd pointing at tenant B leaked B's body — cwd is NOT inert: {text}"
    );

    // 4b. Read the id that exists ONLY in tenant B, again spoofing `cwd` at B.
    //     Isolation makes this a not-found / error under A's pinned tree —
    //     the secret must never surface.
    let msg = mcp_call_tool(
        &app,
        &jwt_a,
        &session_a,
        "cosmon_get",
        serde_json::json!({ "id": "task-20260520-onlb", "cwd": tenant_b_root }),
    )
    .await;
    assert!(
        tool_result_text(&msg).is_none(),
        "tenant A read tenant B's exclusive molecule via /mcp — clause (e) breach: {msg}"
    );
    assert!(
        !msg.to_string().contains("top-secret-b"),
        "tenant B's secret leaked through the /mcp cwd vector: {msg}"
    );

    // 5. Symmetric proof — noyau B's session reads B's body for the shared id.
    let jwt_b = oidc.issue(&cosmon_oidc_testkit::IssueJwt {
        subject: "sub-b",
        audience: Some("cosmon-rpp-b"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-mcp-b"),
    });
    let session_b = mcp_open_session(&app, &jwt_b).await;
    let msg = mcp_call_tool(
        &app,
        &jwt_b,
        &session_b,
        "cosmon_get",
        // Spoof `cwd` at tenant A this time — B must still read B.
        serde_json::json!({ "id": "task-20260520-shrd", "cwd": tenant_a.root.to_string_lossy() }),
    )
    .await;
    let text = tool_result_text(&msg)
        .unwrap_or_else(|| panic!("expected a molecule body for B's own id, got: {msg}"));
    assert!(
        text.contains("noyau-b") && !text.contains("noyau-a"),
        "tenant B's session must surface tenant B's molecule regardless of cwd — got: {text}"
    );
}
