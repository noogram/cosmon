// SPDX-License-Identifier: AGPL-3.0-only

//! cosmon-ask — conversational ingress pipeline for `cs ask`.
//!
//! Cosmon offers a single free-text verb (target: five prefix keystrokes,
//! `cs a "`) that auto-selects formula + galaxy and dispatches with
//! zero ceremony. This crate is the write-only composition layer
//! behind that verb:
//!
//! ```text
//! cs ask "<free text>"
//!   └─ A. Parse  → (AskTokens, confidence)
//!   └─ B. Resolve galaxy via cosmon-registry
//!   └─ C. Confidence gate (≥ 0.85 default) — else atomic question
//!   └─ D. Dispatch via existing verbs (cs nucleate + cs tackle + cs wait)
//! ```
//!
//! # Why rule-first
//!
//! The first cut ships a **table-driven matcher** (`RuleParser`) over
//! ~20 intent verbs. No LLM roundtrip, no tokenizer, no daemon. A
//! future follow-up molecule will add `LlmParser` (Haiku, 400 ms
//! budget) as a low-confidence fallback — that trait signature lives
//! here so the pipeline can compose the two later without a breaking
//! change.
//!
//! # Explicit non-goals
//!
//! * No runtime daemon. A cold-start over the 20 canonical patterns
//!   must finish in well under 50 ms (measured by
//!   `rule_parser_canonical_latency` in integration tests). Exceeding
//!   that budget is a signal to file a daemon-arbitration decision,
//!   **not** to add a daemon here.
//! * No mailbox or live dialogue channel — ADR-038 (whisper) and
//!   ADR-066 (wheat-paste viewport) own that plane.
//! * No background worker. `cs ask` is a one-shot composition of
//!   existing write-only verbs. Ctrl-C before dispatch leaves no
//!   state behind.
//!
//! # Invariants
//!
//! * `#![forbid(unsafe_code)]`, `#![deny(missing_docs)]`.
//! * No `unwrap`/`expect` in library code (tests excepted).
//! * The pipeline is a typestate (`AskState`): `Parsed → Resolved →
//!   Dispatched`. Low-confidence parses short-circuit into an atomic
//!   question surface before they can touch the registry.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use cosmon_core::id::FormulaId;
use cosmon_core::kind::MoleculeKind;
use serde::{Deserialize, Serialize};

pub mod atomic_question;
pub mod audit;
pub mod pipeline;
pub mod rule_parser;

pub use atomic_question::{AtomicQuestion, Choice};
pub use audit::{AuditRecord, Outcome};
pub use pipeline::{AskPipeline, AskState};
pub use rule_parser::{Rule, RuleParser, RuleTable, DEFAULT_RULES_TOML};

/// Default minimum confidence required to auto-dispatch without an
/// atomic question. Architect §4 of the panel identified misrouting
/// as the primary failure mode; the gate is the principal safeguard.
pub const DEFAULT_CONFIDENCE_FLOOR: f32 = 0.85;

/// Tokenized intent extracted from the operator's free text.
///
/// The fields here are the minimal surface the pipeline needs to go
/// from "sentence" to "molecule arguments". The parser is free to
/// populate only a subset — unfilled slots fall through to defaults
/// (e.g. `MoleculeKind::Task` with `formula = "task-work"`) or to
/// atomic questions when the confidence does not clear the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskTokens {
    /// The intent verb the parser matched on (e.g. `"fix"`, `"ship"`).
    /// Informative only — dispatch uses [`Self::kind`] and [`Self::formula`].
    pub intent_verb: String,

    /// Molecule kind the matched rule selected (e.g. `Task`, `Issue`).
    pub kind: MoleculeKind,

    /// Formula id the matched rule selected (e.g. `task-work`).
    pub formula: FormulaId,

    /// Canonical galaxy name extracted from the text, if any. The
    /// parser returns the raw token; resolution to a [`cosmon_registry::Galaxy`]
    /// happens in the next pipeline stage.
    pub galaxy_hint: Option<String>,

    /// Full free text, preserved so downstream stages can echo it as
    /// `--var topic="…"` without re-constructing the operator intent.
    pub topic: String,
}

/// Errors surfaced by the ask pipeline.
///
/// Kept deliberately coarse — the pipeline is meant to be composed
/// from a CLI handler that surfaces diagnostics to the operator;
/// internal recovery paths are driven by [`AskState`] transitions,
/// not by error variants.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    /// The rule table failed to parse. Only raised by the builder —
    /// the shipped default table is validated by a unit test.
    #[error("rule table parse error: {0}")]
    RuleTable(String),

    /// The free-text input was empty or whitespace-only. Calling code
    /// should surface the help text rather than an error dialog.
    #[error("ask input was empty")]
    EmptyInput,

    /// Backend registry error bubbled up unchanged.
    #[error(transparent)]
    Registry(#[from] cosmon_registry::RegistryError),

    /// Generic internal error — invalid id produced by a rule, etc.
    /// Should never trip in practice; exists so the trait signatures
    /// remain exhaustive without panicking.
    #[error("internal: {0}")]
    Internal(String),
}

/// A free-text parser.
///
/// The rule-first parser is the mandatory MVP; the LLM fallback
/// (future) plugs in as a second implementation that the pipeline
/// composes into a chain: "try the rule parser, then, if confidence
/// stays below the floor, try the LLM parser with a 400 ms budget."
///
/// Implementations must be **pure**: no I/O, no interior mutability,
/// no side effects. Latency budget is ≤ 5 ms for rule-based parsers,
/// ≤ 400 ms for LLM-based parsers (enforced by the caller, not the
/// trait — this is documentation, not a compile-time guard).
pub trait Parser {
    /// Parse `text` and return the extracted tokens and a confidence
    /// score in `[0.0, 1.0]`. A confidence of `0.0` means the parser
    /// had nothing to say; callers treat it as "fall through".
    ///
    /// # Errors
    ///
    /// Returns [`AskError::EmptyInput`] when `text` is empty or
    /// whitespace-only. Implementations **must not** return `Ok` with
    /// a made-up default tuple in that case — the CLI needs to
    /// distinguish "no input" from "low confidence" so it can print
    /// help rather than prompting.
    fn parse(&self, text: &str) -> Result<(AskTokens, f32), AskError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_floor_constant_is_architect_spec() {
        // architect §4 of delib-20260423-95fe fixed the gate at 0.85.
        // This test exists so any accidental reshuffle of the default
        // trips a very visible red bar.
        assert!((DEFAULT_CONFIDENCE_FLOOR - 0.85).abs() < 1e-6);
    }
}
