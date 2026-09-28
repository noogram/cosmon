// SPDX-License-Identifier: AGPL-3.0-only

//! Worker view — the live process operating on a molecule.

use serde::{Deserialize, Serialize};

use cosmon_core::codex_energy::CodexSubscriptionUsage;
use cosmon_core::price_manifest::{bundled_price_manifest, value_model_segments};
use cosmon_core::usage::{
    ApiEquivalent, ModelUsageSegment, ObservationProvenance, ObservationScope, TokenCount,
    UnavailableReason, UsageRecord,
};

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
    /// Versioned API-equivalent valuation with explicit coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_equivalent: Option<ApiEquivalent>,
    /// Plan allowance observation, independent from API-equivalent USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<CodexSubscriptionUsage>,
    /// Authoritative versioned usage record. Legacy fields above remain a
    /// deprecated projection for existing JSON consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageRecord>,
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
    /// Returns `None` if the file cannot be parsed or counters overflow.
    /// Every turn is priced against its exact realized model under the
    /// bundled current rate card; unknown models retain a partial subtotal.
    #[must_use]
    pub fn from_session_log(path: &std::path::Path) -> Option<Self> {
        let log = claudion::parse_session(path).ok()?;
        let (input, cached_input, output) = claude_totals(&log)?;
        let (segments, complete) = claude_model_segments(&log);
        let provenance = ObservationProvenance {
            source: "claude_code_session_log".to_owned(),
            provider: Some("anthropic".to_owned()),
            observed_at: log.end_time,
            captured_at: None,
            scope: ObservationScope::UsageHistory,
        };
        let api_equivalent = bundled_price_manifest()
            .ok()
            .and_then(|manifest| {
                manifest
                    .current_card()
                    .map(|card| value_model_segments(card, &segments, complete, provenance))
            })
            .unwrap_or(ApiEquivalent::Unavailable {
                reason: UnavailableReason::MissingPricingCategory,
            });
        Some(Self {
            input_tokens: input,
            cached_input_tokens: cached_input,
            output_tokens: output,
            reasoning_output_tokens: 0,
            cost: legacy_cost_projection(&api_equivalent),
            api_equivalent: Some(api_equivalent),
            subscription: None,
            usage: None,
            context_window: None,
        })
    }
}

fn claude_totals(session: &claudion::SessionLog) -> Option<(u64, u64, u64)> {
    let mut input = 0_u64;
    let mut cached = 0_u64;
    let mut output = 0_u64;
    for turn in &session.turns {
        input = input
            .checked_add(turn.input_tokens.get())?
            .checked_add(turn.cache_creation_input_tokens.get())?
            .checked_add(turn.cache_read_input_tokens.get())?;
        cached = cached.checked_add(turn.cache_read_input_tokens.get())?;
        output = output.checked_add(turn.output_tokens.get())?;
    }
    Some((input, cached, output))
}

fn claude_model_segments(session: &claudion::SessionLog) -> (Vec<ModelUsageSegment>, bool) {
    let mut segments = Vec::new();
    let mut complete = true;
    for turn in &session.turns {
        let Some(model) = turn.model.as_deref().filter(|model| !model.is_empty()) else {
            complete = false;
            continue;
        };
        let Some(input_tokens) = turn
            .input_tokens
            .get()
            .checked_add(turn.cache_creation_input_tokens.get())
            .and_then(|total| total.checked_add(turn.cache_read_input_tokens.get()))
        else {
            complete = false;
            continue;
        };
        segments.push(ModelUsageSegment {
            model: model.to_owned(),
            input_tokens: TokenCount::Measured {
                tokens: input_tokens,
            },
            cached_input_tokens: TokenCount::Measured {
                tokens: turn.cache_read_input_tokens.get(),
            },
            cache_write_tokens: TokenCount::Measured {
                tokens: turn.cache_creation_input_tokens.get(),
            },
            output_tokens: TokenCount::Measured {
                tokens: turn.output_tokens.get(),
            },
            reasoning_output_tokens: TokenCount::Unavailable {
                reason: UnavailableReason::Unsupported,
            },
        });
    }
    (segments, complete)
}

fn legacy_cost_projection(value: &ApiEquivalent) -> EnergyCost {
    match value {
        ApiEquivalent::Estimated { amount_usd, .. } => EnergyCost::ReferenceUsd {
            usd: amount_usd.get(),
        },
        ApiEquivalent::Unavailable { .. } | _ => EnergyCost::Unknown,
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
    use std::io::Write as _;

    #[test]
    fn energy_total_sums_tokens() {
        let b = EnergyBudget {
            input_tokens: 10,
            cached_input_tokens: 4,
            output_tokens: 32,
            reasoning_output_tokens: 8,
            cost: EnergyCost::Unknown,
            api_equivalent: None,
            subscription: None,
            usage: None,
            context_window: Some(200_000),
        };
        assert_eq!(b.total(), 42);
    }

    #[test]
    fn session_reader_prices_each_realized_model_not_fixed_opus() {
        let mut log = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            log,
            r#"{{"type":"assistant","sessionId":"s","timestamp":"2026-09-28T08:00:00Z","message":{{"model":"claude-opus-4-6","usage":{{"input_tokens":1000000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":100000}}}}}}"#
        )
        .unwrap();
        writeln!(
            log,
            r#"{{"type":"assistant","sessionId":"s","timestamp":"2026-09-28T08:01:00Z","message":{{"model":"claude-sonnet-5","usage":{{"input_tokens":1000000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":100000}}}}}}"#
        )
        .unwrap();

        let energy = EnergyBudget::from_session_log(log.path()).unwrap();
        // Opus: $5 + $2.50. Sonnet: $2 + $1.00.
        assert_eq!(energy.cost.reference_usd(), Some(10.5));
    }
}
