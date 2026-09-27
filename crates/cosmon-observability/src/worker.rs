// SPDX-License-Identifier: AGPL-3.0-only

//! Worker view — the live process operating on a molecule.

use serde::{Deserialize, Serialize};

/// Newtype wrapper for a worker identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerId(pub String);

impl std::fmt::Display for WorkerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for WorkerId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// How an observer may describe the worker's monetary or subscription cost.
///
/// The enum makes the absence of a per-token bill explicit.  In particular,
/// [`Self::Subscription`] cannot collapse to the numeric value zero and be
/// mistaken for free work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnergyCost {
    /// No billing or reference-price observation is available.
    #[default]
    Unknown,
    /// A reference estimate at published per-token list prices.
    ReferenceUsd {
        /// Estimated US-dollar amount.
        usd: f64,
    },
    /// A `ChatGPT` subscription allowance rather than a per-token bill.
    Subscription {
        /// Plan label reported by the adapter, when available.
        plan_type: Option<String>,
        /// Share of the primary allowance window consumed, in percent.
        used_percent: f64,
        /// Length of that allowance window, when reported.
        window_minutes: Option<u64>,
    },
}

impl EnergyCost {
    /// Return the reference-price estimate when this observation has one.
    #[must_use]
    pub fn reference_usd(&self) -> Option<f64> {
        match self {
            Self::ReferenceUsd { usd } => Some(*usd),
            Self::Unknown | Self::Subscription { .. } => None,
        }
    }
}

/// Token accounting for a worker — projected from adapter session probes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EnergyBudget {
    /// Cumulative input tokens observed, including the cached subset.
    pub input_tokens: u64,
    /// Cached-input subset of [`Self::input_tokens`].
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Cumulative output tokens observed, including the reasoning subset.
    pub output_tokens: u64,
    /// Reasoning-output subset of [`Self::output_tokens`].
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    /// Billing interpretation for this observation.
    #[serde(default)]
    pub cost: EnergyCost,
    /// Context window size, if known.
    pub context_window: Option<u64>,
}

impl EnergyBudget {
    /// Sum of input + output tokens.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Build an [`EnergyBudget`] from a Claude Code session JSONL file.
    ///
    /// Returns `None` if the file cannot be parsed. The pricing model is
    /// `PricingModel::opus()` — adjust at the call site if a different
    /// model mix is needed.
    #[must_use]
    pub fn from_session_log(path: &std::path::Path) -> Option<Self> {
        let log = claudion::parse_session(path).ok()?;
        let metrics = claudion::compute_metrics(&log, &claudion::PricingModel::opus());
        let input = metrics.total_input + metrics.total_cache_creation + metrics.total_cache_read;
        Some(Self {
            input_tokens: input.get(),
            cached_input_tokens: metrics.total_cache_read.get(),
            output_tokens: metrics.total_output.get(),
            reasoning_output_tokens: 0,
            cost: EnergyCost::ReferenceUsd {
                usd: metrics.total_cost.get(),
            },
            context_window: None,
        })
    }
}

/// Whether this worker is the resident runtime or a cognitive process.
///
/// Mirrors `cosmon_core::worker::WorkerRole` without adding a crate
/// dependency edge — the observability layer stays free of core types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    /// Cognition process (Claude, Codex, etc.). Default.
    #[default]
    Cognition,
    /// Resident runtime driving a macro-molecule DAG.
    Runtime,
}

impl WorkerRole {
    /// Both roles, cognition first — the order the `cs peek` glyph
    /// legend lists them in. Exists so that legend can be derived from
    /// the enum instead of transcribed beside it.
    pub const ALL: &'static [WorkerRole] = &[Self::Cognition, Self::Runtime];
}

/// A worker executing a molecule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Worker {
    /// Worker identifier.
    pub id: WorkerId,
    /// Molecule this worker is currently executing, if any.
    pub molecule_id: Option<String>,
    /// Tmux session the worker lives in.
    pub session: String,
    /// Current energy accounting.
    pub energy: EnergyBudget,
    /// Liveness hint from transport probe (e.g. `"working"`, `"idle"`, `"dead"`).
    pub live: String,
    /// Runtime vs cognition discriminator. Defaults
    /// to [`WorkerRole::Cognition`] when absent in a legacy snapshot.
    #[serde(default)]
    pub role: WorkerRole,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_total_sums_tokens() {
        let b = EnergyBudget {
            input_tokens: 10,
            cached_input_tokens: 4,
            output_tokens: 32,
            reasoning_output_tokens: 8,
            cost: EnergyCost::Unknown,
            context_window: Some(200_000),
        };
        assert_eq!(b.total(), 42);
    }
}
