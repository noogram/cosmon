// SPDX-License-Identifier: AGPL-3.0-only

//! Pluggable invariants over the scheduler model.
//!
//! An invariant is a predicate that inspects the *incoming* event against the
//! *current* [`SchedulerState`] and either (a) returns `Ok(())` — the event
//! is legal under this invariant — or (b) returns a [`Violation`] describing
//! what broke.
//!
//! The validator runs every invariant *before* applying the event to the
//! state. This keeps each predicate stateless and composable: it always sees
//! the same "before" image that the TLA+ spec's `Next` action would.
//!
//! # Baseline vs. spec-derived
//!
//! Phase 1 (this crate) ships [`baseline_invariants`] — a conservative set
//! drawn from the known-good molecule lifecycle. They are sufficient to
//! reject every currently-documented class of trace bug
//! (`convoy-cascade`-style status skips, step monotonicity violations,
//! orphan merges) without depending on the TLA+ spec landing.
//!
//! A later phase will lower the TLA+ spec's `TypeOK` + `Next` action into
//! the same [`Invariant`] trait so the validator can swap the baseline set
//! for the spec-refined set without touching the CLI.

use cosmon_core::event_v2::{Envelope, EventV2};
use cosmon_core::molecule::MoleculeStatus;

use crate::error::Violation;
use crate::model::SchedulerState;

/// A single invariant over the scheduler model.
///
/// Implementors inspect the incoming envelope against the pre-transition
/// state and report a violation if the event is illegal. They must **not**
/// mutate `state` — mutation happens in the validator's `apply` pass after
/// every invariant has voted.
pub trait Invariant: Send + Sync {
    /// Machine-readable identifier. Shows up in `Violation::invariant` and
    /// in CI logs. Keep it short and `snake_case`.
    fn id(&self) -> &'static str;

    /// Human-readable summary for `cs verify-trace --list`.
    fn description(&self) -> &'static str;

    /// Evaluate this invariant. `Ok(())` = legal; `Err(Violation)` = illegal.
    ///
    /// `line` is the 1-indexed line number in the source trace so violations
    /// can be reported back to the operator with a concrete pointer.
    ///
    /// # Errors
    ///
    /// Returns a [`Violation`] describing why the invariant was broken. The
    /// replay loop short-circuits on the first `Err` so earlier invariants
    /// in the set get priority.
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation>;
}

/// Boxed invariant — the shape the validator stores.
pub type InvariantBox = Box<dyn Invariant>;

// ---------------------------------------------------------------------------
// Baseline invariant set
// ---------------------------------------------------------------------------

/// The conservative baseline invariants shipped with Phase 1.
///
/// Each entry corresponds to a TLA+ refinement predicate the spec will later
/// formalize. The set is ordered from cheapest to most expensive so early
/// failures short-circuit deeper checks.
#[must_use]
pub fn baseline_invariants() -> Vec<InvariantBox> {
    vec![
        Box::new(MoleculeExistsBeforeUse),
        Box::new(StatusTransitionLegal),
        Box::new(NoEventsAfterTerminal),
        Box::new(StepMonotone),
        Box::new(StepWithinTotal),
        Box::new(WorkerSpawnedBeforeKilled),
        Box::new(MergeCompletionPairsDispatch),
    ]
}

/// A molecule must be nucleated before any other event can reference it.
pub struct MoleculeExistsBeforeUse;

impl Invariant for MoleculeExistsBeforeUse {
    fn id(&self) -> &'static str {
        "molecule_exists_before_use"
    }
    fn description(&self) -> &'static str {
        "every event that names a molecule_id must be preceded by a molecule_nucleated for that id"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let referenced = match &envelope.event {
            EventV2::MoleculeStatusChanged { molecule_id, .. }
            | EventV2::MoleculeStepCompleted { molecule_id, .. }
            | EventV2::MoleculeCompleted { molecule_id, .. }
            | EventV2::MoleculeCollapsed { molecule_id, .. }
            | EventV2::MoleculeStuck { molecule_id, .. }
            | EventV2::Expired { molecule_id, .. }
            | EventV2::GateStarted { molecule_id, .. }
            | EventV2::GateCompleted { molecule_id, .. }
            | EventV2::GateFailed { molecule_id, .. }
            | EventV2::NativeStarted { molecule_id, .. }
            | EventV2::NativeCompleted { molecule_id, .. }
            | EventV2::NativeFailed { molecule_id, .. }
            | EventV2::Resurrected { molecule_id, .. } => Some(molecule_id.clone()),
            EventV2::MergeDispatched { molecule, .. }
            | EventV2::MergeCompleted { molecule, .. } => Some(molecule.clone()),
            EventV2::DecaySpliced { parent, .. } => Some(parent.clone()),
            _ => None,
        };
        if let Some(mid) = referenced {
            if !state.molecules.contains_key(&mid) {
                return Err(Violation::new(
                    self.id(),
                    Some(envelope.seq),
                    line,
                    format!("event references unknown molecule {mid}"),
                ));
            }
        }
        Ok(())
    }
}

/// `MoleculeStatusChanged` must reflect the molecule's current status in
/// `from`, and `to` must be reachable from `from` per the lifecycle machine.
pub struct StatusTransitionLegal;

impl Invariant for StatusTransitionLegal {
    fn id(&self) -> &'static str {
        "status_transition_legal"
    }
    fn description(&self) -> &'static str {
        "molecule_status_changed.from must equal the current status and to must be a legal successor"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let EventV2::MoleculeStatusChanged {
            molecule_id,
            from,
            to,
        } = &envelope.event
        else {
            return Ok(());
        };
        let Some(current) = state.status_of(molecule_id) else {
            // Handled by MoleculeExistsBeforeUse — stay silent here.
            return Ok(());
        };
        let parsed_from: MoleculeStatus = from.parse().map_err(|_| {
            Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!("unparseable 'from' status {from:?} on {molecule_id}"),
            )
        })?;
        let parsed_to: MoleculeStatus = to.parse().map_err(|_| {
            Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!("unparseable 'to' status {to:?} on {molecule_id}"),
            )
        })?;
        if parsed_from != current {
            return Err(Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!(
                    "{molecule_id} status_changed says from={parsed_from} but recorded state is {current}"
                ),
            ));
        }
        // The baseline mirrors `MoleculeStatus::can_transition_to` but tolerates
        // `Pending → Queued`, `Pending → Running`, and `Queued → Running` —
        // those are legal in the real pipeline even though the typestate
        // machine collapses them. Note: `Pending → Running` moved out of
        // `can_transition_to` when the typestate lift (Chantier 2,
        // task-20260419-a64f) made it a compile-time `Molecule<Pending>::
        // tackle` transition; event-log verification still needs to
        // accept it because `cs tackle` writes the status change.
        let allowed = parsed_from.can_transition_to(parsed_to)
            || matches!(
                (parsed_from, parsed_to),
                (
                    MoleculeStatus::Pending,
                    MoleculeStatus::Queued | MoleculeStatus::Pending | MoleculeStatus::Running
                ) | (
                    MoleculeStatus::Queued,
                    MoleculeStatus::Running | MoleculeStatus::Pending
                )
            );
        if !allowed {
            return Err(Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!("{molecule_id} illegal transition {parsed_from} → {parsed_to}"),
            ));
        }
        Ok(())
    }
}

/// After a molecule reaches a terminal status, no further molecule-scoped
/// events (beyond merge completion) may appear for it.
pub struct NoEventsAfterTerminal;

impl Invariant for NoEventsAfterTerminal {
    fn id(&self) -> &'static str {
        "no_events_after_terminal"
    }
    fn description(&self) -> &'static str {
        "after molecule_completed or molecule_collapsed, no further lifecycle events may reference the molecule"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let (mid, event_kind) = match &envelope.event {
            EventV2::MoleculeStepCompleted { molecule_id, .. } => {
                (molecule_id, "molecule_step_completed")
            }
            EventV2::MoleculeCompleted { molecule_id, .. } => (molecule_id, "molecule_completed"),
            EventV2::MoleculeCollapsed { molecule_id, .. } => (molecule_id, "molecule_collapsed"),
            EventV2::MoleculeStatusChanged { molecule_id, .. } => {
                (molecule_id, "molecule_status_changed")
            }
            _ => return Ok(()),
        };
        if let Some(status) = state.status_of(mid) {
            if status.is_terminal() {
                return Err(Violation::new(
                    self.id(),
                    Some(envelope.seq),
                    line,
                    format!("{mid} is already {status}, rejecting {event_kind}"),
                ));
            }
        }
        Ok(())
    }
}

/// `MoleculeStepCompleted.step` must be non-decreasing per molecule.
pub struct StepMonotone;

impl Invariant for StepMonotone {
    fn id(&self) -> &'static str {
        "step_monotone"
    }
    fn description(&self) -> &'static str {
        "consecutive molecule_step_completed events must have non-decreasing step indices"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let EventV2::MoleculeStepCompleted {
            molecule_id, step, ..
        } = &envelope.event
        else {
            return Ok(());
        };
        let Some(mol) = state.molecules.get(molecule_id) else {
            return Ok(());
        };
        if let Some(prev) = mol.last_step {
            if *step < prev {
                return Err(Violation::new(
                    self.id(),
                    Some(envelope.seq),
                    line,
                    format!("{molecule_id} step {step} is below last observed step {prev}"),
                ));
            }
        }
        Ok(())
    }
}

/// Within a molecule, `step < total` must hold and `total` must be stable.
pub struct StepWithinTotal;

impl Invariant for StepWithinTotal {
    fn id(&self) -> &'static str {
        "step_within_total"
    }
    fn description(&self) -> &'static str {
        "molecule_step_completed.step must be < total and total must not change across events"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let EventV2::MoleculeStepCompleted {
            molecule_id,
            step,
            total,
            ..
        } = &envelope.event
        else {
            return Ok(());
        };
        if *step >= *total {
            return Err(Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!("{molecule_id} step {step} ≥ total {total}"),
            ));
        }
        if let Some(mol) = state.molecules.get(molecule_id) {
            if let Some(prev_total) = mol.total_steps {
                if prev_total != *total {
                    return Err(Violation::new(
                        self.id(),
                        Some(envelope.seq),
                        line,
                        format!("{molecule_id} total changed from {prev_total} to {total}"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A worker can only be killed if it was spawned first. Legacy traces that
/// pre-date `WorkerSpawned` emission are tolerated when the worker has never
/// been seen at all (the invariant only fires once the replay has observed
/// at least one worker event).
pub struct WorkerSpawnedBeforeKilled;

impl Invariant for WorkerSpawnedBeforeKilled {
    fn id(&self) -> &'static str {
        "worker_spawned_before_killed"
    }
    fn description(&self) -> &'static str {
        "worker_killed must follow a prior worker_spawned for the same worker_id"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let EventV2::WorkerKilled { worker_id, .. } = &envelope.event else {
            return Ok(());
        };
        if !state.workers.contains_key(worker_id) {
            return Err(Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!("worker_killed for unknown worker {worker_id}"),
            ));
        }
        Ok(())
    }
}

/// `MergeCompleted` must follow a matching `MergeDispatched` for the same
/// molecule. This catches orphan merges and missing dispatch events.
pub struct MergeCompletionPairsDispatch;

impl Invariant for MergeCompletionPairsDispatch {
    fn id(&self) -> &'static str {
        "merge_completion_pairs_dispatch"
    }
    fn description(&self) -> &'static str {
        "merge_completed must follow a prior merge_dispatched with the same molecule and branch"
    }
    fn check(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        let EventV2::MergeCompleted {
            molecule, branch, ..
        } = &envelope.event
        else {
            return Ok(());
        };
        match state.pending_merges.get(molecule) {
            Some(pending_branch) if pending_branch == branch => Ok(()),
            Some(pending_branch) => Err(Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!(
                    "merge_completed on {molecule} branch {branch:?} does not match pending dispatch {pending_branch:?}"
                ),
            )),
            None => Err(Violation::new(
                self.id(),
                Some(envelope.seq),
                line,
                format!("merge_completed on {molecule} with no prior merge_dispatched"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::Seq;
    use cosmon_core::id::MoleculeId;

    fn env(event: EventV2) -> Envelope {
        Envelope::new(Seq(0), None, event)
    }

    #[test]
    fn molecule_exists_rejects_dangling_reference() {
        let inv = MoleculeExistsBeforeUse;
        let state = SchedulerState::new();
        let evt = env(EventV2::MoleculeCompleted {
            molecule_id: MoleculeId::new("cs-20260414-aaaa").unwrap(),
            duration_ms: None,
            reason: "ok".into(),
        });
        assert!(inv.check(&state, &evt, 1).is_err());
    }

    #[test]
    fn baseline_set_is_non_empty() {
        assert!(!baseline_invariants().is_empty());
    }
}
