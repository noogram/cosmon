// SPDX-License-Identifier: AGPL-3.0-only

//! Agent definition types.
//!
//! An [`AgentDefinition`] is a portable, framework-agnostic specification of an
//! AI agent's identity, capabilities, and constraints. It describes WHO the agent
//! is and WHAT it can do, independent of WHERE or HOW it runs.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::agent::AgentRole;
//!
//! let role: AgentRole = "orchestration".parse().unwrap();
//! assert_eq!(role, AgentRole::Orchestration);
//! assert_eq!(role.to_string(), "orchestration");
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::capability::Capability;
use crate::clearance::Clearance;
use crate::id::AgentId;

/// Maximum nesting depth for agent spawning chains.
///
/// Prevents unbounded recursion when an orchestrator spawns agents that
/// themselves spawn further agents. Without this limit, a misconfigured
/// workflow could exhaust resources by creating arbitrarily deep spawn trees.
pub const MAX_AGENT_DEPTH: u32 = 5;

/// Validated nesting depth within a spawn tree.
///
/// Tracks how deep a worker sits in a chain of agent-spawns-agent.
/// Depth 0 is the root (top-level orchestrator). Each spawn increments
/// by one. Construction and increment are capped at [`MAX_AGENT_DEPTH`].
///
/// # Examples
///
/// ```
/// use cosmon_core::agent::AgentDepth;
///
/// let root = AgentDepth::root();
/// assert_eq!(root.value(), 0);
/// assert!(root.can_spawn());
///
/// let child = root.spawn_child().unwrap();
/// assert_eq!(child.value(), 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct AgentDepth(u32);

/// Error returned when an agent depth exceeds [`MAX_AGENT_DEPTH`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("agent depth {depth} exceeds maximum {max}", max = MAX_AGENT_DEPTH)]
pub struct DepthExceeded {
    /// The depth that was attempted.
    pub depth: u32,
}

impl AgentDepth {
    /// Create a root-level depth (0).
    #[must_use]
    pub fn root() -> Self {
        Self(0)
    }

    /// Create a depth from a raw value, validating against [`MAX_AGENT_DEPTH`].
    ///
    /// # Errors
    /// Returns [`DepthExceeded`] if `value > MAX_AGENT_DEPTH`.
    pub fn new(value: u32) -> Result<Self, DepthExceeded> {
        if value > MAX_AGENT_DEPTH {
            return Err(DepthExceeded { depth: value });
        }
        Ok(Self(value))
    }

    /// Return the raw depth value.
    #[must_use]
    pub fn value(self) -> u32 {
        self.0
    }

    /// Whether a worker at this depth is allowed to spawn a child.
    #[must_use]
    pub fn can_spawn(self) -> bool {
        self.0 < MAX_AGENT_DEPTH
    }

    /// Return the depth for a spawned child, one level deeper.
    ///
    /// # Errors
    /// Returns [`DepthExceeded`] if the current depth is already at
    /// [`MAX_AGENT_DEPTH`].
    pub fn spawn_child(self) -> Result<Self, DepthExceeded> {
        Self::new(self.0 + 1)
    }
}

impl fmt::Display for AgentDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<u32> for AgentDepth {
    type Error = DepthExceeded;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AgentDepth> for u32 {
    fn from(depth: AgentDepth) -> Self {
        depth.0
    }
}

/// Roles an agent can fulfil within a Cosmon rig.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Coordinates other agents and manages workflows.
    Orchestration,
    /// Investigates questions and gathers information.
    Research,
    /// Writes code and implements features.
    Implementation,
    /// Manages infrastructure and tooling.
    Infrastructure,
    /// Provides expert review and guidance.
    Advisory,
    /// Validates outputs, checks correctness, and enforces quality gates.
    Validation,
    /// Resident runtime driving a macro-molecule DAG.
    ///
    /// Created by `cs tackle` when the target molecule has outgoing
    /// `Blocks` links (i.e. it is the root of a DAG). The runtime itself
    /// is registered as a worker so `cs ensemble` and `cs patrol` can
    /// observe and tear it down uniformly.
    Runtime,
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Orchestration => f.write_str("orchestration"),
            Self::Research => f.write_str("research"),
            Self::Implementation => f.write_str("implementation"),
            Self::Infrastructure => f.write_str("infrastructure"),
            Self::Advisory => f.write_str("advisory"),
            Self::Validation => f.write_str("validation"),
            Self::Runtime => f.write_str("runtime"),
        }
    }
}

impl FromStr for AgentRole {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "orchestration" => Ok(Self::Orchestration),
            "research" => Ok(Self::Research),
            "implementation" => Ok(Self::Implementation),
            "infrastructure" => Ok(Self::Infrastructure),
            "advisory" => Ok(Self::Advisory),
            "validation" => Ok(Self::Validation),
            "runtime" => Ok(Self::Runtime),
            _ => Err(ParseEnumError {
                type_name: "AgentRole",
                value: s.to_owned(),
            }),
        }
    }
}

/// Error returned when parsing an enum variant from a string fails.
#[derive(Debug, Clone)]
pub struct ParseEnumError {
    /// The enum type that was being parsed.
    pub type_name: &'static str,
    /// The string value that failed to parse.
    pub value: String,
}

impl fmt::Display for ParseEnumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown {} variant: {:?}", self.type_name, self.value)
    }
}

impl std::error::Error for ParseEnumError {}

/// Portable specification of an AI agent's identity, capabilities, and constraints.
///
/// An `AgentDefinition` is pure cognition — it describes the agent independent of
/// any runtime (no process paths, no session IDs, no fleet references).
/// Multiple [`Worker`](crate::worker::Worker)s may instantiate the same definition.
///
/// JSON serialization is implemented manually to support schema evolution:
/// unknown fields are silently ignored on read, and new optional fields can be
/// added without breaking existing JSON files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    /// Unique name identifying this agent definition.
    pub name: AgentId,
    /// Role the agent fulfils within a rig.
    pub role: AgentRole,
    /// Permission level granted to workers running this definition.
    pub clearance: Clearance,
    /// Fine-grained capabilities granted to this agent (ADR-008 layer 2).
    ///
    /// Capabilities are more granular than clearance levels. An agent's
    /// effective permissions are: `clearance >= required` AND
    /// `required_capabilities ⊆ capabilities`.
    pub capabilities: BTreeSet<Capability>,
}

impl AgentDefinition {
    /// Create a new agent definition with no capabilities.
    #[must_use]
    pub fn new(name: AgentId, role: AgentRole, clearance: Clearance) -> Self {
        Self {
            name,
            role,
            clearance,
            capabilities: BTreeSet::new(),
        }
    }

    /// Create a new agent definition with the given capabilities.
    #[must_use]
    pub fn with_capabilities(
        name: AgentId,
        role: AgentRole,
        clearance: Clearance,
        capabilities: BTreeSet<Capability>,
    ) -> Self {
        Self {
            name,
            role,
            clearance,
            capabilities,
        }
    }
}

impl Serialize for AgentDefinition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let has_caps = !self.capabilities.is_empty();
        let field_count = if has_caps { 4 } else { 3 };
        let mut map = serializer.serialize_map(Some(field_count))?;
        map.serialize_entry("name", self.name.as_str())?;
        map.serialize_entry("role", &self.role)?;
        map.serialize_entry("clearance", &self.clearance)?;
        if has_caps {
            let caps: Vec<&Capability> = self.capabilities.iter().collect();
            map.serialize_entry("capabilities", &caps)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AgentDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        let obj = v
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("AgentDefinition must be a JSON object"))?;

        let name_str = obj
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("name"))?;
        let name = AgentId::new(name_str).map_err(serde::de::Error::custom)?;

        let role: AgentRole = obj
            .get("role")
            .ok_or_else(|| serde::de::Error::missing_field("role"))
            .and_then(|v| serde_json::from_value(v.clone()).map_err(serde::de::Error::custom))?;

        let clearance: Clearance = obj
            .get("clearance")
            .ok_or_else(|| serde::de::Error::missing_field("clearance"))
            .and_then(|v| serde_json::from_value(v.clone()).map_err(serde::de::Error::custom))?;

        let capabilities: BTreeSet<Capability> = obj
            .get("capabilities")
            .map(|v| serde_json::from_value(v.clone()).map_err(serde::de::Error::custom))
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            name,
            role,
            clearance,
            capabilities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_agent_depth_value() {
        assert_eq!(MAX_AGENT_DEPTH, 5);
        assert_ne!(MAX_AGENT_DEPTH, 0, "depth limit must be positive");
    }

    #[test]
    fn test_agent_depth_root() {
        let root = AgentDepth::root();
        assert_eq!(root.value(), 0);
        assert!(root.can_spawn());
    }

    #[test]
    fn test_agent_depth_new_valid() {
        for v in 0..=MAX_AGENT_DEPTH {
            let d = AgentDepth::new(v).unwrap();
            assert_eq!(d.value(), v);
        }
    }

    #[test]
    fn test_agent_depth_new_exceeds() {
        let err = AgentDepth::new(MAX_AGENT_DEPTH + 1).unwrap_err();
        assert_eq!(err.depth, MAX_AGENT_DEPTH + 1);
        assert!(err.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn test_agent_depth_spawn_chain() {
        let mut depth = AgentDepth::root();
        for expected in 1..=MAX_AGENT_DEPTH {
            depth = depth.spawn_child().unwrap();
            assert_eq!(depth.value(), expected);
        }
        assert!(!depth.can_spawn());
        assert!(depth.spawn_child().is_err());
    }

    #[test]
    fn test_agent_depth_display() {
        assert_eq!(AgentDepth::root().to_string(), "0");
        assert_eq!(AgentDepth::new(3).unwrap().to_string(), "3");
    }

    #[test]
    fn test_agent_depth_serde_roundtrip() {
        let depth = AgentDepth::new(3).unwrap();
        let json = serde_json::to_string(&depth).unwrap();
        assert_eq!(json, "3");
        let back: AgentDepth = serde_json::from_str(&json).unwrap();
        assert_eq!(depth, back);
    }

    #[test]
    fn test_agent_depth_serde_rejects_overflow() {
        let json = "99";
        let result: Result<AgentDepth, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_depth_ord() {
        let d0 = AgentDepth::root();
        let d3 = AgentDepth::new(3).unwrap();
        let d5 = AgentDepth::new(5).unwrap();
        assert!(d0 < d3);
        assert!(d3 < d5);
    }

    #[test]
    fn test_agent_role_exhaustive() {
        // No wildcard arm — compiler enforces exhaustiveness.
        // Each arm does something distinct to avoid match_same_arms lint.
        let roles = [
            AgentRole::Orchestration,
            AgentRole::Research,
            AgentRole::Implementation,
            AgentRole::Infrastructure,
            AgentRole::Advisory,
            AgentRole::Validation,
            AgentRole::Runtime,
        ];
        let mut seen = std::collections::HashSet::new();
        for role in roles {
            let label = match role {
                AgentRole::Orchestration => "orchestration",
                AgentRole::Research => "research",
                AgentRole::Implementation => "implementation",
                AgentRole::Infrastructure => "infrastructure",
                AgentRole::Advisory => "advisory",
                AgentRole::Validation => "validation",
                AgentRole::Runtime => "runtime",
            };
            assert!(seen.insert(label), "duplicate role: {label}");
        }
        assert_eq!(seen.len(), 7);
    }

    #[test]
    fn test_agent_role_display_roundtrip() {
        let roles = [
            AgentRole::Orchestration,
            AgentRole::Research,
            AgentRole::Implementation,
            AgentRole::Infrastructure,
            AgentRole::Advisory,
            AgentRole::Validation,
            AgentRole::Runtime,
        ];
        for role in roles {
            let s = role.to_string();
            let parsed: AgentRole = s.parse().unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn test_agent_definition_json_roundtrip() {
        let def = AgentDefinition::new(
            AgentId::new("witness").unwrap(),
            AgentRole::Orchestration,
            Clearance::Execute,
        );
        let json = serde_json::to_string_pretty(&def).unwrap();
        let back: AgentDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn test_agent_definition_ignores_unknown_fields() {
        let json = r#"{
            "name": "researcher",
            "role": "research",
            "clearance": "read",
            "future_field": "should be ignored"
        }"#;
        let def: AgentDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.name.as_str(), "researcher");
        assert_eq!(def.role, AgentRole::Research);
        assert_eq!(def.clearance, Clearance::Read);
    }
}
