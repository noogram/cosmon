// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/session` — the worker message-thread read
//! (issue #51 follow-up).
//!
//! Every scenario here is one of the brief's falsifiers, and each was watched
//! fail before the route existed (the whole file 404'd on an unmounted path):
//!
//! 1. A worker whose thread holds N entries: the route returns them in order,
//!    attributed, with `source` naming where they came from.
//! 2. A worker that has exited: `/session` still returns what was retrievable
//!    — or an explicit `source: "none"`. Never a 500, never an empty 200 with
//!    no header.
//! 3. A thread ending on a permission prompt sets `waiting`; a plain running
//!    log does not.
//! 4. A JWT for noyau A cannot read noyau B's session.
//! 5. `tail=3` returns exactly the last three entries.
//!
//! Falsifier 6 (nothing writes into the session) is a property of the *code*,
//! not of one request, and is pinned by `tests/session_is_read_only.rs`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::id::{AgentId, WorkerId};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantPath, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, JwksStore, Posture, SharedHabilitationMap,
};
use cosmon_state::{StateStore, WorkerData};
use serde_json::Value;
use tower::ServiceExt;

/// The claude configuration root every test in this binary shares.
///
/// `AgentSessionRoots::from_env` reads `CLAUDE_CONFIG_DIR` — process-global
/// state, and cargo runs these tests in parallel threads. So it is set exactly
/// once, to one directory, before any request is served; per-test isolation
/// comes from each test using a **distinct fake worker cwd**, which sanitises
/// to a distinct `projects/` subdirectory.
fn claude_root() -> &'static std::path::Path {
    static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().expect("claude config tempdir");
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path());
        dir
    })
    .path()
}

struct Fixture {
    oidc: OidcMock,
    tenant: TenantPath,
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
    app: axum::Router,
}

/// Build the app with tenants `a` and `b` bound to distinct noyaux, so the
/// admission-boundary falsifier has a second tenant to fail against.
async fn fixture() -> Fixture {
    let _ = claude_root();
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    let _ = tenants.add("b");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned(), "cosmon-rpp-b".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let _ = oidc.write_jwks_file(security_dir.path()).unwrap();
    let jwks = JwksStore::load(security_dir.path()).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .insert(
            oidc.issuer(),
            "sub-b",
            HabilitationId::new("nuc-b"),
            Noyau::new("b"),
            "cosmon-rpp-b",
        )
        .build();
    let rate_limiter =
        IngressRateLimiter::new(security_dir.path().join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list =
        DenyList::new(security_dir.path().to_path_buf()).with_ttl(Duration::from_secs(0));

    let state = AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::SharedBackend(Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.path().to_path_buf(),
        inbox_root: security_dir.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: security_dir.path().join("artifacts"),
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
    };
    let app = router(state);
    Fixture {
        oidc,
        tenant,
        _tenants: tenants,
        _security_dir: security_dir,
        app,
    }
}

fn jwt(fx: &Fixture, sub: &str, audience: &str, scopes: &[&str], jti: &str) -> String {
    fx.oidc.issue(&IssueJwt {
        subject: sub,
        audience: Some(audience),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

/// The read grant this route requires.
fn logs_jwt(fx: &Fixture, jti: &str) -> String {
    jwt(fx, "sub-a", "cosmon-rpp-a", &["cosmon:logs:subscribe"], jti)
}

async fn get_session(fx: &Fixture, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = fx
        .app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Seed a molecule bound to a worker whose recorded worktree is `cwd`, and
/// plant `log` as that worker's claude session transcript.
///
/// This is the post-mortem path on purpose: nothing here is alive. No tmux
/// session exists in the test environment, the molecule is `completed`, and
/// the transcript still resolves from the *recorded* cwd — which is the whole
/// claim the route makes.
fn seed_worker_with_transcript(tenant: &TenantPath, id: &str, worker: &str, log: &str) {
    let cwd = std::env::temp_dir().join(format!("cosmon-session-{worker}"));
    let mut w = WorkerData::new(
        WorkerId::new(worker).unwrap(),
        AgentId::new("claude").unwrap(),
        cosmon_core::agent::AgentRole::Implementation,
        cosmon_core::clearance::Clearance::Execute,
        cosmon_core::worker::WorkerStatus::Active,
    );
    w.repo = Some(cwd.to_string_lossy().into_owned());
    let store = cosmon_filestore::FileStore::new(&tenant.state_dir);
    let mut fleet = store.load_fleet().unwrap();
    fleet.workers.insert(w.id.clone(), w);
    store.save_fleet(&fleet).unwrap();

    let project =
        claude_root()
            .join("projects")
            .join(cosmon_core::session_thread::sanitise_agent_path(
                &cwd.to_string_lossy(),
            ));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("session.jsonl"), log).unwrap();

    tenant
        .insert_molecule(
            id,
            &serde_json::json!({
                "status": "completed",
                "assigned_worker": worker,
                "adapter": "claude",
            }),
        )
        .unwrap();
}

/// A four-entry claude transcript: the operator's brief, two worker turns, one
/// tool result.
fn transcript(last_worker_line: &str) -> String {
    format!(
        concat!(
            r#"{{"type":"user","timestamp":"2026-09-07T18:50:05Z","message":{{"role":"user","content":"the brief"}}}}"#,
            "\n",
            r#"{{"type":"assistant","timestamp":"2026-09-07T18:50:07Z","message":{{"role":"assistant","content":[{{"type":"text","text":"reading the conventions"}}]}}}}"#,
            "\n",
            r#"{{"type":"user","timestamp":"2026-09-07T18:50:13Z","message":{{"role":"user","content":[{{"type":"tool_result","content":"Cargo.toml"}}]}}}}"#,
            "\n",
            r#"{{"type":"assistant","timestamp":"2026-09-07T18:50:20Z","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}}}}"#,
        ),
        last_worker_line
    )
}

#[tokio::test]
async fn session_rejects_missing_bearer_with_401() {
    let fx = fixture().await;
    let (status, _) = get_session(&fx, "/v1/molecules/task-20260907-e376/session", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn molecule_read_alone_does_not_lift_to_the_session_thread() {
    // The scope decision, pinned: the thread is worker OUTPUT, not the
    // molecule's state, so the basic read grant every onboarding tenant holds
    // is not enough. If someone widens this route to `molecule:read`, this
    // test is the sentence they have to argue with.
    let fx = fixture().await;
    let token = jwt(
        &fx,
        "sub-a",
        "cosmon-rpp-a",
        &["cosmon:molecule:read", "cosmon:events:subscribe"],
        "jti-no-logs",
    );
    let (status, _) = get_session(
        &fx,
        "/v1/molecules/task-20260907-e376/session",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_exited_worker_still_answers_200_with_an_explicit_source() {
    // Falsifier 2. `/logs` ends immediately for this molecule (no tmux
    // session). `/session` must not: it answers 200 and SAYS that nothing was
    // retrievable, rather than 500-ing or handing back an empty 200 that reads
    // like an empty conversation.
    let fx = fixture().await;
    let id = "task-20260907-gone";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({"status": "completed"}))
        .unwrap();
    let token = logs_jwt(&fx, "jti-gone");
    let (status, body) =
        get_session(&fx, &format!("/v1/molecules/{id}/session"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "none");
    assert_eq!(body["retrospective"], false);
    assert_eq!(body["live"], false);
    assert_eq!(body["total"], 0);
    assert_eq!(body["entries"].as_array().unwrap().len(), 0);
    assert_eq!(body["status"], "completed");
    assert_eq!(body["waiting"]["waiting"], false);
}

#[tokio::test]
async fn the_thread_is_returned_in_order_and_attributed_after_the_worker_is_gone() {
    // Falsifier 1, in its load-bearing form: no live pane anywhere, and the
    // thread still comes back — ordered, attributed, timestamped — with
    // `source` naming the plane it came from.
    let fx = fixture().await;
    let id = "task-20260907-thrd";
    seed_worker_with_transcript(&fx.tenant, id, "w-thread", &transcript("running tests"));
    let token = logs_jwt(&fx, "jti-thread");
    let (status, body) =
        get_session(&fx, &format!("/v1/molecules/{id}/session"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "claude-transcript");
    assert_eq!(body["retrospective"], true);
    assert_eq!(body["adapter"], "claude");
    assert_eq!(body["total"], 4);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(
        entries
            .iter()
            .map(|e| e["origin"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["operator", "worker", "system", "worker"]
    );
    assert_eq!(entries[0]["text"], "the brief");
    assert_eq!(entries[3]["text"], "running tests");
    assert_eq!(entries[0]["ordinal"], 1);
    assert!(entries[0]["at"].is_string(), "a transcript entry is dated");
}

#[tokio::test]
async fn tail_3_returns_exactly_the_last_three_entries() {
    // Falsifier 5.
    let fx = fixture().await;
    let id = "task-20260907-tail";
    seed_worker_with_transcript(&fx.tenant, id, "w-tail", &transcript("done"));
    let token = logs_jwt(&fx, "jti-tail");
    let (status, body) = get_session(
        &fx,
        &format!("/v1/molecules/{id}/session?tail=3"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 4);
    assert_eq!(body["returned"], 3);
    assert_eq!(body["offset"], 1);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries
            .iter()
            .map(|e| e["ordinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3, 4],
        "the tail keeps the ordinals of the FULL thread, so a client can see \
         what it skipped"
    );
}

#[tokio::test]
async fn waiting_is_set_when_the_thread_ends_on_a_permission_prompt() {
    // Falsifier 3, positive half. The marker vocabulary is the one
    // `cs patrol --dialogue-scan` uses; nothing is re-invented here.
    let fx = fixture().await;
    let id = "task-20260907-wait";
    seed_worker_with_transcript(
        &fx.tenant,
        id,
        "w-wait",
        &transcript("Do you want to proceed?"),
    );
    let token = logs_jwt(&fx, "jti-wait");
    let (_, body) = get_session(&fx, &format!("/v1/molecules/{id}/session"), Some(&token)).await;
    assert_eq!(body["waiting"]["waiting"], true);
    assert_eq!(body["waiting"]["class"], "permission");
    assert!(body["waiting"]["evidence"].is_string());
}

#[tokio::test]
async fn waiting_is_clear_on_a_plain_running_log() {
    // Falsifier 3, negative half — without which the positive half proves
    // nothing but that the field exists.
    let fx = fixture().await;
    let id = "task-20260907-busy";
    seed_worker_with_transcript(
        &fx.tenant,
        id,
        "w-busy",
        &transcript("Compiling cosmon-core, 12 tests running"),
    );
    let token = logs_jwt(&fx, "jti-busy");
    let (_, body) = get_session(&fx, &format!("/v1/molecules/{id}/session"), Some(&token)).await;
    assert_eq!(body["waiting"]["waiting"], false);
    assert_eq!(body["waiting"]["class"], "none");
    assert!(body["waiting"]["evidence"].is_null());
}

#[tokio::test]
async fn a_noyau_a_jwt_cannot_read_a_noyau_b_session() {
    // Falsifier 4 — the admission boundary, same five clauses as every other
    // molecule route. The molecule exists, under tenant A; tenant B's token
    // must not see it, and must not learn that it exists either.
    let fx = fixture().await;
    let id = "task-20260907-isol";
    seed_worker_with_transcript(&fx.tenant, id, "w-isol", &transcript("secret work"));

    let a = logs_jwt(&fx, "jti-isol-a");
    let (status_a, body_a) =
        get_session(&fx, &format!("/v1/molecules/{id}/session"), Some(&a)).await;
    assert_eq!(status_a, StatusCode::OK);
    assert_eq!(body_a["source"], "claude-transcript");

    let b = jwt(
        &fx,
        "sub-b",
        "cosmon-rpp-b",
        &["cosmon:logs:subscribe"],
        "jti-isol-b",
    );
    let (status_b, body_b) =
        get_session(&fx, &format!("/v1/molecules/{id}/session"), Some(&b)).await;
    assert_eq!(status_b, StatusCode::NOT_FOUND);
    assert_ne!(body_b["source"], "claude-transcript");
}

#[tokio::test]
async fn a_malformed_id_is_a_404_not_a_shell_argument() {
    // The id flows toward a tmux `-t` argument on the fallback path. It never
    // gets there: an unparseable MoleculeId is refused first, and refused as
    // 404 rather than 400 so the route stays free of an existence oracle.
    let fx = fixture().await;
    let token = logs_jwt(&fx, "jti-bad");
    for bad in ["..", "a%2Fb", "$(whoami)"] {
        let (status, _) =
            get_session(&fx, &format!("/v1/molecules/{bad}/session"), Some(&token)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "id {bad:?} must not be served"
        );
    }
}
