// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test for the unscoped-dispatch stall.
//!
//! # The bug
//!
//! `cs run <root>` is a *connected-component* walk: it must touch only the
//! molecules reachable from `<root>` through `Blocks` / `BlockedBy` /
//! `DecayProduct` links. But the runtime's merge-before-dispatch pass
//! iterated the **whole** `FleetSnapshot` (which always carries the entire
//! store) and called [`Executor::on_complete`] — i.e. `cs done`, a real git
//! merge + worktree teardown — on *every* `Completed` molecule in the store.
//!
//! In a mature galaxy (showroom: 183 molecules, ~150 completed) that turned
//! tick 1 into a multi-minute subprocess storm: ~150 sequential `cs done`
//! calls against unrelated molecules. The storm blocked the loop long enough
//! for a human to beat the runtime to the ready frontier (`cs run` ran 144
//! ticks and applied **0 actions** while the operator manually `cs tackle`d
//! the root), and it merged branches the operator never named. The visible
//! tell was a `cs done` against a molecule that is not in the root's DAG
//! at all.
//!
//! # The fix under test
//!
//! [`cosmon_runtime::Policy::tracks_molecule`] scopes every per-molecule side
//! effect of the loop to the policy's DAG closure. This test proves the
//! merge pass honours that scope: a completed molecule **outside** the DAG is
//! never handed to `on_complete`, while the in-scope predecessor and the root
//! still are.

use std::collections::HashMap;
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

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("valid molecule id")
}

/// Seed one molecule with an explicit status / `merged_at` / typed links.
fn seed(
    store: &dyn StateStore,
    id: &MoleculeId,
    status: MoleculeStatus,
    merged_at: Option<chrono::DateTime<Utc>>,
    links: Vec<MoleculeLink>,
) {
    let data = MoleculeData {
        id: id.clone(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
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
        merged_at,
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

/// An [`Executor`] that records which molecules `on_complete` was called for,
/// and (on dispatch) flips the molecule to `Completed` in the store so the
/// runtime drains the DAG deterministically.
struct RecordingExecutor {
    store_path: std::path::PathBuf,
    /// Ids handed to `on_complete`, in call order.
    merged: Arc<Mutex<Vec<MoleculeId>>>,
    /// Ids handed to `dispatch`, in call order.
    dispatched: Arc<Mutex<Vec<MoleculeId>>>,
}

impl Executor for RecordingExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.dispatched.lock().unwrap().push(id.clone());
        // Simulate a worker that completes immediately so the DAG advances.
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

    fn on_complete(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.merged.lock().unwrap().push(id.clone());
        Ok(())
    }
}

/// `cs run <root>` over a two-node DAG (`pred → root`) must never `cs done`
/// an unrelated completed molecule (`outsider`) that happens to share the
/// store. Before the `tracks_molecule` fix the merge pass storms the whole
/// store; after it, only the DAG closure is touched.
#[test]
fn completion_pass_is_scoped_to_dag_closure() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let pred = mol_id("task-20260610-aaaa");
    let root = mol_id("task-20260610-bbbb");
    let outsider = mol_id("task-20260610-cccc");

    // pred → root (symmetric typed links, as `cs nucleate --blocks` writes).
    seed(
        &store,
        &pred,
        MoleculeStatus::Completed,
        // No merged_at yet: the runtime's merge pass is what stamps it, and
        // the frontier reducer gates `root` on it being stamped.
        None,
        vec![MoleculeLink::Blocks {
            target: root.clone(),
        }],
    );
    seed(
        &store,
        &root,
        MoleculeStatus::Pending,
        None,
        vec![MoleculeLink::BlockedBy {
            source: pred.clone(),
        }],
    );
    // An unrelated completed molecule, NOT reachable from `root`. The buggy
    // loop would `cs done` this; the fixed loop must not.
    seed(
        &store,
        &outsider,
        MoleculeStatus::Completed,
        None,
        Vec::new(),
    );

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&root)).expect("compile");
    let policy = DagPolicy::new(plan, edges);

    let merged = Arc::new(Mutex::new(Vec::new()));
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let executor = RecordingExecutor {
        store_path: tmp.path().to_path_buf(),
        merged: Arc::clone(&merged),
        dispatched: Arc::clone(&dispatched),
    };

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        max_runtime: Some(Duration::from_secs(5)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };
    let mut runtime = Runtime::new(
        Box::new(FileStore::new(tmp.path())),
        Box::new(policy),
        Box::new(executor),
        config,
    );
    let report = runtime.run().expect("runtime run");

    // The plan must have drained on its own (not hit the deadline) — proof
    // the DAG actually progressed rather than wedging.
    assert_eq!(
        report.reason,
        ShutdownReason::PolicyDrained,
        "runtime should drain the 2-node DAG, not time out"
    );

    let merged = merged.lock().unwrap();
    let dispatched = dispatched.lock().unwrap();

    // The outsider is completed but outside the DAG closure — it must NEVER
    // be merged. This is the load-bearing assertion: the regression let it
    // through (`cs done task-20260418-c3c8` in the ONCUE-100 log).
    assert!(
        !merged.contains(&outsider),
        "out-of-scope completed molecule must not be cs-done'd, got merged={merged:?}"
    );
    // The in-scope predecessor's branch must be merged so `root` can clear.
    assert!(
        merged.contains(&pred),
        "in-scope completed predecessor must be merged, got merged={merged:?}"
    );
    // And the runtime must have actually dispatched the root (the symptom the
    // operator hit was a runtime that registered the ensemble and dispatched
    // nothing).
    assert!(
        dispatched.contains(&root),
        "runtime must dispatch the ready root, got dispatched={dispatched:?}"
    );
}
