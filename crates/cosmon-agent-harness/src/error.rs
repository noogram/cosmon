// SPDX-License-Identifier: AGPL-3.0-only

//! Harness-level error type — the loud failure surface
//! [`crate::spine::run_loop`] returns to its caller.
//!
//! ADR-102 §D-3 names the spine's failure perimeter as a superset of
//! the per-provider error: provider-level transport/decode/rate-limit
//! failures bubble through as [`HarnessError::Provider`], harness-level
//! invariant breaches (I3 context overflow, I1 turn exhaustion) live
//! in dedicated variants, and tool-dispatch failures land in
//! [`HarnessError::Tool`].
//!
//! ## Generic over `Provider::Error`
//!
//! The provider error type is left abstract via the `E` type
//! parameter. The OpenAI wrapper crate
//! (`cosmon-provider::openai::run_agent_loop`) instantiates
//! `HarnessError<OpenAiError>` and pattern-matches on the variants to
//! emit the [ADR-100](../../docs/adr/100-direct-api-adapter-substrate.md)
//! `AdapterLivenessProbed` Stuck event with the matching SF class
//! (SF-1 / SF-2 / SF-3 / SF-5). Keeping `E` generic at the spine
//! avoids the lossy `Box<dyn Error>` round-trip the v0 panel rejected
//! (tolnay §Q1 — *"every `pub` you ship today is a contract you
//! maintain for years"*).

use crate::tool::ToolError;

/// Errors returned by [`crate::spine::run_loop`].
///
/// `E` is the provider's error type, surfaced verbatim through
/// [`Self::Provider`] so the calling Adapter can map each variant
/// back onto its named SF class (ADR-100 §5) without a downcast.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum HarnessError<E: std::error::Error + Send + Sync + 'static> {
    /// The provider's `one_turn` impl returned an error (transport,
    /// decode, rate-limit, etc). The caller is expected to map the
    /// inner `E` onto its SF-1 / SF-2 / SF-3 telemetry path.
    #[error("provider error: {0}")]
    Provider(#[source] E),

    /// A whitelisted tool dispatch failed (bad arguments, refused
    /// path, IO error). The inner [`ToolError`] names which subclass.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// I3 — estimated input tokens exceeded the configured
    /// [`crate::budget::ContextBudget::max_input_tokens`]. The
    /// pre-flight check at the head of `run_loop` caught the breach
    /// before the first provider round-trip. Maps to SF-5 in the
    /// ADR-100 taxonomy.
    #[error("context overflow: estimated {estimated_tokens} > {limit}")]
    ContextOverflow {
        /// Estimated input-token count from the 4-chars-per-token
        /// heuristic.
        estimated_tokens: u32,
        /// Configured ceiling that was breached.
        limit: u32,
    },

    /// I1 — the loop ran [`crate::budget::TurnBudget::max_turns`]
    /// iterations without the provider returning [`crate::spine::Turn::Stop`].
    /// Loud terminator, not a silent retry — the caller sees the
    /// breach with the budget value attached.
    #[error("turn budget exhausted: {limit} turns")]
    TurnBudgetExhausted {
        /// The configured turn ceiling that was hit.
        limit: u32,
    },

    /// I2 — cumulative dispatched tool calls exceeded the configured
    /// [`crate::budget::ToolBudget::max_tool_calls`]. Without this
    /// guard a single assistant turn emitting `tool_calls` in a tight
    /// loop could burn `K_turns × J_per_turn` dispatches (the worst-
    /// case 30×64=1920) before
    /// the turn budget kicks in. The Lyapunov-variant
    /// `V = (K − turn, J − used_tools)` from the spine's termination
    /// proof requires both axes to be enforced, not just the turn axis.
    /// Loud terminator, not silent retry — the caller sees the breach
    /// with the budget value attached.
    #[error("tool budget exhausted: {limit} tool calls")]
    ToolBudgetExhausted {
        /// The configured tool-call ceiling that was hit.
        limit: u32,
    },

    /// C4 mechanism 5 — the tool-call log went cyclic: the same
    /// `(tool, args)` block of length `period` repeated `repeats` times in
    /// a row. A weak oracle stuck in a loop (`read A`, `read B`, `read A`,
    /// …) burns turn/tool budget without progress; the cycle detector
    /// (`cosmon_core::oracle_boundary::detect_tool_call_cycle`) catches it
    /// before the budget drains. Loud terminator — the caller sees the
    /// period/repeats so it can classify the stall.
    #[error("tool-call cycle detected: period {period} repeated {repeats}×")]
    ToolCallCycle {
        /// Length of the repeating block (1 = same call, 2 = A/B alternation).
        period: usize,
        /// How many consecutive copies of the block were observed.
        repeats: usize,
    },
}
