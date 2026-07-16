// SPDX-License-Identifier: AGPL-3.0-only

//! Scheduler state model — the projection of an event stream onto the
//! variables an invariant needs to reason over.
//!
//! This is intentionally coarser than the full `cosmon-core` state store.
//! The validator does not need to rebuild molecule directories or worker
//! processes — it only needs the minimum state an invariant might inspect:
//!
//! - which molecules exist and their last-seen status,
//! - which workers are alive,
//! - how far each molecule has progressed through its formula,
//! - outstanding merge dispatches.
//!
//! The projection is deliberately additive: invariants that later need
//! more state can extend this struct without breaking existing ones.

use std::collections::HashMap;

use cosmon_core::event_v2::MergeResult;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;

/// Per-molecule state the validator carries across the replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoleculeTraceState {
    /// Current lifecycle status. Starts at [`MoleculeStatus::Pending`] on
    /// nucleation.
    pub status: MoleculeStatus,

    /// Highest `step` index seen in a `MoleculeStepCompleted` event for this
    /// molecule. `None` until the first step is observed.
    pub last_step: Option<usize>,

    /// Total step count recorded by the first `MoleculeStepCompleted` event.
    /// Later events must agree with this value (the formula cannot grow or
    /// shrink mid-run).
    pub total_steps: Option<usize>,

    /// Formula id recorded at nucleation, kept for reporting.
    pub formula_id: String,
}

/// Per-worker state the validator carries across the replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTraceState {
    /// Whether the worker is still alive (spawned but not killed).
    pub alive: bool,
}

/// The dynamic state of the scheduler as reconstructed from the event log.
///
/// This is the variable bundle that every [`crate::invariants::Invariant`]
/// reads and mutates as each event is processed. Keeping it public lets
/// downstream crates (including a future TLA+-refined spec) define their
/// own invariants over the same projection.
#[derive(Debug, Default, Clone)]
pub struct SchedulerState {
    /// Molecules that have been nucleated, keyed by their id.
    pub molecules: HashMap<MoleculeId, MoleculeTraceState>,

    /// Workers that have been spawned, keyed by their id.
    pub workers: HashMap<WorkerId, WorkerTraceState>,

    /// Merges that have been dispatched but not yet completed.
    /// The value is the branch name that was dispatched.
    pub pending_merges: HashMap<MoleculeId, String>,

    /// Merges that have completed — kept for reporting and as a cheap sanity
    /// check (a second `MergeCompleted` for the same molecule is tolerated
    /// but observable by more advanced invariants).
    pub completed_merges: HashMap<MoleculeId, MergeResult>,

    /// Monotone counter of events processed so far, for diagnostics.
    pub events_seen: u64,
}

impl SchedulerState {
    /// Build an empty state — no molecules, no workers, nothing pending.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a molecule's last-seen status, if it has been nucleated.
    #[must_use]
    pub fn status_of(&self, molecule_id: &MoleculeId) -> Option<MoleculeStatus> {
        self.molecules.get(molecule_id).map(|m| m.status)
    }

    /// Number of distinct molecules seen so far.
    #[must_use]
    pub fn molecule_count(&self) -> usize {
        self.molecules.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_has_no_molecules() {
        let s = SchedulerState::new();
        assert_eq!(s.molecule_count(), 0);
        let id = MoleculeId::new("cs-20260414-aaaa").unwrap();
        assert!(s.status_of(&id).is_none());
    }
}
