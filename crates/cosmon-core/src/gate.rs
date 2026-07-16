// SPDX-License-Identifier: AGPL-3.0-only

//! Feature gate checking — the unified entry point for authorization decisions.
//!
//! Combines the three layers of feature gating (ADR-008):
//! - Layer 1: Cargo features (compile-time, not checked here)
//! - Layer 2: [`Clearance`] + [`Capability`] (per-agent)
//! - Layer 3: [`FeatureFlags`] (per-deployment)
//!
//! # Examples
//!
//! ```
//! use cosmon_core::agent::AgentDefinition;
//! use cosmon_core::capability::Capability;
//! use cosmon_core::clearance::Clearance;
//! use cosmon_core::feature_flags::FeatureFlags;
//! use cosmon_core::gate::{check_gate, GateRequirement};
//! use cosmon_core::id::AgentId;
//! use std::collections::BTreeSet;
//!
//! let mut caps = BTreeSet::new();
//! caps.insert(Capability::SpawnSubagent);
//!
//! let agent = AgentDefinition {
//!     name: AgentId::new("witness").unwrap(),
//!     role: "orchestration".parse().unwrap(),
//!     clearance: Clearance::Execute,
//!     capabilities: caps,
//! };
//!
//! let flags = FeatureFlags::from_iter([
//!     ("dispatch_convoy_routing", true),
//! ]);
//!
//! let req = GateRequirement {
//!     clearance: Clearance::Write,
//!     capabilities: vec![Capability::SpawnSubagent],
//!     flag: Some("dispatch_convoy_routing"),
//! };
//!
//! assert!(check_gate(&agent, &req, &flags).is_ok());
//! ```

use std::collections::BTreeSet;
use std::fmt;

use crate::agent::AgentDefinition;
use crate::capability::Capability;
use crate::clearance::Clearance;
use crate::feature_flags::FeatureFlags;

/// What is required to access a gated feature.
#[derive(Debug, Clone)]
pub struct GateRequirement<'a> {
    /// Minimum clearance level required.
    pub clearance: Clearance,
    /// Capabilities the agent must possess.
    pub capabilities: Vec<Capability>,
    /// Feature flag that must be enabled (if any).
    pub flag: Option<&'a str>,
}

/// Reason a gate check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDenied {
    /// Agent's clearance is insufficient.
    InsufficientClearance {
        /// The agent's actual clearance level.
        actual: Clearance,
        /// The required clearance level.
        required: Clearance,
    },
    /// Agent is missing one or more required capabilities.
    MissingCapabilities {
        /// Capabilities the agent lacks.
        missing: Vec<Capability>,
    },
    /// A required feature flag is not enabled.
    FlagDisabled {
        /// The flag that is not enabled.
        flag: String,
    },
}

impl fmt::Display for GateDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientClearance { actual, required } => {
                write!(
                    f,
                    "insufficient clearance: agent has {actual}, requires {required}"
                )
            }
            Self::MissingCapabilities { missing } => {
                let names: Vec<_> = missing.iter().map(ToString::to_string).collect();
                write!(f, "missing capabilities: {}", names.join(", "))
            }
            Self::FlagDisabled { flag } => {
                write!(f, "feature flag disabled: {flag}")
            }
        }
    }
}

impl std::error::Error for GateDenied {}

/// Check whether a feature is available given all gating layers.
///
/// Layer 1 (compile-time Cargo features) is enforced by the compiler and not
/// checked here. This function checks layers 2 and 3:
///
/// - **Layer 2 (clearance):** `agent.clearance >= requirement.clearance`
/// - **Layer 2 (capabilities):** `requirement.capabilities ⊆ agent.capabilities`
/// - **Layer 3 (feature flag):** if a flag is specified, it must be enabled
///
/// Returns `Ok(())` if all layers pass, or the first `GateDenied` encountered.
///
/// # Errors
///
/// Returns [`GateDenied`] if any gate check fails.
pub fn check_gate(
    agent: &AgentDefinition,
    requirement: &GateRequirement<'_>,
    flags: &FeatureFlags,
) -> Result<(), GateDenied> {
    // Layer 2a: Clearance check
    if agent.clearance < requirement.clearance {
        return Err(GateDenied::InsufficientClearance {
            actual: agent.clearance,
            required: requirement.clearance,
        });
    }

    // Layer 2b: Capability check
    let agent_caps: &BTreeSet<Capability> = &agent.capabilities;
    let missing: Vec<Capability> = requirement
        .capabilities
        .iter()
        .filter(|c| !agent_caps.contains(c))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(GateDenied::MissingCapabilities { missing });
    }

    // Layer 3: Feature flag check
    if let Some(flag) = requirement.flag {
        if !flags.is_enabled(flag) {
            return Err(GateDenied::FlagDisabled {
                flag: flag.to_owned(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentRole;
    use crate::id::AgentId;

    fn make_agent(clearance: Clearance, caps: &[Capability]) -> AgentDefinition {
        AgentDefinition {
            name: AgentId::new("test-agent").unwrap(),
            role: AgentRole::Implementation,
            clearance,
            capabilities: caps.iter().copied().collect(),
        }
    }

    #[test]
    fn test_all_gates_pass() {
        let agent = make_agent(
            Clearance::Execute,
            &[Capability::SpawnSubagent, Capability::Patrol],
        );
        let flags = FeatureFlags::from_iter([("dispatch_convoy_routing", true)]);
        let req = GateRequirement {
            clearance: Clearance::Write,
            capabilities: vec![Capability::SpawnSubagent],
            flag: Some("dispatch_convoy_routing"),
        };
        assert!(check_gate(&agent, &req, &flags).is_ok());
    }

    #[test]
    fn test_insufficient_clearance() {
        let agent = make_agent(Clearance::Read, &[]);
        let flags = FeatureFlags::new();
        let req = GateRequirement {
            clearance: Clearance::Write,
            capabilities: vec![],
            flag: None,
        };
        let err = check_gate(&agent, &req, &flags).unwrap_err();
        assert_eq!(
            err,
            GateDenied::InsufficientClearance {
                actual: Clearance::Read,
                required: Clearance::Write,
            }
        );
    }

    #[test]
    fn test_missing_capabilities() {
        let agent = make_agent(Clearance::Execute, &[Capability::Patrol]);
        let flags = FeatureFlags::new();
        let req = GateRequirement {
            clearance: Clearance::Read,
            capabilities: vec![Capability::SpawnSubagent, Capability::ManageFleet],
            flag: None,
        };
        let err = check_gate(&agent, &req, &flags).unwrap_err();
        match err {
            GateDenied::MissingCapabilities { missing } => {
                assert!(missing.contains(&Capability::SpawnSubagent));
                assert!(missing.contains(&Capability::ManageFleet));
                assert_eq!(missing.len(), 2);
            }
            other => panic!("expected MissingCapabilities, got {other}"),
        }
    }

    #[test]
    fn test_flag_disabled() {
        let agent = make_agent(Clearance::Execute, &[]);
        let flags = FeatureFlags::new();
        let req = GateRequirement {
            clearance: Clearance::Read,
            capabilities: vec![],
            flag: Some("experimental_feature"),
        };
        let err = check_gate(&agent, &req, &flags).unwrap_err();
        assert_eq!(
            err,
            GateDenied::FlagDisabled {
                flag: "experimental_feature".to_owned(),
            }
        );
    }

    #[test]
    fn test_no_requirements_always_passes() {
        let agent = make_agent(Clearance::Read, &[]);
        let flags = FeatureFlags::new();
        let req = GateRequirement {
            clearance: Clearance::Read,
            capabilities: vec![],
            flag: None,
        };
        assert!(check_gate(&agent, &req, &flags).is_ok());
    }

    #[test]
    fn test_clearance_checked_before_capabilities() {
        let agent = make_agent(Clearance::Read, &[]);
        let flags = FeatureFlags::new();
        let req = GateRequirement {
            clearance: Clearance::Execute,
            capabilities: vec![Capability::SpawnSubagent],
            flag: Some("disabled_flag"),
        };
        // Should fail on clearance first, not capabilities or flags
        let err = check_gate(&agent, &req, &flags).unwrap_err();
        assert!(matches!(err, GateDenied::InsufficientClearance { .. }));
    }

    #[test]
    fn test_gate_denied_display() {
        let err = GateDenied::InsufficientClearance {
            actual: Clearance::Read,
            required: Clearance::Execute,
        };
        assert_eq!(
            err.to_string(),
            "insufficient clearance: agent has read, requires execute"
        );

        let err = GateDenied::MissingCapabilities {
            missing: vec![Capability::SpawnSubagent],
        };
        assert_eq!(err.to_string(), "missing capabilities: spawn_subagent");

        let err = GateDenied::FlagDisabled {
            flag: "test_flag".to_owned(),
        };
        assert_eq!(err.to_string(), "feature flag disabled: test_flag");
    }
}
