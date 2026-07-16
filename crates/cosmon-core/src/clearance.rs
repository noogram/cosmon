// SPDX-License-Identifier: AGPL-3.0-only

//! Clearance levels for agent permissions.
//!
//! Ordering is explicit via [`Clearance::rank`] — not derived from variant
//! declaration order. This prevents silent preemption logic changes if
//! variants are reordered or new ones are added.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;

/// Permission level. Ordered: `Read < Write < Execute`.
///
/// `PartialOrd` and `Ord` are implemented manually via [`Clearance::rank`]
/// to prevent variant reordering from silently changing preemption behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Clearance {
    /// Read-only access to state.
    Read,
    /// Read and write access to state.
    Write,
    /// Full access including agent lifecycle operations.
    Execute,
}

impl Clearance {
    /// Explicit numeric rank for ordering.
    ///
    /// Adding a new variant forces a conscious decision about its rank.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Read => 0,
            Self::Write => 1,
            Self::Execute => 2,
        }
    }
}

#[allow(clippy::derive_ord_xor_partial_ord)]
impl PartialOrd for Clearance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Intentionally manual — prevents variant reordering from silently breaking
// preemption logic. See code review finding from Tolnay.
#[allow(clippy::derivable_impls)]
impl Ord for Clearance {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl fmt::Display for Clearance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
            Self::Execute => f.write_str("execute"),
        }
    }
}

impl FromStr for Clearance {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "execute" => Ok(Self::Execute),
            _ => Err(ParseEnumError {
                type_name: "Clearance",
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clearance_ordering() {
        assert!(Clearance::Read < Clearance::Write);
        assert!(Clearance::Write < Clearance::Execute);
        assert!(Clearance::Read < Clearance::Execute);

        // Simulate: worker.clearance >= required_clearance
        let worker_clearance = Clearance::Write;
        let required = Clearance::Read;
        assert!(worker_clearance >= required);

        let required = Clearance::Execute;
        assert!(worker_clearance < required);
    }

    #[test]
    fn test_clearance_rank_is_explicit() {
        assert_eq!(Clearance::Read.rank(), 0);
        assert_eq!(Clearance::Write.rank(), 1);
        assert_eq!(Clearance::Execute.rank(), 2);
    }

    #[test]
    fn test_clearance_display_roundtrip() {
        for c in [Clearance::Read, Clearance::Write, Clearance::Execute] {
            let s = c.to_string();
            let parsed: Clearance = s.parse().unwrap();
            assert_eq!(parsed, c);
        }
    }
}
