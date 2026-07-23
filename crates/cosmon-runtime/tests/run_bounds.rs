// SPDX-License-Identifier: AGPL-3.0-only

//! B1/B2/B3 moussage bounds — named-exit tests.
//!
//! A worker is Turing-complete in what it nucleates, so "does this DAG
//! drain?" is undecidable in general. Totality is *forced* by bounds:
//!
//! - **B3 `max_actions`** (decreasing budget) — every applied action
//!   decrements it; the floor is the NAMED exit
//!   [`ShutdownReason::BudgetExhausted`], never a stall (I4).
//! - **B2 `max_molecules`** (cardinality) — a fleet wider than the
//!   bound exits [`ShutdownReason::MoleculeQuotaExceeded`].
//! - **B1 `max_depth`** is a compile-time property: [`dag_depth`] is
//!   what the caller (`cs run`, the tenant drain route) checks before
//!   starting the loop.
//!
//! The tests drive the REAL `Runtime::run` loop against a real
//! `FileStore`; only the worker is a stub that completes synchronously
//! (the LLM is not what these bounds are about).

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    compile_plan, dag_depth, DagPolicy, Executor, RunBounds, Runtime, RuntimeConfig, RuntimeError,
    ShutdownReason,
};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("valid molecule id")
}

fn seed(store: &dyn StateStore, id: &MoleculeId, links: Vec<MoleculeLink>) {
    let data = MoleculeData {
        id: id.clone(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Pending,
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
        typed_links: links,
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
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
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
        adapter: None,
    };
    store.save_molecule(id, &data).expect("seed molecule");
}

/// Executor stub that completes every dispatched molecule synchronously
/// — the fastest possible "worker", so the loop's own bookkeeping (not
/// worker latency) is what the bounds tests observe.
struct CompletingExecutor {
    store_path: std::path::PathBuf,
}

impl Executor for CompletingExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        let store = FileStore::new(&self.store_path);
        let mut data = store
            .load_molecule(id)
            .map_err(|e| RuntimeError::Dispatch {
                id: id.clone(),
                reason: e.to_string(),
            })?;
        data.status = MoleculeStatus::Completed;
        data.updated_at = Utc::now();
        store
            .save_molecule(id, &data)
            .map_err(|e| RuntimeError::Dispatch {
                id: id.clone(),
                reason: e.to_string(),
            })?;
        Ok(())
    }
}

/// Root + 3 children fan-out: root Blocks each child (the E2E shape of
/// the B1 gate).
fn seed_fanout(store: &dyn StateStore) -> MoleculeId {
    let root = mol_id("task-20260610-root");
    let kids = [
        mol_id("task-20260610-kid1"),
        mol_id("task-20260610-kid2"),
        mol_id("task-20260610-kid3"),
    ];
    seed(
        store,
        &root,
        kids.iter()
            .map(|k| MoleculeLink::Blocks { target: k.clone() })
            .collect(),
    );
    for k in &kids {
        seed(
            store,
            k,
            vec![MoleculeLink::BlockedBy {
                source: root.clone(),
            }],
        );
    }
    root
}

fn config() -> RuntimeConfig {
    RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        max_runtime: Some(Duration::from_secs(10)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    }
}

fn runtime_on(tmp: &TempDir, root: &MoleculeId, bounds: RunBounds) -> Runtime {
    let store = FileStore::new(tmp.path());
    let (plan, edges) = compile_plan(&store, std::slice::from_ref(root)).expect("compile");
    let policy = DagPolicy::new(plan, edges);
    Runtime::new(
        Box::new(FileStore::new(tmp.path())),
        Box::new(policy),
        Box::new(CompletingExecutor {
            store_path: tmp.path().to_path_buf(),
        }),
        config(),
    )
    .with_run_bounds(bounds)
}

#[test]
fn unbounded_default_drains_the_fanout() {
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    let root = seed_fanout(&store);

    let report = runtime_on(&tmp, &root, RunBounds::default())
        .run()
        .expect("run");
    assert_eq!(
        report.reason,
        ShutdownReason::PolicyDrained,
        "no bounds → pre-existing behaviour unchanged"
    );
    // 4 molecules, each dispatched exactly once.
    assert_eq!(report.actions_applied, 4);
    for raw in [
        "task-20260610-root",
        "task-20260610-kid1",
        "task-20260610-kid2",
        "task-20260610-kid3",
    ] {
        let m = store.load_molecule(&mol_id(raw)).unwrap();
        assert_eq!(m.status, MoleculeStatus::Completed, "{raw} must drain");
    }
}

#[test]
fn b3_budget_floor_is_a_named_exit_not_a_stall() {
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    let root = seed_fanout(&store);

    let report = runtime_on(
        &tmp,
        &root,
        RunBounds {
            max_actions: Some(2),
            max_molecules: None,
        },
    )
    .run()
    .expect("run");

    assert_eq!(
        report.reason,
        ShutdownReason::BudgetExhausted,
        "budget floor must be the NAMED exit, got {:?}",
        report.reason
    );
    assert_eq!(
        report.actions_applied, 2,
        "the bound is exact: action max+1 is never applied"
    );
}

#[test]
fn b2_molecule_quota_is_a_named_exit_before_any_dispatch() {
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    let root = seed_fanout(&store); // 4 molecules

    let report = runtime_on(
        &tmp,
        &root,
        RunBounds {
            max_actions: None,
            max_molecules: Some(3),
        },
    )
    .run()
    .expect("run");

    assert_eq!(report.reason, ShutdownReason::MoleculeQuotaExceeded);
    assert_eq!(
        report.actions_applied, 0,
        "quota is checked on the snapshot before dispatching"
    );
}

#[test]
fn b1_dag_depth_measures_the_longest_chain() {
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());

    // Chain root → a → b (depth 3) with a lateral sibling under root
    // (the diamond shoulder does not deepen the chain).
    let root = mol_id("task-20260610-droo");
    let a = mol_id("task-20260610-daaa");
    let b = mol_id("task-20260610-dbbb");
    let side = mol_id("task-20260610-dsid");
    seed(
        &store,
        &root,
        vec![
            MoleculeLink::Blocks { target: a.clone() },
            MoleculeLink::Blocks {
                target: side.clone(),
            },
        ],
    );
    seed(
        &store,
        &a,
        vec![
            MoleculeLink::BlockedBy {
                source: root.clone(),
            },
            MoleculeLink::Blocks { target: b.clone() },
        ],
    );
    seed(
        &store,
        &b,
        vec![MoleculeLink::BlockedBy { source: a.clone() }],
    );
    seed(
        &store,
        &side,
        vec![MoleculeLink::BlockedBy {
            source: root.clone(),
        }],
    );

    let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&root)).expect("compile");
    assert_eq!(dag_depth(&edges), 3, "longest chain root→a→b");

    // The fan-out shape of the E2E gate is depth 2.
    let tmp2 = TempDir::new().unwrap();
    let store2 = FileStore::new(tmp2.path());
    let root2 = seed_fanout(&store2);
    let (_p2, edges2) = compile_plan(&store2, std::slice::from_ref(&root2)).expect("compile");
    assert_eq!(dag_depth(&edges2), 2, "root + 3 children is depth 2");

    // Degenerate cases.
    assert_eq!(dag_depth(&[]), 0, "no edges, no nodes → depth 0");
}
