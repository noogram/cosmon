// SPDX-License-Identifier: AGPL-3.0-only

//! The `503` credential contract of `POST /v1/molecules/{id}/tackle`
//! (issue #48), as a non-regression test replaying the exact trigger the
//! v3.10 bake measured.
//!
//! # The trigger, verbatim
//!
//! On 2026-09-10 the forgeron bake of `c2c8ba61` ran, on both arches and
//! under the shipped Compose, a `tackle` against an instance with **no
//! Claude credential provisioned**. The published v3.9 image (`de97ff2d`)
//! answered `503 {"error":"worker_credential_missing"}`. `c2c8ba61`
//! answered `200` and a receipt: the worker booted, sat on `Not logged in
//! · Run /login`, and read as healthy to every liveness probe cosmon has.
//! Issue #54 U6 had moved the dispatch off the `cs` subprocess onto the
//! library executor, and the precondition — which lived inside `cs
//! tackle`'s spawn arm — was never ported across.
//!
//! # What each test pins
//!
//! * [`tackle_without_a_credential_is_refused_with_503`] is the falsifier:
//!   it fails on the pre-fix tree (a `200` and a receipt) and passes after.
//!   It asserts the **label**, not merely the status, because the label is
//!   the contract identifier the issue-#48 reporter consumes.
//! * [`tackle_with_a_usable_credential_clears_the_precondition`] is the
//!   control: the check is a precondition, not a blanket refusal — a
//!   dispatch with a provisioned credential is not refused for this
//!   reason. Without it, `return 503` would pass the falsifier.
//! * [`auth_me_and_tackle_agree_on_the_same_artifact`] pins the coherence
//!   half of issue #48 (commit `0ffa3651`): `claude_credentials_present`
//!   and the tackle precondition are one verdict on one file, not two
//!   opinions that happen to agree.
//!
//! The dispatch never reaches a real spawn: `MockBackend` stands in for
//! the transport, so a cleared precondition is observable without a tmux
//! pane or a paid worker.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::auth_claude::{
    AuthClaudeConfig, AuthClaudeState, FilesystemSessionStore, SessionStore,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::worker_env::WorkerBackends;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_transport::mock::{MockBackend, MockCall};
use serde_json::Value;
use tower::ServiceExt;

/// The molecule every scenario dispatches.
const TARGET: &str = "task-20260911-be1e";

/// A credentials file a worker could actually use: an access token, and an
/// expiry far enough out that the classifier reads it as `usable` rather
/// than `refreshable`.
const USABLE_CREDENTIALS: &str = r#"{"claudeAiOauth":{"accessToken":"tok","refreshToken":"ref","expiresAt":99999999999999,"scopes":["user:inference"]}}"#;

/// Positive readiness evidence for the two TUI adapters exercised below.
const CLAUDE_READY: &str =
    "  ⏵⏵ bypass permissions on (shift+tab to cycle)\n ❯ Type your message\n";
const CODEX_READY: &str = "OpenAI Codex\nworkdir: /tmp/worktree\n";

/// Build an [`AppState`] whose declared credentials file lives under
/// `home` — the same path `GET /v1/auth/me` classifies.
fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
    home: &std::path::Path,
) -> AppState {
    make_state_with_worker(
        oidc,
        tenants,
        security_dir,
        home,
        WorkerBackends::fixed(Arc::new(MockBackend::new())),
        None,
    )
}

/// Build the route state with an observable worker backend and RPP model
/// default. The public tackle route must carry that default through the same
/// resolution as a formula pin, so tests inspect the exact spawn handed to the
/// transport port rather than a library-executor unit seam.
fn make_state_with_worker(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
    home: &std::path::Path,
    worker_backend: WorkerBackends,
    claude_model: Option<String>,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let nucleons = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 256.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));
    let store: Arc<dyn SessionStore> = Arc::new(FilesystemSessionStore::new(security_dir).unwrap());

    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend,
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleons),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: Some(Arc::new(AuthClaudeState::new(
            AuthClaudeConfig::defaults_with_home(home),
            store,
        ))),
        artifact_root: security_dir.join("artifacts"),
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            security_dir.join("dist"),
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

/// Drive the authenticated RPP tackle route and return the exact argv the
/// injected transport backend received after the worker envelope was applied.
async fn rpp_tackle_spawn_args(
    tenants: &TenantWorkspaces,
    claude_model: &str,
    jti: &str,
    ready_output: &str,
) -> Vec<String> {
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    provision_credential(home.path());
    let backend = MockBackend::new();
    backend.set_canned_output(ready_output);
    let state = make_state_with_worker(
        &oidc,
        tenants,
        security_dir.path(),
        home.path(),
        WorkerBackends::fixed(Arc::new(backend.clone())),
        Some(claude_model.to_owned()),
    );

    let (status, body) = post_tackle(router(state), &spawn_jwt(&oidc, jti)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the RPP tackle must reach the spawn seam; body: {body}"
    );
    backend
        .calls()
        .into_iter()
        .find_map(|call| match call {
            MockCall::Spawn { args, .. } => Some(args),
            _ => None,
        })
        .expect("the RPP tackle route must spawn one worker")
}

/// Assert the two native Claude carriers agree on one resolved model.
fn assert_model_carriers(args: &[String], model: &str) {
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--model" && pair[1] == model),
        "the resolved model must reach the worker argv as `--model {model}`; argv: {args:?}"
    );
    assert!(
        args.iter()
            .any(|arg| arg == &format!("ANTHROPIC_MODEL={model}")),
        "ANTHROPIC_MODEL and `--model` must carry the same resolved model; argv: {args:?}"
    );
}

/// A JWT carrying both scopes `tackle` composes (`AND`, not `OR`).
fn spawn_jwt(oidc: &OidcMock, jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write", "cosmon:worker:spawn"],
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

/// Write a usable credentials file at the path
/// [`AuthClaudeConfig::defaults_with_home`] resolves under `home`.
fn provision_credential(home: &std::path::Path) {
    let path = home.join(".claude");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(".credentials.json"), USABLE_CREDENTIALS).unwrap();
}

/// One tenant holding one pending molecule, ready to be tackled.
///
/// The tenant's project config pins `adapters.default = "claude"`. That
/// pin is load-bearing, not decoration: the built-in floor adapter is
/// `local`, and the credential precondition is a property of the Claude
/// arm. The shipped Compose makes the same choice through
/// `COSMON_DEFAULT_ADAPTER=claude` — a process-environment knob a
/// hermetic test must not depend on, so the tenant states it in config
/// instead and the selection chain reaches the same adapter.
fn seeded_tenants() -> TenantWorkspaces {
    shadow_model_environment();
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    let tenant = tenants.tenant("a").expect("noyau 'a' must be registered");
    tenant
        .insert_molecule(TARGET, &serde_json::json!({"status": "pending"}))
        .unwrap();
    let cosmon_dir = tenant.root.join(".cosmon");
    std::fs::create_dir_all(&cosmon_dir).unwrap();
    std::fs::write(
        cosmon_dir.join("config.toml"),
        "[adapters]\ndefault = \"claude\"\n",
    )
    .unwrap();
    tenant.install_task_work_formula().unwrap();
    git_init(&tenant.root);
    tenants
}

/// Keep model-resolution fixtures independent of the developer machine.
///
/// This test binary shares one process, so the shadow is installed once and
/// every test observes the same empty ambient model and global-config tiers.
fn shadow_model_environment() {
    static SHADOW: OnceLock<()> = OnceLock::new();
    SHADOW.get_or_init(|| {
        std::env::remove_var("COSMON_DEFAULT_MODEL");
        std::env::remove_var("ANTHROPIC_MODEL");
        let scratch =
            std::env::temp_dir().join(format!("cosmon-rpp-model-contract-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        std::env::set_var("COSMON_CONFIG_HOME", scratch);
    });
}

/// Make the tenant root a real git repository with one commit.
///
/// Load-bearing for the falsifier's strength, not scene-dressing. Without
/// it the dispatch dies at `git_repo_root` and EVERY outcome is
/// `tackle_unavailable` — a test that would stay red after the fix for the
/// wrong reason, and that could never observe the `200` + receipt the bake
/// actually measured. With it, a dispatch that clears the precondition
/// runs all the way to the `MockBackend` spawn and answers a receipt, so
/// the falsifier distinguishes "refused" from "dispatched" rather than
/// "refused" from "broken".
fn git_init(root: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git must be on PATH");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--initial-branch=main"]);
    run(&["config", "user.email", "test@example.invalid"]);
    run(&["config", "user.name", "test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("README.md"), "tenant\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "--no-verify", "-m", "seed"]);
}

async fn post_tackle(app: axum::Router, jwt: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/molecules/{TARGET}/tackle"))
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn auth_me(app: axum::Router, jwt: &str) -> Value {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/me")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// THE falsifier. A tackle with no provisioned credential must be refused
/// with `503 worker_credential_missing`, before anything is spawned.
///
/// Fails on the pre-fix tree with `200` and a `tackle` receipt.
#[tokio::test]
async fn tackle_without_a_credential_is_refused_with_503() {
    let tenants = seeded_tenants();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    // A home with no `.claude/.credentials.json` — the container that has
    // never completed a login, which is what the bake measured.
    let home = tempfile::tempdir().unwrap();
    let app = router(make_state(
        &oidc,
        &tenants,
        security_dir.path(),
        home.path(),
    ));

    let (status, body) = post_tackle(app, &spawn_jwt(&oidc, "jti-no-credential")).await;

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a dispatch whose worker has no credential must be refused, never \
         answered with a receipt; body: {body}"
    );
    assert_eq!(
        body["error"], "worker_credential_missing",
        "the refusal must carry the stable issue-#48 identifier — an \
         external reporter matches on this string; body: {body}"
    );
    assert!(
        body.get("tackle").is_none(),
        "a refused dispatch must not carry a receipt; body: {body}"
    );
}

/// The control: the check is a precondition, not a blanket refusal. With a
/// usable credential the dispatch is not refused for this reason.
///
/// Without this test, `return 503` unconditionally would satisfy the
/// falsifier above.
#[tokio::test]
async fn tackle_with_a_usable_credential_clears_the_precondition() {
    let tenants = seeded_tenants();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    provision_credential(home.path());
    let app = router(make_state(
        &oidc,
        &tenants,
        security_dir.path(),
        home.path(),
    ));

    let (status, body) = post_tackle(app, &spawn_jwt(&oidc, "jti-credential-ok")).await;

    assert_ne!(
        body["error"], "worker_credential_missing",
        "a provisioned credential must clear the precondition; status {status}, body: {body}"
    );
}

/// The coherence half of issue #48: `GET /v1/auth/me` and the tackle
/// precondition answer from the SAME artifact through the SAME classifier.
///
/// Both directions are pinned, because a single direction is satisfied by
/// two checks that merely happen to agree on one input.
#[tokio::test]
async fn auth_me_and_tackle_agree_on_the_same_artifact() {
    let tenants = seeded_tenants();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    // Red: no credential → auth/me says absent AND tackle refuses.
    let jwt = spawn_jwt(&oidc, "jti-coherence-red");
    let me = auth_me(
        router(make_state(
            &oidc,
            &tenants,
            security_dir.path(),
            home.path(),
        )),
        &jwt,
    )
    .await;
    assert_eq!(me["claude_credentials_present"], serde_json::json!(false));
    assert_eq!(me["claude_credentials_status"], serde_json::json!("absent"));
    let (_, refused) = post_tackle(
        router(make_state(
            &oidc,
            &tenants,
            security_dir.path(),
            home.path(),
        )),
        &jwt,
    )
    .await;
    assert_eq!(
        refused["error"], "worker_credential_missing",
        "auth/me reported the credential absent — tackle must refuse; body: {refused}"
    );

    // Green: provision the very file auth/me classifies, and BOTH flip.
    provision_credential(home.path());
    let jwt = spawn_jwt(&oidc, "jti-coherence-green");
    let me = auth_me(
        router(make_state(
            &oidc,
            &tenants,
            security_dir.path(),
            home.path(),
        )),
        &jwt,
    )
    .await;
    assert_eq!(me["claude_credentials_present"], serde_json::json!(true));
    let (_, dispatched) = post_tackle(
        router(make_state(
            &oidc,
            &tenants,
            security_dir.path(),
            home.path(),
        )),
        &jwt,
    )
    .await;
    assert_ne!(
        dispatched["error"], "worker_credential_missing",
        "auth/me reported the credential usable — tackle must not refuse for \
         its absence; body: {dispatched}"
    );
}

/// Issue #114 default case: the model resolved from the RPP deployment
/// configuration reaches both native Claude carriers on the real HTTP dispatch
/// path. This is deliberately not another library-executor-only assertion.
#[tokio::test]
async fn rpp_configured_default_model_reaches_worker_argv_and_env() {
    let tenants = seeded_tenants();
    let args = rpp_tackle_spawn_args(
        &tenants,
        "rpp-configured-model-114",
        "jti-rpp-configured-model",
        CLAUDE_READY,
    )
    .await;

    assert_model_carriers(&args, "rpp-configured-model-114");
}

/// Issue #114 explicit case: a formula-step pin outranks the RPP deployment
/// default, and the winning value is carried identically in argv and env.
#[tokio::test]
async fn rpp_explicit_model_pin_reaches_worker_argv_and_env() {
    let tenants = seeded_tenants();
    let tenant = tenants.tenant("a").expect("noyau 'a' is registered");
    tenant
        .insert_formula(
            "task-work",
            r#"
formula = "task-work"
version = 1
description = "model-carrier regression fixture"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Implement"
description = "Do the work."
model = "rpp-formula-model-114"
"#,
        )
        .unwrap();
    let args = rpp_tackle_spawn_args(
        &tenants,
        "rpp-default-model-114",
        "jti-rpp-explicit-model",
        CLAUDE_READY,
    )
    .await;

    assert_model_carriers(&args, "rpp-formula-model-114");
    assert!(
        !args
            .iter()
            .any(|arg| arg == "ANTHROPIC_MODEL=rpp-default-model-114"),
        "the losing deployment default must not disagree with the explicit model pin; argv: {args:?}"
    );
}

/// A blank formula pin has no native carrier and therefore falls through the
/// ordinary model chain before the configured RPP floor is considered.
#[tokio::test]
async fn rpp_blank_formula_model_falls_through_to_tenant_config() {
    let tenants = seeded_tenants();
    let tenant = tenants.tenant("a").expect("noyau 'a' is registered");
    std::fs::write(
        tenant.root.join(".cosmon/config.toml"),
        "[adapters]\ndefault = \"claude\"\n\n[adapters.claude]\ndefault_model = \"tenant-model-114\"\n",
    )
    .unwrap();
    tenant
        .insert_formula(
            "task-work",
            r#"
formula = "task-work"
version = 1
description = "blank model-carrier regression fixture"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Implement"
description = "Do the work."
model = ""
"#,
        )
        .unwrap();
    let args = rpp_tackle_spawn_args(
        &tenants,
        "rpp-default-model-114",
        "jti-rpp-blank-model",
        CLAUDE_READY,
    )
    .await;

    assert_model_carriers(&args, "tenant-model-114");
    assert!(
        !args
            .iter()
            .any(|arg| arg == "ANTHROPIC_MODEL=rpp-default-model-114"),
        "a blank formula pin must not skip the tenant model tier: {args:?}"
    );
}

/// The RPP default is a server floor, not a replacement for a tenant-owned
/// `[adapters.claude].default_model` pin.
#[tokio::test]
async fn tenant_model_config_outranks_the_rpp_default() {
    let tenants = seeded_tenants();
    let tenant = tenants.tenant("a").expect("noyau 'a' is registered");
    std::fs::write(
        tenant.root.join(".cosmon/config.toml"),
        "[adapters]\ndefault = \"claude\"\n\n[adapters.claude]\ndefault_model = \"tenant-model-114\"\n",
    )
    .unwrap();

    let args = rpp_tackle_spawn_args(
        &tenants,
        "rpp-default-model-114",
        "jti-rpp-tenant-model",
        CLAUDE_READY,
    )
    .await;

    assert_model_carriers(&args, "tenant-model-114");
}

/// A Claude deployment default is scoped to the Claude adapter. Selecting a
/// different adapter with no model pin must not receive that model on argv.
#[tokio::test]
async fn rpp_claude_default_does_not_pin_a_codex_dispatch() {
    let tenants = seeded_tenants();
    let tenant = tenants.tenant("a").expect("noyau 'a' is registered");
    tenant
        .insert_formula(
            "task-work",
            r#"
formula = "task-work"
version = 1
description = "adapter-scoped model floor fixture"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Implement"
description = "Do the work."
adapter = "codex"
"#,
        )
        .unwrap();

    let args = rpp_tackle_spawn_args(
        &tenants,
        "rpp-default-model-114",
        "jti-rpp-codex-no-model",
        CODEX_READY,
    )
    .await;

    assert!(
        args.iter().any(|arg| arg == "codex"),
        "the formula must reach the codex adapter; argv: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "--model"),
        "the Claude-only RPP default must not become a codex model pin; argv: {args:?}"
    );
    let events = std::fs::read_to_string(
        tenants
            .tenant("a")
            .expect("noyau 'a' is registered")
            .state_dir
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        !events.contains("rpp-default-model-114"),
        "the Claude-only fallback must not enter the codex selection record: {events}"
    );
}
