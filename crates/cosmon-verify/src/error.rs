// SPDX-License-Identifier: AGPL-3.0-only

//! Violation reporting and parse errors for the trace validator.

use cosmon_core::event_v2::Seq;
use serde::Serialize;
use thiserror::Error;

/// An invariant violation discovered while replaying a trace.
///
/// Carries enough context for an operator to find the offending line:
/// the sequence number (if known), the 1-indexed line number in the input,
/// a machine-readable invariant id, and a human-readable message.
///
/// Serializes to JSON so `cs verify-trace --json` can emit it directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// Machine-readable invariant identifier (e.g. `"molecule_exists"`).
    pub invariant: &'static str,

    /// Sequence number of the offending event, if the envelope carried one.
    pub seq: Option<u64>,

    /// 1-indexed line number in the input (blank lines still count).
    pub line: usize,

    /// Human-readable description of why the invariant failed.
    pub message: String,
}

impl Violation {
    /// Convenience constructor.
    #[must_use]
    pub fn new(
        invariant: &'static str,
        seq: Option<Seq>,
        line: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            seq: seq.map(|s| s.0),
            line,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.seq {
            Some(seq) => write!(
                f,
                "line {} seq {} [{}]: {}",
                self.line, seq, self.invariant, self.message
            ),
            None => write!(
                f,
                "line {} [{}]: {}",
                self.line, self.invariant, self.message
            ),
        }
    }
}

/// Parse-time errors raised before replay begins.
///
/// The replay itself never fails with a parse error — malformed lines short-
/// circuit `validate_*` with this variant so the caller can distinguish
/// "trace is ill-formed" from "trace violates an invariant".
#[derive(Debug, Error)]
pub enum ValidationError {
    /// A line could not be parsed as either an `EventV2` envelope or a legacy
    /// `events.jsonl` shape.
    #[error("line {line}: failed to parse event envelope: {source}")]
    Parse {
        /// 1-indexed line number.
        line: usize,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// I/O error while reading the trace from disk.
    #[error("failed to read trace from disk: {0}")]
    Io(#[from] std::io::Error),
}
