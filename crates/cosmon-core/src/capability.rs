// SPDX-License-Identifier: AGPL-3.0-only

//! Named capabilities for fine-grained agent authorization.
//!
//! [`Capability`] extends the coarse [`Clearance`](crate::clearance::Clearance) levels
//! with specific, named grants. An agent's effective permissions are determined by
//! both its clearance level AND its granted capabilities.
//!
//! This is layer 2 of the three-layer feature gating design (ADR-008):
//! - Layer 1: Cargo `cfg(feature)` — compile-time (not represented in types)
//! - **Layer 2: Clearance + Capability — runtime, per-agent**
//! - Layer 3: [`FeatureFlags`](crate::feature_flags::FeatureFlags) — dynamic config
//!
//! # Examples
//!
//! ```
//! use cosmon_core::capability::Capability;
//!
//! let cap: Capability = "spawn_subagent".parse().unwrap();
//! assert_eq!(cap, Capability::SpawnSubagent);
//! assert_eq!(cap.to_string(), "spawn_subagent");
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;

/// A named capability that an agent may or may not possess.
///
/// Capabilities are more granular than [`Clearance`](crate::clearance::Clearance) levels.
/// An agent's effective permissions are: clearance level AND granted capabilities.
///
/// # Ordering
///
/// Capabilities are ordered alphabetically by convention (for deterministic
/// serialization in `BTreeSet`), not by privilege level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Access MCP (Model Context Protocol) server/client features.
    AccessMcp,
    /// Manage fleet state: start, stop, reassign workers.
    ManageFleet,
    /// Create or modify formula definitions.
    ModifyFormula,
    /// Run patrol loops to monitor fleet health.
    Patrol,
    /// Spawn sub-agents during execution.
    SpawnSubagent,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessMcp => f.write_str("access_mcp"),
            Self::ManageFleet => f.write_str("manage_fleet"),
            Self::ModifyFormula => f.write_str("modify_formula"),
            Self::Patrol => f.write_str("patrol"),
            Self::SpawnSubagent => f.write_str("spawn_subagent"),
        }
    }
}

impl FromStr for Capability {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "access_mcp" => Ok(Self::AccessMcp),
            "manage_fleet" => Ok(Self::ManageFleet),
            "modify_formula" => Ok(Self::ModifyFormula),
            "patrol" => Ok(Self::Patrol),
            "spawn_subagent" => Ok(Self::SpawnSubagent),
            _ => Err(ParseEnumError {
                type_name: "Capability",
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn test_capability_display_roundtrip() {
        let caps = [
            Capability::AccessMcp,
            Capability::ManageFleet,
            Capability::ModifyFormula,
            Capability::Patrol,
            Capability::SpawnSubagent,
        ];
        for cap in caps {
            let s = cap.to_string();
            let parsed: Capability = s.parse().unwrap();
            assert_eq!(parsed, cap);
        }
    }

    #[test]
    fn test_capability_ordering_is_deterministic() {
        let mut set = BTreeSet::new();
        set.insert(Capability::SpawnSubagent);
        set.insert(Capability::AccessMcp);
        set.insert(Capability::Patrol);

        let ordered: Vec<_> = set.iter().collect();
        assert_eq!(
            ordered,
            vec![
                &Capability::AccessMcp,
                &Capability::Patrol,
                &Capability::SpawnSubagent,
            ]
        );
    }

    #[test]
    fn test_capability_parse_unknown_fails() {
        let result = "unknown_cap".parse::<Capability>();
        assert!(result.is_err());
    }

    #[test]
    fn test_capability_serde_roundtrip() {
        let cap = Capability::SpawnSubagent;
        let json = serde_json::to_string(&cap).unwrap();
        assert_eq!(json, "\"spawn_subagent\"");
        let back: Capability = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cap);
    }
}
