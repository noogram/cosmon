// SPDX-License-Identifier: AGPL-3.0-only

//! Regression tests for the phantom-workers bug surfaced by the accord
//! galaxy on 2026-04-25 (mission-20260424-0362, tenant-demo v1.2.0).
//!
//! The bug is documented in `docs/diagnostic/2026-04-25-phantom-workers.md`.
//! These tests cover the two structural fixes:
//!
//! 1. **Fix 1** (`crates/cosmon-cli/src/cmd/tackle.rs::tackle_as_runtime`):
//!    `cs tackle <id> --force-runtime` must not pre-mutate the root's
//!    `status` / `assigned_worker` / `session_name`. The runtime daemon
//!    is responsible for dispatching the root via the normal frontier
//!    path. Pre-mutating the root would lift it out of
//!    [`cosmon_state::frontier::compute_from_molecules`]'s
//!    `Pending && assigned_worker.is_none()` filter, leaving a phantom
//!    worker (status `Running`, zero progress, zero token cost).
//!
//!    The Fix-1 invariant is exercised here by asserting that the
//!    frontier reducer keeps a Pending-with-no-assigned-worker root
//!    visible — this is the contract `tackle_as_runtime` now upholds.
//!
//! 2. **Fix 2** (`crates/cosmon-runtime/src/dag_policy.rs::with_pre_completed`
//!    + `crates/cosmon-cli/src/cmd/run.rs::run`): `cs run <terminal-root>`
//!      pre-seeds the policy's `completed` skip-set with the named root
//!      so its forward `Blocks` dependents drain immediately at tick 0.
//!      Since task-20260706-4d1e a collapsed root also releases its
//!      descendants on its own when re-absorbed (blocked-by releases on
//!      done, not on verdict), so this hook is now the tick-0 fast path
//!      rather than the sole unblock mechanism.
//!
//! 3. **Part 2 — Fix A** (`crates/cosmon-runtime/src/lib.rs::Runtime::run`):
//!    a periodic in-loop `orphan_scan` resets any `Running` molecule
//!    whose tmux session is dead back to `Pending` (clearing
//!    `assigned_worker` and `session_name`), so the frontier reducer
//!    can re-dispatch it. Closes the gap between the one-shot startup
//!    orphan scan and the manual `cs purge` sweep — see
//!    `docs/diagnostic/2026-04-25-phantom-workers-part2-invariance-review.md`.

use std::collections::HashMap;

use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    compile_plan, DagPolicy, FleetSnapshot, LivenessCheck, NoOpExecutor, NoOpPolicy, Policy,
    Runtime, RuntimeAction, RuntimeConfig, ShutdownReason,
};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("test molecule id")
}

fn seed_molecule(
    store: &dyn StateStore,
    id: &MoleculeId,
    status: MoleculeStatus,
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
    store.save_molecule(id, &data).expect("save molecule");
}

/// Reproduces the symptom side of fix #1: a Pending DAG root with active
/// dependents must remain dispatchable by the runtime daemon (i.e. it
/// must surface in the frontier reducer). This is the post-fix invariant
/// `tackle_as_runtime` upholds by *not* writing `status = Running` /
/// `assigned_worker` on the root before the runtime daemon boots.
#[test]
fn pending_dag_root_with_active_dependents_stays_in_frontier() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    // Root with one alive `Blocks` dependent — has_active_dependents()
    // would route this through the runtime path in cs tackle.
    let root = mol_id("task-20260425-rt01");
    let child = mol_id("task-20260425-ch01");

    seed_molecule(
        &store,
        &root,
        MoleculeStatus::Pending,
        vec![MoleculeLink::Blocks {
            target: child.clone(),
        }],
    );
    seed_molecule(
        &store,
        &child,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: root.clone(),
        }],
    );

    let molecules = store
        .list_molecules(&cosmon_state::MoleculeFilter::default())
        .expect("list");
    let frontier = cosmon_state::frontier::compute_from_molecules(&molecules);

    assert!(
        frontier.contains(&root),
        "root must be in the frontier (Pending + no assigned_worker) so the \
         runtime daemon can dispatch it via cs tackle --leaf — this is the \
         exact invariant violated by the phantom-workers bug pre-fix"
    );
    assert!(
        !frontier.contains(&child),
        "child must remain gated by its still-Pending parent"
    );
}

/// Demonstrates the phantom-worker pathway: if the root is mutated to
/// `Running` with an `assigned_worker` (the *pre-fix* behaviour of
/// `tackle_as_runtime`), the frontier reducer drops it. With nothing to
/// dispatch and the children blocked-by a Running parent, the runtime
/// would loop on `"actions empty + has_running"` forever. This is the
/// scenario fix #1 makes structurally impossible.
#[test]
fn pre_fix_phantom_pathway_is_now_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let root = mol_id("task-20260425-pp01");
    let child = mol_id("task-20260425-pp02");

    // Pre-fix simulation: root has been pre-marked Running with an
    // assigned_worker pointing at the runtime tmux session. We seed
    // the post-fix store identically — without this rogue state, fix
    // 1 holds.
    let mut root_data = MoleculeData {
        id: root.clone(),
        fleet_id: FleetId::new("default").expect("fleet id"),
        formula_id: FormulaId::new("task-work").expect("formula id"),
        status: MoleculeStatus::Pending, // post-fix: stays Pending
        variables: HashMap::new(),
        assigned_worker: None, // post-fix: stays None
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
        typed_links: vec![MoleculeLink::Blocks {
            target: child.clone(),
        }],
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
    store.save_molecule(&root, &root_data).expect("save root");
    seed_molecule(
        &store,
        &child,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: root.clone(),
        }],
    );

    // Post-fix snapshot: root is in frontier, child is gated. Runtime
    // would dispatch root.
    let molecules = store
        .list_molecules(&cosmon_state::MoleculeFilter::default())
        .expect("list");
    let post_fix = cosmon_state::frontier::compute_from_molecules(&molecules);
    assert!(post_fix.contains(&root));
    assert!(!post_fix.contains(&child));

    // Now apply the pre-fix mutation in memory (do NOT save — this is
    // a no-store-side-effect simulation of the buggy state) and prove
    // that the frontier reducer would have dropped the root, which is
    // exactly how the runtime daemon used to find no actions to emit.
    root_data.status = MoleculeStatus::Running;
    root_data.assigned_worker =
        Some(cosmon_core::id::WorkerId::new("runtime-phantom-test-pp01").expect("wid"));

    let mut buggy_molecules = molecules.clone();
    if let Some(slot) = buggy_molecules.iter_mut().find(|m| m.id == root) {
        *slot = root_data;
    }
    let pre_fix_phantom = cosmon_state::frontier::compute_from_molecules(&buggy_molecules);
    assert!(
        !pre_fix_phantom.contains(&root),
        "pre-fix: root with status=Running + assigned_worker is invisible to \
         the frontier reducer, which is what created the phantom worker"
    );
    assert!(
        !pre_fix_phantom.contains(&child),
        "pre-fix: child remains blocked-by Running root, also invisible — \
         net result: zero actions, has_running=true, runtime loops forever"
    );
}

/// Fix #2 + task-20260706-4d1e: a Collapsed root releases its forward
/// `Blocks` descendants. Since task-20260706-4d1e a collapsed molecule is
/// absorbed as a terminal event on the first tick — it enters the skip-set
/// and splices its `Blocks` children — so descendants drain **without**
/// `with_pre_completed` (blocked-by releases on done, not on verdict). The
/// explicit `with_pre_completed` hook remains valid and idempotent: it
/// pre-seeds the same skip-set entry before tick 0.
#[test]
fn pre_completed_collapsed_root_releases_descendants() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let parent = mol_id("task-20260425-pc01");
    let child_a = mol_id("task-20260425-pc02");
    let child_b = mol_id("task-20260425-pc03");

    seed_molecule(
        &store,
        &parent,
        MoleculeStatus::Collapsed,
        vec![
            MoleculeLink::Blocks {
                target: child_a.clone(),
            },
            MoleculeLink::Blocks {
                target: child_b.clone(),
            },
        ],
    );
    seed_molecule(
        &store,
        &child_a,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: parent.clone(),
        }],
    );
    seed_molecule(
        &store,
        &child_b,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: parent.clone(),
        }],
    );

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&parent)).expect("compile");

    // Without pre-completion: since task-20260706-4d1e the collapsed
    // parent is absorbed as a terminal event on the first tick — it
    // enters the skip-set and splices its `Blocks` children — so the
    // descendants are released and dispatched even without the explicit
    // hook. (Before task-20260706-4d1e option B held them gated, which was
    // the "Torn down 1 completed molecule(s)" symptom of 2026-04-25.)
    let mut without_fix = DagPolicy::new(plan.clone(), edges.clone());
    let snapshot = FleetSnapshot::load(&store).expect("snapshot");
    let dispatched_without_fix: std::collections::HashSet<MoleculeId> = without_fix
        .next_actions(&snapshot)
        .iter()
        .filter_map(|a| match a {
            RuntimeAction::Evolve { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        dispatched_without_fix.contains(&child_a) && dispatched_without_fix.contains(&child_b),
        "collapsed parent releases both children on done, no explicit \
         pre-completion needed ({dispatched_without_fix:?})"
    );

    // With pre-completion (the fix-2 path): the parent enters the
    // skip-set, the rebuild_plan unblocks the children, and the policy
    // emits Evolve actions for both on the first tick.
    let mut with_fix =
        DagPolicy::new(plan, edges).with_pre_completed(std::iter::once(parent.clone()));
    let actions_with_fix = with_fix.next_actions(&snapshot);
    let dispatched: std::collections::HashSet<MoleculeId> = actions_with_fix
        .iter()
        .filter_map(|a| match a {
            RuntimeAction::Evolve { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        dispatched.contains(&child_a),
        "post-fix: child_a should be in the first ready frontier ({dispatched:?})"
    );
    assert!(
        dispatched.contains(&child_b),
        "post-fix: child_b should be in the first ready frontier ({dispatched:?})"
    );
}

/// Idempotency: pre-completing a molecule twice is a no-op. The hook
/// must tolerate being called on already-skipped ids without breaking
/// the plan.
#[test]
fn pre_completed_is_idempotent() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let root = mol_id("task-20260425-id01");
    let child = mol_id("task-20260425-id02");

    seed_molecule(
        &store,
        &root,
        MoleculeStatus::Completed,
        vec![MoleculeLink::Blocks {
            target: child.clone(),
        }],
    );
    seed_molecule(
        &store,
        &child,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: root.clone(),
        }],
    );

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&root)).expect("compile");
    let policy = DagPolicy::new(plan, edges)
        .with_pre_completed(std::iter::once(root.clone()))
        .with_pre_completed(std::iter::once(root.clone()));

    assert!(policy.completed().contains(&root));
}

// ----------------------------------------------------------------------------
// Part 2 — Fix A: in-loop liveness recheck resets orphaned Running molecules.
// ----------------------------------------------------------------------------

/// Stub liveness check that reports every session as dead. Used to
/// drive the in-loop recheck path deterministically without spinning
/// up a real tmux server.
struct AllDead;
impl LivenessCheck for AllDead {
    fn is_session_alive(&self, _session_name: &str) -> bool {
        false
    }
}

/// Mark a freshly-seeded molecule as Running with a stamped
/// `assigned_worker` and `session_name`, simulating the on-disk state
/// `cs tackle --leaf` produces just after spawning a worker. Used by
/// the Fix-A tests to mimic the moment after dispatch but before the
/// session itself dies.
fn mark_running_with_worker(store: &dyn StateStore, id: &MoleculeId, session: &str) {
    let mut mol = store.load_molecule(id).expect("load");
    mol.status = MoleculeStatus::Running;
    mol.assigned_worker = Some(WorkerId::new(session).expect("worker id"));
    mol.session_name = Some(session.to_owned());
    mol.updated_at = Utc::now();
    store.save_molecule(id, &mol).expect("save running");
}

/// Fix-A: a Running molecule whose worker tmux session has died (the
/// "phantom" state from the operator's reproduction) must be reset to
/// `Pending` by the in-loop liveness recheck, so the frontier reducer
/// re-surfaces it and the policy re-dispatches on the next tick.
///
/// The test runs the runtime with:
/// - `NoOpPolicy` so we never emit Evolve actions on top of the orphan
///   reset (we are testing the recheck, not the dispatch path);
/// - `liveness_recheck_every: Some(1)` so the recheck runs on the
///   first tick (production default is every 10 ticks);
/// - `AllDead` liveness so every Running molecule is classified
///   orphan;
/// - a short `max_runtime` deadline (the loop doesn't drain on its own
///   because we want to observe a state mutation, not a clean exit).
#[test]
fn part2_in_loop_liveness_resets_orphan_running_to_pending() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let root = mol_id("task-20260425-or01");
    seed_molecule(&store, &root, MoleculeStatus::Pending, Vec::new());
    mark_running_with_worker(&store, &root, "runtime-orphan-or01");

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        max_runtime: Some(Duration::from_millis(200)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: Some(1),
    };
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(
        store_box,
        Box::new(NoOpPolicy),
        Box::new(NoOpExecutor),
        config,
    )
    .with_liveness_check(Box::new(AllDead));

    // NoOpPolicy returns an empty action vec, so the loop drains on
    // the first tick — but only after the in-loop liveness check has
    // run. The drain reason is PolicyDrained.
    let report = runtime.run().expect("runtime should not error");
    assert_eq!(report.reason, ShutdownReason::PolicyDrained);
    assert!(report.ticks >= 1);

    // The orphan molecule should now be Pending with no assigned
    // worker — the frontier reducer will re-surface it on the next
    // dispatch cycle.
    let final_store = FileStore::new(tmp.path());
    let mol = final_store.load_molecule(&root).expect("load");
    assert_eq!(
        mol.status,
        MoleculeStatus::Pending,
        "orphan must be reset to Pending so frontier re-dispatches"
    );
    assert!(
        mol.assigned_worker.is_none(),
        "orphan reset must clear assigned_worker"
    );
    assert!(
        mol.session_name.is_none(),
        "orphan reset must clear session_name"
    );
}

/// The recheck must NOT fire when `liveness_recheck_every` is `None`
/// (test compatibility) or when the worker is alive (production
/// non-incident case). Asserts the in-loop hook is gated correctly so
/// healthy long-running molecules are not accidentally reset.
#[test]
fn part2_in_loop_liveness_skips_when_disabled() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let root = mol_id("task-20260425-or02");
    seed_molecule(&store, &root, MoleculeStatus::Pending, Vec::new());
    mark_running_with_worker(&store, &root, "runtime-healthy-or02");

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        max_runtime: Some(Duration::from_millis(80)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None, // disabled
    };
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(
        store_box,
        Box::new(NoOpPolicy),
        Box::new(NoOpExecutor),
        config,
    )
    .with_liveness_check(Box::new(AllDead));

    let _ = runtime.run().expect("runtime should not error");
    let final_store = FileStore::new(tmp.path());
    let mol = final_store.load_molecule(&root).expect("load");
    assert_eq!(
        mol.status,
        MoleculeStatus::Running,
        "with recheck disabled, Running orphan must NOT be reset"
    );
    assert!(
        mol.assigned_worker.is_some(),
        "with recheck disabled, assigned_worker must be preserved"
    );
}

/// The recheck must NOT touch a Running molecule whose session is
/// alive. This is the most common production case (the runtime
/// dispatched a worker, the worker is doing real work) and a false
/// positive would corrupt the DAG by re-dispatching healthy work.
#[test]
fn part2_in_loop_liveness_preserves_alive_running_molecules() {
    struct AllAlive;
    impl LivenessCheck for AllAlive {
        fn is_session_alive(&self, _session_name: &str) -> bool {
            true
        }
    }

    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let root = mol_id("task-20260425-or03");
    seed_molecule(&store, &root, MoleculeStatus::Pending, Vec::new());
    mark_running_with_worker(&store, &root, "runtime-alive-or03");

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        max_runtime: Some(Duration::from_millis(80)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: Some(1),
    };
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
    let mut runtime = Runtime::new(
        store_box,
        Box::new(NoOpPolicy),
        Box::new(NoOpExecutor),
        config,
    )
    .with_liveness_check(Box::new(AllAlive));

    let _ = runtime.run().expect("runtime should not error");
    let final_store = FileStore::new(tmp.path());
    let mol = final_store.load_molecule(&root).expect("load");
    assert_eq!(
        mol.status,
        MoleculeStatus::Running,
        "alive Running molecules must never be reset"
    );
    assert!(
        mol.assigned_worker.is_some(),
        "alive Running molecules must keep their worker assignment"
    );
    assert!(
        mol.session_name.is_some(),
        "alive Running molecules must keep their session_name"
    );
}
