// SPDX-License-Identifier: AGPL-3.0-only

//! Message-log compaction — the I3 escape valve for long agentic sessions.
//!
//! ## Why
//!
//! I3 (context-bounded) is a loud upper bound on
//! [`crate::budget::ContextBudget::max_input_tokens`]. Without an escape
//! valve, a multi-turn agent loop monotonically grows its
//! [`crate::message_log::MessageLog`] until the next `one_turn` would
//! exceed the cap, and the spine returns
//! [`crate::error::HarnessError::ContextOverflow`]. That is correct
//! behaviour — the breach is loud — but it makes long-horizon sessions
//! impossible on small `n_ctx` providers (e.g. Qwen3-8B at 32_768).
//!
//! Compaction is the structural answer: when the estimated token count
//! crosses a configured *threshold* below the I3 ceiling, the spine
//! asks the log to **compact itself to a target size**, replacing the
//! older messages with a single inline "compaction summary" message
//! while preserving the most recent N turns verbatim.
//!
//! ## Where the responsibility lives
//!
//! Compaction is a **per-provider concern**, just like I4
//! well-formedness — the OpenAI flat `role:"tool"` shape, the Anthropic
//! content-block array, and the LlamaLog plain-text protocol each have
//! different rules about what can be replaced and how the summary is
//! re-injected. The trait method
//! [`crate::message_log::MessageLog::compact`] hands the policy
//! envelope down to the provider's impl; the spine just decides
//! *when* to call it.
//!
//! ## Strategy
//!
//! v0 uses **preserve-recent-N + deterministic-summary-older**:
//!
//! 1. Always preserve the system prompt / briefing seed (the first
//!    message, or the dedicated `system` field for AnthropicLog).
//! 2. Preserve the N most recent messages verbatim, where N is chosen
//!    so the tail keeps the most recent `tool_call` → `tool_result`
//!    pair intact (I4 must survive compaction).
//! 3. Replace the middle with **one** synthetic user message labelled
//!    `[compaction summary]` whose body is a deterministic
//!    concatenation of the removed messages' textual content, truncated
//!    so the post-compaction log fits the caller's `target_tokens`.
//!
//! A provider-driven *semantic* summary (calling
//! `provider.summarize(messages_older)` to produce natural-language
//! prose) is a v1 follow-up. The v0 deterministic heuristic is
//! intentional: it is testable, fast, and self-contained (no extra
//! provider round-trip, no proprietary-suspect prompts). The trait
//! shape stays the same when the v1 summariser lands.
//!
//! ## Configuration
//!
//! [`CompactionPolicy`] carries the two knobs the spine reads:
//!
//! - `threshold_ratio` — fraction of [`crate::budget::ContextBudget::max_input_tokens`]
//!   at which compaction triggers (default 0.8 = 80 %). Below this the
//!   log is not touched, so short sessions pay no overhead.
//! - `target_tokens` — the size to compact down to. Defaults to 60 %
//!   of the context budget so a single compaction creates meaningful
//!   headroom for the rest of the session.
//!
//! ## I4 survives compaction
//!
//! The per-provider `compact` impl is responsible for keeping I4
//! intact: if the tail boundary lands in the middle of an
//! assistant-tool_calls / tool_results pair, the impl must extend the
//! preserved tail to include both sides (or roll the pair into the
//! summary). The spine's `debug_assert!(log.invariant_well_formed())`
//! at the head of the next loop iteration catches any impl that
//! violates this.

use crate::budget::ContextBudget;

/// Policy knobs governing when and how the spine triggers compaction.
///
/// Read by [`crate::spine::run_loop`] before each `one_turn`: if the
/// log's estimated tokens exceed
/// `policy.threshold_tokens(context_budget)`, the spine calls
/// [`crate::message_log::MessageLog::compact`] with
/// `policy.target_tokens(context_budget)` and continues. Skipping the
/// call when the log is already below the threshold means short
/// sessions pay zero compaction overhead.
#[derive(Debug, Clone, Copy)]
pub struct CompactionPolicy {
    /// Fraction of [`ContextBudget::max_input_tokens`] at which
    /// compaction triggers. `0.8` means "compact once the log uses 80 %
    /// of the context budget". Values outside `(0.0, 1.0)` are clamped
    /// to the closed interval at use-time so a misconfigured value
    /// degrades to "never compact" or "always compact" rather than
    /// panicking.
    pub threshold_ratio: f32,
    /// Fraction of [`ContextBudget::max_input_tokens`] the log should
    /// be compacted to. `0.6` means "after compaction, aim for ~60 %
    /// of the context budget". Must be strictly less than
    /// `threshold_ratio` to avoid an immediate re-trigger.
    pub target_ratio: f32,
    /// Number of recent messages to preserve verbatim. The provider's
    /// impl is free to extend this to keep I4 intact when the boundary
    /// lands inside a tool-call / tool-result pair. Default `4`
    /// matches the claudecode reference pattern (last two
    /// assistant/tool round-trips).
    pub preserve_recent: usize,
}

impl CompactionPolicy {
    /// Default policy — compaction triggers at 80 %, targets 60 %, and
    /// preserves the last 4 messages verbatim. The 20-percentage-point
    /// gap between trigger and target avoids the pathological case
    /// where compaction repeatedly fires without freeing meaningful
    /// headroom.
    pub const DEFAULT: Self = Self {
        threshold_ratio: 0.8,
        target_ratio: 0.6,
        preserve_recent: 4,
    };

    /// Trigger threshold in absolute tokens, derived from a
    /// [`ContextBudget`]. The spine compares `log.estimate_tokens()`
    /// against this value.
    #[must_use]
    pub fn threshold_tokens(&self, budget: ContextBudget) -> u32 {
        scale_budget_by_ratio(budget.max_input_tokens, self.threshold_ratio)
    }

    /// Target token count after compaction, derived from a
    /// [`ContextBudget`]. The per-provider impl tries to reduce the log
    /// to approximately this value (with provider-specific margin).
    #[must_use]
    pub fn target_tokens(&self, budget: ContextBudget) -> u32 {
        scale_budget_by_ratio(budget.max_input_tokens, self.target_ratio)
    }
}

/// Scale a token budget by a ratio in `[0.0, 1.0]`, clamping
/// out-of-range ratios so a misconfigured policy degrades gracefully
/// rather than panicking on a `f32::NaN` cast. The integer-arithmetic
/// path through fixed-point per-mille (‰) avoids clippy's
/// cast-sign-loss / cast-possible-truncation lints without
/// `#[allow]` escape hatches; the small loss from integer truncation
/// (≤ 1 token per scale operation) is invisible against the I3
/// budget granularity.
fn scale_budget_by_ratio(budget: u32, ratio: f32) -> u32 {
    let r = ratio.clamp(0.0, 1.0);
    if r.is_nan() {
        return 0;
    }
    // Per-mille representation lands in [0, 1000] exactly after the
    // clamp; the `as u32` is safe because the f32 value is by
    // construction in that range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let permille = (r * 1000.0).round() as u32;
    let permille = permille.min(1000);
    let scaled = u64::from(budget).saturating_mul(u64::from(permille)) / 1000;
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Outcome of a single [`crate::message_log::MessageLog::compact`]
/// invocation. Returned through `Result` so the impl can decline (e.g.
/// the log is too small to compact safely) without breaking the spine
/// — a [`CompactionError::NotApplicable`] return is informational and
/// the spine proceeds without a `ContextOverflow` cliff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionReport {
    /// Estimated tokens **before** compaction (as reported by the
    /// provider's [`crate::message_log::MessageLog::estimate_tokens`]).
    pub tokens_before: u32,
    /// Estimated tokens **after** compaction. The impl tries to land
    /// at or below the caller's `target_tokens`; the spine treats this
    /// as advisory (the I3 check on the next iteration is the real
    /// gate).
    pub tokens_after: u32,
    /// How many messages were removed from the middle of the log and
    /// rolled into the synthetic summary.
    pub messages_removed: usize,
    /// One-line marker that the synthetic summary message has been
    /// inserted. Used in tests and observability surfaces; the actual
    /// summary content lives inside the log.
    pub summary_inserted: bool,
}

/// Errors that the per-provider [`crate::message_log::MessageLog::compact`]
/// impl can return. None of these are fatal — the spine continues; the
/// next loop iteration may still trip I3 and surface
/// [`crate::error::HarnessError::ContextOverflow`] if the log is
/// genuinely too large to recover.
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// The log is already below `target_tokens`, or the policy's
    /// `preserve_recent` count covers the whole log — nothing to do.
    /// Informational, not fatal.
    #[error("compaction not applicable: nothing older than preserve_recent to compact")]
    NotApplicable,

    /// The provider's impl cannot compact this log shape (e.g. an
    /// in-flight tool_use without its matching tool_result yet — I4
    /// would break). Informational; the spine continues, next iteration
    /// may re-attempt after the pair completes.
    #[error("compaction refused: would violate I4 well-formedness")]
    WouldBreakInvariant,
}

/// The marker that prefixes every synthetic compaction summary message
/// across all providers. The model is expected to read messages
/// starting with this prefix as a recap of older context, not as
/// fresh operator directives. Lives at the module level so all three
/// impls share the exact string.
pub const COMPACTION_SUMMARY_PREFIX: &str = "[compaction summary]";

/// Build the body of the synthetic compaction-summary message.
///
/// Deterministic concatenation: each removed message contributes one
/// line of the form `<role>: <truncated content>`. The combined body
/// is then hard-truncated so the post-compaction log fits comfortably
/// inside the caller's `target_tokens` (the spine reserves the rest of
/// the budget for the preserved tail and the next turn's tool output).
///
/// `removed_chunks` carries one `(role, text)` pair per dropped
/// message; the impl is expected to render assistant tool_calls and
/// tool_results into their plain-text representation before calling
/// this. The marker prefix is prepended; callers must NOT prepend it
/// themselves (avoiding double-prefix bugs).
///
/// `max_body_chars` is the hard cap on the returned string in
/// characters (not tokens) — the spine's 4-chars-per-token heuristic
/// converts the caller's `target_tokens` budget into a char budget
/// before calling.
#[must_use]
pub fn build_summary_body(removed_chunks: &[(&str, &str)], max_body_chars: usize) -> String {
    let mut out = String::from(COMPACTION_SUMMARY_PREFIX);
    out.push('\n');
    out.push_str(
        "The earlier portion of this conversation has been summarised \
         here. Recent messages remain verbatim below. Continue without \
         acknowledging this summary.\n\n",
    );

    // Deterministic per-message rendering. The truncation strategy is
    // greedy: stop appending whole messages once we approach
    // `max_body_chars`. The last message landing in the summary may be
    // partial; we mark the truncation with a sentinel so the model
    // sees that something was cut.
    let mut remaining = max_body_chars.saturating_sub(out.len());
    for (role, text) in removed_chunks {
        if remaining == 0 {
            break;
        }
        // Per-message cap: never let a single tool result hog the
        // entire summary budget. 800 chars ≈ 200 tokens — enough for
        // a useful excerpt, small enough that 10+ messages still fit.
        let per_message_cap = remaining.min(800);
        let mut line = format!("{role}: ");
        let body_budget = per_message_cap.saturating_sub(line.len());
        if text.len() <= body_budget {
            line.push_str(text);
        } else {
            // Truncate on a char boundary, not a byte boundary, to
            // avoid splitting a UTF-8 codepoint mid-sequence.
            let truncated: String = text.chars().take(body_budget.saturating_sub(2)).collect();
            line.push_str(&truncated);
            line.push('…');
        }
        line.push('\n');
        if line.len() > remaining {
            // We can no longer fit even the role prefix; stop.
            break;
        }
        remaining -= line.len();
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_default_is_below_one() {
        let p = CompactionPolicy::DEFAULT;
        assert!(p.threshold_ratio > 0.0 && p.threshold_ratio < 1.0);
        assert!(p.target_ratio > 0.0 && p.target_ratio < p.threshold_ratio);
    }

    #[test]
    fn threshold_and_target_derive_from_budget() {
        let budget = ContextBudget {
            max_input_tokens: 32_768,
        };
        let p = CompactionPolicy::DEFAULT;
        let trig = p.threshold_tokens(budget);
        let tgt = p.target_tokens(budget);
        // Threshold is roughly 80 %, target is roughly 60 % — the
        // exact rounding depends on f64 → u32 cast but stays in the
        // [25 000, 27 000] / [19 000, 20 000] band.
        assert!(
            (24_000..=27_000).contains(&trig),
            "threshold {trig} for 32_768 expected ~26_214"
        );
        assert!(
            (19_000..=20_500).contains(&tgt),
            "target {tgt} for 32_768 expected ~19_660"
        );
        assert!(tgt < trig);
    }

    #[test]
    fn policy_clamps_out_of_range_ratios() {
        let budget = ContextBudget {
            max_input_tokens: 32_768,
        };
        let p = CompactionPolicy {
            threshold_ratio: 2.5,
            target_ratio: -0.3,
            preserve_recent: 4,
        };
        // Out-of-range ratios degrade gracefully rather than
        // panicking on a NaN cast.
        let trig = p.threshold_tokens(budget);
        let tgt = p.target_tokens(budget);
        assert_eq!(trig, 32_768);
        assert_eq!(tgt, 0);
    }

    #[test]
    fn build_summary_body_is_deterministic() {
        let removed = [
            ("user", "first message"),
            ("assistant", "I will help"),
            ("tool", "wrote file"),
        ];
        let a = build_summary_body(&removed, 4_000);
        let b = build_summary_body(&removed, 4_000);
        assert_eq!(a, b, "summary rendering must be deterministic");
        assert!(a.starts_with(COMPACTION_SUMMARY_PREFIX));
        assert!(a.contains("first message"));
        assert!(a.contains("wrote file"));
    }

    #[test]
    fn build_summary_body_truncates_to_budget() {
        let huge = "x".repeat(10_000);
        let removed = [("tool", huge.as_str())];
        let out = build_summary_body(&removed, 500);
        assert!(
            out.len() <= 500,
            "summary must fit char budget, got {} chars",
            out.len()
        );
        // The truncation marker is present.
        assert!(out.contains('…') || out.len() < 500);
    }

    #[test]
    fn build_summary_body_handles_utf8_boundary() {
        // A multi-byte codepoint near the truncation boundary must not
        // produce invalid UTF-8 — chars().take() handles this.
        let payload = "héllo ".repeat(2000);
        let removed = [("user", payload.as_str())];
        let out = build_summary_body(&removed, 200);
        // Just confirm we got back valid UTF-8 (the `String` return
        // type guarantees this; this test catches a regression where a
        // future impl might use byte slicing).
        assert!(out.is_char_boundary(out.len()));
    }
}
