// SPDX-License-Identifier: AGPL-3.0-only

//! B2 bounded drain — `POST /v1/molecules/:id/run` integration tests
//! (ADR-124).
//!
//! Scenarios:
//!
//! 1. Happy path: JWT with `write+spawn` → 202, body carries the
//!    server-resolved bounds (defaults — never unbounded), and the
//!    detached loop publishes `drain.started` then `drain.terminated`
//!    with reason `drained` on the events bus.
//! 2. Named bound exit: a pinned `cs run` exit 90 surfaces as the
//!    stable `budget_exhausted` token (B3 mirror).
//! 3. Single slot per noyau: a second `run` while the loop is held
//!    open → 409 `drain_already_active`; after termination the slot
//!    is reusable.
//! 4. Missing `cosmon:worker:spawn` scope → 403 (a drain burns
//!    credit — same composed grid as tackle).

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
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

fn run_request(jwt: &str, id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/molecules/{id}/run"))
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Content-Type", "application/json")
        .body(Body::empty())
        .unwrap()
}

/// Await the next event with the given name on the bus, with a hard
/// timeout so a missing publication fails the test instead of hanging.
async fn await_event(
    rx: &mut tokio::sync::broadcast::Receiver<cosmon_rpp_adapter::MoleculeEvent>,
    name: &str,
) -> cosmon_rpp_adapter::MoleculeEvent {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let ev = rx.recv().await.expect("events bus closed");
            if ev.event == name {
                return ev;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no `{name}` event within 15s"))
}

#[tokio::test]
async fn run_returns_202_and_terminates_drained() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260610-root", &json!({}))
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
    let mut rx = state.events.subscribe();
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &[
            "cosmon:molecule:read",
            "cosmon:molecule:write",
            "cosmon:worker:spawn",
        ],
        lifetime_secs: Some(60),
        jti: Some("jti-run-1"),
    });

    let resp = app
        .oneshot(run_request(&jwt, "task-20260610-root"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.get("request_id").is_some(), "missing request_id");
    assert_eq!(body["drain"]["root"], "task-20260610-root");
    assert_eq!(body["drain"]["status"], "started");
    // Server defaults (task-20260610-e5f6): a tenant drain is never
    // unbounded — absent [drain_bounds] resolves to 128/8/256.
    assert_eq!(body["drain"]["bounds"]["budget"], 128);
    assert_eq!(body["drain"]["bounds"]["max_depth"], 8);
    assert_eq!(body["drain"]["bounds"]["max_molecules"], 256);

    let started = await_event(&mut rx, "drain.started").await;
    assert_eq!(started.molecule_id, "task-20260610-root");
    assert_eq!(started.data["bounds"]["budget"], 128);

    let terminated = await_event(&mut rx, "drain.terminated").await;
    assert_eq!(terminated.molecule_id, "task-20260610-root");
    assert_eq!(terminated.data["reason"], "drained");
}

#[tokio::test]
async fn pinned_budget_exit_surfaces_stable_token() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    let state_json = tenant_a
        .insert_molecule("task-20260610-b3", &json!({}))
        .unwrap();
    // Pin the fake-cs drain to the B3 exit (90 → budget_exhausted).
    std::fs::write(state_json.parent().unwrap().join("drain-exit"), "90").unwrap();

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
    let mut rx = state.events.subscribe();
    let app = router(state);

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write", "cosmon:worker:spawn"],
        lifetime_secs: Some(60),
        jti: Some("jti-run-b3"),
    });

    let resp = app
        .oneshot(run_request(&jwt, "task-20260610-b3"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let terminated = await_event(&mut rx, "drain.terminated").await;
    assert_eq!(
        terminated.data["reason"], "budget_exhausted",
        "exit 90 must surface as the stable B3 token",
    );
}

#[tokio::test]
async fn second_run_while_active_is_409_then_slot_reusable() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    let state_json = tenant_a
        .insert_molecule("task-20260610-hold", &json!({}))
        .unwrap();
    let mol_dir = state_json.parent().unwrap().to_path_buf();
    // Hold the drain open so the slot stays claimed.
    std::fs::write(mol_dir.join("drain-hold"), "1").unwrap();

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
    let mut rx = state.events.subscribe();
    let app = router(state.clone());

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write", "cosmon:worker:spawn"],
        lifetime_secs: Some(60),
        jti: Some("jti-run-hold"),
    });

    let resp = app
        .clone()
        .oneshot(run_request(&jwt, "task-20260610-hold"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let _ = await_event(&mut rx, "drain.started").await;

    // Second drain on the same noyau while the first holds the slot.
    let resp2 = app
        .clone()
        .oneshot(run_request(&jwt, "task-20260610-hold"))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(resp2.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "drain_already_active");

    // Release the hold; the loop exits and the slot frees up.
    std::fs::remove_file(mol_dir.join("drain-hold")).unwrap();
    let terminated = await_event(&mut rx, "drain.terminated").await;
    assert_eq!(terminated.data["reason"], "drained");
    assert!(
        !state.drains.is_active("a"),
        "slot must be released after termination",
    );

    let resp3 = app
        .oneshot(run_request(&jwt, "task-20260610-hold"))
        .await
        .unwrap();
    assert_eq!(resp3.status(), StatusCode::ACCEPTED, "slot reusable");
}

#[tokio::test]
async fn missing_spawn_scope_returns_403() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260610-noscope", &json!({}))
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
    let drains = Arc::clone(&state.drains);
    let app = router(state);

    // Write scope only — no worker:spawn. A drain burns credit; the
    // composed grid must refuse exactly like tackle.
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-run-noscope"),
    });

    let resp = app
        .oneshot(run_request(&jwt, "task-20260610-noscope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        !drains.is_active("a"),
        "refused request must not claim the drain slot",
    );
}
