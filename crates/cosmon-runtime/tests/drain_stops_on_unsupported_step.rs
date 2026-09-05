// SPDX-License-Identifier: AGPL-3.0-only

//! Falsifier for the permanent-refusal busy-loop on the LIBRARY drain
//! (issue #54 fix-2, finding 1 of the PR #57 review).
//!
//! # The defect
//!
//! [`cosmon_runtime::tackle_exec::LibraryExecutor`] refuses a gate / native
//! / query / llm step with the typed `TackleExecError::UnsupportedStep` —
//! a PERMANENT condition: the formula does not change between ticks, so an
//! identical retry reproduces the refusal exactly. The executor used to map
//! it onto the retryable `RuntimeError::Dispatch` class, whose loop arm
//! logs, skips `actions_applied += 1`, and retries next tick. The molecule
//! was reset to `Pending` and re-dispatched every poll interval until
//! `max_runtime`: a condition known in full on the FIRST tick, reported as
//! a bound (`Deadline` — the adapter's `timeout` token).
//!
//! # The fix, and what this test pins
//!
//! `UnsupportedStep` now maps to the non-retryable
//! `RuntimeError::DispatchRefused`, and the loop stops on it with
//! [`ShutdownReason::DispatchRefused`], carrying the refused molecule and
//! step kind on [`RunReport::refusal`](cosmon_runtime::RunReport). We drain
//! a DAG whose one ready node is a **gate** step and assert the loop exits
//! with the typed reason on the first dispatching tick — RED before the fix
//! (the loop ran the full `max_runtime` and reported `Deadline`), GREEN
//! after. The generous deadline doubles as the RED signal: reaching it at
//! all is the busy-loop.

use std::time::{Duration, Instant};

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::tackle_exec::LibraryExecutor;
use cosmon_runtime::{compile_plan, DagPolicy, Runtime, RuntimeConfig, ShutdownReason};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::MockBackend;

/// A pending molecule in the canonical fleet shape (the same fixture shape
/// as `library_executor.rs`).
fn pending_molecule(id: &str) -> MoleculeData {
    let now = chrono::Utc::now();
    MoleculeData {
        id: MoleculeId::new(id).expect("id"),
        fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet"),
        formula_id: cosmon_core::id::FormulaId::new("gate-first").expect("formula"),
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

/// The drain of a DAG whose ready node is a gate step must exit with the
/// typed `DispatchRefused` reason on the tick that observes the refusal —
/// never spin to the wall-clock deadline.
#[test]
fn gate_step_root_exits_with_dispatch_refused_within_one_tick() {
    // A galaxy whose one formula opens on a GATE step — the execution kind
    // the library executor refuses by contract.
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let init = std::process::Command::new("git")
        .args(["-C", &project.to_string_lossy(), "init", "-q"])
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let cosmon_dir = project.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::create_dir_all(&formulas_dir).expect("formulas dir");
    std::fs::write(cosmon_dir.join("config.toml"), "").expect("config marker");
    std::fs::write(
        formulas_dir.join("gate-first.formula.toml"),
        r#"formula = "gate-first"
version = 1
description = "opens on a gate step"

[[steps]]
id = "gate"
title = "a gate step the library executor refuses"
command = "true"

[[steps]]
id = "work"
title = "never reached"
"#,
    )
    .expect("formula");

    let store = FileStore::new(&state_dir);
    let mol = pending_molecule("task-20260905-9a7e");
    store.save_molecule(&mol.id, &mol).expect("seed molecule");

    // The same loop shape `run_drain` builds: compiled plan → DagPolicy →
    // Runtime over the library executor.
    let (plan, edges) =
        compile_plan(&store, std::slice::from_ref(&mol.id)).expect("compile plan");
    let policy = DagPolicy::new(plan, edges);
    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(50),
        // Generous on purpose: WITHOUT the fix the loop busy-loops the
        // refusal for this whole window and exits `Deadline` — the RED
        // shape. WITH it, the run ends in milliseconds.
        max_runtime: Some(Duration::from_secs(10)),
        ..RuntimeConfig::default()
    };
    let mut runtime = Runtime::new(
        Box::new(FileStore::new(&state_dir)),
        Box::new(policy),
        Box::new(LibraryExecutor::new(&project, MockBackend::new())),
        config,
    );

    let started = Instant::now();
    let report = runtime.run().expect("the loop must reach a named exit");

    assert_eq!(
        report.reason,
        ShutdownReason::DispatchRefused,
        "a permanent refusal must be a typed exit, not a retry-to-Deadline; \
         got {report:?}"
    );
    let refusal = report
        .refusal
        .as_ref()
        .expect("the report names the refused molecule");
    assert_eq!(refusal.molecule, mol.id);
    assert!(
        refusal.reason.contains("gate"),
        "the refusal names the step kind: {}",
        refusal.reason
    );
    // "Within one tick": the loop stopped on the tick that dispatched, not
    // after re-dispatching every poll interval to the 10 s deadline.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the loop must stop on the refusing tick, not run toward the \
         deadline (elapsed {:?}, ticks {})",
        started.elapsed(),
        report.ticks
    );

    // The molecule is honestly parked for the operator, not stranded
    // Running with no worker.
    let observed = store.load_molecule(&mol.id).expect("re-read");
    assert_eq!(observed.status, MoleculeStatus::Pending);
}
