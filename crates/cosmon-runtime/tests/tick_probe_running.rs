// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the runtime's per-tick probe (round-3 / F-01 —
//! the realized-model runtime consumer seam).
//!
//! `cs run` installs a probe via [`Runtime::with_tick_probe`] that reads the
//! live claude/codex session of every in-scope `Running` molecule and emits
//! `ModelObserved` at the first model-bearing turn. This test proves the seam
//! itself: the probe fires for a `Running` molecule *during* the loop — i.e.
//! while the worker would still be alive — and never for molecules outside
//! the policy's scope or in non-Running states. The capture the production
//! probe performs is proven separately (energy_probe crash-durability tests);
//! together they close "observe during the run, not at teardown".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    Executor, FleetSnapshot, Policy, Runtime, RuntimeAction, RuntimeConfig, RuntimeError,
    ShutdownReason,
};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

/// Executor that never dispatches — the Running molecule just sits there,
/// modelling a live worker the runtime is babysitting.
struct InertExecutor;

impl Executor for InertExecutor {
    fn dispatch(&self, _id: &MoleculeId) -> Result<(), RuntimeError> {
        Ok(())
    }
}

/// Policy with no actions (drains on the first tick) — one tick is exactly
/// what the probe needs to prove it fires while the molecule is Running.
struct DrainedPolicy;

impl Policy for DrainedPolicy {
    fn next_actions(&mut self, _snapshot: &FleetSnapshot) -> Vec<RuntimeAction> {
        Vec::new()
    }
}

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("molecule id")
}

fn seed(store: &dyn StateStore, id: &MoleculeId, status: MoleculeStatus) {
    let now = Utc::now();
    let data = MoleculeData {
        id: id.clone(),
        fleet_id: FleetId::new("default").expect("fleet id"),
        formula_id: FormulaId::new("task-work").expect("formula id"),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: now,
        updated_at: now,
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

fn config() -> RuntimeConfig {
    RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        max_runtime: Some(Duration::from_secs(2)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    }
}

/// The tick probe fires for the Running molecule during the loop, and never
/// for a Completed sibling — the seam `cs run` uses to observe realized
/// models mid-run.
#[test]
fn tick_probe_fires_for_running_molecules_only() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());
    store
        .save_fleet(&cosmon_state::Fleet::default())
        .expect("save fleet");

    let running = mol_id("task-20260718-prb1");
    let done = mol_id("task-20260718-prb2");
    seed(&store, &running, MoleculeStatus::Running);
    seed(&store, &done, MoleculeStatus::Completed);

    let probed: Arc<Mutex<Vec<MoleculeId>>> = Arc::new(Mutex::new(Vec::new()));
    let probed_in = Arc::clone(&probed);

    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(
        store_box,
        Box::new(DrainedPolicy),
        Box::new(InertExecutor),
        config(),
    )
    .with_tick_probe(Box::new(move |id| {
        probed_in.lock().expect("lock").push(id.clone());
    }));

    // With a Running molecule the loop stays alive (has-running guard) until
    // the deadline — exactly the babysitting window the probe rides.
    let report = runtime.run().expect("run");
    assert_eq!(report.reason, ShutdownReason::Deadline);

    let probed = probed.lock().expect("lock");
    assert!(
        probed.contains(&running),
        "the probe must fire for the Running molecule while the loop is live"
    );
    assert!(
        !probed.contains(&done),
        "the probe must not fire for a non-Running molecule"
    );
}
