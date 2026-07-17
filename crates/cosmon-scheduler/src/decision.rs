// SPDX-License-Identifier: AGPL-3.0-only

//! Outcome of evaluating a single patrol on a single tick.
//!
//! The scheduler never mutates state or spawns processes during evaluation;
//! it only produces a [`Decision`]. Step 1 uses `Decision` as the return
//! value of `tick --dry-run`; Step 2 will feed the same enum into the
//! dispatcher (fire on `WouldFire`, log on `WouldSkip`, alert on `Invalid`).

use serde::{Deserialize, Serialize};

/// The outcome of evaluating a patrol. String reasons are human-facing and
/// intended for log lines and `cs scheduler status`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// All gates passed — the patrol should be dispatched.
    WouldFire,

    /// A gate rejected the patrol with an explanatory reason.
    WouldSkip {
        /// Human-readable reason (kill-switch present, disabled, env missing).
        reason: String,
    },

    /// The patrol's `[patrol.sunset]` rule converged — the dispatcher should
    /// run the sunset action (launchctl unload advisory + event emission) and
    /// record `sunset_decided_at` in state. Subsequent ticks short-circuit
    /// via `WouldSkip` with reason `"already sunsetted"`.
    WouldSunset {
        /// Human-readable reason describing which predicate fired (e.g.
        /// `"variance-threshold converged"`, `"sample-count reached 100"`).
        reason: String,
    },

    /// The patrol declaration itself is malformed (schema-level). Surfaced as
    /// a distinct variant so operators can see configuration drift separately
    /// from routine "not due" skips.
    Invalid {
        /// Human-readable reason (e.g. `"XOR violation"`).
        reason: String,
    },
}

impl Decision {
    /// Convenience constructor for the common "skip with reason" case.
    pub fn skip(reason: impl Into<String>) -> Self {
        Decision::WouldSkip {
            reason: reason.into(),
        }
    }

    /// Convenience constructor for malformed patrols.
    pub fn invalid(reason: impl Into<String>) -> Self {
        Decision::Invalid {
            reason: reason.into(),
        }
    }

    /// Convenience constructor for the convergence-fired case.
    pub fn sunset(reason: impl Into<String>) -> Self {
        Decision::WouldSunset {
            reason: reason.into(),
        }
    }

    /// Short one-word label for table-style dry-run output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Decision::WouldFire => "FIRE",
            Decision::WouldSkip { .. } => "SKIP",
            Decision::WouldSunset { .. } => "SUNSET",
            Decision::Invalid { .. } => "INVALID",
        }
    }

    /// Human-readable trailing detail (empty for `WouldFire`).
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Decision::WouldFire => "",
            Decision::WouldSkip { reason }
            | Decision::WouldSunset { reason }
            | Decision::Invalid { reason } => reason.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_stable() {
        assert_eq!(Decision::WouldFire.label(), "FIRE");
        assert_eq!(Decision::skip("x").label(), "SKIP");
        assert_eq!(Decision::sunset("x").label(), "SUNSET");
        assert_eq!(Decision::invalid("x").label(), "INVALID");
    }

    #[test]
    fn detail_is_empty_for_fire() {
        assert_eq!(Decision::WouldFire.detail(), "");
    }

    #[test]
    fn detail_carries_reason() {
        assert_eq!(Decision::skip("disabled").detail(), "disabled");
        assert_eq!(Decision::sunset("converged").detail(), "converged");
        assert_eq!(Decision::invalid("bad cadence").detail(), "bad cadence");
    }

    #[test]
    fn sunset_serde_tag_is_would_sunset() {
        let d = Decision::sunset("variance-threshold converged");
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("would_sunset"), "tagged kind: {json}");
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn serde_roundtrip_with_tagged_kind() {
        let d = Decision::skip("kill-switch present");
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("would_skip"), "tagged kind: {json}");
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
