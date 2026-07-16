// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the per-noyau live-worker ceiling on
//! `POST /v1/molecules/{id}/tackle` (delib-20260709-943e M3, turing
//! exploit #3 defense).
//!
//! `cosmon:worker:spawn` proves the caller MAY spawn; the ceiling bounds
//! HOW MANY workers a single noyau can hold live at once. Each tackle burns
//! Anthropic credit + drops a worktree, so an unbounded stream — even from a
//! correctly-scoped tenant — is a budget-burn / disk-exhaustion vector.
//!
//! The ceiling's external witness authenticates a PID with its launch time:
//! `kill(pid, 0)` alone would count an unrelated process after kernel PID
//! reuse and hold a phantom slot indefinitely.
//!
//! Scenarios:
//!
//! 1. A noyau already holding [`DEFAULT_TACKLE_CEILING_PER_NOYAU`] live
//!    workers → the next tackle is refused with `429 tackle_ceiling`
//!    BEFORE any subprocess spawns (no credit burned).
//! 2. A noyau holding `ceiling - 1` live workers → the ceiling gate is open
//!    (the request is not our 429; whatever the downstream subprocess does
//!    is out of scope here).
//!
//! The count is read from the process record plus its external identity
//! witness. Tests seed records directly, so no real worker is spawned.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::routes::molecules::DEFAULT_TACKLE_CEILING_PER_NOYAU;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::Value;
use tower::ServiceExt;

/// Build an [`AppState`] over the testkit primitives (mirrors the helper in
/// `v1_workers.rs`, kept local so this file is self-contained).
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

    // Generous rate-limit capacity so the ceiling — not the leaky bucket —
    // is the mechanism under test.
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 256.0, 0.0);
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

/// Partial JSON object planting a live `process` record on a molecule
/// (same shape the `GET /v1/workers` reader consults).
fn process_override(worker_id: &str) -> Value {
    serde_json::json!({
        "status": "running",
        "process": {
            "worker_id": worker_id,
            "tmux_session": worker_id,
            "started_at": "2026-07-09T08:00:00+00:00",
            "status": "active",
            "pid": Value::Null,
            "adapter_name": "claude",
        }
    })
}

/// An active record whose PID was observed dead. This models SIGKILL before a
/// detached local worker can persist its terminal marker.
fn dead_pid_process_override(worker_id: &str) -> Value {
    serde_json::json!({
        "status": "running",
        "process": {
            "worker_id": worker_id,
            "tmux_session": worker_id,
            "started_at": "2026-07-09T08:00:00+00:00",
            "status": "active",
            "pid": 2147483647_u32,
            "pid_start_time": 1_u64,
            "adapter_name": "local",
        }
    })
}

/// An active record whose numeric PID is currently owned by this test process,
/// but whose launch fingerprint deliberately belongs to the dead former owner.
/// This is the PID-reuse race the ceiling must reject.
fn reused_pid_process_override(worker_id: &str) -> Value {
    serde_json::json!({
        "status": "running",
        "process": {
            "worker_id": worker_id,
            "tmux_session": worker_id,
            "started_at": "2026-07-09T08:00:00+00:00",
            "status": "active",
            "pid": std::process::id(),
            "pid_start_time": 0_u64,
            "adapter_name": "local",
        }
    })
}

/// Partial process record for a detached worker that has already returned.
/// Its forensic record remains, but it must not consume a tackle-ceiling slot.
fn stopped_process_override(worker_id: &str) -> Value {
    serde_json::json!({
        "status": "completed",
        "process": {
            "worker_id": worker_id,
            "tmux_session": worker_id,
            "started_at": "2026-07-09T08:00:00+00:00",
            "status": "stopped",
            "pid": Value::Null,
            "adapter_name": "local",
        }
    })
}

/// Seed `n` molecules each carrying a live worker process into noyau `a`.
fn seed_live_workers(tenants: &TenantWorkspaces, n: usize) {
    let tenant_a = tenants.tenant("a").expect("noyau 'a' must be registered");
    for i in 0..n {
        // 4-char alphanumeric suffix (`w000`, `w001`, …) — a valid MoleculeId.
        let id = format!("task-20260709-w{i:03}");
        tenant_a
            .insert_molecule(&id, &process_override(&format!("worker-{i}")))
            .unwrap();
    }
}

/// A spawn-scoped JWT for noyau `a`: carries BOTH `molecule:write` and
/// `worker:spawn`, so it clears the tackle scope gate and reaches the
/// ceiling seam.
fn spawn_jwt(oidc: &OidcMock, jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write", "cosmon:worker:spawn"],
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

async fn tackle_status(app: axum::Router, jwt: &str, target: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/molecules/{target}/tackle"))
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body_bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn tackle_refused_at_ceiling_with_429() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    // Fill the noyau to exactly the ceiling with live workers.
    seed_live_workers(&tenants, DEFAULT_TACKLE_CEILING_PER_NOYAU);
    tenants
        .tenant("a")
        .expect("noyau 'a' must be registered")
        .insert_molecule(
            "task-20260709-zzzz",
            &serde_json::json!({"status": "pending"}),
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
    let jwt = spawn_jwt(&oidc, "jti-ceiling-full");

    let (status, body) = tackle_status(app, &jwt, "task-20260709-zzzz").await;

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a noyau at the live-worker ceiling must be refused; body: {body}"
    );
    assert_eq!(
        body["error"], "tackle_ceiling",
        "the refusal must carry the stable `tackle_ceiling` label"
    );
}

#[tokio::test]
async fn tackle_below_ceiling_clears_the_gate() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    // One slot below the ceiling — the gate must NOT fire.
    seed_live_workers(&tenants, DEFAULT_TACKLE_CEILING_PER_NOYAU - 1);

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
    let jwt = spawn_jwt(&oidc, "jti-ceiling-open");

    let (status, _body) = tackle_status(app, &jwt, "task-20260709-zzzz").await;

    // Below the ceiling the gate is open: whatever the downstream fake-cs
    // subprocess answers, it must not be OUR ceiling 429.
    assert_ne!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "one worker below the ceiling must clear the pre-spawn gate"
    );
}

#[tokio::test]
async fn stopped_detached_workers_do_not_exhaust_the_tackle_ceiling() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    let tenant_a = tenants.tenant("a").expect("noyau 'a' must be registered");
    for i in 0..DEFAULT_TACKLE_CEILING_PER_NOYAU {
        let id = format!("task-20260715-s{i:03}");
        tenant_a
            .insert_molecule(&id, &stopped_process_override(&format!("local-{i}")))
            .unwrap();
    }

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
    let jwt = spawn_jwt(&oidc, "jti-ceiling-stopped");

    let (status, body) = tackle_status(app, &jwt, "task-20260715-zzzz").await;
    assert_ne!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "terminal records must not trigger tackle_ceiling; body: {body}"
    );
}

#[tokio::test]
async fn fifth_tackle_clears_active_records_whose_pids_are_dead() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    let tenant_a = tenants.tenant("a").expect("noyau 'a' must be registered");
    for i in 0..DEFAULT_TACKLE_CEILING_PER_NOYAU {
        let id = format!("task-20260716-d{i:03}");
        tenant_a
            .insert_molecule(&id, &dead_pid_process_override(&format!("dead-{i}")))
            .unwrap();
    }

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
    let jwt = spawn_jwt(&oidc, "jti-ceiling-dead-pids");

    let (status, body) = tackle_status(app, &jwt, "task-20260716-fifth").await;
    assert_ne!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a real fifth tackle must clear the ceiling after the external PID witness finds no worker; body: {body}"
    );
}

#[tokio::test]
async fn fifth_tackle_clears_a_pid_reused_by_an_unrelated_live_process() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    let tenant_a = tenants.tenant("a").expect("noyau 'a' must be registered");
    for i in 0..DEFAULT_TACKLE_CEILING_PER_NOYAU {
        let id = format!("task-20260716-r{i:03}");
        tenant_a
            .insert_molecule(&id, &reused_pid_process_override(&format!("reused-{i}")))
            .unwrap();
    }

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
    let jwt = spawn_jwt(&oidc, "jti-ceiling-pid-reuse");

    let (status, body) = tackle_status(app, &jwt, "task-20260716-fifth").await;
    assert_ne!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a live unrelated process with a reused PID must not retain the dead worker's slot; body: {body}"
    );
}

#[tokio::test]
async fn tackle_ceiling_is_per_noyau_not_global() {
    // Noyau `a` is saturated; noyau `b` is empty. `b`'s tackle must clear
    // the gate — the ceiling is per-noyau, never a global cap.
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    let _ = tenants.add("b");
    seed_live_workers(&tenants, DEFAULT_TACKLE_CEILING_PER_NOYAU);

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

    let jwt_b = oidc.issue(&IssueJwt {
        subject: "sub-b",
        audience: Some("cosmon-rpp-b"),
        scopes: &["cosmon:molecule:write", "cosmon:worker:spawn"],
        lifetime_secs: Some(60),
        jti: Some("jti-ceiling-b"),
    });

    let (status, _body) = tackle_status(app, &jwt_b, "task-20260709-zzzz").await;

    assert_ne!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "noyau b's empty fleet must clear the gate even while noyau a is saturated"
    );
}
