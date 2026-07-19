// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the anti-preemption human-claim lease.
//!
//! The resident runtime and a human are two concurrent writers racing on the
//! same `Pending → Active` transition. The runtime polls every few seconds
//! while a human types with fingers, so the runtime almost always wins and
//! **raffles a molecule the human manually `cs tackle`d** — a scope bug of
//! the convoy-cascade family. The fix is a dispatch lease recorded in the
//! molecule's own state (`tackled_by`), honoured by the walker's
//! pre-dispatch disk re-read.
//!
//! These tests exercise the legacy in-process walker
//! ([`Runtime`] + [`Runtime::apply_evolve`] via [`Executor::dispatch`]),
//! which is the documented production `cs run` path (`cs run <root>`, no
//! `--resident`). They prove:
//!
//! - **(a)** a molecule a human tackled (`tackled_by == human`) is never
//!   dispatched by the runtime, even though the policy surfaced it; and
//! - **(b)** the walker re-reads disk before dispatch and skips a candidate
//!   that flipped to `Running` since the (stale) snapshot the policy saw.
//!
//! A positive control proves an unclaimed pending molecule *is* dispatched
//! and stamped with a `runtime:<pid>` claim.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::tackle::TackledBy;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    Executor, FleetSnapshot, Policy, RuntimeAction, RuntimeConfig, RuntimeError, ShutdownReason,
};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Records dispatched molecule ids without completing them. The runtime
/// only calls [`Executor::dispatch`] for a molecule that survives the
/// `apply_evolve` guards — so an empty log proves the guard skipped the
/// molecule before any worker was spawned.
struct RecordingExecutor {
    dispatched: Arc<Mutex<Vec<MoleculeId>>>,
}

impl RecordingExecutor {
    fn new() -> Self {
        Self {
            dispatched: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn log(&self) -> Arc<Mutex<Vec<MoleculeId>>> {
        Arc::clone(&self.dispatched)
    }
}

impl Executor for RecordingExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.dispatched.lock().expect("lock").push(id.clone());
        Ok(())
    }
}

/// A policy that emits `Evolve` for a fixed set of ids on its first tick and
/// nothing afterwards. This models a *stale snapshot*: the policy believes a
/// molecule is dispatchable (it was `Pending` when last polled), and the
/// runtime must re-validate against disk truth before acting.
struct OneShotEvolvePolicy {
    ids: Vec<MoleculeId>,
    fired: bool,
}

impl OneShotEvolvePolicy {
    fn new(ids: Vec<MoleculeId>) -> Self {
        Self { ids, fired: false }
    }
}

impl Policy for OneShotEvolvePolicy {
    fn next_actions(&mut self, _snapshot: &FleetSnapshot) -> Vec<RuntimeAction> {
        if self.fired {
            return Vec::new();
        }
        self.fired = true;
        self.ids
            .iter()
            .map(|id| RuntimeAction::Evolve {
                id: id.clone(),
                evidence: "test: one-shot evolve".to_owned(),
            })
            .collect()
    }
}

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("molecule id")
}

/// Seed a molecule with an explicit status and optional dispatch claim.
fn seed(
    store: &dyn StateStore,
    id: &MoleculeId,
    status: MoleculeStatus,
    tackled_by: Option<TackledBy>,
) {
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
        tackled_by: tackled_by.clone(),
        tackled_at: tackled_by.map(|_| now),
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

// ---------------------------------------------------------------------------
// (a) human claim is never preempted
// ---------------------------------------------------------------------------

#[test]
fn runtime_never_dispatches_a_human_claimed_molecule() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());
    store
        .save_fleet(&cosmon_state::Fleet::default())
        .expect("save fleet");

    // A molecule a human manually tackled, then revised back to Pending.
    // The sticky `human` claim must survive the revision: the runtime sees
    // a Pending molecule but must NOT raffle it.
    let m = mol_id("task-20260531-hmn1");
    seed(&store, &m, MoleculeStatus::Pending, Some(TackledBy::Human));

    let executor = RecordingExecutor::new();
    let log = executor.log();
    let policy = OneShotEvolvePolicy::new(vec![m.clone()]);
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime =
        cosmon_runtime::Runtime::new(store_box, Box::new(policy), Box::new(executor), config());

    let report = runtime.run().expect("run");
    assert_eq!(report.reason, ShutdownReason::PolicyDrained);

    // The worker was never spawned: the human-claim guard fired before
    // dispatch.
    assert!(
        log.lock().expect("lock").is_empty(),
        "runtime must not dispatch a human-claimed molecule, dispatched: {:?}",
        log.lock().expect("lock")
    );

    // The molecule is untouched: still Pending, still human-claimed.
    let reloaded = store.load_molecule(&m).expect("reload");
    assert_eq!(reloaded.status, MoleculeStatus::Pending);
    assert_eq!(reloaded.tackled_by, Some(TackledBy::Human));
}

// ---------------------------------------------------------------------------
// (b) pre-dispatch disk re-read skips a candidate that flipped to Active
// ---------------------------------------------------------------------------

#[test]
fn walker_rereads_disk_and_skips_a_candidate_that_flipped_to_active() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());
    store
        .save_fleet(&cosmon_state::Fleet::default())
        .expect("save fleet");

    // The policy's snapshot is stale: it thinks `m` is dispatchable. On disk
    // `m` is already `Running` (a human's `cs tackle` landed in the gap, or
    // another writer flipped it). The walker's fresh `load_molecule`
    // re-read must see Running and skip — never re-dispatching it.
    let m = mol_id("task-20260531-flp1");
    seed(&store, &m, MoleculeStatus::Running, Some(TackledBy::Human));

    let executor = RecordingExecutor::new();
    let log = executor.log();
    let policy = OneShotEvolvePolicy::new(vec![m.clone()]);
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime =
        cosmon_runtime::Runtime::new(store_box, Box::new(policy), Box::new(executor), config());

    // The molecule sits in `Running` with no executor to complete it, so the
    // runtime legitimately waits for it (it is "alive") and exits on the
    // 2s deadline rather than draining. What matters is that it was never
    // re-dispatched.
    let report = runtime.run().expect("run");
    assert_eq!(report.reason, ShutdownReason::Deadline);

    assert!(
        log.lock().expect("lock").is_empty(),
        "walker must skip a candidate no longer Pending, dispatched: {:?}",
        log.lock().expect("lock")
    );

    // The pre-existing Running state is preserved — the runtime did not
    // stomp it.
    let reloaded = store.load_molecule(&m).expect("reload");
    assert_eq!(reloaded.status, MoleculeStatus::Running);
}

// ---------------------------------------------------------------------------
// positive control: an unclaimed pending molecule IS dispatched + claimed
// ---------------------------------------------------------------------------

#[test]
fn runtime_dispatches_unclaimed_pending_and_stamps_runtime_claim() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());
    store
        .save_fleet(&cosmon_state::Fleet::default())
        .expect("save fleet");

    let m = mol_id("task-20260531-fre1");
    seed(&store, &m, MoleculeStatus::Pending, None);

    let executor = RecordingExecutor::new();
    let log = executor.log();
    let policy = OneShotEvolvePolicy::new(vec![m.clone()]);
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime =
        cosmon_runtime::Runtime::new(store_box, Box::new(policy), Box::new(executor), config());

    runtime.run().expect("run");

    // The unclaimed molecule was dispatched...
    assert_eq!(
        *log.lock().expect("lock"),
        vec![m.clone()],
        "an unclaimed pending molecule must be dispatched"
    );

    // ...and the runtime stamped its own (non-sticky) dispatch claim.
    let reloaded = store.load_molecule(&m).expect("reload");
    assert_eq!(reloaded.status, MoleculeStatus::Running);
    match reloaded.tackled_by {
        Some(TackledBy::Runtime { pid }) => {
            assert_eq!(pid, std::process::id(), "claim must carry the walker pid");
        }
        other => panic!("expected a runtime claim, got {other:?}"),
    }
    assert!(
        reloaded.tackled_at.is_some(),
        "tackled_at must be stamped alongside the claim"
    );
    assert!(
        !reloaded.is_human_claimed(),
        "a runtime claim is not a human claim"
    );
}
