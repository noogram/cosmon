// SPDX-License-Identifier: AGPL-3.0-only

//! Preemption policy — pure domain logic for Slurm-style worker preemption.
//!
//! Determines whether a higher-clearance worker should preempt a
//! lower-clearance incumbent. This is the "priority tier" mechanism
//! from Slurm, mapped to Cosmon's [`Clearance`] ordering:
//! `Read < Write < Execute`.
//!
//! No I/O — this module contains only pure policy functions.

use crate::clearance::Clearance;
use crate::worker::WorkerStatus;

/// Determine whether an incumbent worker should be preempted by a challenger.
///
/// Preemption occurs when:
/// 1. The challenger has **strictly higher** clearance than the incumbent.
/// 2. The incumbent is currently `Active` (only active workers can be preempted).
///
/// # Examples
///
/// ```
/// use cosmon_core::preempt::should_preempt;
/// use cosmon_core::clearance::Clearance;
/// use cosmon_core::worker::WorkerStatus;
///
/// // Execute-level challenger preempts Read-level incumbent.
/// assert!(should_preempt(Clearance::Read, &WorkerStatus::Active, Clearance::Execute));
///
/// // Same clearance: no preemption.
/// assert!(!should_preempt(Clearance::Write, &WorkerStatus::Active, Clearance::Write));
///
/// // Incumbent is Stopped: no preemption (nothing to preempt).
/// assert!(!should_preempt(Clearance::Read, &WorkerStatus::Stopped, Clearance::Execute));
/// ```
#[must_use]
pub fn should_preempt(
    incumbent_clearance: Clearance,
    incumbent_status: &WorkerStatus,
    challenger_clearance: Clearance,
) -> bool {
    challenger_clearance > incumbent_clearance && *incumbent_status == WorkerStatus::Active
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_higher_clearance_preempts_active() {
        assert!(should_preempt(
            Clearance::Read,
            &WorkerStatus::Active,
            Clearance::Write
        ));
        assert!(should_preempt(
            Clearance::Read,
            &WorkerStatus::Active,
            Clearance::Execute
        ));
        assert!(should_preempt(
            Clearance::Write,
            &WorkerStatus::Active,
            Clearance::Execute
        ));
    }

    #[test]
    fn test_same_clearance_no_preemption() {
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Active,
            Clearance::Read
        ));
        assert!(!should_preempt(
            Clearance::Write,
            &WorkerStatus::Active,
            Clearance::Write
        ));
        assert!(!should_preempt(
            Clearance::Execute,
            &WorkerStatus::Active,
            Clearance::Execute
        ));
    }

    #[test]
    fn test_lower_clearance_no_preemption() {
        assert!(!should_preempt(
            Clearance::Execute,
            &WorkerStatus::Active,
            Clearance::Read
        ));
        assert!(!should_preempt(
            Clearance::Write,
            &WorkerStatus::Active,
            Clearance::Read
        ));
    }

    #[test]
    fn test_inactive_worker_not_preemptable() {
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Stopped,
            Clearance::Execute
        ));
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Paused,
            Clearance::Execute
        ));
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Unresponsive,
            Clearance::Execute
        ));
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Stale,
            Clearance::Execute
        ));
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Stopping,
            Clearance::Execute
        ));
        assert!(!should_preempt(
            Clearance::Read,
            &WorkerStatus::Starting,
            Clearance::Execute
        ));
    }
}
