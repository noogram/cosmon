// SPDX-License-Identifier: AGPL-3.0-only

//! Keyed collapse against the real file store, across real processes
//! (issue #207, ADR-187).
//!
//! Each "process" is this test binary re-executed with
//! `--ignored --exact child_process`: the child reads its instructions from
//! the environment, runs `cosmon_state::ops::collapse::execute_with_hook`
//! against a `FileStore` on a shared directory, and writes its outcome to a
//! file. A child can be told to sleep at a stage (to hold a race window
//! open) or to `abort()` at a stage (a crash: no unwinding, no cleanup, the
//! kernel releases the lock).
//!
//! What is asserted is the requested property, not a proxy: the number of
//! `molecule_status_changed → collapsed` events in `events.jsonl`, and the
//! bytes of the receipts each caller got back.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use cosmon_core::collapse_contract::{
    CallerContext, CollapseCondition, CollapseOutcomeKind, CollapsibleStatus, IdempotencyKey,
    ReceiptLookup,
};
use cosmon_core::error::CosmonError;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::ops::collapse::{
    current_revision, execute, execute_with_hook, lookup_receipt, CollapseOutcome, CollapseRequest,
    CollapseStage, KeyedCollapse, NoLeaseCheck,
};
use cosmon_state::{MoleculeData, StateStore};

const KEY: &str = "broker-req-0001-abcdefgh";
const MOL: &str = "task-20261016-c0l1";
const REASON: &str = "operator approved the collapse";

fn molecule(id: &str) -> MoleculeData {
    let now = Utc::now();
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Running,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: now,
        updated_at: now,
        total_steps: 2,
        current_step: 1,
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
        tags: BTreeSet::new(),
        escalations: vec![],
        freeze_on_last_step: false,
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
        collapse_receipt: None,
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

/// A state dir holding one running molecule per id.
fn seeded(ids: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let store = FileStore::new(&state);
    for id in ids {
        let m = molecule(id);
        store.save_molecule(&m.id, &m).unwrap();
    }
    (tmp, state)
}

fn condition(state: &Path, id: &str) -> CollapseCondition {
    let store = FileStore::new(state);
    CollapseCondition {
        key: IdempotencyKey::parse(KEY).unwrap(),
        expect_status: CollapsibleStatus::Running,
        expect_revision: current_revision(&store, &MoleculeId::new(id).unwrap()).unwrap(),
        approval_digest: None,
        not_after: None,
    }
}

fn caller() -> CallerContext {
    CallerContext::Local { session: None }
}

fn keyed(
    state: &Path,
    id: &str,
    reason: &str,
    condition: CollapseCondition,
    hook: &dyn Fn(CollapseStage) -> Result<(), CosmonError>,
) -> KeyedCollapse {
    let store = FileStore::new(state);
    match execute_with_hook(
        &store,
        state,
        &NoLeaseCheck,
        &caller(),
        &MoleculeId::new(id).unwrap(),
        CollapseRequest::for_reason(reason),
        Some(condition),
        Utc::now(),
        hook,
    )
    .expect("keyed collapse must not error")
    {
        CollapseOutcome::Keyed(k) => k,
        CollapseOutcome::Unconditional { .. } => panic!("keyed request, unconditional outcome"),
    }
}

/// `molecule_status_changed → collapsed` events for `id`.
fn transition_events(state: &Path, id: &str) -> usize {
    let Ok(text) = std::fs::read_to_string(state.join("events.jsonl")) else {
        return 0;
    };
    fn is_transition(v: &serde_json::Value, id: &str) -> bool {
        match v {
            serde_json::Value::Object(map) => {
                (map.get("type").and_then(|t| t.as_str()) == Some("molecule_status_changed")
                    && map.get("molecule_id").and_then(|m| m.as_str()) == Some(id)
                    && map.get("to").and_then(|t| t.as_str()) == Some("collapsed"))
                    || map.values().any(|v| is_transition(v, id))
            }
            serde_json::Value::Array(items) => items.iter().any(|v| is_transition(v, id)),
            _ => false,
        }
    }
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| is_transition(v, id))
        .count()
}

fn status(state: &Path, id: &str) -> MoleculeStatus {
    FileStore::new(state)
        .load_molecule(&MoleculeId::new(id).unwrap())
        .unwrap()
        .status
}

fn lookup(state: &Path, id: &str) -> ReceiptLookup {
    lookup_receipt(
        &FileStore::new(state),
        &MoleculeId::new(id).unwrap(),
        &IdempotencyKey::parse(KEY).unwrap(),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// The child process
// ---------------------------------------------------------------------------

const ENV_STATE: &str = "KEYED_COLLAPSE_CHILD_STATE";
const ENV_REVISION: &str = "KEYED_COLLAPSE_CHILD_REVISION";
const ENV_OUT: &str = "KEYED_COLLAPSE_CHILD_OUT";
const ENV_SLEEP_AT: &str = "KEYED_COLLAPSE_CHILD_SLEEP_AT";
const ENV_ABORT_AT: &str = "KEYED_COLLAPSE_CHILD_ABORT_AT";

fn stage_name(stage: CollapseStage) -> &'static str {
    match stage {
        CollapseStage::Decided => "decided",
        CollapseStage::Reserved => "reserved",
        CollapseStage::StateSaved => "state_saved",
    }
}

/// Not a test of its own: the body of a child process. Inert unless the
/// parent set its environment.
#[test]
#[ignore = "child process body, run by the tests below"]
fn child_process() {
    let Ok(state) = std::env::var(ENV_STATE) else {
        return;
    };
    let state = PathBuf::from(state);
    let sleep_at = std::env::var(ENV_SLEEP_AT).ok();
    let abort_at = std::env::var(ENV_ABORT_AT).ok();
    let hook = move |stage: CollapseStage| -> Result<(), CosmonError> {
        if abort_at.as_deref() == Some(stage_name(stage)) {
            std::process::abort();
        }
        if sleep_at.as_deref() == Some(stage_name(stage)) {
            std::thread::sleep(Duration::from_millis(700));
        }
        Ok(())
    };
    let condition = CollapseCondition {
        key: IdempotencyKey::parse(KEY).unwrap(),
        expect_status: CollapsibleStatus::Running,
        expect_revision: cosmon_core::collapse_contract::MoleculeRevision::parse(
            &std::env::var(ENV_REVISION).unwrap(),
        )
        .unwrap(),
        approval_digest: None,
        not_after: None,
    };
    let out = keyed(&state, MOL, REASON, condition, &hook);
    let receipt = out
        .receipt
        .as_ref()
        .map(|r| String::from_utf8(r.canonical_bytes().unwrap()).unwrap());
    std::fs::write(
        std::env::var(ENV_OUT).unwrap(),
        serde_json::json!({ "outcome": out.outcome.as_str(), "receipt": receipt }).to_string(),
    )
    .unwrap();
}

fn spawn_child(state: &Path, revision: &str, out: &Path, extra: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--ignored", "--exact", "child_process", "--test-threads=1"])
        .stdout(std::process::Stdio::null())
        .env(ENV_STATE, state)
        .env(ENV_REVISION, revision)
        .env(ENV_OUT, out);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd
}

fn read_out(path: &Path) -> (String, Option<String>) {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    (
        v["outcome"].as_str().unwrap().to_owned(),
        v["receipt"].as_str().map(str::to_owned),
    )
}

// ---------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------

/// Two processes send the identical request at once: exactly one transition
/// event, and both get byte-equal receipts.
///
/// Each child sleeps after reserving the key, which holds the window between
/// "decided" and "state written" open for 700 ms. Under the dispatch lock
/// the second child cannot even read until the first is done, so it replays.
/// Replace `FileStore::lock_molecule` with a no-op guard and both children
/// decide on the running state, both write, and this test counts two events
/// and two different receipts.
#[test]
fn two_processes_with_the_identical_request_converge_on_one_transition() {
    let (tmp, state) = seeded(&[MOL]);
    let revision = condition(&state, MOL).expect_revision;
    let outs = [tmp.path().join("a.json"), tmp.path().join("b.json")];
    let mut children: Vec<_> = outs
        .iter()
        .map(|out| {
            spawn_child(
                &state,
                revision.as_str(),
                out,
                &[(ENV_SLEEP_AT, "reserved")],
            )
            .spawn()
            .unwrap()
        })
        .collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success(), "child process failed");
    }

    let (a, b) = (read_out(&outs[0]), read_out(&outs[1]));
    let mut outcomes = [a.0.as_str(), b.0.as_str()];
    outcomes.sort_unstable();
    assert_eq!(outcomes, ["replayed", "transitioned"]);
    assert!(a.1.is_some(), "both callers get a receipt");
    assert_eq!(a.1, b.1, "receipts must be byte-equal");
    assert_eq!(
        transition_events(&state, MOL),
        1,
        "exactly one transition event"
    );
    assert!(matches!(lookup(&state, MOL), ReceiptLookup::Committed(_)));
}

/// A crash after the key is reserved, before the state write: lookup says
/// reserved, the identical retry transitions, one event in total.
#[test]
fn a_crash_after_reservation_converges_on_retry() {
    let (tmp, state) = seeded(&[MOL]);
    let revision = condition(&state, MOL).expect_revision;
    let out = tmp.path().join("crash.json");
    let crashed = spawn_child(
        &state,
        revision.as_str(),
        &out,
        &[(ENV_ABORT_AT, "reserved")],
    )
    .status()
    .unwrap();
    assert!(!crashed.success(), "the child must have died at the stage");
    assert!(!out.exists());

    assert_eq!(status(&state, MOL), MoleculeStatus::Running);
    assert!(matches!(
        lookup(&state, MOL),
        ReceiptLookup::ReservedNotApplied { .. }
    ));
    assert_eq!(transition_events(&state, MOL), 0);

    // Restart: a fresh store instance, the identical request.
    let retry = keyed(&state, MOL, REASON, condition(&state, MOL), &|_| Ok(()));
    assert_eq!(retry.outcome, CollapseOutcomeKind::Transitioned);
    assert_eq!(transition_events(&state, MOL), 1);
    assert_eq!(
        lookup(&state, MOL),
        ReceiptLookup::Committed(retry.receipt.clone().unwrap())
    );

    let again = keyed(&state, MOL, REASON, condition_with(&revision), &|_| Ok(()));
    assert_eq!(again.outcome, CollapseOutcomeKind::Replayed);
    assert_eq!(again.receipt, retry.receipt);
    assert_eq!(transition_events(&state, MOL), 1);
}

fn condition_with(
    revision: &cosmon_core::collapse_contract::MoleculeRevision,
) -> CollapseCondition {
    CollapseCondition {
        key: IdempotencyKey::parse(KEY).unwrap(),
        expect_status: CollapsibleStatus::Running,
        expect_revision: revision.clone(),
        approval_digest: None,
        not_after: None,
    }
}

/// A crash after the state write, before the key record is committed: the
/// state's rename was the commit point, so lookup already says committed;
/// the identical retry replays, finishes the commit and announces the one
/// transition nobody announced.
#[test]
fn a_crash_after_the_state_save_converges_on_retry() {
    let (tmp, state) = seeded(&[MOL]);
    let revision = condition(&state, MOL).expect_revision;
    let out = tmp.path().join("crash.json");
    let crashed = spawn_child(
        &state,
        revision.as_str(),
        &out,
        &[(ENV_ABORT_AT, "state_saved")],
    )
    .status()
    .unwrap();
    assert!(!crashed.success());

    assert_eq!(status(&state, MOL), MoleculeStatus::Collapsed);
    let ReceiptLookup::Committed(before_retry) = lookup(&state, MOL) else {
        panic!("the state write is the commit point");
    };
    assert_eq!(
        transition_events(&state, MOL),
        0,
        "the crash preceded the events"
    );

    // The caller's revision is the one it confirmed before the crash; replay
    // ignores that the collapse itself moved it.
    let retry = keyed(&state, MOL, REASON, condition_with(&revision), &|_| Ok(()));
    assert_eq!(retry.outcome, CollapseOutcomeKind::Replayed);
    assert!(retry.announced, "the retry finished the commit");
    assert_eq!(retry.receipt.as_ref(), Some(&before_retry));
    assert_eq!(transition_events(&state, MOL), 1);

    let again = keyed(&state, MOL, REASON, condition_with(&revision), &|_| Ok(()));
    assert_eq!(again.outcome, CollapseOutcomeKind::Replayed);
    assert!(!again.announced);
    assert_eq!(transition_events(&state, MOL), 1);
    assert_eq!(lookup(&state, MOL), ReceiptLookup::Committed(before_retry));
}

/// Everything a refusal could have touched, by bytes.
fn snapshot(state: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if !p.ends_with("dispatch.lock") {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(state, &mut out);
    out.sort();
    out
}

/// A one-byte reason change, or the same key on another molecule, is
/// `idempotency_key_reused` and writes nothing.
#[test]
fn a_changed_request_under_a_used_key_is_refused_and_writes_nothing() {
    const OTHER: &str = "task-20261016-c0l2";
    let (_tmp, state) = seeded(&[MOL, OTHER]);
    let first = keyed(&state, MOL, REASON, condition(&state, MOL), &|_| Ok(()));
    assert_eq!(first.outcome, CollapseOutcomeKind::Transitioned);
    let before = snapshot(&state);

    let changed = keyed(
        &state,
        MOL,
        &format!("{REASON}."),
        condition(&state, MOL),
        &|_| Ok(()),
    );
    assert_eq!(changed.outcome, CollapseOutcomeKind::IdempotencyKeyReused);
    assert_ne!(changed.digest, first.digest);

    let elsewhere = keyed(&state, OTHER, REASON, condition(&state, OTHER), &|_| Ok(()));
    assert_eq!(elsewhere.outcome, CollapseOutcomeKind::IdempotencyKeyReused);

    assert_eq!(snapshot(&state), before, "a refusal writes nothing");
    assert_eq!(status(&state, OTHER), MoleculeStatus::Running);
}

/// A stale revision and a wrong status are distinct refusals, and neither
/// writes.
#[test]
fn stale_revision_and_unexpected_status_are_distinct_refusals() {
    let (_tmp, state) = seeded(&[MOL]);
    let confirmed = condition(&state, MOL);

    // Same status, something else changed since the caller read it.
    let store = FileStore::new(&state);
    let id = MoleculeId::new(MOL).unwrap();
    let mut m = store.load_molecule(&id).unwrap();
    m.tags.insert("temp:warm".parse().unwrap());
    store.save_molecule(&id, &m).unwrap();
    let before = snapshot(&state);

    let stale = keyed(&state, MOL, REASON, confirmed, &|_| Ok(()));
    assert_eq!(stale.outcome, CollapseOutcomeKind::StaleRevision);

    let mut wrong_status = condition(&state, MOL);
    wrong_status.expect_status = CollapsibleStatus::Frozen;
    let unexpected = keyed(&state, MOL, REASON, wrong_status, &|_| Ok(()));
    assert_eq!(unexpected.outcome, CollapseOutcomeKind::UnexpectedStatus);

    assert_ne!(stale.outcome.http_status(), 0);
    assert_eq!(snapshot(&state), before, "refusals write nothing");
    assert_eq!(status(&state, MOL), MoleculeStatus::Running);
}

/// An expired approval is refused, but a committed key still replays after
/// its deadline: expiry must not turn a known outcome into an unknown one.
#[test]
fn expiry_refuses_new_requests_but_not_replays() {
    let (_tmp, state) = seeded(&[MOL]);
    let mut c = condition(&state, MOL);
    c.not_after = Some(Utc::now() - chrono::Duration::seconds(1));
    let expired = keyed(&state, MOL, REASON, c, &|_| Ok(()));
    assert_eq!(expired.outcome, CollapseOutcomeKind::ApprovalExpired);
    assert_eq!(status(&state, MOL), MoleculeStatus::Running);

    let deadline = Utc::now() + chrono::Duration::milliseconds(300);
    let mut c = condition(&state, MOL);
    c.not_after = Some(deadline);
    let first = keyed(&state, MOL, REASON, c.clone(), &|_| Ok(()));
    assert_eq!(first.outcome, CollapseOutcomeKind::Transitioned);
    std::thread::sleep(Duration::from_millis(400));
    let replay = keyed(&state, MOL, REASON, c, &|_| Ok(()));
    assert_eq!(replay.outcome, CollapseOutcomeKind::Replayed);
}

/// The unconditional mode is today's behaviour: idempotent on collapsed,
/// and it runs without the dispatch lock (a worker's own `cs collapse`
/// under an in-process tackle must not wait on its parent).
#[test]
fn unconditional_collapse_does_not_wait_for_the_dispatch_lock() {
    let (_tmp, state) = seeded(&[MOL]);
    let id = MoleculeId::new(MOL).unwrap();
    let _held = FileStore::new(&state).acquire_dispatch_lock(&id).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    std::thread::spawn(move || {
        let store = FileStore::new(&worker_state);
        let out = execute(
            &store,
            &worker_state,
            &NoLeaseCheck,
            &caller(),
            &MoleculeId::new(MOL).unwrap(),
            CollapseRequest::for_reason(REASON),
            None,
            Utc::now(),
        )
        .map(|o| matches!(o, CollapseOutcome::Unconditional { .. }));
        let _ = tx.send(out.is_ok_and(|unconditional| unconditional));
    });
    let done = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("an unconditional collapse must not wait on a held dispatch lock");
    assert!(done);
    assert_eq!(status(&state, MOL), MoleculeStatus::Collapsed);
    assert_eq!(transition_events(&state, MOL), 1);
}

/// Controlled time for the production clock entrypoint, advanced while the
/// real dispatch lock is held by another thread.
struct DecisionClock {
    origin: chrono::DateTime<Utc>,
    millis: std::sync::atomic::AtomicI64,
}

impl cosmon_core::harness::Clock for DecisionClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        self.origin
            + chrono::Duration::milliseconds(self.millis.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// Observe the attempt before delegating to the real FileStore lock. Every
/// state/key read and write still uses the actual file adapter.
struct LockAttemptStore {
    inner: FileStore,
    attempted: std::sync::mpsc::Sender<()>,
}

impl StateStore for LockAttemptStore {
    fn load_fleet(&self) -> Result<cosmon_state::Fleet, CosmonError> {
        self.inner.load_fleet()
    }
    fn save_fleet(&self, fleet: &cosmon_state::Fleet) -> Result<(), CosmonError> {
        self.inner.save_fleet(fleet)
    }
    fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
        self.inner.load_molecule(id)
    }
    fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError> {
        self.inner.save_molecule(id, data)
    }
    fn list_molecules(
        &self,
        filter: &cosmon_state::MoleculeFilter,
    ) -> Result<Vec<MoleculeData>, CosmonError> {
        self.inner.list_molecules(filter)
    }
    fn molecule_dir(&self, id: &MoleculeId) -> PathBuf {
        self.inner.molecule_dir(id)
    }
    fn lock_molecule(
        &self,
        id: &MoleculeId,
    ) -> Result<Box<dyn cosmon_state::MoleculeGuard + '_>, CosmonError> {
        self.attempted
            .send(())
            .map_err(|_| CosmonError::StateStore {
                reason: "clock witness receiver closed".into(),
            })?;
        self.inner.lock_molecule(id)
    }
    fn load_molecule_revisioned(
        &self,
        id: &MoleculeId,
    ) -> Result<
        (
            MoleculeData,
            cosmon_core::collapse_contract::MoleculeRevision,
        ),
        CosmonError,
    > {
        self.inner.load_molecule_revisioned(id)
    }
    fn save_molecule_durable(
        &self,
        id: &MoleculeId,
        data: &MoleculeData,
    ) -> Result<(), CosmonError> {
        self.inner.save_molecule_durable(id, data)
    }
    fn read_collapse_key(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<cosmon_core::collapse_contract::CollapseKeyRecord>, CosmonError> {
        self.inner.read_collapse_key(key)
    }
    fn reserve_collapse_key(
        &self,
        record: &cosmon_core::collapse_contract::CollapseKeyRecord,
    ) -> Result<bool, CosmonError> {
        self.inner.reserve_collapse_key(record)
    }
    fn commit_collapse_key(
        &self,
        record: &cosmon_core::collapse_contract::CollapseKeyRecord,
    ) -> Result<(), CosmonError> {
        self.inner.commit_collapse_key(record)
    }
}

type ClockTestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn clocked_after_lock_wait(
    state: &Path,
    clock: std::sync::Arc<DecisionClock>,
    condition: CollapseCondition,
    advance_millis: i64,
) -> ClockTestResult<KeyedCollapse> {
    use std::sync::atomic::Ordering;
    let id = MoleculeId::new(MOL)?;
    let store = FileStore::new(state);
    let held = store.acquire_dispatch_lock(&id)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_state = state.to_path_buf();
    let worker_clock = clock.clone();
    let worker = std::thread::spawn(move || -> ClockTestResult<_> {
        let store = LockAttemptStore {
            inner: FileStore::new(&worker_state),
            attempted: tx,
        };
        Ok(cosmon_state::ops::collapse::execute_with_clock(
            &store,
            &worker_state,
            &NoLeaseCheck,
            &caller(),
            &id,
            CollapseRequest::for_reason(REASON),
            Some(condition),
            &*worker_clock,
        )?)
    });
    // This signal is inside the StateStore lock port, before the real blocking
    // flock. A pre-lock clock sample necessarily precedes the clock advance.
    rx.recv_timeout(Duration::from_secs(5))?;
    clock.millis.store(advance_millis, Ordering::SeqCst);
    drop(held);
    match worker
        .join()
        .map_err(|_| "clock regression worker panicked")??
    {
        CollapseOutcome::Keyed(outcome) => Ok(outcome),
        CollapseOutcome::Unconditional { .. } => Err("expected keyed outcome".into()),
    }
}

#[test]
fn approval_clock_is_sampled_after_lock_and_replay_wins_after_expiry() -> ClockTestResult<()> {
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    };
    let (_tmp, state) = seeded(&[MOL]);
    let origin = "2026-10-17T10:00:00Z".parse::<chrono::DateTime<Utc>>()?;
    let mut condition = condition(&state, MOL);
    condition.not_after = Some(origin + chrono::Duration::seconds(1));
    let clock = Arc::new(DecisionClock {
        origin,
        millis: AtomicI64::new(0),
    });
    let before = snapshot(&state);
    let expired = clocked_after_lock_wait(&state, clock.clone(), condition.clone(), 2000)?;
    assert_eq!(expired.outcome, CollapseOutcomeKind::ApprovalExpired);
    assert!(expired.receipt.is_none());
    assert_eq!(
        snapshot(&state),
        before,
        "expiry after waiting must write no state/key/event"
    );
    assert_eq!(lookup(&state, MOL), ReceiptLookup::Unknown);

    // A separate wait crosses 500 ms but stays inside the approval window.
    clock.millis.store(0, Ordering::SeqCst);
    let applied = clocked_after_lock_wait(&state, clock.clone(), condition.clone(), 500)?;
    assert_eq!(applied.outcome, CollapseOutcomeKind::Transitioned);
    let receipt = applied.receipt.ok_or("transitioned without receipt")?;
    assert_eq!(
        receipt.committed_at,
        origin + chrono::Duration::milliseconds(500)
    );
    let committed = snapshot(&state);

    // Same intent after expiry: the original durable receipt wins, HTTP 200.
    clock.millis.store(2000, Ordering::SeqCst);
    let result = cosmon_state::ops::collapse::execute_with_clock(
        &FileStore::new(&state),
        &state,
        &NoLeaseCheck,
        &caller(),
        &MoleculeId::new(MOL)?,
        CollapseRequest::for_reason(REASON),
        Some(condition),
        &*clock,
    )?;
    let CollapseOutcome::Keyed(replayed) = result else {
        return Err("expected keyed replay".into());
    };
    assert_eq!(replayed.outcome, CollapseOutcomeKind::Replayed);
    assert_eq!(replayed.outcome.http_status(), 200);
    assert_eq!(
        replayed
            .receipt
            .as_ref()
            .ok_or("replay receipt missing")?
            .canonical_bytes()?,
        receipt.canonical_bytes()?
    );
    assert_eq!(
        snapshot(&state),
        committed,
        "replay after expiry must not rewrite the commit"
    );
    assert_eq!(transition_events(&state, MOL), 1);
    Ok(())
}
