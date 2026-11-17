// SPDX-License-Identifier: AGPL-3.0-only

//! Admission at `cs tackle` (issue #214, ADR-190 D2): a pure decision over
//! facts the caller gathered.
//!
//! This module performs no I/O. The molecule count and the free bytes arrive
//! as [`AdmissionFacts`]; where they come from (molecule store, transport
//! liveness, a free-space probe) is the adapter's concern. No load-average or
//! other machine-load signal is an input (ADR-190 D2).

use serde::{Deserialize, Serialize};

use crate::molecule::MoleculeStatus;

/// The optional `[admission]` section of `.cosmon/config.toml`.
///
/// ```toml
/// [admission]
/// max_active = 2
/// min_free_bytes = 10737418240   # 10 GiB
/// ```
///
/// Both bounds are optional. With neither set, `cs tackle` behaves exactly as
/// before: no new refusal and no new cost (no count, no probe).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmissionConfig {
    /// Maximum number of active molecules. `cs tackle` refuses to spawn when
    /// this many are already active.
    ///
    /// **Counting rule** ([`counts_as_active`]): a molecule is active when its
    /// status is `running` and its recorded worker session is alive.
    /// `completed` (harvested or not), `frozen`, `starved`, `collapsed`,
    /// `pending`, `queued` and awaiting-human molecules are not counted. The
    /// molecule being tackled is never counted against itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active: Option<u32>,

    /// Minimum free bytes on the volume that holds the worktrees (and so the
    /// build directories inside them). `cs tackle` refuses to spawn when the
    /// volume has fewer.
    ///
    /// TOML integers are signed 64-bit, so the largest value a config file can
    /// state is 9223372036854775807.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_free_bytes: Option<u64>,
}

impl AdmissionConfig {
    /// Whether any bound is set. When `false`, the caller must skip gathering
    /// facts altogether.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.max_active.is_some() || self.min_free_bytes.is_some()
    }
}

/// The counting rule for `max_active`, in one place.
///
/// A molecule counts as active when its status is [`MoleculeStatus::Running`]
/// and its recorded worker session is alive. Every other status, and a Running
/// molecule whose session is gone, does not count. Changing the rule touches
/// only this function.
#[must_use]
pub fn counts_as_active(status: MoleculeStatus, session_alive: bool) -> bool {
    status == MoleculeStatus::Running && session_alive
}

/// Facts the adapter gathered for one admission decision.
///
/// `None` means the fact could not be established. Unknown admits: a failed
/// probe is not a refusal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionFacts {
    /// Active molecules, by [`counts_as_active`].
    pub active: Option<u32>,
    /// Free bytes on the volume holding the worktrees.
    pub free_bytes: Option<u64>,
}

/// Outcome of [`admit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Nothing exceeded a bound.
    Admitted,
    /// The active-molecule cap is reached.
    RefusedMaxActive {
        /// Configured cap.
        max_active: u32,
        /// Observed active count.
        active: u32,
    },
    /// Free space is below the floor.
    RefusedMinFree {
        /// Configured floor in bytes.
        min_free_bytes: u64,
        /// Observed free bytes.
        free_bytes: u64,
    },
}

/// Decide whether a new spawn is admitted.
///
/// The cap is checked first; a bound whose fact is unknown never refuses.
#[must_use]
pub fn admit(config: &AdmissionConfig, facts: &AdmissionFacts) -> Admission {
    if let (Some(max_active), Some(active)) = (config.max_active, facts.active) {
        if active >= max_active {
            return Admission::RefusedMaxActive { max_active, active };
        }
    }
    if let (Some(min_free_bytes), Some(free_bytes)) = (config.min_free_bytes, facts.free_bytes) {
        if free_bytes < min_free_bytes {
            return Admission::RefusedMinFree {
                min_free_bytes,
                free_bytes,
            };
        }
    }
    Admission::Admitted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max_active: Option<u32>, min_free_bytes: Option<u64>) -> AdmissionConfig {
        AdmissionConfig {
            max_active,
            min_free_bytes,
        }
    }

    fn facts(active: Option<u32>, free_bytes: Option<u64>) -> AdmissionFacts {
        AdmissionFacts { active, free_bytes }
    }

    #[test]
    fn at_the_cap_the_spawn_is_refused_naming_the_observed_count() {
        assert_eq!(
            admit(&cfg(Some(2), None), &facts(Some(2), None)),
            Admission::RefusedMaxActive {
                max_active: 2,
                active: 2
            }
        );
    }

    #[test]
    fn below_the_cap_the_spawn_is_admitted() {
        assert_eq!(
            admit(&cfg(Some(2), None), &facts(Some(1), None)),
            Admission::Admitted
        );
    }

    #[test]
    fn free_space_below_the_floor_is_refused_naming_the_observed_bytes() {
        assert_eq!(
            admit(&cfg(None, Some(1000)), &facts(None, Some(999))),
            Admission::RefusedMinFree {
                min_free_bytes: 1000,
                free_bytes: 999
            }
        );
    }

    #[test]
    fn free_space_exactly_at_the_floor_is_admitted() {
        assert_eq!(
            admit(&cfg(None, Some(1000)), &facts(None, Some(1000))),
            Admission::Admitted
        );
    }

    #[test]
    fn unset_config_admits_whatever_the_facts() {
        assert_eq!(
            admit(&cfg(None, None), &facts(Some(u32::MAX), Some(0))),
            Admission::Admitted
        );
    }

    #[test]
    fn unknown_facts_admit() {
        assert_eq!(
            admit(&cfg(Some(1), Some(1000)), &facts(None, None)),
            Admission::Admitted
        );
    }

    #[test]
    fn the_cap_is_reported_before_the_floor_when_both_are_exceeded() {
        assert_eq!(
            admit(&cfg(Some(1), Some(1000)), &facts(Some(1), Some(0))),
            Admission::RefusedMaxActive {
                max_active: 1,
                active: 1
            }
        );
    }

    #[test]
    fn only_a_running_molecule_with_a_live_session_counts() {
        for status in MoleculeStatus::ALL.iter().copied() {
            for alive in [true, false] {
                let expected = status == MoleculeStatus::Running && alive;
                assert_eq!(
                    counts_as_active(status, alive),
                    expected,
                    "{status} {alive}"
                );
            }
        }
    }

    #[test]
    fn the_section_parses_and_rejects_unknown_keys() {
        let ok: AdmissionConfig = toml::from_str("max_active = 2\nmin_free_bytes = 5\n").unwrap();
        assert_eq!(ok, cfg(Some(2), Some(5)));
        assert!(toml::from_str::<AdmissionConfig>("max_activ = 2\n").is_err());
    }
}
