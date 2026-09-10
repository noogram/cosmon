// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest door as a **library** — issue #54 U3, ADR-176 §-amendment.
//!
//! # Why this module exists
//!
//! Until issue #54, the door's body lived in `cosmon-cli`'s binary-private
//! `cmd/land.rs` — a verb issue #51 later withdrew — and the only way for
//! the §8p adapter to open it was the
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
use cosmon_core::harvest_door::{
    reservation_requiring_seal, DoorOutcome, DoorRefusal, EffectFailure, HarvestOptions,
};
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
    /// No effect implementation is wired in this deployment. Typed rather
    /// than a magic string: the caller that has to answer `501` for it
    /// must not recognise it by comparing an error message, which is a
    /// mirror that drifts the first time somebody rewords the message.
    EffectUnavailable,
}

impl std::fmt::Display for LandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => write!(f, "harvest refused ({refused})"),
            Self::Fault(e) => write!(f, "harvest fault: {e}"),
            Self::EffectFailed(msg) => write!(f, "harvest effect failed: {msg}"),
            Self::EffectUnavailable => f.write_str("no harvest effect is wired in this deployment"),
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
    /// This exact harvest already happened — the branch landed, or the
    /// molecule was archived without one. The caller reports the same
    /// success as the first call and mutates nothing — idempotence is what
    /// makes a retry over a network that loses responses safe.
    ///
    /// `merged` is what keeps the retry as informative as the call it
    /// repeats: an archived `no_merge` closure and a landed merge are both
    /// "already done", and only this field separates them.
    AlreadyLanded {
        /// Whether the molecule's branch is on the trunk.
        merged: bool,
    },
    /// The request asked for nothing to be done in this condition:
    /// `if_completed` on a molecule that is not `Completed`. The caller
    /// reports [`DoorOutcome::NoOp`] and never reaches the effect —
    /// running it would spawn a whole sealed transaction to discover the
    /// same fact this store read already has.
    NoOp,
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

    /// Perform the sealed harvest of `molecule` with the caller's options.
    ///
    /// The options are the requester's, in full, since the D4 reversal —
    /// merge strategy, `force`, the hook waivers, the reason. They carry no
    /// authority: whether the effect may happen at all is still the
    /// operator's `[harvest_authority]` arming, checked by [`decide`] and
    /// re-derived at the effect boundary (D1, ADR-172 D3).
    ///
    /// # Errors
    ///
    /// [`EffectFailure`], which is typed on purpose. An implementation that
    /// knows *by name* why it refused — a `cs` child that exited on one of
    /// the door's codes 70–77, an authority boundary that declined before
    /// touching anything — says so with [`EffectFailure::Refused`], and the
    /// door keeps that name. For the unnamed [`EffectFailure::Failed`] the
    /// door still re-reads the trunk-side record, which the sealed
    /// transaction writes under the lock on every failure path, and derives
    /// the refusal from that record rather than from error text (a string
    /// match on a message is a mirror that drifts the first time someone
    /// edits the message).
    fn harvest(
        &mut self,
        molecule: &MoleculeId,
        options: &HarvestOptions,
    ) -> Result<(), EffectFailure>;
}

/// The door's decision half — the ordered pre-effect refusal checks.
///
/// The order of the checks is the order of the cost they avoid: the armed
/// second key first (a doctrine violation, and the cheapest read), then the
/// two that read only the molecule, the reservation scan, and the backlog
/// census last. Exactly the order the withdrawn `cmd/land.rs` established;
/// moving the body
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
    options: &HarvestOptions,
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
        return Ok(DoorDecision::AlreadyLanded {
            merged: mol.merged_at.is_some(),
        });
    }

    // 3. Admissibility. `is_terminal()` is `Completed | Collapsed`, and only
    //    one of the two is work anyone asked to land — the ADR-176 §1 defect
    //    stated generally.
    //
    //    `force` waives this check and nothing else, because that is exactly
    //    what `cs done --force` waives ("proceed even if the molecule is not
    //    in a terminal state"). Since the D4 reversal the flag crosses the
    //    wire, and a flag that crossed the wire but was silently refused
    //    here would be worse than one that never crossed: the requester
    //    would read `not_completed` for a request that named the override.
    //    It waives no authority: the second key above is checked before
    //    this, the reservation and backlog checks after it, and `force` is
    //    invisible to all three.
    //    `if_completed` is checked first and answers *success*, because it
    //    is a different question: `--force` says "close it anyway",
    //    `--if-completed` says "close it only if there is something to
    //    close". A sweep that sends the second has already declared this
    //    condition acceptable, and `cs done --if-completed` exits zero on
    //    it (`cmd/done.rs`'s `not_completed` no-op branch). The door
    //    answering `not_completed` for the same input was the door
    //    refusing an option it had just been handed — the PR #62 defect,
    //    reported against the newly exposed wire field.
    if options.if_completed && mol.status != MoleculeStatus::Completed {
        return Ok(DoorDecision::NoOp);
    }
    if !options.force && mol.status != MoleculeStatus::Completed {
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
/// The §8p route calls this with the requester's options and the
/// deployment's effect implementation. The trunk-lock discipline between the
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
    options: &HarvestOptions,
    effect: &mut dyn SealedHarvestEffect,
) -> Result<DoorOutcome, LandError> {
    // 0. The argument set, before anything is read. A harvest with no
    //    stated reason is refused rather than given a fabricated one — the
    //    one gap the reporters of issue #51 named explicitly in `land`.
    options.validate().map_err(|refusal| {
        LandError::Refused(DoorRefused::with(
            refusal,
            "the request named no reason for closing this molecule",
        ))
    })?;

    match decide(store, cfg, molecule, options)? {
        DoorDecision::AlreadyLanded { merged } => return Ok(DoorOutcome::AlreadyLanded { merged }),
        DoorDecision::NoOp => return Ok(DoorOutcome::NoOp),
        DoorDecision::Proceed => {}
    }

    // 6. The effect — serialized by whoever owns the flock (module header).
    //    The guard is RAII: a panicking effect releases the lock on unwind
    //    rather than wedging every later harvest of the kernel.
    let effect_result = if effect.binds_trunk_lock() {
        effect.harvest(molecule, options)
    } else {
        let _guard = store.lock_trunk("harvest")?;
        effect.harvest(molecule, options)
    };

    interpret_effect(store, molecule, options, effect_result)
}

/// The door's post-effect interpretation, **relative to what was asked**.
///
/// The effect succeeding is not by itself proof that the work landed:
/// `--if-completed` exits success on a no-op, and a molecule with no branch
/// archives without a merge. So the state is re-read and the trunk-side
/// record answers — for failure too, where the record beats the error text
/// (the sealed transaction writes `non_integration` under the lock on every
/// failure path).
///
/// # Why `options` is a parameter here
///
/// A record alone does not say whether what happened is what the requester
/// wanted. `merge-skipped` is written by a *successful* `cs done
/// --no-merge`, and reading it without the request produced the PR #62
/// defect: a closure the operator asked for came back as
/// `pre_done_refused`, a hook refusal nobody performed, and only the retry
/// — by then archived — reported success. Interpretation therefore takes
/// the requested options, and [`closure_without_merge`] is the one place
/// that decides which non-integration records a given request had asked
/// for.
fn interpret_effect(
    store: &dyn StateStore,
    molecule: &MoleculeId,
    options: &HarvestOptions,
    effect_result: Result<(), EffectFailure>,
) -> Result<DoorOutcome, LandError> {
    match effect_result {
        Ok(()) => {
            let after = store.load_molecule(molecule)?;
            match after.non_integration.as_ref() {
                None if after.merged_at.is_some() => Ok(DoorOutcome::Landed),
                // Archived with nothing recorded: terminal, and nothing
                // reached the trunk. Reporting it as `Landed` would tell a
                // caller the branch shipped when it did not.
                None if after.archived => Ok(DoorOutcome::ClosedWithoutMerge),
                // Nothing happened and nothing was recorded. Whether that
                // is a refusal depends entirely on the request: a harvest
                // that asked to run unconditionally got nothing and must
                // hear so, while `--if-completed` asked for exactly this
                // when there was nothing to close. The effect can reach
                // here with the option set — `force` carries a decision
                // past the pre-effect check and the sealed `cs done` then
                // takes its own no-op branch — so the interpretation half
                // must know the option too, not only `decide`.
                None if options.if_completed => Ok(DoorOutcome::NoOp),
                None => Err(LandError::Refused(DoorRefused::with(
                    DoorRefusal::NotCompleted,
                    "the harvest was a no-op and nothing was recorded",
                ))),
                Some(record) => closure_without_merge(options, &after, record)
                    .ok_or_else(|| LandError::Refused(refusal_from_record(record))),
            }
        }
        // A refusal the effect **named itself**. It is kept, whatever the
        // store holds: an authority boundary declines before it mutates
        // anything, so requiring a trunk-side record before believing the
        // name meant the most consequential refusals — the ones that
        // touched nothing — were the ones that lost it. The record is
        // still read, for the *detail* an exit code cannot carry (the
        // conflicted files), and only when it names the same refusal; a
        // record left by an earlier attempt cannot rename this one.
        Err(EffectFailure::Refused(refusal)) => {
            let detail = store
                .load_molecule(molecule)
                .ok()
                .and_then(|m| m.non_integration)
                .map(|record| refusal_from_record(&record))
                .filter(|from_record| from_record.refusal == refusal)
                .and_then(|from_record| from_record.detail);
            Err(LandError::Refused(DoorRefused { refusal, detail }))
        }
        Err(EffectFailure::Unavailable) => Err(LandError::EffectUnavailable),
        Err(EffectFailure::Failed(message)) => {
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

/// The non-integration records that are a **success** for this request,
/// or `None` when the record names something nobody asked for.
///
/// Both admitted shapes have the same signature on disk — archived,
/// terminal, no `merged_at` — and the same recovery, which is none: no
/// retry will integrate them, because integration was never the point.
///
/// * `merge-skipped` **and** `no_merge` requested: the operator asked for
///   closure without integration and got exactly that. Without the second
///   half of that condition the record still refuses — a skip nobody
///   requested is a fact about the world the requester must be told, and
///   this function is deliberately not a blanket amnesty for the reason
///   tag.
/// * `no-branch`: there was nothing to integrate. No option produces it and
///   none suppresses it; `cs done` archives and exits zero.
///
/// Everything else — a conflict, a hard merge failure, a refused
/// `pre_done` gate — is a refusal whatever was requested.
fn closure_without_merge(
    options: &HarvestOptions,
    after: &cosmon_state::MoleculeData,
    record: &NonIntegration,
) -> Option<DoorOutcome> {
    if !after.archived || after.merged_at.is_some() {
        return None;
    }
    match record.reason {
        NonIntegrationReason::MergeSkipped if options.no_merge => {
            Some(DoorOutcome::ClosedWithoutMerge)
        }
        NonIntegrationReason::NoBranch => Some(DoorOutcome::ClosedWithoutMerge),
        _ => None,
    }
}

/// Map the trunk-side non-integration record to a door refusal.
///
/// The two mechanical reasons are the ones ADR-176 D7 calls an *execution
/// event* and a *configuration error*; `PreDoneRefused` is the *verdict*.
///
/// `NoBranch` and `MergeSkipped` reach this function only when the door's
/// private `closure_without_merge` has already declined them — a `merge-skipped`
/// record on a request that never sent `no_merge`, or either record on a
/// molecule the effect left un-archived. Both mean the world did something
/// the requester did not ask for, and the recovery is the same as a refused
/// gate: a human looks. They are mapped there rather than given a variant
/// of their own, which is why this function is not the place that decides
/// whether a skip was requested.
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
            harvest_reason: None,
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

    /// The options every door test lands with: a stated reason and the
    /// documented defaults. A test that varied them would be testing the
    /// merge, not the door.
    fn opts() -> HarvestOptions {
        HarvestOptions::new("the door test closes this molecule")
    }

    impl SealedHarvestEffect for OverlapProbe {
        fn binds_trunk_lock(&self) -> bool {
            false // no boundary of its own: the door must serialize it.
        }

        fn harvest(
            &mut self,
            _molecule: &MoleculeId,
            _options: &HarvestOptions,
        ) -> Result<(), EffectFailure> {
            let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(120));
            self.inside.fetch_sub(1, Ordering::SeqCst);
            Err(EffectFailure::Failed(
                "probe effect performs no harvest".to_owned(),
            ))
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
                    let _ = land(&store, &armed(), &id, &opts(), &mut effect);
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
            fn harvest(
                &mut self,
                _molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
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
            let _ = land(&store, &armed(), &id_clone, &opts(), &mut PanickingEffect);
        })
        .join();
        assert!(crashed.is_err(), "the probe effect must actually panic");

        // A held lock would block this second call forever; the nonblocking
        // probe turns "forever" into a readable failure. The guard
        // serialises the process-global toggle against the blocking
        // trunk-lock tests (see `crate::trunk_lock_env_serial`).
        let _env = crate::trunk_lock_env_serial();
        std::env::set_var("COSMON_TRUNK_LOCK_NONBLOCKING", "1");
        let mut probe = OverlapProbe {
            inside: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
        };
        let outcome = land(&w.store, &armed(), &id, &opts(), &mut probe);
        std::env::remove_var("COSMON_TRUNK_LOCK_NONBLOCKING");
        assert!(
            !matches!(
                outcome,
                Err(LandError::Fault(CosmonError::LockFailed { .. }))
            ),
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
            fn harvest(
                &mut self,
                _molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
                std::env::set_var("COSMON_TRUNK_LOCK_NONBLOCKING", "1");
                let acquired = self.store.acquire_trunk_lock("effect-boundary");
                std::env::remove_var("COSMON_TRUNK_LOCK_NONBLOCKING");
                match acquired {
                    Ok(_guard) => Err(EffectFailure::Failed(
                        "boundary lock acquired as it must be".to_owned(),
                    )),
                    Err(e) => Err(EffectFailure::Failed(format!("DOOR HELD THE LOCK: {e}"))),
                }
            }
        }

        let w = world();
        let id = mol("task-20260904-bind");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        // Serialise the process-global toggle the effect flips (see
        // `crate::trunk_lock_env_serial`).
        let _env = crate::trunk_lock_env_serial();
        let mut effect = SelfBinding { store: &w.store };
        let out = land(&w.store, &armed(), &id, &opts(), &mut effect);
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
        fn harvest(
            &mut self,
            _molecule: &MoleculeId,
            _options: &HarvestOptions,
        ) -> Result<(), EffectFailure> {
            Ok(())
        }
    }

    #[test]
    fn an_unarmed_galaxy_refuses_not_authorized_fail_closed() {
        let w = world();
        let id = mol("task-20260904-cold");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let out = land(
            &w.store,
            &ProjectConfig::default(),
            &id,
            &opts(),
            &mut InertEffect,
        );
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

        match land(&w.store, &armed(), &id, &opts(), &mut InertEffect) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::NotCompleted);
                assert_eq!(r.detail.as_deref(), Some("status is running"));
            }
            other => panic!("work in flight must refuse, got {other:?}"),
        }
    }

    /// The gap the reporters of issue #51 named: `land` fabricated a
    /// generic reason where the caller supplied none. The door refuses
    /// instead — before it reads the store, so a caller with nothing to say
    /// never reaches the effect.
    #[test]
    fn a_harvest_with_no_reason_is_refused_not_given_a_generic_one() {
        let w = world();
        let id = mol("task-20260101-aaaa");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});
        let mut effect = InertEffect;
        for blank in ["", "   ", "\n\t"] {
            let out = land(
                &w.store,
                &armed(),
                &id,
                &HarvestOptions::new(blank),
                &mut effect,
            );
            match out {
                Err(LandError::Refused(r)) => {
                    assert_eq!(r.refusal, DoorRefusal::MissingReason);
                }
                other => panic!("a blank reason must be refused, got {other:?}"),
            }
        }
        // And the converse: the caller's own sentence passes through
        // unchanged rather than being replaced by one the door wrote.
        let opts = HarvestOptions::new("the spike answered its question");
        assert_eq!(opts.reason, "the spike answered its question");
        assert!(opts.validate().is_ok());
    }

    /// `force` changes behaviour on a molecule that is not terminal:
    /// refused without it, admitted with it.
    ///
    /// The falsifier that a parameter which crosses the wire is not inert
    /// once it gets there. A flag the requester can send and the door
    /// silently ignores is worse than one they cannot send: they would
    /// read `not_completed` for a request that named the override.
    #[test]
    fn force_waives_the_not_completed_refusal_and_nothing_else() {
        let w = world();
        let id = mol("task-20260101-bbbb");
        plant(&w, &id, MoleculeStatus::Running, |_| {});

        // Without it: the named refusal, as before.
        match decide(&w.store, &armed(), &id, &opts()) {
            Err(LandError::Refused(r)) => assert_eq!(r.refusal, DoorRefusal::NotCompleted),
            other => panic!("a running molecule must refuse not_completed, got {other:?}"),
        }

        // With it: admitted.
        let mut forced = opts();
        forced.force = true;
        assert_eq!(
            decide(&w.store, &armed(), &id, &forced).expect("force admits"),
            DoorDecision::Proceed,
        );

        // And it waives NOTHING else. The second key is checked before it,
        // the reservation after it; `force` is invisible to both.
        match decide(&w.store, &ProjectConfig::default(), &id, &forced) {
            Err(LandError::Refused(r)) => assert_eq!(r.refusal, DoorRefusal::NotAuthorized),
            other => panic!("force must not arm an unarmed galaxy, got {other:?}"),
        }
        let reserved = mol("task-20260101-cccc");
        plant(&w, &reserved, MoleculeStatus::Running, |m| {
            m.tags
                .insert(cosmon_core::tag::Tag::new("needs-review").expect("tag"));
        });
        match decide(&w.store, &armed(), &reserved, &forced) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::ReservationRequiresSeal);
            }
            other => panic!("force must not lift a human reservation, got {other:?}"),
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

        match land(&w.store, &armed(), &id, &opts(), &mut InertEffect) {
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

        let out = land(&w.store, &armed(), &id, &opts(), &mut InertEffect).expect("idempotent");
        assert_eq!(out, DoorOutcome::AlreadyLanded { merged: true });
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

        match land(&w.store, &armed(), &id, &opts(), &mut InertEffect) {
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

        match land(&w.store, &armed(), &id, &opts(), &mut InertEffect) {
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
            fn harvest(
                &mut self,
                molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
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
                Err(EffectFailure::Failed(
                    "prose that must not be parsed".to_owned(),
                ))
            }
        }

        let w = world();
        let id = mol("task-20260904-clash");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(
            &w.store,
            &armed(),
            &id,
            &opts(),
            &mut FailingEffect { w: &w },
        ) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::MergeConflict);
                let detail = r.detail.expect("detail");
                assert!(detail.contains("against main"), "{detail}");
                assert!(detail.contains("src/lib.rs"), "{detail}");
            }
            other => panic!("the record must name the refusal, got {other:?}"),
        }
    }

    /// An effect that closes the molecule the way `cs done --no-merge`
    /// does: it archives, and it records the deliberate skip trunk-side.
    /// Nothing failed — the operator asked for exactly this.
    struct SkippingEffect<'a> {
        w: &'a World,
        reason: NonIntegrationReason,
    }
    impl SealedHarvestEffect for SkippingEffect<'_> {
        fn binds_trunk_lock(&self) -> bool {
            true
        }
        fn harvest(
            &mut self,
            molecule: &MoleculeId,
            _options: &HarvestOptions,
        ) -> Result<(), EffectFailure> {
            let mut m = self.w.store.load_molecule(molecule).expect("load");
            m.archived = true;
            m.non_integration = Some(NonIntegration {
                reason: self.reason,
                at: chrono::Utc::now(),
                base_branch: Some("main".to_owned()),
                detail: Some("integration skipped by the operator".to_owned()),
            });
            self.w.store.save_molecule(molecule, &m).expect("save");
            Ok(())
        }
    }

    /// The PR #62 defect: a harvest that *asked* for `no_merge` and got
    /// exactly that read as `pre_done_refused` — a hook refusal nobody
    /// performed — because the door interpreted the record without ever
    /// looking at what was requested.
    #[test]
    fn a_requested_no_merge_closure_is_a_success_not_a_hook_refusal() {
        let w = world();
        let id = mol("task-20260909-skip");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let mut asked = opts();
        asked.no_merge = true;
        let out = land(
            &w.store,
            &armed(),
            &id,
            &asked,
            &mut SkippingEffect {
                w: &w,
                reason: NonIntegrationReason::MergeSkipped,
            },
        )
        .expect("a requested no-merge closure is a success");
        assert_eq!(out, DoorOutcome::ClosedWithoutMerge);
        assert!(!out.merged(), "nothing was merged and the outcome says so");

        // And the retry, now that the molecule is archived, is the
        // idempotent success — still naming that nothing landed.
        let again = land(
            &w.store,
            &armed(),
            &id,
            &asked,
            &mut SkippingEffect {
                w: &w,
                reason: NonIntegrationReason::MergeSkipped,
            },
        )
        .expect("the retry is idempotent");
        assert_eq!(again, DoorOutcome::AlreadyLanded { merged: false });
    }

    /// The converse, which is what keeps the fix from being a blanket
    /// amnesty: a `merge-skipped` record on a request that never asked
    /// for it is still a refusal. Interpretation is relative to the
    /// requested options, not to the record alone.
    #[test]
    fn an_unrequested_merge_skip_is_still_refused() {
        let w = world();
        let id = mol("task-20260909-nask");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(
            &w.store,
            &armed(),
            &id,
            &opts(),
            &mut SkippingEffect {
                w: &w,
                reason: NonIntegrationReason::MergeSkipped,
            },
        ) {
            Err(LandError::Refused(r)) => assert_eq!(r.refusal, DoorRefusal::PreDoneRefused),
            other => panic!("an unrequested skip must still refuse, got {other:?}"),
        }
    }

    /// The same defect on the path the old comment claimed could not
    /// arise: a molecule with no branch archives successfully and records
    /// `no-branch`, and the door read that success as a refused hook.
    #[test]
    fn a_molecule_with_no_branch_closes_successfully() {
        let w = world();
        let id = mol("task-20260909-nobr");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        let out = land(
            &w.store,
            &armed(),
            &id,
            &opts(),
            &mut SkippingEffect {
                w: &w,
                reason: NonIntegrationReason::NoBranch,
            },
        )
        .expect("a molecule with nothing to integrate closes");
        assert_eq!(out, DoorOutcome::ClosedWithoutMerge);
    }

    #[test]
    fn every_non_integration_reason_maps_to_a_named_refusal() {
        // Exhaustive over the persisted partition, so a sixth reason added
        // to `cosmon-state` cannot reach the door as an unnamed failure.
        //
        // This pins the *refusal* mapping only. Whether a record reaches
        // it at all is `closure_without_merge`'s question, asked first and
        // asked with the request in hand — which is why `NoBranch` and
        // `MergeSkipped` still appear here with a refusal beside them and
        // are nonetheless successes on the requests that asked for them.
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

    /// The PR #62 review's third finding, at the seam it names: a refusal
    /// the effect produced **without touching anything** keeps its name.
    ///
    /// `not_authorized` is the honest example — an authority boundary
    /// declines before it mutates, so there is no trunk-side record to
    /// re-derive the refusal from. The door used to require that record
    /// and, finding none, answered `EffectFailed`, which the route renders
    /// as an anonymous `500 harvest_failed`. Make `interpret_effect` fall
    /// back to reading the store for a typed refusal and this goes red on
    /// the variant.
    #[test]
    fn a_typed_refusal_that_wrote_nothing_keeps_its_name() {
        struct RefusingEffect;
        impl SealedHarvestEffect for RefusingEffect {
            fn binds_trunk_lock(&self) -> bool {
                true
            }
            fn harvest(
                &mut self,
                _molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
                // No state write on purpose: this is what an authority
                // boundary looks like from the outside.
                Err(EffectFailure::Refused(DoorRefusal::NotAuthorized))
            }
        }

        let w = world();
        let id = mol("task-20260904-typed");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(&w.store, &armed(), &id, &opts(), &mut RefusingEffect) {
            Err(LandError::Refused(r)) => assert_eq!(r.refusal, DoorRefusal::NotAuthorized),
            other => panic!("a refusal the effect named must survive the seam, got {other:?}"),
        }
    }

    /// The other half of the same contract: the typed name wins, and the
    /// *record* still supplies the detail an exit code cannot carry — but
    /// only when it names the same refusal, so a record from an earlier
    /// attempt cannot re-label this one.
    #[test]
    fn a_typed_refusal_takes_its_detail_from_a_record_that_agrees() {
        struct ConflictEffect<'a> {
            w: &'a World,
        }
        impl SealedHarvestEffect for ConflictEffect<'_> {
            fn binds_trunk_lock(&self) -> bool {
                true
            }
            fn harvest(
                &mut self,
                molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
                let mut m = self.w.store.load_molecule(molecule).expect("load");
                m.non_integration = Some(NonIntegration {
                    reason: NonIntegrationReason::Conflict,
                    at: chrono::Utc::now(),
                    base_branch: Some("main".to_owned()),
                    detail: Some("src/lib.rs".to_owned()),
                });
                self.w.store.save_molecule(molecule, &m).expect("save");
                Err(EffectFailure::Refused(DoorRefusal::MergeConflict))
            }
        }

        let w = world();
        let id = mol("task-20260904-tdet");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(
            &w.store,
            &armed(),
            &id,
            &opts(),
            &mut ConflictEffect { w: &w },
        ) {
            Err(LandError::Refused(r)) => {
                assert_eq!(r.refusal, DoorRefusal::MergeConflict);
                let detail = r.detail.expect("the agreeing record supplies the files");
                assert!(detail.contains("src/lib.rs"), "{detail}");
            }
            other => panic!("expected a detailed merge conflict, got {other:?}"),
        }
    }

    /// An unavailable effect is typed all the way, not recognised by
    /// comparing an error message.
    #[test]
    fn an_unavailable_effect_is_its_own_land_error() {
        struct NoEffect;
        impl SealedHarvestEffect for NoEffect {
            fn binds_trunk_lock(&self) -> bool {
                true
            }
            fn harvest(
                &mut self,
                _molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
                Err(EffectFailure::Unavailable)
            }
        }

        let w = world();
        let id = mol("task-20260904-noeffect");
        plant(&w, &id, MoleculeStatus::Completed, |_| {});

        match land(&w.store, &armed(), &id, &opts(), &mut NoEffect) {
            Err(LandError::EffectUnavailable) => {}
            other => panic!("an absent effect is not a failed one, got {other:?}"),
        }
    }

    /// Options carrying `if_completed`, the way a sweep sends them.
    fn if_completed_opts() -> HarvestOptions {
        let mut o = opts();
        o.if_completed = true;
        o
    }

    /// The PR #62 review's fifth finding: `if_completed` is a **no-op
    /// option**, not an ignored one.
    ///
    /// `cs done --if-completed` on running work exits zero and does
    /// nothing; the door answered `not_completed`, a refusal, for the one
    /// condition the caller had explicitly declared acceptable. Remove the
    /// `if_completed` branch from `decide` and this goes red.
    #[test]
    fn if_completed_on_running_work_is_a_successful_no_op() {
        let w = world();
        let id = mol("task-20260904-running");
        plant(&w, &id, MoleculeStatus::Running, |_| {});

        assert_eq!(
            decide(&w.store, &armed(), &id, &if_completed_opts()).expect("a no-op is a success"),
            DoorDecision::NoOp,
        );

        // Without the option the same molecule is still refused: this is
        // not an amnesty for `not_completed`, it is one request's answer.
        match decide(&w.store, &armed(), &id, &opts()) {
            Err(LandError::Refused(r)) => assert_eq!(r.refusal, DoorRefusal::NotCompleted),
            other => panic!("a bare harvest of running work must still refuse, got {other:?}"),
        }
    }

    /// And the effect is never reached for it — the door does not spawn a
    /// whole sealed transaction to discover what one store read already
    /// said.
    #[test]
    fn the_no_op_never_reaches_the_effect() {
        struct MustNotRun;
        impl SealedHarvestEffect for MustNotRun {
            fn binds_trunk_lock(&self) -> bool {
                true
            }
            fn harvest(
                &mut self,
                _molecule: &MoleculeId,
                _options: &HarvestOptions,
            ) -> Result<(), EffectFailure> {
                panic!("the effect must not run for a no-op");
            }
        }

        let w = world();
        let id = mol("task-20260904-nope");
        plant(&w, &id, MoleculeStatus::Running, |_| {});

        assert_eq!(
            land(
                &w.store,
                &armed(),
                &id,
                &if_completed_opts(),
                &mut MustNotRun
            )
            .expect("a no-op is a success"),
            DoorOutcome::NoOp,
        );
    }

    /// The post-effect half of the same option. `--force` carries the
    /// request past the pre-effect check, the sealed `cs done` then takes
    /// its own `--if-completed` no-op branch, and what comes back is an
    /// effect that succeeded having recorded nothing. Interpreted without
    /// the options that is `not_completed`; interpreted with them it is
    /// the no-op the caller asked for.
    #[test]
    fn a_forced_if_completed_no_op_is_still_a_no_op_after_the_effect() {
        let w = world();
        let id = mol("task-20260904-fnop");
        plant(&w, &id, MoleculeStatus::Running, |_| {});

        let mut options = if_completed_opts();
        options.force = true;

        assert_eq!(
            land(&w.store, &armed(), &id, &options, &mut InertEffect)
                .expect("a no-op is a success"),
            DoorOutcome::NoOp,
        );

        // The molecule is untouched: a no-op that closed something would
        // be a very expensive lie.
        let after = w.store.load_molecule(&id).expect("load");
        assert!(!after.archived);
        assert!(after.merged_at.is_none());
    }

    /// An already-harvested molecule answers `already_landed` whatever
    /// `if_completed` says — the sweep's other documented no-op, and the
    /// one the door already had. Pinned here so the new branch cannot
    /// swallow it: a molecule that landed must never come back as `no_op`,
    /// which would tell a caller nothing happened when a merge did.
    #[test]
    fn if_completed_does_not_swallow_already_landed() {
        let w = world();
        let id = mol("task-20260904-swept");
        plant(&w, &id, MoleculeStatus::Completed, |m| {
            m.merged_at = Some(chrono::Utc::now());
        });

        assert_eq!(
            decide(&w.store, &armed(), &id, &if_completed_opts()).expect("idempotent success"),
            DoorDecision::AlreadyLanded { merged: true },
        );
    }
}
