// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: `freeze_on_last_step` runtime enforcement.
//!
//! Validates that when a molecule's `freeze_on_last_step` field is `true`,
//! the runtime transitions it from `Completed` → `Frozen` after the worker
//! finishes, instead of leaving it in `Completed`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
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
// Helpers
// ---------------------------------------------------------------------------

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("test molecule id")
}

fn seed_molecule(store: &dyn StateStore, id: &MoleculeId, freeze: bool) {
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
        typed_links: Vec::new(),
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: freeze,
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
    store.save_molecule(id, &data).expect("save molecule");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn molecule_with_freeze_on_last_step_ends_frozen() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let id = mol_id("task-20260411-frz1");
    seed_molecule(&store, &id, true);

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&id)).expect("compile_plan");
    let policy = DagPolicy::new(plan, edges);
    let executor = CompletingExecutor::new(tmp.path().to_path_buf());

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(1),
        max_runtime: Some(Duration::from_secs(5)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };

    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(executor), config);

    let report = runtime.run().expect("runtime should not error");
    assert_eq!(report.reason, ShutdownReason::PolicyDrained);

    // The molecule should be Frozen, not Completed.
    let final_store = FileStore::new(tmp.path());
    let mol = final_store.load_molecule(&id).expect("load molecule");
    assert_eq!(
        mol.status,
        MoleculeStatus::Frozen,
        "molecule with freeze_on_last_step=true should end Frozen, got {:?}",
        mol.status,
    );
}

#[test]
fn molecule_without_freeze_on_last_step_ends_completed() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let id = mol_id("task-20260411-nfr1");
    seed_molecule(&store, &id, false);

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&id)).expect("compile_plan");
    let policy = DagPolicy::new(plan, edges);
    let executor = CompletingExecutor::new(tmp.path().to_path_buf());

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(1),
        max_runtime: Some(Duration::from_secs(5)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };

    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(executor), config);

    let report = runtime.run().expect("runtime should not error");
    assert_eq!(report.reason, ShutdownReason::PolicyDrained);

    // The molecule should remain Completed (no freeze).
    let final_store = FileStore::new(tmp.path());
    let mol = final_store.load_molecule(&id).expect("load molecule");
    assert_eq!(
        mol.status,
        MoleculeStatus::Completed,
        "molecule with freeze_on_last_step=false should end Completed, got {:?}",
        mol.status,
    );
}
