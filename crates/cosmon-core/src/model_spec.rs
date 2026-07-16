// SPDX-License-Identifier: AGPL-3.0-only

//! Model specification types for LLM selection and configuration.
//!
//! A [`ModelSpec`] captures the constraints an agent places on its backing LLM:
//! which model to use, how hard the model should reason, and what cost tier is
//! acceptable. These are **demand-side** declarations — the runtime maps them to
//! concrete API parameters at invocation time.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::model_spec::{ModelSpec, ReasoningEffort, CostTier};
//! use cosmon_core::energy::Temperature;
//!
//! let spec = ModelSpec::new("claude-opus-4-6")
//!     .with_reasoning_effort(ReasoningEffort::High)
//!     .with_cost_tier(CostTier::Premium)
//!     .with_temperature(Temperature::COOL);
//!
//! assert_eq!(spec.model(), "claude-opus-4-6");
//! assert_eq!(spec.reasoning_effort(), ReasoningEffort::High);
//! assert_eq!(spec.cost_tier(), CostTier::Premium);
//! ```

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::energy::Temperature;

// ---------------------------------------------------------------------------
// ReasoningEffort
// ---------------------------------------------------------------------------

/// How hard the model should reason before responding.
///
/// Maps to the `reasoning_effort` parameter in Claude's extended thinking API.
/// Higher effort produces more thorough analysis at the cost of latency and tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Minimal reasoning — fast, shallow responses.
    Low,
    /// Balanced reasoning — the default for most tasks.
    #[default]
    Medium,
    /// Deep reasoning — thorough analysis, higher latency and cost.
    High,
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => f.write_str("low"),
            Self::Medium => f.write_str("medium"),
            Self::High => f.write_str("high"),
        }
    }
}

impl FromStr for ReasoningEffort {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            _ => Err(ParseEnumError {
                type_name: "ReasoningEffort",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// CostTier
// ---------------------------------------------------------------------------

/// Acceptable cost tier for model invocations.
///
/// Agents declare their cost tolerance; the runtime uses this to select or
/// reject model choices that would exceed the tier's implied spend rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostTier {
    /// Cheapest models — suitable for high-volume, low-stakes tasks.
    Budget,
    /// Mid-range models — the default balance of capability and cost.
    #[default]
    Standard,
    /// Top-tier models — maximum capability, highest cost.
    Premium,
}

impl fmt::Display for CostTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget => f.write_str("budget"),
            Self::Standard => f.write_str("standard"),
            Self::Premium => f.write_str("premium"),
        }
    }
}

impl FromStr for CostTier {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "budget" => Ok(Self::Budget),
            "standard" => Ok(Self::Standard),
            "premium" => Ok(Self::Premium),
            _ => Err(ParseEnumError {
                type_name: "CostTier",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// ModelSpec
// ---------------------------------------------------------------------------

/// Specification of model requirements for an agent or step.
///
/// A `ModelSpec` is a demand-side declaration: "I need a model with these
/// properties." The runtime resolves it to a concrete API call. This keeps
/// the core domain free of provider-specific details.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Model identifier (e.g. `"claude-opus-4-6"`, `"claude-haiku-4-5-20251001"`).
    model: String,
    /// How deeply the model should reason.
    reasoning_effort: ReasoningEffort,
    /// Acceptable cost tier.
    cost_tier: CostTier,
    /// Sampling temperature (optional override; `None` uses the model's default).
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<Temperature>,
}

impl ModelSpec {
    /// Create a new model spec with default reasoning effort and cost tier.
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            reasoning_effort: ReasoningEffort::default(),
            cost_tier: CostTier::default(),
            temperature: None,
        }
    }

    /// Set the reasoning effort.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Set the cost tier.
    #[must_use]
    pub fn with_cost_tier(mut self, tier: CostTier) -> Self {
        self.cost_tier = tier;
        self
    }

    /// Set the sampling temperature.
    #[must_use]
    pub fn with_temperature(mut self, temp: Temperature) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// The model identifier.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The reasoning effort level.
    #[must_use]
    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    /// The cost tier.
    #[must_use]
    pub fn cost_tier(&self) -> CostTier {
        self.cost_tier
    }

    /// The sampling temperature override, if any.
    #[must_use]
    pub fn temperature(&self) -> Option<Temperature> {
        self.temperature
    }
}

impl fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (reasoning={}, cost={})",
            self.model, self.reasoning_effort, self.cost_tier
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ReasoningEffort --

    #[test]
    fn test_reasoning_effort_default_is_medium() {
        assert_eq!(ReasoningEffort::default(), ReasoningEffort::Medium);
    }

    #[test]
    fn test_reasoning_effort_display_roundtrip() {
        let variants = [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ];
        for v in variants {
            let s = v.to_string();
            let parsed: ReasoningEffort = s.parse().unwrap();
            assert_eq!(parsed, v);
        }
    }

    #[test]
    fn test_reasoning_effort_parse_error() {
        let err = "extreme".parse::<ReasoningEffort>().unwrap_err();
        assert_eq!(err.type_name, "ReasoningEffort");
        assert_eq!(err.value, "extreme");
    }

    #[test]
    fn test_reasoning_effort_serde_roundtrip() {
        let v = ReasoningEffort::High;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"high\"");
        let back: ReasoningEffort = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    // -- CostTier --

    #[test]
    fn test_cost_tier_default_is_standard() {
        assert_eq!(CostTier::default(), CostTier::Standard);
    }

    #[test]
    fn test_cost_tier_display_roundtrip() {
        let variants = [CostTier::Budget, CostTier::Standard, CostTier::Premium];
        for v in variants {
            let s = v.to_string();
            let parsed: CostTier = s.parse().unwrap();
            assert_eq!(parsed, v);
        }
    }

    #[test]
    fn test_cost_tier_parse_error() {
        let err = "free".parse::<CostTier>().unwrap_err();
        assert_eq!(err.type_name, "CostTier");
        assert_eq!(err.value, "free");
    }

    #[test]
    fn test_cost_tier_serde_roundtrip() {
        let v = CostTier::Budget;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"budget\"");
        let back: CostTier = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    // -- ModelSpec --

    #[test]
    fn test_model_spec_defaults() {
        let spec = ModelSpec::new("claude-opus-4-6");
        assert_eq!(spec.model(), "claude-opus-4-6");
        assert_eq!(spec.reasoning_effort(), ReasoningEffort::Medium);
        assert_eq!(spec.cost_tier(), CostTier::Standard);
        assert_eq!(spec.temperature(), None);
    }

    #[test]
    fn test_model_spec_builder() {
        let spec = ModelSpec::new("claude-haiku-4-5-20251001")
            .with_reasoning_effort(ReasoningEffort::Low)
            .with_cost_tier(CostTier::Budget)
            .with_temperature(Temperature::FROZEN);

        assert_eq!(spec.model(), "claude-haiku-4-5-20251001");
        assert_eq!(spec.reasoning_effort(), ReasoningEffort::Low);
        assert_eq!(spec.cost_tier(), CostTier::Budget);
        assert_eq!(spec.temperature(), Some(Temperature::FROZEN));
    }

    #[test]
    fn test_model_spec_display() {
        let spec = ModelSpec::new("claude-opus-4-6")
            .with_reasoning_effort(ReasoningEffort::High)
            .with_cost_tier(CostTier::Premium);
        assert_eq!(
            spec.to_string(),
            "claude-opus-4-6 (reasoning=high, cost=premium)"
        );
    }

    #[test]
    fn test_model_spec_serde_roundtrip() {
        let spec = ModelSpec::new("claude-opus-4-6")
            .with_reasoning_effort(ReasoningEffort::High)
            .with_cost_tier(CostTier::Premium)
            .with_temperature(Temperature::COOL);

        let json = serde_json::to_string_pretty(&spec).unwrap();
        let back: ModelSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn test_model_spec_serde_without_temperature() {
        let spec = ModelSpec::new("claude-opus-4-6");
        let json = serde_json::to_string(&spec).unwrap();
        // temperature should be absent from JSON when None
        assert!(!json.contains("temperature"));
        let back: ModelSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }
}
