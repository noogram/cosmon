// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #54 / U5 falsifier — the library executor dispatches with **no
//! `cs` binary on `PATH`**, the subprocess executor cannot.
//!
//! The mission's claim is structural: after U5, the runtime's dispatch leg
//! is reachable as a library (`plan → execute` via
//! [`cosmon_runtime::tackle_exec::LibraryExecutor`] over
//! `cosmon_transport::MockBackend`), where the historical
//! [`cosmon_runtime::SubprocessExecutor`] needs the `cs` binary. Both
//! halves run under the same shadow `PATH` (git only, no `cs`), against the
//! same fixture shape, and are judged on the same observables: the dispatch
//! ledger record (molecule process binding + fleet worker) and the
//! `AdapterSelected` event on `events.jsonl`.
//!
//! RED with the subprocess executor / GREEN with the library one — the
//! contrast is the evidence that the `cs`-binary dependency, not the
//! fixture, is what changed.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_runtime::tackle_exec::LibraryExecutor;
use cosmon_runtime::{
    DispatchPin, Executor, FleetSnapshot, Policy, Runtime, RuntimeAction, RuntimeConfig,
    SubprocessExecutor,
};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::{MockBackend, MockCall};

/// Install the shadow environment exactly once per test process:
/// a `PATH` carrying the system tool directories (git lives there) but no
/// `cs`, and none of the cosmon env tiers that would let a dispatch read
/// state or defaults from outside the fixture.
///
/// Process-global by nature (`PATH` is), hence the `OnceLock`: every test
/// in this binary shares the same shadow, so concurrent test threads never
/// race on divergent values.
fn shadow_env() {
    static SHADOW: OnceLock<()> = OnceLock::new();
    SHADOW.get_or_init(|| {
        // git resolves from /usr/bin (macOS, Linux distros) — `cs` is
        // installed under the user's cargo bin or the repo target dir,
        // neither of which is on this PATH.
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
        // Point the global-config tier at an empty home so the operator's
        // real ~/.config/cosmon/config.toml cannot leak into the fixture.
        let scratch = std::env::temp_dir().join(format!("cosmon-u5-cfg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        std::env::set_var("COSMON_CONFIG_HOME", &scratch);
        // The refusal the whole file rests on: `cs` must NOT be reachable.
        assert!(
            std::process::Command::new("cs")
                .arg("--version")
                .output()
                .is_err(),
            "the shadow PATH still resolves a `cs` binary — the falsifier \
             cannot distinguish the executors under it"
        );
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

/// Build a throwaway galaxy: a git repository whose root carries
/// `.cosmon/state/` with one pending molecule.
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
    // The walk-up project marker: `.cosmon/config.toml` must exist for the
    // executor's state-dir discovery to land on this fixture.
    std::fs::write(project.join(".cosmon").join("config.toml"), "").expect("config marker");
    let store = FileStore::new(&state_dir);
    let mol = pending_molecule(id);
    store.save_molecule(&mol.id, &mol).expect("seed molecule");
    (dir, project, store, mol)
}

/// Read the raw event log, or empty when nothing was ever emitted.
fn events_text(state_dir: &Path) -> String {
    std::fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default()
}

/// The RED half: under the shadow PATH the subprocess executor — the
/// ADR-080 clause (e) envelope — cannot dispatch at all, and (because its
/// child never ran) leaves no ledger record and no `AdapterSelected`.
#[test]
fn subprocess_executor_needs_the_cs_binary_and_records_nothing_without_it() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260905-aaaa");

    let executor = SubprocessExecutor::new(&project).quiet(true);
    let err = executor
        .dispatch(&mol.id)
        .expect_err("with no `cs` on PATH the subprocess envelope must fail");
    assert!(
        err.to_string().contains("cs tackle"),
        "the failure names the missing subprocess: {err}"
    );

    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert!(
        observed.process.is_none(),
        "no dispatch ledger record may exist without a spawn"
    );
    let fleet = store.load_fleet().unwrap_or_default();
    assert!(
        fleet.workers.is_empty(),
        "no fleet worker may be registered"
    );
    assert!(
        !events_text(store.state_root()).contains("adapter_selected"),
        "no AdapterSelected may be emitted by a dispatch that never ran"
    );
}

/// One-shot policy: evolve the fixture molecule on the first tick, then
/// report drained. This is the minimal driver that makes the *runtime loop*
/// — not the test body — the caller of the executor seam.
struct EvolveOnce {
    id: MoleculeId,
    fired: bool,
}

impl Policy for EvolveOnce {
    fn next_actions(&mut self, _snapshot: &FleetSnapshot) -> Vec<RuntimeAction> {
        if self.fired {
            return Vec::new();
        }
        self.fired = true;
        vec![RuntimeAction::Evolve {
            id: self.id.clone(),
            evidence: "u5 falsifier dispatch".to_owned(),
        }]
    }
}

/// The GREEN half: the same shadow PATH, the same fixture shape, and the
/// runtime loop dispatches anyway — through the library executor over
/// `MockBackend`. The observables the RED half proved absent are all
/// present: the ledger record (process binding + fleet worker), the
/// `AdapterSelected` event, and the transport saw the spawn plus the
/// briefing injection.
#[test]
fn runtime_loop_dispatches_through_the_library_executor_with_no_cs_on_path() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260905-bbbb");
    let backend = MockBackend::new();

    let runtime = Runtime::new(
        Box::new(FileStore::new(store.state_root())),
        Box::new(EvolveOnce {
            id: mol.id.clone(),
            fired: false,
        }),
        Box::new(LibraryExecutor::new(&project, backend.clone())),
        RuntimeConfig::default(),
    );
    let mut runtime = runtime;
    let report = runtime
        .run()
        .expect("the loop must run to a clean shutdown");
    assert!(report.actions_applied >= 1, "the evolve must be applied");

    // Ledger record: the molecule is Running with a bound process record
    // naming the session, and the fleet knows the worker.
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert_eq!(observed.status, MoleculeStatus::Running);
    let process = observed
        .process
        .as_ref()
        .expect("the dispatch ledger must bind a process record before the spawn");
    assert_eq!(process.tmux_session, mol.id.as_str());
    let fleet = store.load_fleet().expect("fleet");
    assert_eq!(
        fleet.workers.len(),
        1,
        "exactly one fleet worker must be registered"
    );

    // Attribution: AdapterSelected (and its WorkerSpawned partner) are on
    // the wire — emitted in-process, since no `cs` child could have.
    let events = events_text(store.state_root());
    assert!(
        events.contains("adapter_selected"),
        "AdapterSelected must be emitted by the library path:\n{events}"
    );
    assert!(
        events.contains("worker_spawned"),
        "WorkerSpawned must be emitted with the ledger record:\n{events}"
    );

    // Transport: the worker was spawned through the injected backend and
    // received the plan's prompt (the briefing injection).
    let calls = backend.calls();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, MockCall::Spawn { agent_id, .. } if agent_id == mol.id.as_str())),
        "the backend must have spawned the worker: {calls:?}"
    );

    // ADR-079 §5 obligation 3: the worker runs *in* the molecule worktree.
    // Asserting the directory exists is not the same claim — a backend that
    // spawns without a stated cwd starts the worker wherever the dispatching
    // process happened to be, and only the recorded spawn cwd falsifies that.
    let worktree = project.join(".worktrees").join(mol.id.as_str());
    let spawn_cwd = calls
        .iter()
        .find_map(|c| match c {
            MockCall::Spawn { agent_id, cwd, .. } if agent_id == mol.id.as_str() => {
                Some(cwd.clone())
            }
            _ => None,
        })
        .expect("the spawn call must be recorded");
    let spawn_cwd = spawn_cwd.expect("the spawn must state a working directory, not inherit one");
    // Canonicalised on both sides: on macOS the fixture's `/var/folders/…`
    // tempdir is a symlink to `/private/var/…`, and the executor resolves the
    // repo root. The claim is "the same directory", not "the same spelling".
    assert_eq!(
        std::fs::canonicalize(&spawn_cwd).expect("spawn cwd resolves"),
        std::fs::canonicalize(&worktree).expect("worktree resolves"),
        "the spawn must carry the molecule worktree as the worker's cwd: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| matches!(
            c,
            MockCall::SendInput { input, .. } if input.contains(mol.id.as_str())
        )),
        "the worker must have received its bootstrap prompt: {calls:?}"
    );

    // And the isolation worktree exists on the branch `cs done` merges.
    assert!(
        project.join(".worktrees").join(mol.id.as_str()).exists(),
        "the worker's isolation worktree must exist"
    );
}

/// A pinned re-dispatch reproduces the recorded adapter instead of
/// re-reading ambient env — the library executor honours the same
/// [`DispatchPin`] contract as the subprocess `--adapter` stamp.
#[test]
fn library_executor_honours_the_dispatch_pin() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260905-cccc");
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend);

    let pin = DispatchPin {
        adapter: Some("claude".to_owned()),
        model: None,
        base_branch: None,
    };
    executor
        .dispatch_with_pin(&mol.id, &pin)
        .expect("pinned dispatch must succeed");

    let observed = store.load_molecule(&mol.id).expect("re-read");
    let process = observed.process.as_ref().expect("process record");
    assert_eq!(
        process.adapter_name.as_deref(),
        Some("claude"),
        "the pin's adapter must be reproduced verbatim on the ledger"
    );
    assert_eq!(
        process.model, None,
        "a floor pin must reproduce the floor, not an ambient model"
    );
}

/// PR #57 review, finding 2: the library path must stamp the PID witness
/// exactly as `cs tackle` step 9 does — without it, `orphan_scan`'s PID
/// liveness axis is blind for every adapter-dispatched molecule. The mock's
/// spawn handle witnesses the test process's own PID, so both the pid and
/// its launch fingerprint must land on the ledger's process record. RED
/// before the fix (`stamp_pid_witness` had one caller, in the CLI), GREEN
/// after.
#[test]
fn library_dispatch_stamps_the_pid_witness_on_the_ledger() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260905-dddd");
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend);

    executor
        .dispatch(&mol.id)
        .expect("library dispatch must succeed");

    let observed = store.load_molecule(&mol.id).expect("re-read");
    let process = observed.process.as_ref().expect("process record");
    assert_eq!(
        process.pid,
        Some(std::process::id()),
        "the spawned session's witnessed PID must be stamped on the ledger"
    );
    assert!(
        process.pid_start_time.is_some(),
        "the launch fingerprint must be stamped with the PID so the \
         liveness axis can authenticate, not just match a recycled pid"
    );
}

/// PR #57 review, finding 3: EVERY post-worktree error path removes the
/// worktree and branch — the rollback contract the module docs promise.
/// The spawn-failure path exercises the new single cleanup seam (the
/// identifier and ledger error paths used to leak because each path carried
/// its own cleanup, or none).
#[test]
fn failed_spawn_rolls_back_worktree_ledger_and_status() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260905-eeee");
    let backend = MockBackend::new();
    backend.set_spawn_error("no seats left");
    let executor = LibraryExecutor::new(&project, backend);

    let err = executor
        .dispatch(&mol.id)
        .expect_err("a failing backend must fail the dispatch");
    assert!(err.to_string().contains("no seats left"), "{err}");

    // Ledger rolled back: molecule restored, no process binding, no fleet
    // worker.
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert_eq!(observed.status, MoleculeStatus::Pending);
    assert!(observed.process.is_none(), "no process record may survive");
    let fleet = store.load_fleet().unwrap_or_default();
    assert!(fleet.workers.is_empty(), "no fleet worker may survive");

    // Worktree and branch removed by the outer cleanup seam.
    assert!(
        !project.join(".worktrees").join(mol.id.as_str()).exists(),
        "the partial worktree must be removed on rollback"
    );
    let branches = std::process::Command::new("git")
        .args(["-C", &project.to_string_lossy(), "branch", "--list"])
        .output()
        .expect("git branch");
    let branches = String::from_utf8_lossy(&branches.stdout).into_owned();
    assert!(
        !branches.contains(&format!("feat/{}", mol.id.as_str())),
        "the feature branch must be removed on rollback: {branches}"
    );
}

/// Second-family review of PR #57, finding 1: **rollback must not own
/// resources it did not create.**
///
/// `create_worktree` is idempotent — an existing worktree is reused and an
/// existing branch is tolerated — but the dispatch-error path used to call
/// `remove_worktree_and_branch` unconditionally. So retrying a molecule
/// whose previous worker crashed with its worktree preserved, and hitting
/// a backend/ledger failure on the retry, destroyed the prior work with
/// `git worktree remove --force` + `git branch -D` instead of undoing this
/// attempt's own allocations.
///
/// The falsifier is the uncommitted file: it belongs to the *previous*
/// dispatch, so a correct rollback cannot touch it.
#[test]
fn rollback_preserves_a_reused_worktree_and_its_branch() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260909-a1a1");
    let worktree = project.join(".worktrees").join(mol.id.as_str());
    let branch = format!("feat/{}", mol.id.as_str());

    // A prior dispatch's worktree, with work in it that was never committed.
    cosmon_runtime::tackle_exec::create_worktree(&project, &worktree, &branch, None)
        .expect("the prior dispatch's worktree must be creatable");
    let prior_work = worktree.join("prior-uncommitted.txt");
    std::fs::write(&prior_work, "work the previous worker had not committed")
        .expect("seed prior work");

    // The retry fails after the worktree step.
    let backend = MockBackend::new();
    backend.set_spawn_error("no seats left");
    let executor = LibraryExecutor::new(&project, backend);
    let err = executor
        .dispatch(&mol.id)
        .expect_err("a failing backend must fail the dispatch");

    assert!(
        prior_work.exists(),
        "rollback destroyed uncommitted work it did not create: {err}"
    );
    let branches = std::process::Command::new("git")
        .args(["-C", &project.to_string_lossy(), "branch", "--list"])
        .output()
        .expect("git branch");
    let branches = String::from_utf8_lossy(&branches.stdout).into_owned();
    assert!(
        branches.contains(&branch),
        "rollback deleted a branch it did not create: {branches}"
    );
    // The ledger is still rolled back — only the *filesystem* resources are
    // preserved, because only they predate this attempt.
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert_eq!(observed.status, MoleculeStatus::Pending);
    assert!(observed.process.is_none(), "no process record may survive");
    assert!(
        err.to_string().contains("preserved"),
        "the error must name what rollback deliberately left behind: {err}"
    );
}

/// Second-family review of PR #57, finding 3: **a failed prompt delivery
/// must not leave a live worker behind.**
///
/// `spawn` succeeds — the transport has committed a detached session to the
/// operating system — and `send_input_observed` then fails. The executor
/// used to propagate that failure straight through, rolling the ledger back
/// and removing the worktree while the session it had just created kept
/// running: a live process with no registration and no working directory,
/// exactly the §8ab shape ("an effect without a record is visible to
/// nothing").
#[test]
fn failed_prompt_delivery_terminates_the_spawned_session() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260909-b2b2");
    let backend = MockBackend::new();
    backend.set_send_input_error("pane vanished mid-paste");
    let executor = LibraryExecutor::new(&project, backend.clone());

    let err = executor
        .dispatch(&mol.id)
        .expect_err("a failed prompt delivery must fail the dispatch");
    assert!(err.to_string().contains("pane vanished mid-paste"), "{err}");

    assert!(
        backend
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::Terminate { .. })),
        "the successfully spawned session must be terminated when its \
         prompt could not be delivered: {:?}",
        backend.calls()
    );
    assert!(
        backend.list_sessions().expect("list").is_empty(),
        "no session may outlive a rolled-back dispatch"
    );
    // Termination confirmed ⇒ the ordinary rollback symmetry applies.
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert_eq!(observed.status, MoleculeStatus::Pending);
    assert!(observed.process.is_none(), "no process record may survive");
}

/// The other half of finding 3: when the teardown itself cannot be
/// confirmed, the dispatch record and the worktree are **retained**, not
/// rolled back.
///
/// A live worker with a rolled-back ledger is invisible to every sweep;
/// a live worker with a stale-but-present record is discoverable, which is
/// the recoverable shape §8ab asks for. The error names both the session
/// and the worktree so an operator can finish the teardown by hand.
#[test]
fn unconfirmed_termination_retains_the_record_and_the_worktree() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260909-c3c3");
    let backend = MockBackend::new();
    backend.set_send_input_error("pane vanished mid-paste");
    backend.set_terminate_error("tmux server unreachable");
    let executor = LibraryExecutor::new(&project, backend.clone());

    let err = executor
        .dispatch(&mol.id)
        .expect_err("an unconfirmed teardown must fail the dispatch");
    let rendered = err.to_string();
    assert!(
        rendered.contains(mol.id.as_str()) && rendered.contains(".worktrees"),
        "the error must name the surviving session and its worktree: {rendered}"
    );

    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert!(
        observed.process.is_some(),
        "the dispatch record must be RETAINED so the possibly-live worker \
         stays discoverable"
    );
    assert!(
        project.join(".worktrees").join(mol.id.as_str()).exists(),
        "the worktree of a possibly-live worker must not be removed"
    );
}

/// A precondition refusal costs the molecule NOTHING: no worktree, no
/// branch, no ledger record, no spawn, and no attribution event.
///
/// The ordering is the whole point of the port (issue #48, restored on
/// this seam by task-20260911-be1e). A refusal placed after the worktree
/// would still answer the right label while leaving a branch and a
/// directory behind for a dispatch that never happened; a refusal placed
/// after the ledger would leave a record of a worker that was never
/// spawned. So the test asserts the absences, not merely the error.
#[test]
fn a_refused_precondition_leaves_nothing_behind() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260911-be1e");
    let backend = MockBackend::new();

    /// A preflight that refuses everything, naming the adapter it saw so
    /// the test can prove the check ran AFTER selection resolved.
    #[derive(Debug)]
    struct AlwaysRefuses;
    impl cosmon_runtime::SpawnPreflight for AlwaysRefuses {
        fn check(
            &self,
            ctx: &cosmon_runtime::PreflightContext<'_>,
        ) -> Result<(), cosmon_runtime::PreflightRefusal> {
            Err(cosmon_runtime::PreflightRefusal::WorkerCredentialMissing {
                adapter: ctx.adapter.to_owned(),
                detail: "no credential in this fixture".to_owned(),
                remedy: "provision one".to_owned(),
            })
        }
    }

    let executor = LibraryExecutor::new(&project, backend.clone())
        .with_preflight(std::sync::Arc::new(AlwaysRefuses));

    let err = executor
        .dispatch(&mol.id)
        .expect_err("a refused precondition must fail the dispatch");
    assert!(
        err.to_string().contains("no credential in this fixture"),
        "the refusal's cause must reach the caller: {err}"
    );

    assert!(
        backend.calls().is_empty(),
        "nothing may be spawned once the precondition is refused: {:?}",
        backend.calls()
    );
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert!(
        observed.process.is_none(),
        "no dispatch record may survive a refusal — an effectless record is \
         a worker the fleet will look for and never find"
    );
    assert!(
        !project.join(".worktrees").join(mol.id.as_str()).exists(),
        "no worktree may survive a refusal"
    );
}

/// Inside a drain, a precondition refusal STOPS the loop with the cause
/// named rather than being retried every tick.
///
/// Same treatment as an unsupported step kind, for a different reason
/// that lands in the same place: a missing credential is repairable, but
/// not by this drain and not within its budget. Retrying it until
/// `max_runtime` is how a stated cause becomes an unexplained `timeout` —
/// the defect PR #57 finding 1 fixed for the step-kind refusal.
#[test]
fn a_drain_stops_on_a_precondition_refusal_rather_than_retrying_it() {
    shadow_env();
    let (_dir, project, _store, mol) = fixture("task-20260911-be1f");

    #[derive(Debug)]
    struct AlwaysRefuses;
    impl cosmon_runtime::SpawnPreflight for AlwaysRefuses {
        fn check(
            &self,
            _ctx: &cosmon_runtime::PreflightContext<'_>,
        ) -> Result<(), cosmon_runtime::PreflightRefusal> {
            Err(
                cosmon_runtime::PreflightRefusal::AdapterBackendUnreachable {
                    adapter: "local".to_owned(),
                    detail: "connection refused".to_owned(),
                },
            )
        }
    }

    let executor = LibraryExecutor::new(&project, MockBackend::new())
        .with_preflight(std::sync::Arc::new(AlwaysRefuses));
    let err = cosmon_runtime::Executor::dispatch(&executor, &mol.id)
        .expect_err("a refused precondition must fail the dispatch");

    assert!(
        matches!(err, cosmon_runtime::RuntimeError::DispatchRefused { .. }),
        "a precondition refusal must be the non-retryable class, or the \
         drain spins until max_runtime and reports a timeout it knew the \
         cause of at the first tick: {err:?}"
    );
}

/// Issue #72 falsifier 3: the in-process executor spawns through the
/// agent-definition seam, which has no channel for a model flag. A pinned
/// opencode dispatch must therefore refuse with a named error — before any
/// effect and before `ModelSelected` claims the pin — rather than spawn
/// `opencode` with the pin silently dropped.
#[test]
fn a_pinned_opencode_dispatch_refuses_rather_than_dropping_the_model() {
    shadow_env();
    let (_dir, project, store, mol) = fixture("task-20260914-d84c");
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend.clone());

    let pin = DispatchPin {
        adapter: Some("opencode".to_owned()),
        model: Some("openai/gpt-5.2".to_owned()),
        base_branch: None,
    };
    let err = executor
        .tackle(&mol.id, &pin)
        .expect_err("a pinned opencode dispatch must not spawn without its model");
    assert!(
        matches!(
            err,
            cosmon_runtime::tackle_exec::TackleExecError::UnsupportedModelCarrier { .. }
        ),
        "the refusal must be the named model-carrier error: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("opencode") && text.contains("openai/gpt-5.2"),
        "the refusal must name the adapter and the dropped pin: {text}"
    );
    assert!(
        backend.calls().is_empty(),
        "nothing may be spawned: {:?}",
        backend.calls()
    );
    assert!(store
        .load_molecule(&mol.id)
        .expect("re-read")
        .process
        .is_none());
    assert!(
        !events_text(store.state_root()).contains("model_selected"),
        "a refused dispatch must not record a ModelSelected claiming the pin"
    );
}

/// The refusal is scoped to the pin: an unpinned opencode dispatch still
/// goes through (opencode's own default applies, nothing is dropped).
#[test]
fn an_unpinned_opencode_dispatch_is_not_refused() {
    shadow_env();
    let (_dir, project, _store, mol) = fixture("task-20260914-d84d");
    let backend = MockBackend::new();
    let executor = LibraryExecutor::new(&project, backend.clone());

    let pin = DispatchPin {
        adapter: Some("opencode".to_owned()),
        model: None,
        base_branch: None,
    };
    executor
        .tackle(&mol.id, &pin)
        .expect("an unpinned opencode dispatch carries nothing to drop");
    assert!(!backend.calls().is_empty(), "the worker must be spawned");
}
