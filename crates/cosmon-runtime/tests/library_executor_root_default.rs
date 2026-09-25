// SPDX-License-Identifier: AGPL-3.0-only

//! COSMON-DEV #75 residual R1 — the launch port must not **fail open** on a
//! root embedder.
//!
//! [`cosmon_runtime::WorkerLaunchPolicy`] is optional, and its
//! `root_spawn` field is an `Option`. Both absences used to mean the same
//! thing: [`RootSpawnDecision::SpawnAsIs`]. So an embedder that ran as uid 0
//! and forgot to install a policy — or installed one that left the field
//! `None` — dispatched a live cognitive worker as root, with no error, no
//! typed refusal and no event. Contract-20A's forbidden third outcome was one
//! missing builder call away, and nothing in the type system or the test suite
//! looked.
//!
//! This binary is its own integration target on purpose: it mutates
//! `COSMON_SIMULATE_ROOT_DISPATCH` in the process environment, which is the
//! only way to reach the root branch on a non-root box, and a test binary is
//! the smallest unit with an environment of its own. Every test in it sets
//! the variable to the same value and none removes it, so the tests can run
//! in parallel without one clearing the seam under another.
//!
//! # The refusal must also come first (review of c62835da, F1 and F2)
//!
//! The first repair refused only after `git worktree add` — and, on an unborn
//! repository, after a seed `git commit` that ran the repository's hooks under
//! the dispatcher's uid. No worker existed, but git had already acted as root.
//! And the refusal was classed as retryable, so the runtime provisioned and
//! rolled back a worktree on every tick until its deadline. The tests below
//! pin both: nothing on disk changes and no hook runs, and a drain stops on
//! the first attempt with a typed refusal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::root_spawn_policy::SIMULATE_ROOT_DISPATCH_ENV;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    compile_plan, DagPolicy, DispatchPin, Executor, LaunchContext, LaunchPosture, LibraryExecutor,
    Runtime, RuntimeConfig, RuntimeError, ShutdownReason, WorkerLaunchPolicy,
};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::MockBackend;

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
/// with one pending molecule.
fn fixture(id: &str) -> (tempfile::TempDir, PathBuf, MoleculeData) {
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
    (dir, project, mol)
}

/// Pin the dispatch to the `claude` adapter so the fixture's empty config
/// cannot choose which launch arm runs.
fn claude_pin() -> DispatchPin {
    DispatchPin {
        adapter: Some("claude".to_owned()),
        model: None,
        base_branch: None,
    }
}

/// The residual, stated as the property that fails on the pre-fix build: with
/// **no launch policy at all**, a root dispatcher creates no worker.
///
/// The failure guarded against is a success — a spawn recorded at the
/// transport, running as uid 0, indistinguishable from a healthy one. So the
/// assertion is on the backend's call log as much as on the error: a refusal
/// that arrives after the spawn is not a refusal.
#[test]
fn an_unstated_root_spawn_is_refused_rather_than_assumed_safe() {
    std::env::set_var(SIMULATE_ROOT_DISPATCH_ENV, "1");
    let (_dir, project, mol) = fixture("task-20260920-r001");
    let backend = MockBackend::new();
    // No `.with_launch_policy(..)` — exactly the embedder the residual is
    // about.
    let executor = LibraryExecutor::new(&project, backend.clone());

    let err = executor
        .dispatch_with_pin(&mol.id, &claude_pin())
        .expect_err("a root dispatcher with no stated policy must not dispatch");

    assert!(
        err.to_string().contains("root"),
        "the refusal must name what it refused: {err}"
    );
    assert!(
        backend.calls().is_empty(),
        "nothing may reach the transport: {:?}",
        backend.calls()
    );
}

/// A policy that states a posture but leaves the root-spawn decision unstated
/// — the second absence case, next to installing no policy at all.
#[derive(Debug)]
struct SilentPolicy;

impl WorkerLaunchPolicy for SilentPolicy {
    fn posture(&self, _ctx: &LaunchContext<'_>) -> LaunchPosture {
        LaunchPosture::default()
    }
}

/// Every path under `root` with its contents (`None` for a directory), so two
/// snapshots differ when anything was created, removed or rewritten.
fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(root).expect("under root").to_path_buf();
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read(&path).expect("read file")));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// F1: a root refusal happens before git runs at all. The fixture repository
/// is unborn, which is the case where the old order made a seed commit, and it
/// carries an executable pre-commit hook that leaves a marker if git ever
/// invokes it. For both absence cases the whole project tree — `.git`
/// included — must be byte-identical after the refusal.
#[test]
fn a_root_refusal_precedes_every_git_operation() {
    use std::os::unix::fs::PermissionsExt;
    std::env::set_var(SIMULATE_ROOT_DISPATCH_ENV, "1");
    for install_silent_policy in [false, true] {
        let (dir, project, mol) = fixture("task-20260925-r002");
        let marker = dir.path().join("hook-ran");
        let hook = project.join(".git").join("hooks").join("pre-commit");
        std::fs::create_dir_all(hook.parent().expect("hooks dir")).expect("hooks dir");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\n", marker.to_string_lossy()),
        )
        .expect("hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let before = tree_snapshot(&project);

        let backend = MockBackend::new();
        let mut executor = LibraryExecutor::new(&project, backend.clone());
        if install_silent_policy {
            executor = executor.with_launch_policy(Arc::new(SilentPolicy));
        }
        let err = executor
            .dispatch_with_pin(&mol.id, &claude_pin())
            .expect_err("a root dispatcher must not dispatch");

        assert!(
            err.to_string().contains("root-spawn-refused"),
            "silent_policy={install_silent_policy}: the refusal carries its token: {err}"
        );
        assert!(
            backend.calls().is_empty(),
            "silent_policy={install_silent_policy}: nothing may reach the transport"
        );
        assert!(
            !marker.exists(),
            "silent_policy={install_silent_policy}: a repository hook ran before the refusal"
        );
        let after = tree_snapshot(&project);
        let changed: Vec<_> = before
            .keys()
            .chain(after.keys())
            .filter(|k| before.get(*k) != after.get(*k))
            .collect();
        assert!(
            changed.is_empty(),
            "silent_policy={install_silent_policy}: the refusal left residue: {changed:?}"
        );
    }
}

/// Counts dispatch attempts, so the drain test can tell one refusal from a
/// refusal retried every tick.
struct CountingExecutor {
    inner: LibraryExecutor<MockBackend>,
    attempts: Arc<AtomicUsize>,
}

impl Executor for CountingExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.dispatch_with_pin(id, &DispatchPin::default())
    }

    fn dispatch_with_pin(&self, id: &MoleculeId, pin: &DispatchPin) -> Result<(), RuntimeError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.inner.dispatch_with_pin(id, pin)
    }
}

/// F2: a root refusal is terminal. The dispatcher's uid does not change
/// between ticks, so the drain must stop after ONE attempt with
/// [`ShutdownReason::DispatchRefused`] and the original reason — not retry
/// the refusal every poll interval and report the deadline.
#[test]
fn a_root_refusal_stops_the_drain_after_one_attempt() {
    std::env::set_var(SIMULATE_ROOT_DISPATCH_ENV, "1");
    let (_dir, project, mol) = fixture("task-20260925-r003");
    let state_dir = project.join(".cosmon").join("state");
    let store = FileStore::new(&state_dir);
    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&mol.id)).expect("compile plan");
    let attempts = Arc::new(AtomicUsize::new(0));
    let executor = CountingExecutor {
        inner: LibraryExecutor::new(&project, MockBackend::new()),
        attempts: Arc::clone(&attempts),
    };
    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(50),
        // Long enough for many retries: reaching it at all is the defect.
        max_runtime: Some(Duration::from_secs(10)),
        ..RuntimeConfig::default()
    };
    let mut runtime = Runtime::new(
        Box::new(FileStore::new(&state_dir)),
        Box::new(DagPolicy::new(plan, edges)),
        Box::new(executor),
        config,
    );

    let report = runtime.run().expect("the loop must reach a named exit");

    assert_eq!(
        report.reason,
        ShutdownReason::DispatchRefused,
        "a root refusal must end the drain, not be retried: {report:?}"
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "exactly one dispatch attempt"
    );
    let refusal = report
        .refusal
        .as_ref()
        .expect("the report names the refusal");
    assert_eq!(refusal.molecule, mol.id);
    assert!(
        refusal.reason.contains("root-spawn-refused"),
        "the original reason is kept: {}",
        refusal.reason
    );
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert_eq!(observed.status, MoleculeStatus::Pending);
}
