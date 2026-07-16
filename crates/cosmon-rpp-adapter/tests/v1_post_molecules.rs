// SPDX-License-Identifier: AGPL-3.0-only

//! V1 mutation cut — `POST /v1/molecules` integration tests.
//!
//! Mirrors `v0_smoke.rs` but exercises the nucleate route added by
//! T-V1-MUTATIONS-NUCLEATE (2026-05-04). Cross-tenant isolation on the
//! POST path is verified to ensure a noyau-A JWT cannot persist a
//! molecule under noyau-B's `/srv/cosmon/<n>/.cosmon/state/` tree.
//!
//! Scenarios:
//!
//! 1. Valid JWT + `cosmon:molecule:write` scope → 201 + Location +
//!    `molecule.id` parses.
//! 2. JWT *without* `cosmon:molecule:write` scope → 403 + audit event.
//! 3. Tenant isolation — JWT for noyau A creates a molecule that lands
//!    under `/srv/cosmon/a/...`, never under `/srv/cosmon/b/...`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::StateStore as _;
use serde_json::{json, Value};
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
async fn happy_path_post_returns_201_with_id() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    // Library-direct nucleate (T-RPP-LIB-DIRECT) resolves the formula
    // from the tenant's own `.cosmon/formulas/` dir — there is no
    // fake-cs to fabricate an id. Install the minimal task-work formula
    // so the route finds it instead of 404 formula_not_found.
    tenant_a.install_task_work_formula().unwrap();

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
        scopes: &["cosmon:molecule:read", "cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-post-1"),
    });

    let body = json!({
        "formula": "task-work",
        "kind": "task",
        "variables": { "topic": "hello-operator-demo" },
        "tags": ["temp:warm"],
    });

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

    // Location header carries the new molecule id.
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .expect("Location header missing")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        location.starts_with("/v1/molecules/"),
        "unexpected Location: {location}"
    );

    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.get("request_id").is_some(), "missing request_id");
    let id = body["molecule"]["id"]
        .as_str()
        .expect("molecule.id missing or not a string");
    assert_eq!(location, format!("/v1/molecules/{id}"));
    // Library-direct nucleate mints the id from the formula's
    // `id_prefix = "task"` (see `minimal_task_work_formula`).
    assert!(id.starts_with("task-"), "expected task-id, got {id}");
}

#[tokio::test]
async fn missing_write_scope_returns_403() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

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

    // Read-only scope; no write.
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-post-no-write"),
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
}

#[tokio::test]
async fn missing_formula_returns_400() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

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
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-post-bad-body"),
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_without_jwt_returns_401() {
    let mut tenants = TenantWorkspaces::new();
    let _tenant_a = tenants.add("a");

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
                .method("POST")
                .uri("/v1/molecules")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"formula":"task-work"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tenant_isolation_post_lands_under_caller_noyau_only() {
    // JWT for noyau A POSTs a new molecule. The library-direct nucleate
    // resolves the per-tenant store from the admitted noyau
    // (`<galaxies_root>/a/.cosmon/state`), so the molecule lands under
    // tenant_a, NOT tenant_b.
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    let tenant_b = tenants.add("b");
    // Formula must be installed in the caller's tenant for the
    // library-direct nucleate to resolve it (else 404).
    tenant_a.install_task_work_formula().unwrap();

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

    let jwt_a = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-post-iso"),
    });

    let body = json!({"formula": "task-work"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt_a}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let id = body["molecule"]["id"]
        .as_str()
        .expect("molecule.id missing")
        .to_owned();

    // The molecule lands under tenant_a's tree, NOT tenant_b's.
    // Library-direct nucleate persists to the fleet-scoped FileStore
    // layout (`.cosmon/state/fleets/default/molecules/<id>/state.json`),
    // not the legacy flat `.cosmon/state/molecules/<id>/` path.
    let a_state = tenant_a
        .root
        .join(".cosmon/state/fleets/default/molecules")
        .join(&id)
        .join("state.json");
    let b_state = tenant_b
        .root
        .join(".cosmon/state/fleets/default/molecules")
        .join(&id)
        .join("state.json");
    assert!(
        a_state.exists(),
        "expected molecule state under tenant A: {}",
        a_state.display()
    );
    assert!(
        !b_state.exists(),
        "tenant isolation breach: molecule visible in tenant B at {}",
        b_state.display()
    );
}

// ── B1 moussage resident (task-20260610-e5f6) — DAG via the surface ────────

/// Shared helper: POST one molecule, return its id (asserts 201).
async fn post_one(app: axum::Router, jwt: &str, body: &Value) -> (axum::http::StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 65536).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

/// The E2E shape of the B1 gate, surface side: a root and 3 children
/// nucleated through `POST /v1/molecules` (the tenant CLI's wire path),
/// children linked `blocked_by` the root. Asserts the DAG lands on disk
/// with both edge directions — exactly what `compile_plan` (and thus a
/// resident `cs run <root>` inside the container) consumes.
#[tokio::test]
async fn dag_root_plus_three_children_via_blocked_by() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a.install_task_work_formula().unwrap();

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
        scopes: &["cosmon:molecule:read", "cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-dag-1"),
    });

    let (status, root_body) = post_one(
        app.clone(),
        &jwt,
        &json!({
            "formula": "task-work",
            "kind": "task",
            "variables": { "topic": "dag-root" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let root_id = root_body["molecule"]["id"].as_str().unwrap().to_owned();

    let mut child_ids = Vec::new();
    for i in 1..=3 {
        let (status, body) = post_one(
            app.clone(),
            &jwt,
            &json!({
                "formula": "task-work",
                "kind": "task",
                "variables": { "topic": format!("dag-child-{i}") },
                "blocked_by": [root_id.clone()],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "child {i} must nucleate");
        child_ids.push(body["molecule"]["id"].as_str().unwrap().to_owned());
    }

    // On-disk proof: read the tenant store the way the resident drain
    // would (same filesystem, same FileStore) and assert both edge
    // directions landed.
    let store = cosmon_filestore::FileStore::new(
        tenants
            .galaxies_root()
            .join("a")
            .join(".cosmon")
            .join("state"),
    );
    let root = store
        .load_molecule(&cosmon_core::id::MoleculeId::new(&root_id).unwrap())
        .expect("root persisted");
    for child_id in &child_ids {
        let cid = cosmon_core::id::MoleculeId::new(child_id).unwrap();
        let child = store.load_molecule(&cid).expect("child persisted");
        assert!(
            child.typed_links.iter().any(|l| matches!(
                l,
                cosmon_core::interaction::MoleculeLink::BlockedBy { source } if source.as_str() == root_id.as_str()
            )),
            "child {child_id} must carry BlockedBy(root)"
        );
        assert!(
            root.typed_links.iter().any(|l| matches!(
                l,
                cosmon_core::interaction::MoleculeLink::Blocks { target } if *target == cid
            )),
            "root must carry the symmetric Blocks({child_id})"
        );
    }
}

/// Dangling `blocked_by` refs are refused with the stable label —
/// never a half-formed DAG.
#[tokio::test]
async fn dangling_blocked_by_returns_404_named() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a.install_task_work_formula().unwrap();

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
        scopes: &["cosmon:molecule:read", "cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-dag-2"),
    });

    let (status, body) = post_one(
        app,
        &jwt,
        &json!({
            "formula": "task-work",
            "variables": { "topic": "orphan" },
            "blocked_by": ["task-20990101-zzzz"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"].as_str(), Some("blocked_by_not_found"));
}
