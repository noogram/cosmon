// SPDX-License-Identifier: AGPL-3.0-only

//! Energy consciousness types for tracking token usage, costs, and budgets.
//!
//! Models the thermodynamic metaphor from THESIS.md Parts XI and XII:
//! agents consume energy (tokens), operate at a temperature, and their
//! productive output is measured as free energy. Entropy is a computable
//! observable measured in bits (Shannon entropy, log base 2).
//!
//! # Examples
//!
//! ```
//! use cosmon_core::energy::{TokenCount, TokenCost, Temperature, EnergyBudget, BudgetPeriod};
//!
//! // Token counts are newtypes with saturating subtraction:
//! let a = TokenCount::new(100);
//! let b = TokenCount::new(42);
//! assert_eq!((a + b).get(), 142);
//! assert_eq!((b - a).get(), 0); // saturates, no underflow
//!
//! // Temperature is clamped to [0.0, 1.0]:
//! let t = Temperature::new(2.5);
//! assert!((t.get() - 1.0).abs() < f64::EPSILON);
//!
//! // Budgets track consumption and alert on thresholds:
//! let mut budget = EnergyBudget::new(
//!     TokenCount::new(10_000),
//!     BudgetPeriod::Weekly,
//!     0.8,
//! );
//! budget.consume(TokenCount::new(8_500));
//! assert!(budget.is_alert()); // 85% > 80% threshold
//! ```
//!
//! ```
//! use cosmon_core::energy::{Entropy, CompressionRatio, EntropySource};
//!
//! // Entropy is non-negative, measured in bits:
//! let h = Entropy::new(3.5);
//! assert!((h.get() - 3.5).abs() < f64::EPSILON);
//!
//! // Negative values are clamped to zero:
//! let h = Entropy::new(-1.0);
//! assert!((h.get() - 0.0).abs() < f64::EPSILON);
//!
//! // Compression ratio is clamped to [0.0, 1.0]:
//! let cr = CompressionRatio::new(0.42);
//! assert!((cr.get() - 0.42).abs() < f64::EPSILON);
//!
//! // Convert compression ratio to entropy (bits per byte):
//! let h = cr.to_entropy();
//! assert!((h.get() - 0.42 * 8.0).abs() < f64::EPSILON);
//! ```

use std::fmt;
use std::ops::{Add, Sub};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{MoleculeId, StepId, WorkerId};

// ---------------------------------------------------------------------------
// TokenCount
// ---------------------------------------------------------------------------

/// A count of tokens (input or output). Wraps `u64`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TokenCount(u64);

impl TokenCount {
    /// Create a new token count.
    #[must_use]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    /// Return the inner value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Add for TokenCount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for TokenCount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl fmt::Display for TokenCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} tokens", self.0)
    }
}

// ---------------------------------------------------------------------------
// TokenCost
// ---------------------------------------------------------------------------

/// A monetary cost in currency units (e.g. USD). Wraps `f64`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenCost(f64);

impl TokenCost {
    /// Create a new token cost.
    #[must_use]
    pub fn new(amount: f64) -> Self {
        Self(amount)
    }

    /// Return the inner value.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Add for TokenCost {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl fmt::Display for TokenCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.4}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Temperature
// ---------------------------------------------------------------------------

/// LLM sampling temperature, clamped to [0.0, 1.0].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Temperature(f64);

impl Temperature {
    /// Frozen: deterministic output.
    pub const FROZEN: Self = Self(0.0);
    /// Cool: low creativity.
    pub const COOL: Self = Self(0.3);
    /// Warm: balanced.
    pub const WARM: Self = Self(0.7);
    /// Hot: maximum creativity within bounds.
    pub const HOT: Self = Self(1.0);

    /// Create a new temperature, clamping to [0.0, 1.0].
    #[must_use]
    pub fn new(value: f64) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// Return the inner value (always in [0.0, 1.0]).
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Default for Temperature {
    fn default() -> Self {
        Self::WARM
    }
}

impl fmt::Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self.0 {
            0.0 => "frozen",
            x if x <= 0.3 => "cool",
            x if x <= 0.7 => "warm",
            _ => "hot",
        };
        write!(f, "{:.1} ({label})", self.0)
    }
}

// ---------------------------------------------------------------------------
// BudgetPeriod
// ---------------------------------------------------------------------------

/// The time period or scope over which an energy budget applies.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    /// Budget resets every week.
    Weekly,
    /// Budget resets every month.
    Monthly,
    /// Budget applies to a single molecule execution.
    PerMolecule(MoleculeId),
}

impl fmt::Display for BudgetPeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Weekly => f.write_str("weekly"),
            Self::Monthly => f.write_str("monthly"),
            Self::PerMolecule(id) => write!(f, "molecule:{id}"),
        }
    }
}

// ---------------------------------------------------------------------------
// EnergyBudget
// ---------------------------------------------------------------------------

/// A token budget with consumption tracking and alert threshold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnergyBudget {
    /// Maximum tokens allowed in this budget period.
    pub total: TokenCount,
    /// Tokens consumed so far.
    pub consumed: TokenCount,
    /// The time period this budget covers.
    pub period: BudgetPeriod,
    /// Fraction (0.0..=1.0) at which to trigger an alert.
    pub alert_threshold: f64,
}

impl EnergyBudget {
    /// Create a new budget.
    #[must_use]
    pub fn new(total: TokenCount, period: BudgetPeriod, alert_threshold: f64) -> Self {
        Self {
            total,
            consumed: TokenCount::new(0),
            period,
            alert_threshold: alert_threshold.clamp(0.0, 1.0),
        }
    }

    /// Tokens remaining (saturating).
    #[must_use]
    pub fn remaining(&self) -> TokenCount {
        self.total - self.consumed
    }

    /// Fraction consumed (0.0..=1.0).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn utilization(&self) -> f64 {
        if self.total.get() == 0 {
            return 0.0;
        }
        self.consumed.get() as f64 / self.total.get() as f64
    }

    /// Whether utilization has exceeded the alert threshold.
    #[must_use]
    pub fn is_alert(&self) -> bool {
        self.utilization() >= self.alert_threshold
    }

    /// Record token consumption.
    pub fn consume(&mut self, tokens: TokenCount) {
        self.consumed = self.consumed + tokens;
    }
}

// ---------------------------------------------------------------------------
// StepBudget — per-molecule step counter circuit breaker
// ---------------------------------------------------------------------------

/// Per-molecule step counter circuit breaker (THESIS Part XI).
///
/// Decrements once per `cs evolve` step. When `remaining` reaches zero, the
/// next attempted evolve transitions the molecule to `Frozen` with reason
/// `"energy-exhausted"` instead of advancing — the structural protection
/// against silent runaway loops named in karpathy + feynman convergence.
///
/// This is **not** a billing meter. It is a circuit breaker: exhaustion is
/// the signal, and the operator's repair is "inspect and either bump the cap
/// with `cs thaw` workflow or collapse the molecule" — never silent retry.
///
/// `cap` is the immutable budget chosen at nucleate time (the upper bound an
/// auditor can read after the fact); `remaining` is the live counter that
/// `cs evolve` decrements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepBudget {
    /// Immutable cap chosen at nucleate time.
    pub cap: u32,
    /// Steps still allowed before the circuit breaker fires.
    pub remaining: u32,
}

impl StepBudget {
    /// Build a fresh budget with `cap` slots, all of them remaining.
    #[must_use]
    pub const fn new(cap: u32) -> Self {
        Self {
            cap,
            remaining: cap,
        }
    }

    /// `true` when the budget is at zero — the next `cs evolve` MUST refuse.
    #[must_use]
    pub const fn is_exhausted(self) -> bool {
        self.remaining == 0
    }

    /// Try to consume one slot. Returns `true` when a slot was available
    /// (and decremented), `false` when the budget was already exhausted.
    /// Saturating — never wraps below zero.
    pub fn consume(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

impl fmt::Display for StepBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.remaining, self.cap)
    }
}

// ---------------------------------------------------------------------------
// EnergyRecord
// ---------------------------------------------------------------------------

/// A single record of energy (token) consumption.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnergyRecord {
    /// When the consumption occurred.
    pub timestamp: DateTime<Utc>,
    /// The worker that consumed the tokens.
    pub worker: WorkerId,
    /// The molecule being executed.
    pub molecule: MoleculeId,
    /// The step within the molecule.
    pub step: StepId,
    /// The LLM model used (e.g. `"claude-opus-4-6"`).
    pub model: String,
    /// Number of input tokens consumed.
    pub input_tokens: TokenCount,
    /// Number of output tokens produced.
    pub output_tokens: TokenCount,
    /// Monetary cost of this API call.
    pub cost: TokenCost,
}

impl EnergyRecord {
    /// Total tokens for this record.
    #[must_use]
    pub fn total_tokens(&self) -> TokenCount {
        self.input_tokens + self.output_tokens
    }
}

// ---------------------------------------------------------------------------
// EnergyReport
// ---------------------------------------------------------------------------

/// Aggregated energy report with efficiency metrics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EnergyReport {
    /// Token consumption broken down by worker.
    pub by_worker: Vec<(WorkerId, TokenCount)>,
    /// Token consumption broken down by molecule.
    pub by_molecule: Vec<(MoleculeId, TokenCount)>,
    /// Tokens spent on overhead (retries, errors, coordination).
    pub entropy_tax: TokenCount,
    /// Tokens that contributed to useful output.
    pub productive_tokens: TokenCount,
}

impl EnergyReport {
    /// Total tokens across all workers.
    #[must_use]
    pub fn total_tokens(&self) -> TokenCount {
        self.by_worker
            .iter()
            .fold(TokenCount::new(0), |acc, (_, t)| acc + *t)
    }

    /// Free energy ratio: productive / total. Returns 0.0 if total is zero.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn free_energy_ratio(&self) -> f64 {
        let total = self.total_tokens().get();
        if total == 0 {
            return 0.0;
        }
        self.productive_tokens.get() as f64 / total as f64
    }
}

// ---------------------------------------------------------------------------
// Entropy
// ---------------------------------------------------------------------------

/// Shannon entropy measured in bits (log base 2). Non-negative.
///
/// All entropy values in Cosmon use bits as the unit (THESIS.md Part XII).
/// The bridge between entropy (bits) and energy (tokens) is the
/// bits-per-token ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Entropy(f64);

impl Entropy {
    /// Zero entropy — a fully determined state.
    pub const ZERO: Self = Self(0.0);

    /// Create a new entropy value. Negative values are clamped to zero.
    #[must_use]
    pub fn new(bits: f64) -> Self {
        if bits < 0.0 {
            Self(0.0)
        } else {
            Self(bits)
        }
    }

    /// Return the inner value in bits.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Add for Entropy {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Entropy {
    type Output = Self;

    /// Saturating subtraction — entropy cannot go negative.
    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.0 - rhs.0)
    }
}

impl fmt::Display for Entropy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2} bits", self.0)
    }
}

// ---------------------------------------------------------------------------
// CompressionRatio
// ---------------------------------------------------------------------------

/// Ratio of compressed size to raw size, clamped to [0.0, 1.0].
///
/// Used to estimate Shannon entropy from empirical compression:
/// `H ≈ compressed_bytes / raw_bytes × 8` (bits per byte, max 8).
/// See THESIS.md Part XII — message entropy.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CompressionRatio(f64);

impl CompressionRatio {
    /// Create a new compression ratio, clamping to [0.0, 1.0].
    #[must_use]
    pub fn new(ratio: f64) -> Self {
        Self(ratio.clamp(0.0, 1.0))
    }

    /// Return the inner value (always in [0.0, 1.0]).
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// Convert to entropy in bits per byte: `ratio × 8`.
    #[must_use]
    pub fn to_entropy(self) -> Entropy {
        Entropy::new(self.0 * 8.0)
    }
}

impl Default for CompressionRatio {
    fn default() -> Self {
        Self(1.0)
    }
}

impl fmt::Display for CompressionRatio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}%", self.0 * 100.0)
    }
}

// ---------------------------------------------------------------------------
// EntropySource
// ---------------------------------------------------------------------------

/// The four computable sources of entropy in Cosmon (THESIS.md Part XII).
///
/// Ordered from most concrete to most aspirational.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntropySource {
    /// Shannon entropy of inter-agent message stream (compression ratio on JSONL event log).
    Message,
    /// Signal-to-noise ratio of an agent's context window.
    ContextWindow,
    /// Shannon entropy of the codebase (compression ratio of source files).
    Code,
    /// Boltzmann entropy of fleet state: log₂(W) where W = product of possible configurations.
    State,
}

impl fmt::Display for EntropySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message => f.write_str("message"),
            Self::ContextWindow => f.write_str("context_window"),
            Self::Code => f.write_str("code"),
            Self::State => f.write_str("state"),
        }
    }
}

// ---------------------------------------------------------------------------
// EntropyRecord
// ---------------------------------------------------------------------------

/// A single entropy measurement at a point in time.
///
/// Links an entropy value to its source, the worker that measured it,
/// and the molecule context. Analogous to [`EnergyRecord`] but for
/// information-theoretic observables rather than token consumption.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntropyRecord {
    /// When the measurement was taken.
    pub timestamp: DateTime<Utc>,
    /// Which entropy source was measured.
    pub source: EntropySource,
    /// The measured entropy in bits.
    pub entropy: Entropy,
    /// The compression ratio that produced this measurement (if applicable).
    pub compression_ratio: Option<CompressionRatio>,
    /// The worker that took the measurement.
    pub worker: WorkerId,
    /// The molecule context (if any).
    pub molecule: Option<MoleculeId>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_count_arithmetic() {
        let a = TokenCount::new(100);
        let b = TokenCount::new(42);

        assert_eq!((a + b).get(), 142);
        assert_eq!((a - b).get(), 58);

        // Saturating subtraction: no underflow
        assert_eq!((b - a).get(), 0);

        // Ordering
        assert!(a > b);
        assert!(b < a);
        assert_eq!(a, TokenCount::new(100));
    }

    #[test]
    fn test_token_count_display() {
        let t = TokenCount::new(1500);
        assert_eq!(t.to_string(), "1500 tokens");
    }

    #[test]
    fn test_token_cost_arithmetic() {
        let a = TokenCost::new(0.0015);
        let b = TokenCost::new(0.0045);
        let sum = a + b;
        assert!((sum.get() - 0.006).abs() < f64::EPSILON);
    }

    #[test]
    fn test_temperature_clamped_to_01() {
        // Values within range are preserved
        assert!((Temperature::new(0.5).get() - 0.5).abs() < f64::EPSILON);

        // Below 0.0 is clamped to 0.0
        assert!((Temperature::new(-1.0).get() - 0.0).abs() < f64::EPSILON);

        // Above 1.0 is clamped to 1.0
        assert!((Temperature::new(2.5).get() - 1.0).abs() < f64::EPSILON);

        // Named presets are in range
        assert!((Temperature::FROZEN.get() - 0.0).abs() < f64::EPSILON);
        assert!((Temperature::COOL.get() - 0.3).abs() < f64::EPSILON);
        assert!((Temperature::WARM.get() - 0.7).abs() < f64::EPSILON);
        assert!((Temperature::HOT.get() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_temperature_default_is_warm() {
        assert_eq!(Temperature::default(), Temperature::WARM);
    }

    #[test]
    fn test_energy_budget_remaining() {
        let mut budget = EnergyBudget::new(TokenCount::new(10_000), BudgetPeriod::Weekly, 0.8);

        assert_eq!(budget.remaining().get(), 10_000);
        assert!((budget.utilization() - 0.0).abs() < f64::EPSILON);
        assert!(!budget.is_alert());

        // Consume some tokens
        budget.consume(TokenCount::new(3_000));
        assert_eq!(budget.remaining().get(), 7_000);
        assert!((budget.utilization() - 0.3).abs() < f64::EPSILON);
        assert!(!budget.is_alert());

        // Consume past alert threshold (80%)
        budget.consume(TokenCount::new(5_500));
        assert_eq!(budget.remaining().get(), 1_500);
        assert!(budget.is_alert());
    }

    #[test]
    fn test_energy_budget_zero_total() {
        let budget = EnergyBudget::new(TokenCount::new(0), BudgetPeriod::Monthly, 0.8);
        assert!((budget.utilization() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_free_energy_ratio_calculation() {
        let w1 = WorkerId::new("topaz").unwrap();
        let w2 = WorkerId::new("quartz").unwrap();

        let report = EnergyReport {
            by_worker: vec![(w1, TokenCount::new(600)), (w2, TokenCount::new(400))],
            by_molecule: vec![],
            entropy_tax: TokenCount::new(200),
            productive_tokens: TokenCount::new(800),
        };

        // Total = 600 + 400 = 1000
        assert_eq!(report.total_tokens().get(), 1000);
        // Free energy ratio = 800 / 1000 = 0.8
        assert!((report.free_energy_ratio() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_free_energy_ratio_zero_total() {
        let report = EnergyReport::default();
        assert!((report.free_energy_ratio() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_energy_record_total_tokens() {
        let record = EnergyRecord {
            timestamp: Utc::now(),
            worker: WorkerId::new("topaz").unwrap(),
            molecule: MoleculeId::new("cs-20260401-hjdr").unwrap(),
            step: StepId::new("step-3").unwrap(),
            model: "claude-opus-4-6".to_owned(),
            input_tokens: TokenCount::new(1500),
            output_tokens: TokenCount::new(500),
            cost: TokenCost::new(0.006),
        };
        assert_eq!(record.total_tokens().get(), 2000);
    }

    #[test]
    fn test_budget_period_display() {
        assert_eq!(BudgetPeriod::Weekly.to_string(), "weekly");
        assert_eq!(BudgetPeriod::Monthly.to_string(), "monthly");

        let mol_id = MoleculeId::new("cs-20260401-hjdr").unwrap();
        let period = BudgetPeriod::PerMolecule(mol_id);
        assert_eq!(period.to_string(), "molecule:cs-20260401-hjdr");
    }

    #[test]
    fn test_token_count_serde_roundtrip() {
        let tc = TokenCount::new(42);
        let json = serde_json::to_string(&tc).unwrap();
        let back: TokenCount = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, back);
    }

    #[test]
    fn test_temperature_serde_roundtrip() {
        let t = Temperature::new(0.5);
        let json = serde_json::to_string(&t).unwrap();
        let back: Temperature = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    // -- Entropy --

    #[test]
    fn test_entropy_non_negative() {
        let h = Entropy::new(3.5);
        assert!((h.get() - 3.5).abs() < f64::EPSILON);

        // Negative values clamped to zero
        let h = Entropy::new(-1.0);
        assert!((h.get() - 0.0).abs() < f64::EPSILON);

        // Zero is valid
        assert!((Entropy::ZERO.get() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_entropy_arithmetic() {
        let a = Entropy::new(3.0);
        let b = Entropy::new(1.5);

        assert!(((a + b).get() - 4.5).abs() < f64::EPSILON);
        assert!(((a - b).get() - 1.5).abs() < f64::EPSILON);

        // Saturating subtraction
        assert!(((b - a).get() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_entropy_display() {
        let h = Entropy::new(3.15);
        assert_eq!(h.to_string(), "3.15 bits");
    }

    #[test]
    fn test_entropy_default_is_zero() {
        assert_eq!(Entropy::default(), Entropy::ZERO);
    }

    #[test]
    fn test_entropy_ordering() {
        let low = Entropy::new(1.0);
        let high = Entropy::new(5.0);
        assert!(low < high);
    }

    #[test]
    fn test_entropy_serde_roundtrip() {
        let h = Entropy::new(4.2);
        let json = serde_json::to_string(&h).unwrap();
        let back: Entropy = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    // -- CompressionRatio --

    #[test]
    fn test_compression_ratio_clamped() {
        assert!((CompressionRatio::new(0.5).get() - 0.5).abs() < f64::EPSILON);
        assert!((CompressionRatio::new(-0.1).get() - 0.0).abs() < f64::EPSILON);
        assert!((CompressionRatio::new(1.5).get() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compression_ratio_to_entropy() {
        let cr = CompressionRatio::new(0.42);
        let h = cr.to_entropy();
        // H ≈ compressed_bytes / raw_bytes × 8
        assert!((h.get() - 3.36).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compression_ratio_display() {
        let cr = CompressionRatio::new(0.42);
        assert_eq!(cr.to_string(), "42.0%");
    }

    #[test]
    fn test_compression_ratio_default_is_one() {
        assert!((CompressionRatio::default().get() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compression_ratio_serde_roundtrip() {
        let cr = CompressionRatio::new(0.75);
        let json = serde_json::to_string(&cr).unwrap();
        let back: CompressionRatio = serde_json::from_str(&json).unwrap();
        assert_eq!(cr, back);
    }

    // -- EntropySource --

    #[test]
    fn test_entropy_source_display() {
        assert_eq!(EntropySource::Message.to_string(), "message");
        assert_eq!(EntropySource::ContextWindow.to_string(), "context_window");
        assert_eq!(EntropySource::Code.to_string(), "code");
        assert_eq!(EntropySource::State.to_string(), "state");
    }

    #[test]
    fn test_entropy_source_serde_roundtrip() {
        let src = EntropySource::ContextWindow;
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"context_window\"");
        let back: EntropySource = serde_json::from_str(&json).unwrap();
        assert_eq!(src, back);
    }

    // -- EntropyRecord --

    #[test]
    fn test_entropy_record_construction() {
        let record = EntropyRecord {
            timestamp: Utc::now(),
            source: EntropySource::Message,
            entropy: Entropy::new(3.36),
            compression_ratio: Some(CompressionRatio::new(0.42)),
            worker: WorkerId::new("topaz").unwrap(),
            molecule: Some(MoleculeId::new("cs-20260401-hjdr").unwrap()),
        };
        assert_eq!(record.source, EntropySource::Message);
        assert!((record.entropy.get() - 3.36).abs() < f64::EPSILON);
    }

    // -- StepBudget --

    #[test]
    fn test_step_budget_decrements_until_zero() {
        let mut b = StepBudget::new(3);
        assert_eq!(b.cap, 3);
        assert_eq!(b.remaining, 3);
        assert!(!b.is_exhausted());

        assert!(b.consume());
        assert_eq!(b.remaining, 2);
        assert!(b.consume());
        assert_eq!(b.remaining, 1);
        assert!(b.consume());
        assert_eq!(b.remaining, 0);
        assert!(b.is_exhausted());
    }

    #[test]
    fn test_step_budget_consume_after_exhaustion_returns_false() {
        let mut b = StepBudget::new(1);
        assert!(b.consume());
        assert!(b.is_exhausted());
        // saturating: stays at 0, returns false
        assert!(!b.consume());
        assert_eq!(b.remaining, 0);
    }

    #[test]
    fn test_step_budget_zero_cap_is_born_exhausted() {
        let b = StepBudget::new(0);
        assert!(b.is_exhausted());
    }

    #[test]
    fn test_step_budget_display_is_remaining_over_cap() {
        let mut b = StepBudget::new(5);
        assert_eq!(b.to_string(), "5/5");
        b.consume();
        b.consume();
        assert_eq!(b.to_string(), "3/5");
    }

    #[test]
    fn test_step_budget_serde_roundtrip() {
        let mut b = StepBudget::new(7);
        b.consume();
        b.consume();
        let json = serde_json::to_string(&b).unwrap();
        let back: StepBudget = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn test_entropy_record_serde_roundtrip() {
        let record = EntropyRecord {
            timestamp: Utc::now(),
            source: EntropySource::State,
            entropy: Entropy::new(55.0),
            compression_ratio: None,
            worker: WorkerId::new("quartz").unwrap(),
            molecule: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        let back: EntropyRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }
}
