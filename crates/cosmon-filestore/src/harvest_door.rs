// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest door as a **library** — issue #54 U3, ADR-176 §-amendment.
//!
//! # Why this module exists
//!
//! Until issue #54, the door's body lived in `cosmon-cli`'s binary-private
//! `cmd/land.rs`, and the only way for the §8p adapter to open it was the
//! ADR-080 §3.5 clause (e) subprocess envelope around the `cs` binary — a
//! binary the shipped adapter image does not carry. This module is the door
//! itself, callable in-process: the ordered refusal checks, the trunk-lock
//! discipline, and the post-effect interpretation, each returning the same
//! typed vocabulary ([`DoorRefusal`], [`DoorOutcome`]) the CLI renders as
//! exit codes 70–76 and the route renders as wire labels.
//!
//! # The two halves, and the seam between them
//!
//! The door has a **decision half** — is this harvest admissible at all? —
//! and an **effect half** — the sealed `cs done` transaction that merges,
//! records, and tears down. Only the decision half lives here today. The
//! effect half is injected through [`SealedHarvestEffect`], because its one
//! production implementation is still `cmd/done.rs`'s sealed-door path: the
//! merge with lineage trailers, the publish/identity/confidentiality gates,
//! the pre/post hooks, the teardown. Reimplementing that here would create a
//! second door that drifts from the first — the exact failure the shared
//! [`DoorRefusal`] vocabulary exists to prevent — so the port is the seam,
//! and the mission's later unit replaces the subprocess implementation with
//! a library one without touching the decision half again.
//!
//! # The trunk flock (ADR-176 §-amendment, the I1 condition)
//!
//! The advisory `trunk.lock` flock is what makes I1 WRITER-UNIQUE true, and
//! it must bind **exactly once per harvest, at the effect boundary**. Where
//! that is depends on the effect:
//!
//! - The sealed `cs done` transaction — whether reached in-process or as a
//!   subprocess — acquires the flock at its own effect boundary (ADR-172
//!   D3: facts are re-derived under the lock, where the mutation happens).
//!   Such an effect declares [`SealedHarvestEffect::binds_trunk_lock`] and
//!   the door stands aside. It must: `flock(2)` does not nest — a child
//!   process blocks forever against its parent's lock, and a second
//!   descriptor in the same process blocks against the first — so a door
//!   that held the lock across such an effect would deadlock, not
//!   serialize.
//! - An effect with no boundary of its own is serialized **by the door**:
//!   [`land`] wraps it in [`StateStore::lock_trunk`] — the existing RAII
//!   helper, same `.cosmon/state/trunk.lock` path, same blocking semantics,
//!   released on every exit path including panic (guard drop runs on
//!   unwind). `two_concurrent_in_process_lands_serialize` is the falsifier:
//!   it goes red if this acquisition is removed.

use cosmon_core::config::ProjectConfig;
use cosmon_core::error::CosmonError;
use cosmon_core::harvest_door::{reservation_requiring_seal, DoorOutcome, DoorRefusal};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{MoleculeFilter, NonIntegration, NonIntegrationReason, StateStore};

/// A refusal from the door, carrying the named [`DoorRefusal`] and the
/// operator-facing specifics — the conflicted files, the reservation tag,
/// the backlog census. Never a raw stderr dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorRefused {
    /// Which of the seven named refusals fired.
    pub refusal: DoorRefusal,
    /// Specifics an operator can act on, when the door has any.
    pub detail: Option<String>,
}

impl DoorRefused {
    fn with(refusal: DoorRefusal, detail: impl Into<String>) -> Self {
        Self {
            refusal,
            detail: Some(detail.into()),
        }
    }
}

impl std::fmt::Display for DoorRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.refusal.as_str(), self.refusal.message())?;
        if let Some(detail) = &self.detail {
            write!(f, " — {detail}")?;
        }
        Ok(())
    }
}

/// Everything the door can answer with, other than success.
///
/// The split matters to every caller: a [`Self::Refused`] is an **outcome
/// of the door** with a stable name and a stable exit code, while the other
/// two are faults of the invocation or of the effect, which have neither.
#[derive(Debug)]
pub enum LandError {
    /// One of the seven named refusals. The CLI maps it to exit codes
    /// 70–76; the §8p route maps it to its wire label and status.
    Refused(DoorRefused),
    /// A malformed id, an unreadable store, an unacquirable lock — faults
    /// of the invocation, not outcomes of the door.
    Fault(CosmonError),
    /// The effect failed *and* recorded no trunk-side explanation. The
    /// string is the effect's own message; the door refuses to invent a
    /// named refusal for an outcome the closed set does not contain.
    EffectFailed(String),
}

impl std::fmt::Display for LandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => write!(f, "harvest refused ({refused})"),
            Self::Fault(e) => write!(f, "harvest fault: {e}"),
            Self::EffectFailed(msg) => write!(f, "harvest effect failed: {msg}"),
        }
    }
}

impl std::error::Error for LandError {}

impl From<CosmonError> for LandError {
    fn from(e: CosmonError) -> Self {
        Self::Fault(e)
    }
}

/// What the decision half concluded, when it did not refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorDecision {
    /// This exact harvest already landed (or archived, for the no-branch
    /// molecule). The caller reports the same success as the first call and
    /// mutates nothing — idempotence is what makes a retry over a network
    /// that loses responses safe.
    AlreadyLanded,
    /// Every pre-effect check passed; the sealed effect may proceed. The
    /// load-bearing authority check still happens *at the effect boundary*,
    /// under the trunk lock, with every fact re-derived there (ADR-172 D3)
    /// — this decision is the requester-readable preview, not the permit.
    Proceed,
}

/// The effect half of the door — the sealed `cs done` transaction.
///
/// The one production implementation today is `cmd/done.rs`'s sealed-door
/// path ([`super`] module docs say why it is injected rather than moved).
/// The contract every implementation carries: the trunk flock binds exactly
/// once, at the effect boundary — either inside the implementation (declare
/// it via [`Self::binds_trunk_lock`]) or around it, by the door.
pub trait SealedHarvestEffect {
    /// Whether this effect acquires the `trunk.lock` flock at its own
    /// effect boundary.
    ///
    /// `true` for the sealed `cs done` transaction in both of its shapes —
    /// in-process (it flocks before its first git mutation) and subprocess
    /// (the child flocks) — and the door then must **not** hold the lock
    /// across the call: `flock(2)` does not nest across processes or across
    /// descriptors, so holding it would deadlock the effect, not serialize
    /// it. `false` for an effect with no boundary of its own; the door
    /// serializes it under [`StateStore::lock_trunk`].
    fn binds_trunk_lock(&self) -> bool;

    /// Perform the sealed harvest of `molecule`.
    ///
    /// # Errors
    ///
    /// The implementation's own message. The door does not interpret it
    /// directly: on any return it re-reads the trunk-side record, which the
    /// sealed transaction writes under the lock on every failure path, and
    /// derives the named refusal from that record rather than from error
    /// text (a string match on a message is a mirror that drifts the first
    /// time someone edits the message).
    fn harvest(&mut self, molecule: &MoleculeId) -> Result<(), String>;
}

/// The door's decision half — the ordered pre-effect refusal checks.
///
/// The order of the checks is the order of the cost they avoid: the armed
/// second key first (a doctrine violation, and the cheapest read), then the
/// two that read only the molecule, the reservation scan, and the backlog
/// census last. Exactly the order `cmd/land.rs` established; moving the body
/// here must not reorder it, because the refusal a requester sees first is
/// part of the door's observable contract.
///
/// # Errors
///
/// [`LandError::Refused`] for every named pre-effect refusal;
/// [`LandError::Fault`] for an unreadable store.
pub fn decide(
    store: &dyn StateStore,
    cfg: &ProjectConfig,
    molecule: &MoleculeId,
) -> Result<DoorDecision, LandError> {
    // 1. The second key. A galaxy that has not armed harvest authority has
    //    granted nobody anything, and a door that proceeded anyway would be
    //    spending an authority that was never issued. Fail-closed.
    if !cfg.harvest_authority.is_required() {
        return Err(LandError::Refused(DoorRefused::with(
            DoorRefusal::NotAuthorized,
            "this galaxy has not armed `[harvest_authority] required`",
        )));
    }

    let mol = store.load_molecule(molecule)?;

    // 2. Idempotence, before admissibility: a harvest that already landed
    //    must report the same success on every retry, including a retry sent
    //    because the network ate the first response. `archived` is the
    //    disjunct that closes the no-branch molecule's loop — it never
    //    stamps `merged_at` because it has nothing to merge.
    if mol.merged_at.is_some() || mol.archived {
        return Ok(DoorDecision::AlreadyLanded);
    }

    // 3. Admissibility. `is_terminal()` is `Completed | Collapsed`, and only
    //    one of the two is work anyone asked to land — the ADR-176 §1 defect
    //    stated generally.
    if mol.status != MoleculeStatus::Completed {
        return Err(LandError::Refused(DoorRefused::with(
            DoorRefusal::NotCompleted,
            format!("status is {}", mol.status.as_str()),
        )));
    }

    // 4. Reservations. These name a condition somebody attached on purpose,
    //    and the door has no verdict to offer: the missing input is a human
    //    judgement. Checked here as well as at the effect boundary because a
    //    refusal the requester can read beats one buried in a merge log —
    //    the boundary check under the lock remains the load-bearing one,
    //    since a tag added after this line is still caught there.
    let tags: Vec<String> = mol.tags.iter().map(ToString::to_string).collect();
    if let Some(tag) = reservation_requiring_seal(&tags) {
        return Err(LandError::Refused(DoorRefused::with(
            DoorRefusal::ReservationRequiresSeal,
            format!("reserved by `{tag}`"),
        )));
    }

    // 5. The bounded queue (ADR-176 D7). Past the operator's sealed ceiling
    //    of closed-but-unintegrated molecules, further requests are refused.
    //    The ceiling is a field of the configuration, never a parameter: a
    //    queue the requester can lengthen is not a bound.
    let ceiling = cfg.harvest_authority.backlog_ceiling();
    let backlog = unintegrated_census(store)?;
    if backlog >= ceiling as usize {
        return Err(LandError::Refused(DoorRefused::with(
            DoorRefusal::BacklogFull,
            format!("{backlog} closed-but-unintegrated molecules, ceiling {ceiling}"),
        )));
    }

    Ok(DoorDecision::Proceed)
}

/// The whole door: decision, effect, interpretation.
///
/// `cs land` calls this with the sealed `cs done` transaction as the effect;
/// the §8p route calls it in-process. The trunk-lock discipline between the
/// two halves is the module header's §-amendment: the flock binds exactly
/// once, at the effect boundary, and the door acquires it only for an
/// effect that does not bind it itself.
///
/// # Errors
///
/// [`LandError::Refused`] for every named refusal — pre-effect from
/// [`decide`], post-effect from the trunk-side record; [`LandError::Fault`]
/// for invocation faults including an unacquirable lock;
/// [`LandError::EffectFailed`] when the effect failed and recorded nothing.
pub fn land(
    store: &dyn StateStore,
    cfg: &ProjectConfig,
    molecule: &MoleculeId,
    effect: &mut dyn SealedHarvestEffect,
) -> Result<DoorOutcome, LandError> {
    match decide(store, cfg, molecule)? {
        DoorDecision::AlreadyLanded => return Ok(DoorOutcome::AlreadyLanded),
        DoorDecision::Proceed => {}
    }

    // 6. The effect — serialized by whoever owns the flock (module header).
    //    The guard is RAII: a panicking effect releases the lock on unwind
    //    rather than wedging every later harvest of the kernel.
    let effect_result = if effect.binds_trunk_lock() {
        effect.harvest(molecule)
    } else {
        let _guard = store.lock_trunk("land")?;
        effect.harvest(molecule)
    };

    interpret_effect(store, molecule, effect_result)
}

/// The door's post-effect interpretation.
///
/// The effect succeeding is not by itself proof that the work landed:
/// `--if-completed` exits success on a no-op, and a molecule with no branch
/// archives without a merge. So the state is re-read and the trunk-side
/// record answers — for failure too, where the record beats the error text
/// (the sealed transaction writes `non_integration` under the lock on every
/// failure path).
fn interpret_effect(
    store: &dyn StateStore,
    molecule: &MoleculeId,
    effect_result: Result<(), String>,
) -> Result<DoorOutcome, LandError> {
    match effect_result {
        Ok(()) => {
            let after = store.load_molecule(molecule)?;
            match after.non_integration.as_ref() {
                None if after.merged_at.is_some() || after.archived => Ok(DoorOutcome::Landed),
                None => Err(LandError::Refused(DoorRefused::with(
                    DoorRefusal::NotCompleted,
                    "the harvest was a no-op and nothing was recorded",
                ))),
                Some(record) => Err(LandError::Refused(refusal_from_record(record))),
            }
        }
        Err(message) => {
            let recorded = store
                .load_molecule(molecule)
                .ok()
                .and_then(|m| m.non_integration);
            match recorded {
                Some(record) => Err(LandError::Refused(refusal_from_record(&record))),
                None => Err(LandError::EffectFailed(message)),
            }
        }
    }
}

/// Map the trunk-side non-integration record to a door refusal.
///
/// The two mechanical reasons are the ones ADR-176 D7 calls an *execution
/// event* and a *configuration error*; `PreDoneRefused` is the *verdict*.
/// `NoBranch` and `MergeSkipped` cannot arise on this path — the door never
/// passes `--no-merge`, and a molecule with no branch archives successfully —
/// so they are mapped to the refusal whose recovery is the same (a human
/// looks) rather than given a variant that no request can produce.
#[must_use]
pub fn refusal_from_record(record: &NonIntegration) -> DoorRefused {
    let refusal = match record.reason {
        NonIntegrationReason::Conflict => DoorRefusal::MergeConflict,
        NonIntegrationReason::MergeFailed => DoorRefusal::BaseNotFastForward,
        NonIntegrationReason::PreDoneRefused
        | NonIntegrationReason::NoBranch
        | NonIntegrationReason::MergeSkipped => DoorRefusal::PreDoneRefused,
    };
    let base = record
        .base_branch
        .as_ref()
        .map_or_else(String::new, |b| format!(" against {b}"));
    let extra = record
        .detail
        .as_ref()
        .map_or_else(String::new, |d| format!(": {d}"));
    DoorRefused::with(refusal, format!("{}{base}{extra}", record.reason.as_str()))
}

/// How many molecules in this kernel are closed but not integrated.
///
/// The census is the complement `merged_at.is_none()` restricted to
/// `Completed`, which is exactly the condition ADR-176 D7 bounds. Molecules
/// that never reached completion are not a debt: nobody is waiting on a
/// verdict for them.
///
/// # Errors
///
/// [`CosmonError`] when the store cannot enumerate its molecules.
pub fn unintegrated_census(store: &dyn StateStore) -> Result<usize, CosmonError> {
    let filter = MoleculeFilter {
        status: Some(MoleculeStatus::Completed),
        ..MoleculeFilter::default()
    };
    Ok(store
        .list_molecules(&filter)?
        .into_iter()
        .filter(|m| m.merged_at.is_none() && m.non_integration.is_some())
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileStore;
    use cosmon_core::config::HarvestAuthorityConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn armed() -> ProjectConfig {
        ProjectConfig {
            harvest_authority: HarvestAuthorityConfig {
                required: true,
                ..HarvestAuthorityConfig::default()
            },
            ..ProjectConfig::default()
        }
    }

    fn mol(raw: &str) -> MoleculeId {
        MoleculeId::new(raw).expect("fixture molecule id")
    }

    struct World {
        _tmp: TempDir,
        state_root: std::path::PathBuf,
        store: FileStore,
    }

    fn world() -> World {
        let tmp = TempDir::new().expect("tempdir");
        let state_root = tmp.path().join("state");
        let store = FileStore::new(&state_root);
        World {
            _tmp: tmp,
            state_root,
            store,
        }
    }

    fn plant(
        w: &World,
        id: &MoleculeId,
        status: MoleculeStatus,
        mutate: impl FnOnce(&mut cosmon_state::MoleculeData),
    ) {
        let mut data = cosmon_state::MoleculeData {
            id: id.clone(),
            fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet id"),
            formula_id: cosmon_core::id::FormulaId::new("task-work").expect("formula id"),
            status,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
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
            base_branch: None,
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
        mutate(&mut data);
        w.store.save_molecule(id, &data).expect("plant molecule");
    }

    /// An effect that records how deep the door lets callers overlap.
    struct OverlapProbe {
        inside: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    }

    impl SealedHarvestEffect for OverlapProbe {
        fn binds_trunk_lock(&self) -> bool {
            false // no boundary of its own: the door must serialize it.
        }

        fn harvest(&mut self, _molecule: &MoleculeId) -> Result<(), String> {
            let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(120));
            self.inside.fetch_sub(1, Ordering::SeqCst);
            Err("probe effect performs no harvest".to_owned())
        }
    }

    /// THE §-amendment falsifier (ADR-176): two concurrent **in-process**
    /// `land` calls on the same kernel serialize, because the door binds the
    /// same advisory `trunk.lock` flock the subprocess envelope used to
    /// delegate to the child `cs`. Remove the `lock_trunk` acquisition from
    /// [`land`] and `max_seen` reads 2 — this test goes red.
    #[test]
    fn two_concurrent_in_process_lands_serialize() {
        let w = world();
        let id = mol("task-20260904-lock");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let inside = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let state_root = w.state_root.clone();

        std::thread::scope(|scope| {
            for _ in 0..2 {
                let inside = Arc::clone(&inside);
                let max_seen = Arc::clone(&max_seen);
                let state_root = state_root.clone();
                let id = id.clone();
                scope.spawn(move || {
                    // Each thread opens its own store, as two requests in the
                    // adapter process would: the serialization must come from
                    // the flock on disk, not from sharing a Rust object.
                    let store = FileStore::new(state_root);
                    let mut effect = OverlapProbe { inside, max_seen };
                    let _ = land(&store, &armed(), &id, &mut effect);
                });
            }
        });

        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "two in-process harvests overlapped inside the effect: the door \
             no longer binds the trunk flock, and I1 WRITER-UNIQUE is broken",
        );
    }

    /// The lock is released on every exit path, including the effect
    /// panicking: a second harvest after a poisoned first must not hang.
    #[test]
    fn a_panicking_effect_releases_the_flock() {
        struct PanickingEffect;
        impl SealedHarvestEffect for PanickingEffect {
            fn binds_trunk_lock(&self) -> bool {
                false
            }
            fn harvest(&mut self, _molecule: &MoleculeId) -> Result<(), String> {
                panic!("effect crashed mid-harvest");
            }
        }

        let w = world();
        let id = mol("task-20260904-panic");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let state_root = w.state_root.clone();
        let id_clone = id.clone();
        let crashed = std::thread::spawn(move || {
            let store = FileStore::new(state_root);
            let _ = land(&store, &armed(), &id_clone, &mut PanickingEffect);
        })
        .join();
        assert!(crashed.is_err(), "the probe effect must actually panic");

        // A held lock would block this second call forever; the nonblocking
        // probe turns "forever" into a readable failure.
        std::env::set_var("COSMON_TRUNK_LOCK_NONBLOCKING", "1");
        let mut probe = OverlapProbe {
            inside: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
        };
        let outcome = land(&w.store, &armed(), &id, &mut probe);
        std::env::remove_var("COSMON_TRUNK_LOCK_NONBLOCKING");
        assert!(
            !matches!(outcome, Err(LandError::Fault(CosmonError::LockFailed { .. }))),
            "the panicked effect's guard was not released: {outcome:?}",
        );
    }

    /// An effect that binds the flock at its own boundary (the sealed
    /// `cs done` transaction) must run WITHOUT the door holding the lock —
    /// holding it would deadlock the child, not serialize it. Observable
    /// here: the effect can itself acquire the lock, nonblocking.
    #[test]
    fn a_self_binding_effect_runs_outside_the_doors_lock() {
        struct SelfBinding<'a> {
            store: &'a FileStore,
        }
        impl SealedHarvestEffect for SelfBinding<'_> {
            fn binds_trunk_lock(&self) -> bool {
                true
            }
            fn harvest(&mut self, _molecule: &MoleculeId) -> Result<(), String> {
                std::env::set_var("COSMON_TRUNK_LOCK_NONBLOCKING", "1");
                let acquired = self.store.acquire_trunk_lock("effect-boundary");
                std::env::remove_var("COSMON_TRUNK_LOCK_NONBLOCKING");
                match acquired {
                    Ok(_guard) => Err("boundary lock acquired as it must be".to_owned()),
                    Err(e) => Err(format!("DOOR HELD THE LOCK: {e}")),
                }
            }
        }

        let w = world();
        let id = mol("task-20260904-bind");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let mut effect = SelfBinding { store: &w.store };
        let out = land(&w.store, &armed(), &id, &mut effect);
        match out {
            Err(LandError::EffectFailed(msg)) => {
                assert!(
                    msg.contains("as it must be"),
                    "the effect could not take its own boundary lock: {msg}",
                );
            }
            other => panic!("probe effect must surface its message, got {other:?}"),
        }
    }

    struct InertEffect;
    impl SealedHarvestEffect for InertEffect {
        fn binds_trunk_lock(&self) -> bool {
            true // keep decision tests independent of the flock.
        }
        fn harvest(&mut self, _molecule: &MoleculeId) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn an_unarmed_galaxy_refuses_not_authorized_fail_closed() {
        let w = world();
        let id = mol("task-20260904-cold");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let out = land(&w.store, &ProjectConfig::default(), &id, &mut InertEffect);
        match out {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::NotAuthorized);
                assert!(r.detail.expect("detail").contains("harvest_authority"));
            }
            other => panic!("an unarmed galaxy must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_molecule_still_running_refuses_not_completed() {
        let w = world();
        let id = mol("task-20260904-live");
        plant(&w, &id, MoleculeStatus::Running, |_| {});

        match land(&w.store, &armed(), &id, &mut InertEffect) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::NotCompleted);
                assert_eq!(r.detail.as_deref(), Some("status is running"));
            }
            other => panic!("work in flight must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_reservation_names_the_tag_that_fired() {
        let w = world();
        let id = mol("task-20260904-held");
        plant(&w, &id, MoleculeStatus::Completed, |m| {
            m.tags
                .insert(cosmon_core::tag::Tag::new("needs-review").expect("tag"));
        });

        match land(&w.store, &armed(), &id, &mut InertEffect) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::ReservationRequiresSeal);
                assert!(r.detail.expect("detail").contains("needs-review"));
            }
            other => panic!("a reserved molecule must refuse, got {other:?}"),
        }
    }

    #[test]
    fn an_already_landed_molecule_reports_the_same_success_again() {
        let w = world();
        let id = mol("task-20260904-done");
        plant(&w, &id, MoleculeStatus::Completed, |m| {
            m.merged_at = Some(chrono::Utc::now());
        });

        let out = land(&w.store, &armed(), &id, &mut InertEffect).expect("idempotent");
        assert_eq!(out, DoorOutcome::AlreadyLanded);
    }

    #[test]
    fn a_full_backlog_refuses_with_the_census_and_the_ceiling() {
        let w = world();
        let ceiling = armed().harvest_authority.backlog_ceiling();
        for i in 0..ceiling {
            let id = mol(&format!("task-20260904-b{i:03}"));
            plant(&w, &id, MoleculeStatus::Completed, |m| {
                m.non_integration = Some(NonIntegration {
                    reason: NonIntegrationReason::PreDoneRefused,
                    at: chrono::Utc::now(),
                    base_branch: Some("main".to_owned()),
                    detail: None,
                });
            });
        }
        let id = mol("task-20260904-full");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(&w.store, &armed(), &id, &mut InertEffect) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::BacklogFull);
                let detail = r.detail.expect("detail");
                assert!(detail.contains(&ceiling.to_string()), "{detail}");
            }
            other => panic!("a full backlog must refuse, got {other:?}"),
        }
    }

    #[test]
    fn an_effect_no_op_that_recorded_nothing_is_refused_not_landed() {
        let w = world();
        let id = mol("task-20260904-noop");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(&w.store, &armed(), &id, &mut InertEffect) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::NotCompleted);
                assert!(r.detail.expect("detail").contains("no-op"));
            }
            other => panic!("a silent no-op must not read as landed, got {other:?}"),
        }
    }

    #[test]
    fn the_trunk_side_record_beats_the_effect_error_text() {
        struct FailingEffect<'a> {
            w: &'a World,
        }
        impl SealedHarvestEffect for FailingEffect<'_> {
            fn binds_trunk_lock(&self) -> bool {
                true
            }
            fn harvest(&mut self, molecule: &MoleculeId) -> Result<(), String> {
                // The sealed transaction records why under the lock, then
                // fails with unrelated prose — the record must win.
                let mut m = self.w.store.load_molecule(molecule).expect("load");
                m.non_integration = Some(NonIntegration {
                    reason: NonIntegrationReason::Conflict,
                    at: chrono::Utc::now(),
                    base_branch: Some("main".to_owned()),
                    detail: Some("src/lib.rs".to_owned()),
                });
                self.w.store.save_molecule(molecule, &m).expect("save");
                Err("prose that must not be parsed".to_owned())
            }
        }

        let w = world();
        let id = mol("task-20260904-clash");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(&w.store, &armed(), &id, &mut FailingEffect { w: &w }) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::MergeConflict);
                let detail = r.detail.expect("detail");
                assert!(detail.contains("against main"), "{detail}");
                assert!(detail.contains("src/lib.rs"), "{detail}");
            }
            other => panic!("the record must name the refusal, got {other:?}"),
        }
    }

    #[test]
    fn every_non_integration_reason_maps_to_a_named_refusal() {
        // Exhaustive over the persisted partition, so a sixth reason added
        // to `cosmon-state` cannot reach the door as an unnamed failure.
        use NonIntegrationReason as R;
        let cases = [
            (R::Conflict, DoorRefusal::MergeConflict),
            (R::MergeFailed, DoorRefusal::BaseNotFastForward),
            (R::PreDoneRefused, DoorRefusal::PreDoneRefused),
            (R::NoBranch, DoorRefusal::PreDoneRefused),
            (R::MergeSkipped, DoorRefusal::PreDoneRefused),
        ];
        for (reason, expected) in cases {
            let record = NonIntegration {
                reason,
                at: chrono::Utc::now(),
                base_branch: Some("main".to_owned()),
                detail: Some("two files".to_owned()),
            };
            let refused = refusal_from_record(&record);
            assert_eq!(refused.refusal, expected, "{reason:?} mapped wrong");
            assert!(refused.detail.expect("detail").contains("against main"));
        }
    }
}
