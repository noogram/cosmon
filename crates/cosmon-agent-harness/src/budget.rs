// SPDX-License-Identifier: AGPL-3.0-only

//! Budget primitives for the agent-loop spine — the numeric
//! upper-bounds that enforce loop invariants I1–I3.
//!
//! Each budget carries a `Self::DEFAULT` constant matching the
//! current toy-loop semantics from `cosmon-provider::openai::run_agent_loop`
//! (8-turn cap, 4 096-token context ceiling), so the
//! cosmon-agent-harness PR-A refactor is byte-for-byte
//! behaviour-preserving against the pre-extraction baseline.
//!
//! The `max_tool_calls` field on [`ToolBudget`] is named in this v0
//! crate but the spine does **not** enforce it (the toy `write_file`
//! tool cannot cascade beyond the turn cap). Enforcement lands in
//! PR-A.5 with the typestate promotion; the field is named today so
//! the budget API is stable from day one.
//!
//! See [`crate::invariants`] for the formal statements of I1, I2, I3.

/// Loud upper-bound on the number of inference turns a single
/// [`crate::spine::run_loop`] invocation will make.
///
/// The default (8) is generous for the toy `write_file` smoke test
/// and small enough that a runaway tool-call cascade cannot silently
/// burn the operator's API budget. ADR-102 §D-6 names a v1 target of
/// ~30 for real long-horizon work; the bump is a separate decision.
#[derive(Debug, Clone, Copy)]
pub struct TurnBudget {
    /// Hard ceiling on the loop's `for turn in 0..max_turns` iteration.
    pub max_turns: u32,
}

impl TurnBudget {
    /// Default budget — 30 turns, the v1 target named in ADR-102 §D-6
    /// and validated by the harness-v0 smoke chronicle. The
    /// earlier 8-turn anchor was a byte-for-byte preservation of the
    /// pre-extraction `openai::run_agent_loop` toy-cap; the smoke
    /// established that even a 7-callsite mechanical rename + 4 gate
    /// runs cannot fit in 8 turns (each `cargo check`/`test`/`clippy`
    /// burns one turn just for `exec_command`).
    pub const DEFAULT: Self = Self { max_turns: 30 };
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Loud upper-bound on the cumulative number of tool calls a single
/// [`crate::spine::run_loop`] invocation will dispatch.
///
/// **Not enforced by the v0 spine.** Named here so PR-A.5's typestate
/// promotion can bind `max_tool_calls` to an `I2` trait bound without
/// changing the public budget API. The default (64) leaves headroom
/// for the future `exec_command` + `edit_file` + `read_file` cascade.
#[derive(Debug, Clone, Copy)]
pub struct ToolBudget {
    /// Hard ceiling on `used_tools` across the entire loop.
    pub max_tool_calls: u32,
}

impl ToolBudget {
    /// Default budget. Conservative enough that a hostile model
    /// emitting tool_calls in a tight feedback loop cannot exhaust
    /// the operator's API budget before the turn cap kicks in.
    pub const DEFAULT: Self = Self { max_tool_calls: 64 };
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Loud upper-bound on the estimated input-token count, evaluated
/// before each provider round-trip.
///
/// The pre-flight check at the head of [`crate::spine::run_loop`]
/// uses a 4-chars-per-token heuristic (the same one the pre-extraction
/// `openai::MAX_INPUT_TOKENS` enforced) to refuse a briefing whose
/// estimated input exceeds [`Self::max_input_tokens`]. A future
/// PR will replace the heuristic with the model's real `max_input`;
/// the budget shape stays the same.
///
/// Loud failure on breach: [`crate::error::HarnessError::ContextOverflow`]
/// — SF-5 in the ADR-100 silent-failure taxonomy.
#[derive(Debug, Clone, Copy)]
pub struct ContextBudget {
    /// Hard ceiling on estimated input tokens, evaluated pre-dispatch.
    pub max_input_tokens: u32,
}

impl ContextBudget {
    /// Default budget — 32_768 input tokens, raised from the
    /// pre-extraction `openai::MAX_INPUT_TOKENS = 4_096` after the
    /// harness-v0 smoke chronicle
    /// established that cosmon's own `CLAUDE.md` (~35 kB, ~9 k
    /// estimated tokens) already overflows 4 k once `bootstrap`
    /// prepends it to the briefing. The 32 k value gives ~4× headroom
    /// over the current CLAUDE.md and matches the smallest
    /// chat-completions context window across the providers the
    /// adapter targets (`gpt-4o-mini` 128 k, Grok 32 k, Moonshot 32 k).
    /// SF-5 stays loud — it just fires at the *real* ceiling rather
    /// than the artificial toy-loop cap.
    pub const DEFAULT: Self = Self {
        max_input_tokens: 32_768,
    };
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}
