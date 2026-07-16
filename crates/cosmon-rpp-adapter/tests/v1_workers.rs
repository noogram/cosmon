// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `GET /v1/workers`.
//!
//! Five scenarios:
//!
//! 1. Valid JWT with `cosmon:worker:read`, no workers in the noyau →
//!    200 with `{ workers: [], count: 0 }`.
//! 2. Valid JWT, two molecules carrying a live `MoleculeProcess` record
//!    → 200 with both workers, sorted by `started_at` ascending and
//!    the wire schema matches the brief.
//! 3. Missing `Authorization` header → 401.
//! 4. JWT lacks `cosmon:worker:read` (only `cosmon:molecule:read`) →
//!    403; the route does NOT silently fall back to molecule scopes.
//! 5. Cross-tenant: a noyau-A JWT cannot see noyau-B workers. Plant a
//!    worker in B, call with A's JWT → 200 with `workers: []` (A's
//!    own noyau is empty).

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

/// Build the partial JSON object that plants a `process` record on a
/// molecule. The testkit's `insert_molecule` merges this on top of the
/// default `MoleculeData` envelope, so we only have to supply the
/// fields the route reads.
fn process_override(
    worker_id: &str,
    tmux_session: &str,
    started_at_iso: &str,
    pid: Option<u32>,
) -> Value {
    let pid_value = match pid {
        Some(p) => serde_json::Value::from(p),
        None => Value::Null,
    };
    serde_json::json!({
        "status": "running",
        "process": {
            "worker_id": worker_id,
            "tmux_session": tmux_session,
            "started_at": started_at_iso,
            "status": "active",
            "pid": pid_value,
            "adapter_name": "claude",
        }
    })
}

#[tokio::test]
async fn empty_noyau_returns_200_and_empty_array() {
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

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:worker:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-workers-empty"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/workers")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["count"], 0);
    assert_eq!(body["workers"], serde_json::json!([]));
    assert!(body["request_id"].is_string());
}

#[tokio::test]
async fn two_active_workers_returned_in_started_at_order() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    // Plant two molecules with bound processes. The older one's
    // `started_at` MUST be earlier; the route sorts ascending so it
    // appears first.
    tenant_a
        .insert_molecule(
            "task-20260523-aaaa",
            &process_override(
                "older-worker",
                "older-worker",
                "2026-05-23T08:00:00+00:00",
                Some(11_111),
            ),
        )
        .unwrap();
    tenant_a
        .insert_molecule(
            "task-20260523-bbbb",
            &process_override(
                "newer-worker",
                "newer-worker",
                "2026-05-23T09:00:00+00:00",
                Some(22_222),
            ),
        )
        .unwrap();
    // A third molecule with no bound process — must be ignored.
    tenant_a
        .insert_molecule("task-20260523-cccc", &serde_json::json!({}))
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

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:worker:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-workers-two"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/workers")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["count"], 2);
    let workers = body["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 2);

    // Sorted ascending by `started_at`.
    assert_eq!(workers[0]["molecule_id"], "task-20260523-aaaa");
    assert_eq!(workers[0]["session_name"], "older-worker");
    assert_eq!(workers[0]["tmux_session"], "older-worker");
    assert_eq!(workers[0]["pid"], 11_111);
    assert!(workers[0]["started_at"]
        .as_str()
        .unwrap()
        .starts_with("2026-05-23T08:00:00"));

    assert_eq!(workers[1]["molecule_id"], "task-20260523-bbbb");
    assert_eq!(workers[1]["session_name"], "newer-worker");
    assert_eq!(workers[1]["tmux_session"], "newer-worker");
    assert_eq!(workers[1]["pid"], 22_222);
    assert!(workers[1]["started_at"]
        .as_str()
        .unwrap()
        .starts_with("2026-05-23T09:00:00"));
}

#[tokio::test]
async fn missing_authorization_header_returns_401() {
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
                .uri("/v1/workers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn jwt_without_worker_read_scope_returns_403() {
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

    // JWT carries cosmon:molecule:read + cosmon:molecule:write — but
    // NOT cosmon:worker:read. The worker surface MUST refuse it; the
    // whole point of the scope is to keep tenants that only need
    // molecule state away from session-level facts.
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read", "cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-workers-403"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/workers")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_tenant_isolation_a_cannot_see_b_workers() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    let tenant_b = tenants.add("b");
    tenant_b
        .insert_molecule(
            "task-20260523-bbbb",
            &process_override(
                "b-worker",
                "b-worker",
                "2026-05-23T10:00:00+00:00",
                Some(33_333),
            ),
        )
        .unwrap();

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

    // sub-a → noyau "a" → MUST not see "b"'s worker.
    let jwt_a = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:worker:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-workers-cross-a"),
    });

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/workers")
                .header("Authorization", format!("Bearer {jwt_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["count"], 0);
    assert_eq!(body["workers"], serde_json::json!([]));

    // Sanity: sub-b → noyau "b" → DOES see its own worker.
    let jwt_b = oidc.issue(&IssueJwt {
        subject: "sub-b",
        audience: Some("cosmon-rpp-b"),
        scopes: &["cosmon:worker:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-workers-cross-b"),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/workers")
                .header("Authorization", format!("Bearer {jwt_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["count"], 1);
    assert_eq!(body["workers"][0]["molecule_id"], "task-20260523-bbbb");
}
