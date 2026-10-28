// SPDX-License-Identifier: AGPL-3.0-only

//! The remote surface of four `cs` verbs — `evolve`, `resurrect`, `deps`,
//! `prime` (pilot plan 0001, lot E1; asked for by Leanear).
//!
//! Each route calls the operation its `cs` verb calls. These tests drive the
//! production router over a real file store and pin three things per verb:
//! what the route produces, that it is what the shared operation produces,
//! and who may ask. The last half of "same as `cs`" lives in
//! `crates/cosmon-cli/tests/remote_verbs_parity.rs`, which runs the `cs`
//! binary on the same fixtures and asserts the same properties.
//!
//! The scope rules, which are the part a regression would silently loosen:
//!
//! | verb        | scopes                                               |
//! |-------------|------------------------------------------------------|
//! | `evolve`    | `cosmon:molecule:write`                              |
//! | `resurrect` | `cosmon:molecule:write` AND `cosmon:worker:spawn`    |
//! | `deps`      | `cosmon:molecule:read`                               |
//! | `prime`     | `cosmon:molecule:read`                               |

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::id::MoleculeId;
use cosmon_filestore::FileStore;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_remote::client::Client;
use cosmon_remote::config::Profile;
use cosmon_rpp_adapter::auth_claude::{
    AuthClaudeConfig, AuthClaudeState, FilesystemSessionStore, SessionStore,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::routes::molecules::DEFAULT_TACKLE_CEILING_PER_NOYAU;
use cosmon_rpp_adapter::worker_env::WorkerBackends;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::ops;
use cosmon_state::StateStore;
use cosmon_transport::mock::{MockBackend, MockCall};
use serde_json::{json, Value};
use tower::ServiceExt;

const READ: &str = "cosmon:molecule:read";
const WRITE: &str = "cosmon:molecule:write";
const SPAWN: &str = "cosmon:worker:spawn";
const AUDIENCE: &str = "cosmon-rpp-a";

const USABLE_CREDENTIALS: &str = r#"{"claudeAiOauth":{"accessToken":"tok","refreshToken":"ref","expiresAt":99999999999999,"scopes":["user:inference"]}}"#;
const CLAUDE_READY: &str =
    "  ⏵⏵ bypass permissions on (shift+tab to cycle)\n ❯ Type your message\n";

/// A two-step formula, so that one `evolve` leaves the molecule running.
const TWO_STEPS: &str = r#"
formula = "two-steps"
version = 1
description = "two steps, for evolve"
id_prefix = "task"

[[steps]]
id = "design"
title = "Design"
description = "Decide the approach."
acceptance = "The approach is written down."

[[steps]]
id = "build"
title = "Build"
description = "Do the work."
needs = ["design"]
"#;

/// A step that declares a shell verification: the one thing a remote caller
/// must not be able to make the server run.
const VERIFIED_STEP: &str = r#"
formula = "verified"
version = 1
description = "a step with a shell verification"
id_prefix = "task"

[[steps]]
id = "check"
title = "Check"
description = "Run the check."

[steps.verification]
criteria = "true"
max_retries = 0

[[steps]]
id = "after"
title = "After"
description = "Afterwards."
needs = ["check"]
"#;

/// Everything a case needs: a tenant, an IdP, the mock transport.
struct Env {
    oidc: OidcMock,
    tenants: TenantWorkspaces,
    security_dir: tempfile::TempDir,
    home: tempfile::TempDir,
    backend: MockBackend,
    jti: AtomicU32,
}

impl Env {
    async fn new() -> Self {
        shadow_model_environment();
        let mut tenants = TenantWorkspaces::new();
        let _ = tenants.add("a");
        let tenant = tenants.tenant("a").expect("noyau 'a' is registered");
        tenant.install_task_work_formula().unwrap();
        tenant.insert_formula("two-steps", TWO_STEPS).unwrap();
        tenant.insert_formula("verified", VERIFIED_STEP).unwrap();
        // The tenant states its adapter in config: the credential
        // precondition is a property of the Claude arm, and a hermetic test
        // must not lean on a process-environment knob.
        std::fs::write(
            tenant.root.join(".cosmon").join("config.toml"),
            "[adapters]\ndefault = \"claude\"\n",
        )
        .unwrap();
        git_init(&tenant.root);

        let oidc = OidcMock::start_with(OidcMockConfig {
            audiences: vec![AUDIENCE.to_owned()],
            ..OidcMockConfig::default()
        })
        .await;
        let home = tempfile::tempdir().unwrap();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join(".credentials.json"), USABLE_CREDENTIALS).unwrap();
        let backend = MockBackend::new();
        backend.set_canned_output(CLAUDE_READY);
        Self {
            oidc,
            tenants,
            security_dir: tempfile::tempdir().unwrap(),
            home,
            backend,
            jti: AtomicU32::new(0),
        }
    }

    fn tenant_root(&self) -> PathBuf {
        self.tenants.tenant("a").unwrap().root.clone()
    }

    fn state_dir(&self) -> PathBuf {
        self.tenants.tenant("a").unwrap().state_dir.clone()
    }

    fn molecule_dir(&self, id: &str) -> PathBuf {
        self.state_dir()
            .join("fleets")
            .join("default")
            .join("molecules")
            .join(id)
    }

    fn plant(&self, id: &str, overrides: &Value) {
        self.tenants
            .tenant("a")
            .unwrap()
            .insert_molecule(id, overrides)
            .unwrap();
    }

    fn load(&self, id: &str) -> cosmon_state::MoleculeData {
        FileStore::new(self.state_dir())
            .load_molecule(&MoleculeId::new(id).unwrap())
            .unwrap()
    }

    /// A fresh router over the shared tenant, IdP and transport.
    fn app(&self) -> axum::Router {
        let dir = self.security_dir.path();
        let _ = self.oidc.write_jwks_file(dir).unwrap();
        let jwks = JwksStore::load(dir).unwrap();
        let nucleons = HabilitationMap::builder()
            .insert(
                self.oidc.issuer(),
                "sub-a",
                HabilitationId::new("nuc-a"),
                Noyau::new("a"),
                AUDIENCE,
            )
            .build();
        let store: Arc<dyn SessionStore> = Arc::new(FilesystemSessionStore::new(dir).unwrap());
        router(AppState {
            harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
            worker_backend: WorkerBackends::fixed(Arc::new(self.backend.clone())),
            state_dir: dir.to_path_buf(),
            inbox_root: dir.join("whispers/inbox"),
            galaxies_root: self.tenants.galaxies_root().to_path_buf(),
            jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
            nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleons),
            rate_limiter: Arc::new(IngressRateLimiter::new(
                dir.join("oidc-rate-limit"),
                4096.0,
                0.0,
            )),
            deny_list: Arc::new(DenyList::new(dir.to_path_buf()).with_ttl(Duration::from_secs(0))),
            posture: Posture::Prepared,
            drain_timeout: Duration::from_secs(10),
            anthropic_api_key: None,
            claude_model: None,
            backend_health: Arc::new(BackendHealthRegistry::new()),
            auth_claude: Some(Arc::new(AuthClaudeState::new(
                AuthClaudeConfig::defaults_with_home(self.home.path()),
                store,
            ))),
            artifact_root: dir.join("artifacts"),
            dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
                dir.join("dist"),
            )),
            install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
            events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
            metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
            drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
            admin_seal: Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
            provisioner: Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
            portee_provisioner: Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::inert()),
        })
    }

    fn jwt(&self, scopes: &[&str]) -> String {
        let n = self.jti.fetch_add(1, Ordering::SeqCst);
        self.oidc.issue(&IssueJwt {
            subject: "sub-a",
            audience: Some(AUDIENCE),
            scopes,
            lifetime_secs: Some(60),
            jti: Some(&format!("jti-e1-{n}")),
        })
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        scopes: &[&str],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {}", self.jwt(scopes)));
        let body = match body {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self.app().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

/// Keep model resolution independent of the developer machine.
fn shadow_model_environment() {
    static SHADOW: OnceLock<()> = OnceLock::new();
    SHADOW.get_or_init(|| {
        std::env::remove_var("COSMON_DEFAULT_MODEL");
        std::env::remove_var("ANTHROPIC_MODEL");
        let scratch = std::env::temp_dir().join(format!("cosmon-rpp-e1-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        std::env::set_var("COSMON_CONFIG_HOME", scratch);
    });
}

/// A real git repository with one commit: without it the dispatch dies at
/// `git_repo_root` and every outcome is the same refusal.
fn git_init(root: &Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git must be on PATH");
        assert!(out.status.success(), "git {args:?} failed");
    };
    run(&["init", "--initial-branch=main"]);
    run(&["config", "user.email", "test@example.invalid"]);
    run(&["config", "user.name", "test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("README.md"), "tenant\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "--no-verify", "-m", "seed"]);
}

const RUNNING: &str = "task-20261028-aaaa";
const WRECK: &str = "task-20261028-bbbb";

/// A running molecule on the two-step formula.
fn plant_running(env: &Env, id: &str) {
    env.plant(
        id,
        &json!({"status": "running", "formula_id": "two-steps", "total_steps": 2}),
    );
}

/// A wreck: `Frozen`, with the prompt and briefing a resurrection composes from.
fn plant_wreck(env: &Env, id: &str) {
    env.plant(id, &json!({"status": "frozen"}));
    let dir = env.molecule_dir(id);
    std::fs::write(dir.join("prompt.md"), "Original operator intent.").unwrap();
    std::fs::write(dir.join("briefing.md"), "The plan.").unwrap();
}

// ─── evolve ────────────────────────────────────────────────────────────────

/// Advancing a step over HTTP leaves the molecule where `cs evolve` would:
/// next step current, the step recorded, the intent record cleared, the log
/// and briefing rewritten, the events emitted.
#[tokio::test]
async fn evolve_advances_the_molecule_like_cs_evolve() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);

    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{RUNNING}/evolve"),
            &[WRITE],
            Some(json!({"evidence": "approach written down in design.md"})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["evolve"]["completed_step"], "design");
    assert_eq!(body["evolve"]["new_step"], "build");
    assert_eq!(body["evolve"]["new_status"], "running");

    let mol = env.load(RUNNING);
    assert_eq!(mol.current_step, 1);
    assert_eq!(mol.completed_steps.len(), 1);
    assert_eq!(mol.completed_steps[0].as_str(), "design");
    assert!(
        mol.pending_step.is_none(),
        "the intent record must be cleared once the artifacts landed"
    );
    assert!(
        !mol.briefing_seals.is_empty(),
        "the regenerated briefing is sealed, as `cs evolve` seals it"
    );

    let dir = env.molecule_dir(RUNNING);
    let log = std::fs::read_to_string(dir.join("log.md")).unwrap();
    assert!(log.contains("approach written down"), "log.md: {log}");
    let briefing = std::fs::read_to_string(dir.join("briefing.md")).unwrap();
    assert!(briefing.contains("Build"), "briefing.md: {briefing}");

    let events = std::fs::read_to_string(env.state_dir().join("events.jsonl")).unwrap();
    assert!(events.contains("molecule_step_completed"), "{events}");
    assert!(events.contains("molecule_evolved"), "{events}");
}

/// Evolving past the last step completes the molecule.
#[tokio::test]
async fn evolve_past_the_last_step_completes_the_molecule() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    for evidence in ["design done", "build done"] {
        let (status, body) = env
            .call(
                "POST",
                &format!("/v1/molecules/{RUNNING}/evolve"),
                &[WRITE],
                Some(json!({ "evidence": evidence })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }
    assert_eq!(env.load(RUNNING).status.to_string(), "completed");

    // And a completed molecule refuses a further advance, typed.
    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{RUNNING}/evolve"),
            &[WRITE],
            Some(json!({"evidence": "again"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "not_runnable");
}

/// A shell verification is the one effect a remote caller cannot trigger: the
/// step is refused with a typed 501 and the molecule is untouched.
#[tokio::test]
async fn evolve_refuses_a_step_that_declares_a_shell_verification() {
    let env = Env::new().await;
    env.plant(
        RUNNING,
        &json!({"status": "running", "formula_id": "verified", "total_steps": 2}),
    );

    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{RUNNING}/evolve"),
            &[WRITE],
            Some(json!({"evidence": "checked"})),
        )
        .await;

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "body: {body}");
    assert_eq!(body["error"], "evolve_unsupported_step");
    assert_eq!(env.load(RUNNING).current_step, 0, "nothing was persisted");
}

#[tokio::test]
async fn evolve_without_evidence_is_a_bad_request() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{RUNNING}/evolve"),
            &[WRITE],
            Some(json!({"evidence": ""})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"], "missing_evidence");
    assert_eq!(env.load(RUNNING).current_step, 0);
}

// ─── deps ──────────────────────────────────────────────────────────────────

/// A → B → C by `Blocks`/`BlockedBy`, written as the store holds them.
fn plant_chain(env: &Env) -> (String, String, String) {
    let (a, b, c) = (
        "task-20261028-ca01".to_owned(),
        "task-20261028-cb02".to_owned(),
        "task-20261028-cc03".to_owned(),
    );
    let link = |kind: &str, field: &str, other: &str| json!({ "rel": kind, field: other });
    env.plant(&a, &json!({"typed_links": [link("blocks", "target", &b)]}));
    env.plant(
        &b,
        &json!({"typed_links": [link("blocked_by", "source", &a), link("blocks", "target", &c)]}),
    );
    env.plant(
        &c,
        &json!({"typed_links": [link("blocked_by", "source", &b)]}),
    );
    (a, b, c)
}

/// The route returns exactly what the shared walk returns, direct and
/// transitive — which is what `cs deps` prints.
#[tokio::test]
async fn deps_returns_what_the_shared_walk_returns() {
    let env = Env::new().await;
    let (a, b, c) = plant_chain(&env);
    let store = FileStore::new(env.state_dir());

    for transitive in [false, true] {
        let uri = if transitive {
            format!("/v1/molecules/{a}/deps?transitive=true")
        } else {
            format!("/v1/molecules/{a}/deps")
        };
        let (status, body) = env.call("GET", &uri, &[READ], None).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");

        let expected = ops::deps::deps(&store, &a, &ops::DepsRequest { transitive }).unwrap();
        let expected = serde_json::to_value(ops::DepsJson::from_view(&expected)).unwrap();
        assert_eq!(body["deps"], expected, "transitive={transitive}");
    }

    let (_, direct) = env
        .call("GET", &format!("/v1/molecules/{a}/deps"), &[READ], None)
        .await;
    assert_eq!(direct["deps"]["downstream"], json!([b]));
    let (_, closure) = env
        .call(
            "GET",
            &format!("/v1/molecules/{a}/deps?transitive=true"),
            &[READ],
            None,
        )
        .await;
    assert_eq!(closure["deps"]["downstream"], json!([b, c]));
}

/// A cross-galaxy edge is listed and never resolved: resolving it would read
/// another galaxy's state on behalf of a tenant.
#[tokio::test]
async fn deps_lists_cross_galaxy_links_without_resolving_them() {
    let env = Env::new().await;
    env.plant(
        RUNNING,
        &json!({"typed_links": [
            {"rel": "cross_galaxy_blocked_by",
             "source": {"galaxy": "elsewhere", "mol_id": "task-20261028-ffff"}}
        ]}),
    );
    let (status, body) = env
        .call(
            "GET",
            &format!("/v1/molecules/{RUNNING}/deps"),
            &[READ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let edge = &body["deps"]["cross_galaxy_upstream"][0];
    assert_eq!(edge["galaxy"], "elsewhere");
    assert!(
        edge.get("resolution").is_none(),
        "unresolved by design: {edge}"
    );
}

#[tokio::test]
async fn deps_of_an_unknown_molecule_is_a_404() {
    let env = Env::new().await;
    let (status, _) = env
        .call(
            "GET",
            "/v1/molecules/task-20261028-0000/deps",
            &[READ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ─── prime ─────────────────────────────────────────────────────────────────

/// The route's reminder is the reminder the shared operation renders, which is
/// the text `cs prime` prints: the current step, its description and exit
/// criteria, the steps done and to come.
#[tokio::test]
async fn prime_returns_the_step_context_cs_prime_prints() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);

    let (status, body) = env
        .call(
            "GET",
            &format!("/v1/molecules/{RUNNING}/prime"),
            &[READ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["prime"]["terminal"], false);

    let store = FileStore::new(env.state_dir());
    let expected = match ops::prime::prime(
        &store,
        &env.tenant_root().join(".cosmon").join("formulas"),
        &MoleculeId::new(RUNNING).unwrap(),
    )
    .unwrap()
    {
        ops::PrimeView::Context(ctx) => ctx.render(),
        other => panic!("a running molecule must have a context, got {other:?}"),
    };
    assert_eq!(body["prime"]["reminder"], expected);

    let reminder = body["prime"]["reminder"].as_str().unwrap();
    assert!(
        reminder.contains("**Step 1: Design** ◀ EXECUTE NOW"),
        "{reminder}"
    );
    assert!(reminder.contains("Exit criteria: The approach is written down."));
    assert!(reminder.contains("- [ ] Step 2: Build"));
}

/// A terminal molecule has nothing to prime, and the answer says so rather than
/// failing or returning an empty reminder.
#[tokio::test]
async fn prime_on_a_terminal_molecule_returns_the_typed_terminal_answer() {
    let env = Env::new().await;
    env.plant(RUNNING, &json!({"status": "completed"}));

    let (status, body) = env
        .call(
            "GET",
            &format!("/v1/molecules/{RUNNING}/prime"),
            &[READ],
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["prime"]["terminal"], true);
    assert_eq!(body["prime"]["status"], "completed");
    assert!(body["prime"].get("context").is_none());
    assert!(body["prime"].get("reminder").is_none());
}

/// `prime` is a read: it changes nothing on disk.
#[tokio::test]
async fn prime_changes_nothing() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    let before = std::fs::read(env.molecule_dir(RUNNING).join("state.json")).unwrap();
    let _ = env
        .call(
            "GET",
            &format!("/v1/molecules/{RUNNING}/prime"),
            &[READ],
            None,
        )
        .await;
    let after = std::fs::read(env.molecule_dir(RUNNING).join("state.json")).unwrap();
    assert_eq!(before, after);
}

// ─── resurrect ─────────────────────────────────────────────────────────────

/// A wreck comes back with a worker: running, a process bound, the breadcrumb
/// and the `resurrected` event left behind — the same record `cs resurrect`
/// leaves, because the preconditions, the record and the ledger are shared.
#[tokio::test]
async fn resurrect_revives_a_wreck_with_a_fresh_worker() {
    let env = Env::new().await;
    plant_wreck(&env, WRECK);

    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{WRECK}/resurrect"),
            &[WRITE, SPAWN],
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["resurrect"]["status"], "running");
    assert!(body["resurrect"]["worker_session"].is_string());

    let mol = env.load(WRECK);
    assert_eq!(mol.status.to_string(), "running");
    assert!(
        mol.process.as_ref().is_some_and(|p| p.is_active()),
        "the new worker must be bound to the molecule"
    );
    assert!(
        env.backend
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::Spawn { .. })),
        "a worker must have been spawned through the transport port"
    );
    let wrecks: Vec<_> = std::fs::read_dir(env.molecule_dir(WRECK).join("wrecks"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(wrecks.len(), 1, "one breadcrumb per resurrection");
    let events = std::fs::read_to_string(env.state_dir().join("events.jsonl")).unwrap();
    assert!(events.contains("resurrected"), "{events}");
}

/// Only a wreck can be resurrected: a running molecule is refused and no
/// worker is spawned.
#[tokio::test]
async fn resurrect_refuses_a_molecule_that_is_not_a_wreck() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{RUNNING}/resurrect"),
            &[WRITE, SPAWN],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "not_a_wreck");
    assert!(env
        .backend
        .calls()
        .iter()
        .all(|c| !matches!(c, MockCall::Spawn { .. })));
}

/// `dry_run` composes the prompt and changes nothing.
#[tokio::test]
async fn resurrect_dry_run_returns_the_prompt_and_spawns_nothing() {
    let env = Env::new().await;
    plant_wreck(&env, WRECK);
    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{WRECK}/resurrect"),
            &[WRITE, SPAWN],
            Some(json!({"dry_run": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let prompt = body["resurrect"]["prompt"].as_str().unwrap();
    assert!(prompt.contains("Original operator intent."));
    assert!(prompt.contains("The plan."));
    assert_eq!(env.load(WRECK).status.to_string(), "frozen");
    assert!(!env.molecule_dir(WRECK).join("wrecks").exists());
    assert!(env
        .backend
        .calls()
        .iter()
        .all(|c| !matches!(c, MockCall::Spawn { .. })));
}

/// Resurrecting spawns a worker, so it counts against the same per-noyau
/// live-worker ceiling `tackle` does, and is refused before anything is
/// spawned once the ceiling is reached.
#[tokio::test]
async fn resurrect_respects_the_spawn_ceiling_like_tackle() {
    let env = Env::new().await;
    for i in 0..DEFAULT_TACKLE_CEILING_PER_NOYAU {
        env.plant(
            &format!("task-20261028-w{i:03}"),
            &json!({
                "status": "running",
                "process": {
                    "worker_id": format!("worker-{i}"),
                    "tmux_session": format!("worker-{i}"),
                    "started_at": "2026-10-28T08:00:00+00:00",
                    "status": "active",
                    "pid": Value::Null,
                    "adapter_name": "claude",
                }
            }),
        );
    }
    plant_wreck(&env, WRECK);

    let (status, body) = env
        .call(
            "POST",
            &format!("/v1/molecules/{WRECK}/resurrect"),
            &[WRITE, SPAWN],
            None,
        )
        .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"], "tackle_ceiling");
    assert_eq!(env.load(WRECK).status.to_string(), "frozen");
    assert!(env
        .backend
        .calls()
        .iter()
        .all(|c| !matches!(c, MockCall::Spawn { .. })));
}

// ─── scopes ────────────────────────────────────────────────────────────────

/// Every verb refuses a token without its scope, and the refusal precedes any
/// effect.
#[tokio::test]
async fn every_verb_is_403_without_its_scope() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    plant_wreck(&env, WRECK);

    let cases: [(&str, String, &[&str], Option<Value>); 4] = [
        (
            "POST",
            format!("/v1/molecules/{RUNNING}/evolve"),
            &[READ],
            Some(json!({"evidence": "x"})),
        ),
        (
            "POST",
            format!("/v1/molecules/{WRECK}/resurrect"),
            &[READ],
            None,
        ),
        ("GET", format!("/v1/molecules/{RUNNING}/deps"), &[], None),
        ("GET", format!("/v1/molecules/{RUNNING}/prime"), &[], None),
    ];
    for (method, uri, scopes, body) in cases {
        let (status, _) = env.call(method, &uri, scopes, body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} with {scopes:?}"
        );
    }
    assert_eq!(
        env.load(RUNNING).current_step,
        0,
        "evolve must not have run"
    );
    assert_eq!(env.load(WRECK).status.to_string(), "frozen");
}

/// `resurrect` composes its two scopes with AND, as `tackle` does: the write
/// scope alone is not the right to spawn.
#[tokio::test]
async fn resurrect_is_403_with_write_but_without_spawn() {
    let env = Env::new().await;
    plant_wreck(&env, WRECK);
    let (status, _) = env
        .call(
            "POST",
            &format!("/v1/molecules/{WRECK}/resurrect"),
            &[WRITE],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(env.load(WRECK).status.to_string(), "frozen");
    assert!(env
        .backend
        .calls()
        .iter()
        .all(|c| !matches!(c, MockCall::Spawn { .. })));
}

/// The read verbs accept the write scope too (write implies read), as
/// `observe` does.
#[tokio::test]
async fn read_verbs_accept_either_scope() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    for verb in ["deps", "prime"] {
        let (status, body) = env
            .call(
                "GET",
                &format!("/v1/molecules/{RUNNING}/{verb}"),
                &[WRITE],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{verb}: {body}");
    }
}

// ─── cosmon-remote, end to end ─────────────────────────────────────────────

/// A live adapter on loopback, driven by the real `cosmon-remote` client.
async fn serve(env: &Env) -> (String, Profile) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = env.app();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");
    let profile = Profile {
        host: base.clone(),
        sub: "sub-a".to_owned(),
        aud: AUDIENCE.to_owned(),
        oidc_url: env.oidc.issuer().to_owned(),
        issuer: None,
        client_id: None,
        attachment: None,
        noyau: Some("a".to_owned()),
        scopes: vec![READ.to_owned(), WRITE.to_owned()],
        artifacts_dir: None,
        timeout_secs: 30,
        phone_home: false,
    };
    (base, profile)
}

/// `cosmon-remote molecule evolve` and `molecule deps` through the real
/// adapter: the client's own methods, the production router, a real store.
#[tokio::test]
async fn cosmon_remote_drives_evolve_and_deps_through_the_adapter() {
    let env = Env::new().await;
    plant_running(&env, RUNNING);
    let (a, b, _c) = plant_chain(&env);
    let (_base, profile) = serve(&env).await;
    let client = Client::new(&profile, Some(env.jwt(&[READ, WRITE]))).unwrap();

    let evolved = client
        .evolve(RUNNING, "approach written down")
        .await
        .expect("evolve through the adapter");
    assert_eq!(evolved["evolve"]["completed_step"], "design");
    assert_eq!(env.load(RUNNING).current_step, 1);

    let deps = client
        .deps(&a, false)
        .await
        .expect("deps through the adapter");
    assert_eq!(deps["deps"]["downstream"], json!([b]));

    let primed = client
        .prime(RUNNING)
        .await
        .expect("prime through the adapter");
    assert_eq!(primed["prime"]["terminal"], false);
    assert!(primed["prime"]["reminder"]
        .as_str()
        .unwrap()
        .contains("**Step 2: Build** ◀ EXECUTE NOW"));
}
