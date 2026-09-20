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
//! the smallest unit with an environment of its own.

use std::path::PathBuf;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::root_spawn_policy::SIMULATE_ROOT_DISPATCH_ENV;
use cosmon_filestore::FileStore;
use cosmon_runtime::{DispatchPin, Executor, LibraryExecutor};
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
    // Removed at the end of the test; this binary runs nothing else.
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
    std::env::remove_var(SIMULATE_ROOT_DISPATCH_ENV);
}
