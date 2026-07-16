// SPDX-License-Identifier: AGPL-3.0-only

//! Attention Conservation Law — bounds on alive molecule count.
//!
//! Every alive molecule claims a slot in the system's attention budget.
//! When the budget is reached, new nucleation requires completing or
//! collapsing existing work first. This prevents runaway proliferation
//! that the energy budget catches too late.
//!
//! See THESIS.md Part XVII.

use crate::molecule::MoleculeStatus;

/// Result of checking the attention budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionCheck {
    /// Budget not set — no limit.
    Unlimited,
    /// Within budget — proceed.
    WithinBudget {
        /// Current alive count.
        alive: usize,
        /// Maximum allowed.
        budget: usize,
    },
    /// Budget exceeded — warn or reject.
    Exceeded {
        /// Current alive count.
        alive: usize,
        /// Maximum allowed.
        budget: usize,
        /// How many over budget.
        overflow: usize,
    },
}

impl AttentionCheck {
    /// Returns `true` if nucleation should be allowed.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !matches!(self, Self::Exceeded { .. })
    }

    /// Human-readable warning message, if any.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Exceeded {
                alive,
                budget,
                overflow,
            } => Some(format!(
                "Attention budget exceeded: {alive} alive molecules (budget: {budget}, over by {overflow}). \
                 Complete or collapse existing molecules before nucleating new ones."
            )),
            _ => None,
        }
    }
}

/// Check the attention budget given a list of molecule statuses.
///
/// Pure function — no I/O.
#[must_use]
pub fn check_attention_budget(
    budget: Option<usize>,
    statuses: &[MoleculeStatus],
) -> AttentionCheck {
    let Some(budget) = budget else {
        return AttentionCheck::Unlimited;
    };

    let alive = statuses.iter().filter(|s| s.is_alive()).count();

    if alive >= budget {
        AttentionCheck::Exceeded {
            alive,
            budget,
            overflow: alive - budget,
        }
    } else {
        AttentionCheck::WithinBudget { alive, budget }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unlimited_always_ok() {
        let check = check_attention_budget(None, &[MoleculeStatus::Running; 100]);
        assert_eq!(check, AttentionCheck::Unlimited);
        assert!(check.is_ok());
    }

    #[test]
    fn test_within_budget() {
        let statuses = vec![
            MoleculeStatus::Running,
            MoleculeStatus::Pending,
            MoleculeStatus::Completed, // terminal — doesn't count
        ];
        let check = check_attention_budget(Some(5), &statuses);
        assert!(check.is_ok());
        assert!(matches!(
            check,
            AttentionCheck::WithinBudget {
                alive: 2,
                budget: 5
            }
        ));
    }

    #[test]
    fn test_exceeded() {
        let statuses = vec![
            MoleculeStatus::Running,
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
        ];
        let check = check_attention_budget(Some(2), &statuses);
        assert!(!check.is_ok());
        assert!(check.warning().unwrap().contains("exceeded"));
    }

    #[test]
    fn test_frozen_counts_as_alive() {
        let statuses = vec![MoleculeStatus::Frozen, MoleculeStatus::Frozen];
        let check = check_attention_budget(Some(2), &statuses);
        // 2 frozen = 2 alive = exactly at budget
        assert!(!check.is_ok()); // >= budget means exceeded
    }
}
