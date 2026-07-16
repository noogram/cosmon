// SPDX-License-Identifier: AGPL-3.0-only

//! [`PilotError`] — the REPL's failure surface.
//!
//! The driver collapses two unrelated failure sources into one
//! `#[non_exhaustive]` enum: host I/O (reading the operator's stdin,
//! writing the rendered scrollback, appending the transcript) and a
//! fatal harness step. The harness step error
//! ([`cosmon_agent_harness::HarnessError`]) is **generic over the
//! provider's error type**; carrying it verbatim would make `PilotError`
//! generic too and leak the provider type into every caller signature.
//! We keep `PilotError` provider-agnostic by capturing the harness
//! error's `Display` string at the boundary — the REPL never needs to
//! match on the harness error's structure, only to report it and stop.

/// Fatal errors that end a [`crate::repl::run_repl`] loop.
///
/// "Fatal" is the operative word: most things that go wrong inside a turn
/// are **not** errors here. A tool that fails (bad path, missing molecule)
/// is folded back to the model as a tool result by the harness; a spent
/// per-turn budget yields control to the operator. Only a host-I/O failure
/// or a harness step that cannot continue (context overflow, tool-budget
/// exhaustion, transport death) surfaces as a `PilotError` and stops the
/// REPL.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PilotError {
    /// A host-I/O step failed — reading the operator line, writing the
    /// rendered output, or appending the on-disk transcript. Carries the
    /// underlying [`std::io::Error`].
    #[error("pilot host i/o: {0}")]
    Io(#[from] std::io::Error),

    /// A [`cosmon_agent_harness::InteractiveSession`] step (or its
    /// construction) failed fatally. The message is the harness error's
    /// `Display` text, captured at the boundary so this enum stays
    /// provider-agnostic.
    #[error("harness step failed: {0}")]
    Harness(String),
}
