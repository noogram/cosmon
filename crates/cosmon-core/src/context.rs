// SPDX-License-Identifier: AGPL-3.0-only

//! Context manager port — hexagonal port for agent context window management.
//!
//! This module defines the [`ContextManager`] trait (a hexagonal port) that
//! abstracts how an agent's context window is measured, evaluated, and compacted.
//! Concrete adapters (e.g. Claude Code session manager) live outside this crate.
//!
//! Context windows are noisy channels with finite capacity (THESIS.md Part XIV).
//! Over a session, signal-to-noise ratio degrades as stale information accumulates.
//! The `ContextManager` port provides the interface for measuring this degradation
//! and triggering compaction — the "adiabatic compression" step in the agent's
//! thermodynamic cycle.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::energy::{CompressionRatio, Entropy, TokenCount};
use crate::id::WorkerId;

// ---------------------------------------------------------------------------
// CompactionStrategy
// ---------------------------------------------------------------------------

/// How aggressively to compact the context window.
///
/// Maps to the rate-distortion tradeoff from Shannon's theory: higher
/// compression loses more information but frees more capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Preserve all high-entropy content; only discard clearly redundant tokens.
    Conservative,
    /// Balance information retention against capacity recovery.
    Balanced,
    /// Maximize capacity recovery; accept higher information loss.
    Aggressive,
}

impl fmt::Display for CompactionStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conservative => f.write_str("conservative"),
            Self::Balanced => f.write_str("balanced"),
            Self::Aggressive => f.write_str("aggressive"),
        }
    }
}

// ---------------------------------------------------------------------------
// ContextSnapshot
// ---------------------------------------------------------------------------

/// A point-in-time measurement of an agent's context window state.
///
/// Captures the quantities needed to evaluate whether compaction is warranted:
/// current token usage, estimated capacity, and signal-to-noise ratio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    /// The worker whose context was measured.
    pub worker: WorkerId,
    /// Estimated tokens currently in the context window.
    pub used_tokens: TokenCount,
    /// Maximum context window capacity in tokens.
    pub capacity: TokenCount,
    /// Estimated entropy of the context window content (bits).
    pub entropy: Entropy,
    /// Compression ratio of context content (compressed/raw).
    pub compression_ratio: CompressionRatio,
}

impl ContextSnapshot {
    /// Fraction of capacity consumed (0.0..=1.0).
    ///
    /// Returns 0.0 if capacity is zero.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn utilization(&self) -> f64 {
        if self.capacity.get() == 0 {
            return 0.0;
        }
        self.used_tokens.get() as f64 / self.capacity.get() as f64
    }

    /// Tokens available before reaching capacity.
    #[must_use]
    pub fn remaining(&self) -> TokenCount {
        self.capacity - self.used_tokens
    }
}

impl fmt::Display for ContextSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}/{} ({:.0}%), entropy={}, compression={}",
            self.worker,
            self.used_tokens,
            self.capacity,
            self.utilization() * 100.0,
            self.entropy,
            self.compression_ratio,
        )
    }
}

// ---------------------------------------------------------------------------
// CompactionResult
// ---------------------------------------------------------------------------

/// Outcome of a context compaction operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactionResult {
    /// Tokens in the context window before compaction.
    pub tokens_before: TokenCount,
    /// Tokens in the context window after compaction.
    pub tokens_after: TokenCount,
    /// The strategy that was applied.
    pub strategy: CompactionStrategy,
}

impl CompactionResult {
    /// Tokens reclaimed by compaction.
    #[must_use]
    pub fn tokens_reclaimed(&self) -> TokenCount {
        self.tokens_before - self.tokens_after
    }

    /// Compression ratio achieved (after/before). Lower is more aggressive.
    ///
    /// Returns 1.0 if `tokens_before` is zero (nothing to compact).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn ratio(&self) -> f64 {
        if self.tokens_before.get() == 0 {
            return 1.0;
        }
        self.tokens_after.get() as f64 / self.tokens_before.get() as f64
    }
}

impl fmt::Display for CompactionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "compacted {} -> {} ({} reclaimed, {:.0}% ratio, strategy={})",
            self.tokens_before,
            self.tokens_after,
            self.tokens_reclaimed(),
            self.ratio() * 100.0,
            self.strategy,
        )
    }
}

// ---------------------------------------------------------------------------
// ContextError
// ---------------------------------------------------------------------------

/// Errors from context management operations.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// The worker's context window could not be measured.
    #[error("failed to estimate tokens for {0}: {1}")]
    EstimateFailed(WorkerId, String),

    /// Compaction failed.
    #[error("compaction failed for {0}: {1}")]
    CompactionFailed(WorkerId, String),

    /// The worker has no active context (session not running).
    #[error("no active context for worker {0}")]
    NoActiveContext(WorkerId),
}

// ---------------------------------------------------------------------------
// Trait (hexagonal port)
// ---------------------------------------------------------------------------

/// Hexagonal port for agent context window management.
///
/// Implementations handle the mechanics of measuring context window usage,
/// evaluating compaction need, and executing compaction. The domain layer
/// programs against this trait without knowing whether the backend is
/// Claude Code, a test mock, or another LLM runtime.
///
/// # Context as a noisy channel
///
/// The context window is a finite-capacity channel (THESIS.md Part XIV).
/// Over a session, signal-to-noise ratio degrades as stale tool results,
/// verbose logs, and repeated instructions accumulate. The `ContextManager`
/// provides three operations that map to the observe-decide-act cycle:
///
/// 1. [`estimate_tokens`](Self::estimate_tokens) — **observe** the current state
/// 2. [`should_compact`](Self::should_compact) — **decide** if action is needed
/// 3. [`compact`](Self::compact) — **act** to reduce entropy
pub trait ContextManager {
    /// Measure the current state of a worker's context window.
    ///
    /// Returns a snapshot containing token usage, capacity, entropy, and
    /// compression ratio. This is a read-only observation that does not
    /// modify the context.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::NoActiveContext`] if the worker has no running session.
    /// Returns [`ContextError::EstimateFailed`] if measurement fails.
    fn estimate_tokens(&self, worker: &WorkerId) -> Result<ContextSnapshot, ContextError>;

    /// Evaluate whether the worker's context should be compacted.
    ///
    /// Implementations encode the compaction policy: utilization thresholds,
    /// SNR floors, entropy ceilings, or any combination. Returns `true` when
    /// the context has degraded enough to warrant compaction.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::NoActiveContext`] if the worker has no running session.
    /// Returns [`ContextError::EstimateFailed`] if the underlying measurement fails.
    fn should_compact(&self, worker: &WorkerId) -> Result<bool, ContextError>;

    /// Compact the worker's context window using the given strategy.
    ///
    /// This is the "adiabatic compression" step: it reduces context size
    /// (decreasing entropy) while preserving high-value information. The
    /// strategy controls the rate-distortion tradeoff.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::NoActiveContext`] if the worker has no running session.
    /// Returns [`ContextError::CompactionFailed`] if compaction fails.
    fn compact(
        &self,
        worker: &WorkerId,
        strategy: CompactionStrategy,
    ) -> Result<CompactionResult, ContextError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_worker() -> WorkerId {
        WorkerId::new("topaz").unwrap()
    }

    // -- ContextSnapshot --

    #[test]
    fn test_snapshot_utilization() {
        let snap = ContextSnapshot {
            worker: test_worker(),
            used_tokens: TokenCount::new(80_000),
            capacity: TokenCount::new(200_000),
            entropy: Entropy::new(5.0),
            compression_ratio: CompressionRatio::new(0.4),
        };
        assert!((snap.utilization() - 0.4).abs() < f64::EPSILON);
        assert_eq!(snap.remaining().get(), 120_000);
    }

    #[test]
    fn test_snapshot_utilization_zero_capacity() {
        let snap = ContextSnapshot {
            worker: test_worker(),
            used_tokens: TokenCount::new(0),
            capacity: TokenCount::new(0),
            entropy: Entropy::ZERO,
            compression_ratio: CompressionRatio::new(1.0),
        };
        assert!((snap.utilization() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_snapshot_display() {
        let snap = ContextSnapshot {
            worker: test_worker(),
            used_tokens: TokenCount::new(150_000),
            capacity: TokenCount::new(200_000),
            entropy: Entropy::new(4.2),
            compression_ratio: CompressionRatio::new(0.42),
        };
        let s = snap.to_string();
        assert!(s.contains("topaz"), "should contain worker: {s}");
        assert!(s.contains("75%"), "should contain utilization: {s}");
    }

    #[test]
    fn test_snapshot_serde_roundtrip() {
        let snap = ContextSnapshot {
            worker: test_worker(),
            used_tokens: TokenCount::new(80_000),
            capacity: TokenCount::new(200_000),
            entropy: Entropy::new(5.0),
            compression_ratio: CompressionRatio::new(0.4),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: ContextSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    // -- CompactionResult --

    #[test]
    fn test_compaction_result_tokens_reclaimed() {
        let result = CompactionResult {
            tokens_before: TokenCount::new(167_000),
            tokens_after: TokenCount::new(40_000),
            strategy: CompactionStrategy::Balanced,
        };
        assert_eq!(result.tokens_reclaimed().get(), 127_000);
        assert!((result.ratio() - 40_000.0 / 167_000.0).abs() < 1e-10);
    }

    #[test]
    fn test_compaction_result_zero_before() {
        let result = CompactionResult {
            tokens_before: TokenCount::new(0),
            tokens_after: TokenCount::new(0),
            strategy: CompactionStrategy::Conservative,
        };
        assert!((result.ratio() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compaction_result_display() {
        let result = CompactionResult {
            tokens_before: TokenCount::new(100_000),
            tokens_after: TokenCount::new(30_000),
            strategy: CompactionStrategy::Aggressive,
        };
        let s = result.to_string();
        assert!(s.contains("aggressive"), "should contain strategy: {s}");
        assert!(s.contains("70000 tokens"), "should contain reclaimed: {s}");
    }

    #[test]
    fn test_compaction_result_serde_roundtrip() {
        let result = CompactionResult {
            tokens_before: TokenCount::new(100_000),
            tokens_after: TokenCount::new(50_000),
            strategy: CompactionStrategy::Balanced,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: CompactionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
    }

    // -- CompactionStrategy --

    #[test]
    fn test_compaction_strategy_display() {
        assert_eq!(CompactionStrategy::Conservative.to_string(), "conservative");
        assert_eq!(CompactionStrategy::Balanced.to_string(), "balanced");
        assert_eq!(CompactionStrategy::Aggressive.to_string(), "aggressive");
    }

    #[test]
    fn test_compaction_strategy_serde_roundtrip() {
        for strategy in [
            CompactionStrategy::Conservative,
            CompactionStrategy::Balanced,
            CompactionStrategy::Aggressive,
        ] {
            let json = serde_json::to_string(&strategy).unwrap();
            let back: CompactionStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(strategy, back);
        }
    }

    // -- ContextError --

    #[test]
    fn test_context_error_display() {
        let w = test_worker();
        let err = ContextError::EstimateFailed(w.clone(), "timeout".to_owned());
        assert!(err.to_string().contains("topaz"));
        assert!(err.to_string().contains("timeout"));

        let err = ContextError::NoActiveContext(w.clone());
        assert!(err.to_string().contains("topaz"));

        let err = ContextError::CompactionFailed(w, "session busy".to_owned());
        assert!(err.to_string().contains("session busy"));
    }

    // -- Trait object safety --

    #[test]
    fn test_trait_is_object_safe() {
        fn _accepts_dyn(_: &dyn ContextManager) {}
    }
}
