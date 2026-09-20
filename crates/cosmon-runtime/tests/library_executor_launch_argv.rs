// SPDX-License-Identifier: AGPL-3.0-only

//! COSMON-DEV #75 falsifier — the library executor launches a worker with a
//! **posture**, not a bare binary.
//!
//! The reported defect: since the #54 U6 cut-over, `POST
//! /v1/molecules/:id/tackle` dispatches in-process through
//! [`cosmon_runtime::LibraryExecutor`], which built its
//! [`cosmon_core::transport::AgentDefinition`] with `args: Vec::new()`. The
//! remotely dispatched `claude` therefore started bare — no
//! `--permission-mode` (so its first prompt hung in a detached pane nobody was
//! attached to), no browser-MCP strip, no out-of-worktree writable grant, no
//! `--settings` receipt overlay, no privilege drop. `cs tackle` emitted every
//! one of them.
//!
//! What is asserted here is the *observable at the port*: the argv the
//! transport was actually handed. The empty-argv dispatch survived a green
//! suite precisely because nothing looked there.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::root_spawn_policy::RootSpawnDecision;
use cosmon_core::worker_argv::{DEFAULT_PERMISSION_MODE, OPERATOR_BOUND_BROWSER_MCPS};
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    DispatchPin, Executor, LaunchContext, LaunchPosture, LibraryExecutor, WorkerLaunchPolicy,
};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::{MockBackend, MockCall};

/// The same shadow environment the sibling falsifier installs: a `PATH` with
/// no `cs`, and none of the cosmon env tiers that would let a dispatch read
/// adapter defaults from outside the fixture. Without the last part the
/// operator's own `COSMON_DEFAULT_ADAPTER` would decide which arm of the
/// launch builder this test exercises.
fn shadow_env() {
    static SHADOW: OnceLock<()> = OnceLock::new();
    SHADOW.get_or_init(|| {
        std::env::set_var("PATH", "/usr/bin:/bin");
        for var in [
            "COSMON_STATE_DIR",
            "COSMON_CONFIG",
            "COSMON_FORMULAS_DIR",
            "COSMON_DEFAULT_ADAPTER",
            "COSMON_DEFAULT_MODEL",
            "ANTHROPIC_MODEL",
        ] {
            std::env::remove_var(var);
        }
        let scratch = std::env::temp_dir().join(format!("cosmon-75-cfg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        std::env::set_var("COSMON_CONFIG_HOME", &scratch);
    });
}

/// A pending molecule in the canonical fleet shape.
fn pending_molecule(id: &str) -> MoleculeData {
    let now = chrono::Utc::now();
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("id"),
        fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet"),
        formula_id: cosmon_core::id::FormulaId::new("task-work").expect("formula"),
        status: MoleculeStatus::Pending,
        variables: std::collections::HashMap::new(),
        assigned_worker: None,
        created_at: now,
        updated_at: now,
        total_steps: 2,
        current_step: 0,
        completed_steps: vec![],
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: vec![],
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: vec![],
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: vec![],
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        base_branch: None,
        pending_step: None,
        merged_at: None,
        non_integration: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
        adapter: None,
    }
}

/// A throwaway galaxy: a git repository whose root carries `.cosmon/state/`
/// with one pending molecule pinned to the `claude` adapter.
fn fixture(id: &str) -> (tempfile::TempDir, PathBuf, FileStore, MoleculeData) {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let init = std::process::Command::new("git")
        .args(["-C", &project.to_string_lossy(), "init", "-q"])
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let state_dir = project.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(project.join(".cosmon").join("config.toml"), "").expect("config marker");
    let store = FileStore::new(&state_dir);
    let mol = pending_molecule(id);
    store.save_molecule(&mol.id, &mol).expect("seed molecule");
    (dir, project, store, mol)
}

/// Pin the dispatch to the `claude` adapter, so the test exercises the arm of
/// the launch builder it is about rather than whatever adapter the fixture's
/// (empty) config would default to.
fn claude_pin() -> DispatchPin {
    DispatchPin {
        adapter: Some("claude".to_owned()),
        model: None,
        base_branch: None,
    }
}

/// The `(command, args)` one dispatch handed the transport.
fn recorded_launch(backend: &MockBackend, id: &MoleculeId) -> (String, Vec<String>) {
    backend
        .calls()
        .iter()
        .find_map(|c| match c {
            MockCall::Spawn {
                agent_id,
                command,
                args,
                ..
            } if agent_id == id.as_str() => Some((command.clone(), args.clone())),
            _ => None,
        })
        .expect("the spawn call must be recorded")
}

/// Index of `needle` in `argv`, or a panic naming the whole argv — a missing
/// flag reads as "this flag, in this launch", not as `None`.
fn position(argv: &[String], needle: &str) -> usize {
    argv.iter()
        .position(|t| t == needle)
        .unwrap_or_else(|| panic!("`{needle}` missing from the worker launch: {argv:?}"))
}

/// Dispatch a claude worker through the library executor and return what the
/// transport was handed.
///
/// The `TempDir` is returned, not dropped: the assertions resolve paths that
/// appear in `argv`, and a fixture deleted at the end of this function would
/// turn a real grant into an unresolvable path.
fn dispatch(id: &str) -> (tempfile::TempDir, PathBuf, Vec<String>, String) {
    shadow_env();
    let (dir, project, _store, mol) = fixture(id);
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend.clone());
    executor
        .dispatch_with_pin(&mol.id, &claude_pin())
        .expect("the dispatch must reach the spawn");
    let (command, argv) = recorded_launch(&backend, &mol.id);
    (dir, project, argv, command)
}

/// The defect, stated as the property that fails on the pre-fix build: an
/// API-dispatched claude carries its permission mode and its browser-MCP
/// strip. `args: Vec::new()` cannot satisfy either.
#[test]
fn library_dispatch_carries_permission_mode_and_browser_strip() {
    let (_dir, _root, argv, command) = dispatch("task-20260917-c001");

    assert_eq!(
        command, "claude",
        "the adapter binary is the command: {argv:?}"
    );
    let mode = position(&argv, "--permission-mode");
    assert_eq!(
        argv.get(mode + 1).map(String::as_str),
        Some(DEFAULT_PERMISSION_MODE),
        "a worker with no stated mode must get the fleet default, never none: {argv:?}"
    );
    let strip = position(&argv, "--disallowedTools");
    let value = argv.get(strip + 1).expect("the strip must carry a value");
    for server in OPERATOR_BOUND_BROWSER_MCPS {
        assert!(
            value.contains(server),
            "{server} is operator-bound and must be stripped from a headless \
             worker, or its first call deadlocks the pane: {argv:?}"
        );
    }
}

/// The out-of-worktree writable grant (COSMON-DEV #20 facet B) reaches the
/// library path too: the molecule state the worker writes on `cs evolve` lives
/// in the main repo's `.cosmon/`, outside the worktree cwd, so without the
/// grant the worker's own lifecycle write prompts and an unattended pane hangs.
#[test]
fn library_dispatch_grants_the_out_of_worktree_state_dir() {
    let (_dir, root, argv, _command) = dispatch("task-20260917-c002");

    let add_dir = position(&argv, "--add-dir");
    let granted = Path::new(argv.get(add_dir + 1).expect("a granted directory"));
    assert_eq!(
        std::fs::canonicalize(granted).expect("the granted dir resolves"),
        std::fs::canonicalize(root.join(".cosmon")).expect("the state dir resolves"),
        "the grant must name the SAME `.cosmon/` the worker's `cs evolve` \
         resolves by walk-up: {argv:?}"
    );
    let tools = position(&argv, "--allowedTools");
    assert_eq!(
        argv.get(tools + 1..tools + 4),
        Some(["Bash".to_owned(), "Edit".to_owned(), "Write".to_owned()].as_slice()),
        "`--add-dir` grants addressability, not the tool that writes there: {argv:?}"
    );
}

/// A launch policy that states the two environment-dependent halves this
/// crate cannot read for itself.
#[derive(Debug)]
struct RootContainerPolicy {
    overlay: PathBuf,
}

impl WorkerLaunchPolicy for RootContainerPolicy {
    fn posture(&self, ctx: &LaunchContext<'_>) -> LaunchPosture {
        assert_eq!(
            ctx.adapter, "claude",
            "the policy sees the resolved adapter"
        );
        LaunchPosture {
            permission_mode: None,
            receipt_overlay: Some(self.overlay.clone()),
            root_spawn: Some(RootSpawnDecision::Demote { to_uid: 10001 }),
        }
    }
}

/// With a policy installed, the receipt overlay and the contract-20A privilege
/// drop both reach the port — and the drop **exec's** the real binary rather
/// than sitting inertly beside it. A composition that returned `claude` as the
/// command would run the worker as root with the drop dangling, which is the
/// silent third outcome contract-20A forbids.
#[test]
fn a_launch_policy_adds_the_receipt_overlay_and_the_privilege_drop() {
    shadow_env();
    let (_dir, project, _store, mol) = fixture("task-20260917-c003");
    let backend = MockBackend::new();
    let overlay = project.join("receipts").join("settings.json");
    let executor = LibraryExecutor::new(&project, backend.clone()).with_launch_policy(
        std::sync::Arc::new(RootContainerPolicy {
            overlay: overlay.clone(),
        }),
    );
    executor
        .dispatch_with_pin(&mol.id, &claude_pin())
        .expect("the dispatch must reach the spawn");

    let (command, argv) = recorded_launch(&backend, &mol.id);
    assert_eq!(
        command, "setpriv",
        "under a demote the privilege drop IS the command: {argv:?}"
    );
    let sep = position(&argv, "--");
    assert_eq!(
        argv.get(sep + 1).map(String::as_str),
        Some("claude"),
        "the real binary must be exec'd BY the drop: {argv:?}"
    );
    let settings = position(&argv, "--settings");
    assert_eq!(
        argv.get(settings + 1).map(String::as_str),
        Some(overlay.to_string_lossy().as_ref()),
        "the minted receipt overlay must reach the worker: {argv:?}"
    );
}

/// A policy that states the one decision under which no worker may exist.
#[derive(Debug)]
struct RefusingPolicy;

impl WorkerLaunchPolicy for RefusingPolicy {
    fn posture(&self, _ctx: &LaunchContext<'_>) -> LaunchPosture {
        LaunchPosture {
            root_spawn: Some(RootSpawnDecision::Refuse {
                reason: cosmon_core::root_spawn_policy::RootRefusalReason::NoNonRootTarget,
            }),
            ..LaunchPosture::default()
        }
    }
}

/// contract-20A outcome 2, on the library path: a stated `Refuse` produces no
/// worker, no ledger record and no leftover worktree — a typed error instead.
///
/// The failure this guards against is not a crash but a *success*: composing a
/// `Refuse` like a `SpawnAsIs` spawns a live root worker with no error, no
/// event and no log line, which is the one outcome the policy exists to make
/// unrepresentable.
#[test]
fn a_refused_root_spawn_creates_no_worker() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260917-c004");
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend.clone())
        .with_launch_policy(std::sync::Arc::new(RefusingPolicy));

    let err = executor
        .dispatch_with_pin(&mol.id, &claude_pin())
        .expect_err("a refused root spawn must not dispatch");
    assert!(
        err.to_string().contains("root"),
        "the refusal must name what it refused: {err}"
    );
    assert!(
        backend.calls().is_empty(),
        "nothing may reach the transport: {:?}",
        backend.calls()
    );
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert!(
        observed.process.is_none(),
        "no dispatch ledger record may survive a refusal"
    );
    assert!(
        !project.join(".worktrees").join(mol.id.as_str()).exists(),
        "the refusal must roll back this attempt's worktree"
    );
}

/// A `task-work` formula whose executing step pins one harness setting
/// (ADR-177 / issue #65). `claude` carries such a pin as a `--<key> <value>`
/// flag pair, so the pin is observable in the very argv this file measures.
const HARNESS_PINNING_FORMULA: &str = r#"
formula = "task-work"
version = 1
description = "a step pinning a harness setting"

[[steps]]
id = "implement"
title = "Implement"
description = "Needs a stated fallback."

[steps.harness]
fallback-model = "sonnet"
"#;

/// **C5 on the library path.** A `[steps.harness]` pin reaches the launched
/// worker verbatim.
///
/// The tokens were rendered here before the fix and bound to `_harness_args` —
/// accepted and silently dropped, which ADR-177 forbids precisely because it is
/// the shape nobody can see: the spore reads as honoured, the event says it was
/// selected, and the worker runs at the harness's own default. The other tests
/// in this file would all stay green under that regression, because none of
/// them pins a setting. This one is the falsifier for that one clause.
#[test]
fn library_dispatch_carries_a_pinned_harness_setting() {
    shadow_env();
    let (_dir, project, _store, mol) = fixture("task-20260917-c005");
    std::fs::create_dir_all(project.join(".cosmon").join("formulas")).expect("formulas dir");
    std::fs::write(
        project
            .join(".cosmon")
            .join("formulas")
            .join("task-work.formula.toml"),
        HARNESS_PINNING_FORMULA,
    )
    .expect("seed formula");
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend.clone());
    executor
        .dispatch_with_pin(&mol.id, &claude_pin())
        .expect("the dispatch must reach the spawn");

    let (_command, argv) = recorded_launch(&backend, &mol.id);
    let pin = position(&argv, "--fallback-model");
    assert_eq!(
        argv.get(pin + 1).map(String::as_str),
        Some("sonnet"),
        "a pinned harness setting must reach the worker as its own flag \
         pair, never be rendered and discarded: {argv:?}"
    );
    assert!(
        pin < position(&argv, "--disallowedTools"),
        "the pins are appended before the browser strip, as the shared \
         builder renders them on both dispatch paths: {argv:?}"
    );
}
