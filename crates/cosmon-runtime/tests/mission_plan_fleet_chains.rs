// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: a frozen prerequisite holds a multi-stage fleet DAG,
//! which can drain after that prerequisite completes and merges.
//!
//! # What this guards
//!
//! The first child has a `BlockedBy` link to a frozen mission. A freeze does
//! not certify success, so every downstream stage must remain pending. Once
//! the mission is completed and merged, a fresh run can dispatch the chain.

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
        mol.merged_at = Some(Utc::now());
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
        harvest_reason: None,
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
        base_branch: None,
        protected_paths: Vec::new(),
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
    };
    store.save_molecule(id, &data).expect("save molecule");
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// A delivered planner freeze hands off a lineage-linked first child while
/// sibling pipeline order still waits for successful integration.
#[test]
fn frozen_mission_plan_exposes_its_first_child() {
    let formula = include_str!("../../../.cosmon/formulas/mission-plan.formula.toml");
    assert!(formula.contains("--decayed-from {this_mission_mol_id}"));
    assert!(!formula.contains("--blocked-by {this_mission_mol_id}"));
    assert!(formula.contains("freeze_on_last_step = true"));
    let controller = include_str!("../../../.cosmon/formulas/mission-controller.formula.toml");
    assert!(controller.contains("--decayed-from {this_mission_id}"));
    assert!(!controller.contains("--blocked-by {this_mission_id}"));
    assert!(controller.contains("freeze_on_last_step = true"));

    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());
    let mission = mol_id("mission-20261001-aaaa");
    let first = mol_id("task-20261001-bbbb");
    let second = mol_id("task-20261001-cccc");
    seed(
        &store,
        &mission,
        MoleculeStatus::Frozen,
        true,
        vec![
            MoleculeLink::DecayProduct { id: first.clone() },
            MoleculeLink::DecayProduct { id: second.clone() },
        ],
    );
    seed(
        &store,
        &first,
        MoleculeStatus::Pending,
        false,
        vec![MoleculeLink::DecayedFrom {
            id: mission.clone(),
        }],
    );
    seed(
        &store,
        &second,
        MoleculeStatus::Pending,
        false,
        vec![
            MoleculeLink::DecayedFrom {
                id: mission.clone(),
            },
            MoleculeLink::BlockedBy {
                source: first.clone(),
            },
        ],
    );

    let frontier = cosmon_state::frontier::compute(&store).expect("frontier");
    assert_eq!(frontier.ready, vec![first.clone()]);

    let (plan, edges) =
        compile_plan(&store, std::slice::from_ref(&mission)).expect("compile planner DAG");
    assert!(edges.contains(&(mission.clone(), first.clone())));
    let mut runtime = Runtime::new(
        Box::new(FileStore::new(tmp.path())),
        Box::new(DagPolicy::new(plan, edges)),
        Box::new(CompletingExecutor::new(tmp.path().to_path_buf())),
        RuntimeConfig {
            poll_interval: Duration::from_millis(1),
            max_runtime: Some(Duration::from_secs(10)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        },
    );
    let report = runtime.run().expect("runtime run");
    assert_eq!(report.reason, ShutdownReason::PolicyDrained);
    assert_eq!(
        store.load_molecule(&first).expect("first").status,
        MoleculeStatus::Completed
    );
    assert_eq!(
        store.load_molecule(&second).expect("second").status,
        MoleculeStatus::Completed
    );
    assert_eq!(
        store.load_molecule(&mission).expect("mission").status,
        MoleculeStatus::Frozen
    );
}

/// Reproduce a mission-plan fleet topology:
///
/// ```text
///   mission (Frozen)  →  architect  →  builder{1..5}  →  red-team  →  soundness  →  integrator(sink)
/// ```
///
/// Children carry only `BlockedBy` links. The mission is already `Frozen`
/// (it decomposed and parked via `freeze_on_last_step`). The first run must
/// hold the chain; after completing the mission, the second run drains it.
#[test]
#[allow(clippy::too_many_lines)] // one cohesive end-to-end fleet scenario
fn mission_plan_fleet_waits_for_frozen_blocker_then_drains_after_completion() {
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

    // A run can report policy drain with pending descendants when no node is
    // ready; the persisted statuses distinguish this from completed work.
    assert_eq!(
        report.reason,
        ShutdownReason::PolicyDrained,
        "fleet should have no eligible work, got {:?}",
        report.reason,
    );

    // Every worker node stays Pending while the mission is Frozen.
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
            MoleculeStatus::Pending,
            "{id} must remain pending behind the frozen mission — found {:?}",
            mol.status,
        );
    }
    let mission_final = final_store.load_molecule(&mission).expect("load mission");
    assert_eq!(
        mission_final.status,
        MoleculeStatus::Frozen,
        "the first run must not complete the frozen mission",
    );

    // Completion and integration are the explicit release gesture. The
    // executor stamps merged_at for each descendant it finishes as well.
    let mut mission_done = mission_final;
    mission_done.status = MoleculeStatus::Completed;
    mission_done.merged_at = Some(Utc::now());
    final_store
        .save_molecule(&mission, &mission_done)
        .expect("complete mission");
    let (plan, edges) = compile_plan(&final_store, std::slice::from_ref(&integrator))
        .expect("recompile after release");
    let mut runtime = Runtime::new(
        Box::new(FileStore::new(tmp.path())),
        Box::new(DagPolicy::new(plan, edges)),
        Box::new(CompletingExecutor::new(tmp.path().to_path_buf())),
        RuntimeConfig {
            poll_interval: Duration::from_millis(1),
            max_runtime: Some(Duration::from_secs(10)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        },
    );
    let report = runtime.run().expect("runtime should drain after release");
    assert_eq!(report.reason, ShutdownReason::PolicyDrained);
    for id in &all_terminal {
        let mol = final_store
            .load_molecule(id)
            .expect("load released molecule");
        assert_eq!(mol.status, MoleculeStatus::Completed, "{id} should drain");
    }
}
