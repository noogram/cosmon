// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: a multi-stage mission-plan fleet DAG must run to
//! completion via **one** `cs run <sink>` with **zero** manual edge surgery.
//!
//! # What this guards
//!
//! Two cosmon-core defects broke mission-plan fleets end-to-end:
//!
//! - **BUG 1 (orphaned children).** A mission-plan mission `freeze`s itself
//!   on its last step (`freeze_on_last_step`) once it has decomposed into a
//!   child DAG. Every child is nucleated `--blocked-by mission`. A frozen
//!   mission is never `cs done`'d and so never stamps `merged_at` — and the
//!   frontier reducer used to gate `Frozen` predecessors on `merged_at`.
//!   Result: the first stage (`architect`) stayed `Pending` forever behind a
//!   frozen-but-unmerged blocker, the whole fleet froze, and the operator had
//!   to hand-delete the dead `blocked_by` link from every child's
//!   `state.json`.
//!
//! - **BUG 2 (drain at stage boundaries).** `cs run` chains a DAG by
//!   completing each stage and advancing to the next ready frontier. A
//!   blocker that has reached a terminal state (or has been torn down by an
//!   auto-`cs done`) must count as *resolved*. When it did not, a fan-in node
//!   (`red-team` blocked-by all five builders) could never see all its
//!   blockers satisfied at once, and the run drained with downstream nodes
//!   still `Pending`.
//!
//! The fix (in `cosmon_state::frontier::compute_from_molecules`) treats a
//! `Frozen`, `Collapsed`, or *absent* (torn-down) blocker as cleared. This
//! test reproduces the atlas fleet topology and asserts the whole reachable
//! DAG drains to terminal from a single `cs run <sink>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    compile_plan, DagPolicy, Executor, Runtime, RuntimeConfig, RuntimeError, ShutdownReason,
};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// CompletingExecutor — auto-completes dispatched molecules
// ---------------------------------------------------------------------------

/// Simulates instant workers: each dispatched molecule is immediately marked
/// `Completed` in the shared store so the runtime can advance through the
/// whole DAG without spawning real `cs tackle` panes. Mirrors the executor
/// used by `diamond_dag.rs`.
struct CompletingExecutor {
    store_path: PathBuf,
}

impl CompletingExecutor {
    fn new(store_path: PathBuf) -> Self {
        Self { store_path }
    }
}

impl Executor for CompletingExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        let store = FileStore::new(&self.store_path);
        let mut mol = store.load_molecule(id).map_err(RuntimeError::State)?;
        mol.status = MoleculeStatus::Completed;
        mol.current_step = mol.total_steps;
        mol.updated_at = Utc::now();
        store
            .save_molecule(&mol.id.clone(), &mol)
            .map_err(RuntimeError::State)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("test molecule id")
}

/// Seed a molecule with the given status and typed links. Children are wired
/// with **only** `BlockedBy` links (no reciprocal `Blocks`) — exactly the
/// shape `cs nucleate --blocked-by` produces, which is what made the dead-edge
/// class possible.
fn seed(
    store: &dyn StateStore,
    id: &MoleculeId,
    status: MoleculeStatus,
    freeze_on_last_step: bool,
    typed_links: Vec<MoleculeLink>,
) {
    let data = MoleculeData {
        id: id.clone(),
        fleet_id: FleetId::new("default").expect("fleet id"),
        formula_id: FormulaId::new("task-work").expect("formula id"),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links,
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        pending_step: None,
        merged_at: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    };
    store.save_molecule(id, &data).expect("save molecule");
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// Reproduce the atlas `atlas-cleanroom` fleet topology:
///
/// ```text
///   mission (Frozen)  →  architect  →  builder{1..5}  →  red-team  →  soundness  →  integrator(sink)
/// ```
///
/// Children carry only `BlockedBy` links. The mission is already `Frozen`
/// (it decomposed and parked via `freeze_on_last_step`). One `cs run <sink>`
/// — modelled here as `Runtime` + `DagPolicy` rooted at the integrator —
/// must drive every node to terminal with no manual edge surgery.
#[test]
#[allow(clippy::too_many_lines)] // one cohesive end-to-end fleet scenario
fn mission_plan_fleet_chains_to_completion_from_one_run() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let mission = mol_id("mission-20260604-m001");
    let architect = mol_id("task-20260604-arch");
    let builders: Vec<MoleculeId> = (1..=5)
        .map(|i| mol_id(&format!("task-20260604-bl0{i}")))
        .collect();
    let redteam = mol_id("task-20260604-rdtm");
    let soundness = mol_id("task-20260604-snds");
    let integrator = mol_id("task-20260604-intg"); // the sink

    // Mission: already Frozen post-decompose. It owns no Blocks link — the
    // children point UP at it via BlockedBy, exactly as `cs nucleate
    // --blocked-by mission` writes them. `merged_at` is None (a frozen
    // mission is never `cs done`'d).
    seed(&store, &mission, MoleculeStatus::Frozen, true, Vec::new());

    // architect blocked-by mission.
    seed(
        &store,
        &architect,
        MoleculeStatus::Pending,
        false,
        vec![MoleculeLink::BlockedBy {
            source: mission.clone(),
        }],
    );

    // Each builder blocked-by architect.
    for b in &builders {
        seed(
            &store,
            b,
            MoleculeStatus::Pending,
            false,
            vec![MoleculeLink::BlockedBy {
                source: architect.clone(),
            }],
        );
    }

    // red-team is a fan-in: blocked-by ALL five builders.
    seed(
        &store,
        &redteam,
        MoleculeStatus::Pending,
        false,
        builders
            .iter()
            .map(|b| MoleculeLink::BlockedBy { source: b.clone() })
            .collect(),
    );

    // soundness blocked-by red-team.
    seed(
        &store,
        &soundness,
        MoleculeStatus::Pending,
        false,
        vec![MoleculeLink::BlockedBy {
            source: redteam.clone(),
        }],
    );

    // integrator (sink) blocked-by soundness.
    seed(
        &store,
        &integrator,
        MoleculeStatus::Pending,
        false,
        vec![MoleculeLink::BlockedBy {
            source: soundness.clone(),
        }],
    );

    // `cs run <sink>`: compile from the integrator and walk the upstream cone.
    let (plan, edges) =
        compile_plan(&store, std::slice::from_ref(&integrator)).expect("compile_plan from sink");

    // The compiled cone must contain every node reachable from the sink —
    // proving `cs run <sink>` sees the whole fleet, not just one stage.
    let nodes: std::collections::HashSet<&MoleculeId> =
        edges.iter().flat_map(|(a, b)| [a, b]).collect();
    assert!(
        nodes.contains(&mission),
        "cone must include the frozen mission"
    );
    assert!(nodes.contains(&architect), "cone must include architect");
    for b in &builders {
        assert!(nodes.contains(b), "cone must include builder {b}");
    }
    assert!(nodes.contains(&integrator), "cone must include the sink");

    let policy = DagPolicy::new(plan, edges);
    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(1),
        max_runtime: Some(Duration::from_secs(10)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(
        store_box,
        Box::new(policy),
        Box::new(CompletingExecutor::new(tmp.path().to_path_buf())),
        config,
    );

    let report = runtime.run().expect("runtime should not error");

    // The run must drain because the whole DAG reached terminal — NOT because
    // it stalled. (Before the fix it drained on tick 1 with architect..sink
    // all still Pending, behind the frozen mission's unmerged blocker.)
    assert_eq!(
        report.reason,
        ShutdownReason::PolicyDrained,
        "fleet should drain by completion, got {:?}",
        report.reason,
    );

    // Every worker node must be Completed; the mission stays Frozen.
    let final_store = FileStore::new(tmp.path());
    let mut all_terminal = vec![
        architect.clone(),
        redteam.clone(),
        soundness.clone(),
        integrator.clone(),
    ];
    all_terminal.extend(builders.iter().cloned());
    for id in &all_terminal {
        let mol = final_store.load_molecule(id).expect("load molecule");
        assert_eq!(
            mol.status,
            MoleculeStatus::Completed,
            "{id} must be Completed after one cs run — found {:?} (orphaned behind a dead edge?)",
            mol.status,
        );
    }
    let mission_final = final_store.load_molecule(&mission).expect("load mission");
    assert_eq!(
        mission_final.status,
        MoleculeStatus::Frozen,
        "the frozen mission must stay frozen, not be resurrected",
    );
}
