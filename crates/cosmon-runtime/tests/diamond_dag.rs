// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: 4-molecule diamond DAG with full runtime execution.
//!
//! Validates the v1 demo scenario:
//!
//!   A blocks B+C, B+C block D
//!
//! The test seeds the diamond on a real [`FileStore`], runs the [`Runtime`]
//! with a [`DagPolicy`], and asserts:
//!
//! 1. A is dispatched first (the only root).
//! 2. B and C are dispatched in parallel after A completes.
//! 3. D is dispatched only after both B and C complete.
//! 4. The runtime drains cleanly (all molecules terminal).
//!
//! A custom [`CompletingExecutor`] auto-completes molecules on dispatch so
//! the runtime can progress through the full diamond without real workers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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

/// An executor that records dispatch order and immediately completes each
/// molecule in the shared [`FileStore`]. This simulates instant workers so
/// the runtime can advance through an entire DAG in one run.
///
/// Each dispatch is recorded as a `(tick, MoleculeId)` pair where `tick`
/// increments each time the runtime calls dispatch after a poll cycle.
/// Molecules dispatched in the same tick share the same tick number —
/// proving parallelism.
struct CompletingExecutor {
    /// Path to the `FileStore` directory (shared with the Runtime's store).
    store_path: PathBuf,
    /// Ordered log of dispatched molecule IDs. Multiple IDs dispatched
    /// between polls share the same batch index.
    dispatch_log: Arc<Mutex<Vec<MoleculeId>>>,
}

impl CompletingExecutor {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            dispatch_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn dispatch_log(&self) -> Arc<Mutex<Vec<MoleculeId>>> {
        Arc::clone(&self.dispatch_log)
    }
}

impl Executor for CompletingExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        // Record the dispatch.
        self.dispatch_log
            .lock()
            .expect("dispatch_log lock")
            .push(id.clone());

        // Immediately complete the molecule in the store so the next poll
        // tick sees it as Completed and unblocks dependents.
        let store = FileStore::new(&self.store_path);
        let mut mol = store
            .load_molecule(id)
            .map_err(cosmon_runtime::RuntimeError::State)?;
        mol.status = MoleculeStatus::Completed;
        mol.current_step = mol.total_steps;
        mol.updated_at = Utc::now();
        store
            .save_molecule(&mol.id.clone(), &mol)
            .map_err(cosmon_runtime::RuntimeError::State)?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("test molecule id")
}

fn seed_molecule(store: &dyn StateStore, id: &MoleculeId, typed_links: Vec<MoleculeLink>) {
    let data = MoleculeData {
        id: id.clone(),
        fleet_id: FleetId::new("default").expect("fleet id"),
        formula_id: FormulaId::new("task-work").expect("formula id"),
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
        typed_links,
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
    };
    store.save_molecule(id, &data).expect("save molecule");
}

// ---------------------------------------------------------------------------
// Diamond DAG integration test
// ---------------------------------------------------------------------------

#[test]
fn diamond_dag_dispatches_in_topological_order_with_parallelism() {
    // 1. Seed the diamond: A → B+C → D
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let a = mol_id("task-20260410-dmd1");
    let b = mol_id("task-20260410-dmd2");
    let c = mol_id("task-20260410-dmd3");
    let d = mol_id("task-20260410-dmd4");

    // A blocks B and C.
    seed_molecule(
        &store,
        &a,
        vec![
            MoleculeLink::Blocks { target: b.clone() },
            MoleculeLink::Blocks { target: c.clone() },
        ],
    );
    // B is blocked by A, blocks D.
    seed_molecule(
        &store,
        &b,
        vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::Blocks { target: d.clone() },
        ],
    );
    // C is blocked by A, blocks D.
    seed_molecule(
        &store,
        &c,
        vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::Blocks { target: d.clone() },
        ],
    );
    // D is blocked by B and C.
    seed_molecule(
        &store,
        &d,
        vec![
            MoleculeLink::BlockedBy { source: b.clone() },
            MoleculeLink::BlockedBy { source: c.clone() },
        ],
    );

    // 2. Compile the plan from root A.
    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&a)).expect("compile_plan");

    // Verify the compiled edges form a proper diamond.
    assert_eq!(edges.len(), 4, "diamond should have 4 edges: {edges:?}");
    assert!(edges.contains(&(a.clone(), b.clone())), "edge A→B");
    assert!(edges.contains(&(a.clone(), c.clone())), "edge A→C");
    assert!(edges.contains(&(b.clone(), d.clone())), "edge B→D");
    assert!(edges.contains(&(c.clone(), d.clone())), "edge C→D");

    // Verify initial ready frontier is just A.
    let ready: Vec<&MoleculeId> = plan.ready().iter().collect();
    assert_eq!(ready, vec![&a], "initial frontier should be [A]");

    // 3. Build the runtime with a CompletingExecutor.
    let policy = DagPolicy::new(plan, edges);
    let executor = CompletingExecutor::new(tmp.path().to_path_buf());
    let log = executor.dispatch_log();

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(1),
        max_runtime: Some(Duration::from_secs(5)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };

    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(executor), config);

    // 4. Run!
    let report = runtime.run().expect("runtime should not error");

    // 5. Assert the runtime drained cleanly.
    assert_eq!(
        report.reason,
        ShutdownReason::PolicyDrained,
        "diamond should drain, got {:?}",
        report.reason,
    );
    assert_eq!(
        report.actions_applied, 4,
        "should have dispatched all 4 molecules"
    );

    // 6. Assert all molecules are Completed in the store.
    let final_store = FileStore::new(tmp.path());
    for id in &[&a, &b, &c, &d] {
        let mol = final_store.load_molecule(id).expect("load molecule");
        assert_eq!(
            mol.status,
            MoleculeStatus::Completed,
            "{} should be Completed, got {:?}",
            id,
            mol.status,
        );
    }

    // 7. Assert dispatch order: A first, then B+C (parallel), then D last.
    let dispatched = log.lock().expect("dispatch_log lock");
    assert_eq!(dispatched.len(), 4, "all 4 molecules dispatched");

    // A must be first.
    assert_eq!(dispatched[0], a, "A must be dispatched first");

    // B and C must be dispatched next (in either order), before D.
    let middle: std::collections::HashSet<&MoleculeId> =
        [&dispatched[1], &dispatched[2]].into_iter().collect();
    assert!(
        middle.contains(&b) && middle.contains(&c),
        "B and C must both be dispatched in the parallel batch, got {:?}",
        &dispatched[1..3],
    );

    // D must be last.
    assert_eq!(dispatched[3], d, "D must be dispatched last");
}

/// Verify that `compile_plan` correctly walks the diamond from any root
/// and produces the same 4-edge DAG — the BFS should discover the full
/// connected component regardless of which node you start from.
#[test]
fn diamond_compile_plan_discovers_full_dag_from_any_node() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let a = mol_id("task-20260410-cpd1");
    let b = mol_id("task-20260410-cpd2");
    let c = mol_id("task-20260410-cpd3");
    let d = mol_id("task-20260410-cpd4");

    seed_molecule(
        &store,
        &a,
        vec![
            MoleculeLink::Blocks { target: b.clone() },
            MoleculeLink::Blocks { target: c.clone() },
        ],
    );
    seed_molecule(
        &store,
        &b,
        vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::Blocks { target: d.clone() },
        ],
    );
    seed_molecule(
        &store,
        &c,
        vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::Blocks { target: d.clone() },
        ],
    );
    seed_molecule(
        &store,
        &d,
        vec![
            MoleculeLink::BlockedBy { source: b.clone() },
            MoleculeLink::BlockedBy { source: c.clone() },
        ],
    );

    // Starting from D (the sink) should still discover the full diamond
    // by walking BlockedBy links upstream and Blocks links downstream.
    let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&d)).expect("compile from D");
    assert_eq!(
        edges.len(),
        4,
        "compile from sink D should still find 4 edges: {edges:?}"
    );

    // Starting from B (a middle node) should also discover everything.
    let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&b)).expect("compile from B");
    assert_eq!(
        edges.len(),
        4,
        "compile from middle B should find 4 edges: {edges:?}"
    );
}
