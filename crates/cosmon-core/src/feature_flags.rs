// SPDX-License-Identifier: AGPL-3.0-only

//! Dynamic feature flags for deployment-level configuration.
//!
//! [`FeatureFlags`] is layer 3 of the three-layer feature gating design (ADR-008).
//! Flags are loaded from TOML config at startup and are shared across all agents
//! in a deployment. They control experimental features, gradual rollouts, and
//! operational toggles.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::feature_flags::FeatureFlags;
//!
//! let flags = FeatureFlags::new();
//! assert!(!flags.is_enabled("experimental_feature"));
//!
//! let flags = FeatureFlags::from_iter([
//!     ("dispatch_convoy_routing", true),
//!     ("mcp_bidirectional", false),
//! ]);
//! assert!(flags.is_enabled("dispatch_convoy_routing"));
//! assert!(!flags.is_enabled("mcp_bidirectional"));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Runtime feature flags loaded from deployment config.
///
/// All agents in a deployment share the same flags. Flags control
/// experimental features, gradual rollouts, and operational toggles.
///
/// Unknown flags (not in the map) are treated as disabled (`false`).
/// Use [`known_flags`](Self::known_flags) to list all defined flags
/// and [`unknown_flags`](Self::unknown_flags) to detect typos in config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    flags: BTreeMap<String, bool>,
}

/// Known flag names expected by the current version of Cosmon.
///
/// Flags not in this list trigger a warning at startup (likely typos).
const KNOWN_FLAGS: &[&str] = &[
    "dispatch_convoy_routing",
    "mcp_bidirectional",
    "patrol_auto_restart",
    "experimental_energy_dashboard",
];

impl FeatureFlags {
    /// Create an empty feature flags set (all flags disabled).
    #[must_use]
    pub fn new() -> Self {
        Self {
            flags: BTreeMap::new(),
        }
    }

    /// Check whether a named flag is enabled.
    ///
    /// Returns `false` for unknown flags (not in the map).
    #[must_use]
    pub fn is_enabled(&self, flag: &str) -> bool {
        self.flags.get(flag).copied().unwrap_or(false)
    }

    /// List all known flag names for the current Cosmon version.
    ///
    /// Use this at startup to warn about unknown flags in config.
    #[must_use]
    pub fn known_flags() -> &'static [&'static str] {
        KNOWN_FLAGS
    }

    /// Return flag names present in this set but not in the known flags list.
    ///
    /// Non-empty result likely indicates typos in config.
    #[must_use]
    pub fn unknown_flags(&self) -> Vec<&str> {
        self.flags
            .keys()
            .filter(|k| !KNOWN_FLAGS.contains(&k.as_str()))
            .map(String::as_str)
            .collect()
    }

    /// Set a flag value. Returns the previous value, if any.
    pub fn set(&mut self, flag: impl Into<String>, enabled: bool) -> Option<bool> {
        self.flags.insert(flag.into(), enabled)
    }

    /// Return the number of flags defined.
    #[must_use]
    pub fn len(&self) -> usize {
        self.flags.len()
    }

    /// Return `true` if no flags are defined.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.flags.is_empty()
    }
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Into<String>> FromIterator<(S, bool)> for FeatureFlags {
    fn from_iter<I: IntoIterator<Item = (S, bool)>>(iter: I) -> Self {
        Self {
            flags: iter.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_flags_all_disabled() {
        let flags = FeatureFlags::new();
        assert!(!flags.is_enabled("anything"));
        assert!(flags.is_empty());
    }

    #[test]
    fn test_from_iter() {
        let flags = FeatureFlags::from_iter([
            ("dispatch_convoy_routing", true),
            ("mcp_bidirectional", false),
        ]);
        assert!(flags.is_enabled("dispatch_convoy_routing"));
        assert!(!flags.is_enabled("mcp_bidirectional"));
        assert!(!flags.is_enabled("unknown"));
        assert_eq!(flags.len(), 2);
    }

    #[test]
    fn test_set_and_get() {
        let mut flags = FeatureFlags::new();
        assert!(flags.set("patrol_auto_restart", true).is_none());
        assert!(flags.is_enabled("patrol_auto_restart"));
        assert_eq!(flags.set("patrol_auto_restart", false), Some(true));
        assert!(!flags.is_enabled("patrol_auto_restart"));
    }

    #[test]
    fn test_unknown_flags_detection() {
        let flags = FeatureFlags::from_iter([
            ("dispatch_convoy_routing", true),
            ("typo_flag_name", true),
            ("another_typo", false),
        ]);
        let unknown = flags.unknown_flags();
        assert_eq!(unknown.len(), 2);
        assert!(unknown.contains(&"typo_flag_name"));
        assert!(unknown.contains(&"another_typo"));
    }

    #[test]
    fn test_known_flags_not_empty() {
        assert!(!FeatureFlags::known_flags().is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let flags = FeatureFlags::from_iter([
            ("dispatch_convoy_routing", true),
            ("mcp_bidirectional", false),
        ]);
        let json = serde_json::to_string(&flags).unwrap();
        let back: FeatureFlags = serde_json::from_str(&json).unwrap();
        assert_eq!(flags, back);
    }

    #[test]
    fn test_default_is_empty() {
        let flags = FeatureFlags::default();
        assert!(flags.is_empty());
    }
}
