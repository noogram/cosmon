// SPDX-License-Identifier: AGPL-3.0-only

//! Codex session **energy**: token counters and dollar cost, read from the
//! same `rollout-*.jsonl` side-channel the realized-model capture already
//! parses ([`crate::model_realization::realized_models_from_codex_session`]).
//!
//! # Why this module exists
//!
//! Cosmon meters its claude workers (the claudion path) but codex workers
//! showed `—` for INPUT / OUTPUT / COST: the house consumes power, the meter
//! was never installed. Codex writes its own meter readings into the session
//! log as `event_msg`/`token_count` records; this module reads them.
//!
//! # Zero-I/O
//!
//! Same doctrine as the sibling parser: the function takes **already-read
//! bytes** (`&str`) and returns a typed usage struct — no filesystem, no
//! process, no network. Discovering the session file and joining it to a
//! worker is the caller's job (the CLI energy probe), in the shell.
//!
//! # Cumulative, self-totalling counters
//!
//! Each `token_count` record carries a `total_token_usage` object that is
//! **cumulative over the whole session** (codex maintains the running sum
//! itself, alongside a per-turn `last_token_usage`). The last such line in
//! the file therefore *is* the session total — the parser is a "keep the last
//! matching line" fold, with no summation and hence no accumulation bugs by
//! construction. The file is append-only JSONL, so reading a live session
//! yields energy current to the latest completed turn (same semantics the
//! claude path already accepts).
//!
//! # Pricing: data, not scattered constants
//!
//! [`codex_price_for`] is a lookup into one table of per-model rates
//! (`claudion::PricingModel` set the precedent of pricing-as-data). Codex
//! billing shape is input / cached-input / output; **reasoning tokens bill as
//! output** and are already included in `output_tokens`, so they carry no
//! rate of their own. **Honest floor:** a model absent from the table yields
//! `None` — tokens stay computable, cost displays as `—`. Never fabricate a
//! rate.
//!
//! # Cost attribution (v1)
//!
//! A whole session is attributed to the **last** realized model from the
//! `turn_context` trajectory (consistent with the trajectory-collapse
//! semantics of the realized-model display). `total_token_usage` is not split
//! per model, so a mid-session model change makes this an approximation;
//! a per-turn split (`last_token_usage` × the turn's `turn_context.model`)
//! is possible future work if the cost error ever matters.

use serde::{Deserialize, Serialize};

/// Cumulative token counters for a codex session, as reported by the last
/// `event_msg`/`token_count` record's `total_token_usage` object.
///
/// Subset relations, as codex reports them:
/// - `cached_input_tokens` ⊆ `input_tokens` (cached reads are *part of* the
///   input count, not additional to it);
/// - `reasoning_output_tokens` ⊆ `output_tokens` (reasoning bills as output);
/// - `total_tokens` = `input_tokens` + `output_tokens`.
///
/// The struct exists (rather than a bare tuple) so pricing can honor the
/// subset relations explicitly — see [`Self::cost_usd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // field names mirror the codex wire format verbatim
pub struct CodexTokenUsage {
    /// Total input tokens, **including** the cached portion.
    pub input_tokens: u64,
    /// The cached (prompt-cache read) portion of `input_tokens`.
    pub cached_input_tokens: u64,
    /// Total output tokens, **including** the reasoning portion.
    pub output_tokens: u64,
    /// The reasoning ("thinking") portion of `output_tokens`. Informational:
    /// it bills at the output rate and carries no rate of its own.
    pub reasoning_output_tokens: u64,
    /// Grand total as codex reports it (`input_tokens + output_tokens`).
    pub total_tokens: u64,
}

impl CodexTokenUsage {
    /// The non-cached portion of the input, billed at the full input rate.
    ///
    /// Saturating: a malformed log claiming more cached than total input
    /// yields `0` rather than wrapping.
    #[must_use]
    pub fn uncached_input_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_input_tokens)
    }

    /// Dollar cost of this usage at the given per-model rates.
    ///
    /// `(input − cached) × input_rate + cached × cached_rate +
    /// output × output_rate` — reasoning tokens are already inside
    /// `output_tokens` and are **not** billed again.
    #[must_use]
    pub fn cost_usd(&self, price: &CodexModelPrice) -> f64 {
        per_mtok(self.uncached_input_tokens(), price.input_per_mtok)
            + per_mtok(self.cached_input_tokens, price.cached_input_per_mtok)
            + per_mtok(self.output_tokens, price.output_per_mtok)
    }
}

/// Convert a token count to dollars given a USD-per-million-tokens rate.
fn per_mtok(tokens: u64, rate: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let count = tokens as f64;
    count * rate / 1_000_000.0
}

// ---- Parsing ---------------------------------------------------------------

/// One line of a codex `rollout-*.jsonl`, decoded by its `type` discriminator.
/// Only `event_msg` can carry token counters; every other record type falls
/// through to [`Self::Other`] so the parser survives codex schema evolution.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexEnergyLine {
    /// An event record whose payload *may* be a `token_count` event.
    EventMsg(CodexEventPayloadHolder),
    /// Any other record type — ignored for energy purposes.
    #[serde(other)]
    Other,
}

/// The `payload` wrapper of an `event_msg` record.
#[derive(Debug, Deserialize)]
struct CodexEventPayloadHolder {
    #[serde(default)]
    payload: Option<CodexEventPayload>,
}

/// An `event_msg` payload, discriminated by its own inner `type`. Only
/// `token_count` matters here; other event kinds (`task_started`,
/// `agent_message`, …) fall through to [`Self::Other`].
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexEventPayload {
    /// The token-counter event, wrapping an `info` object.
    TokenCount(CodexTokenCountEvent),
    /// Any other event kind — ignored.
    #[serde(other)]
    Other,
}

/// A `token_count` event's fields. `info` is optional because codex emits
/// counter-less `token_count` events in some paths (e.g. rate-limit-only
/// updates); such a line must not clobber a previously seen total.
#[derive(Debug, Deserialize)]
struct CodexTokenCountEvent {
    #[serde(default)]
    info: Option<CodexTokenCountInfo>,
}

/// The `info` object of a `token_count` event. Only the cumulative
/// `total_token_usage` is read; the per-turn `last_token_usage` is v2
/// material (per-turn cost split) and deliberately not modeled yet.
#[derive(Debug, Deserialize)]
struct CodexTokenCountInfo {
    #[serde(default)]
    total_token_usage: Option<CodexTokenUsage>,
}

/// Parse the **session-total** token usage from a codex `rollout-*.jsonl`
/// slice: the `total_token_usage` of the *last* `event_msg`/`token_count`
/// record that carries one.
///
/// Lenient by doctrine: lines that are not valid JSON, records of unknown
/// type, events of other kinds, and `token_count` events without counters are
/// all skipped — the parser must survive codex format drift, old and new.
///
/// Returns `None` when no line carried counters (the honest floor: a session
/// whose energy was never reported shows `—`, it is not fabricated as zero).
#[must_use]
pub fn codex_token_usage_from_session(content: &str) -> Option<CodexTokenUsage> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            match serde_json::from_str::<CodexEnergyLine>(line).ok()? {
                CodexEnergyLine::EventMsg(holder) => match holder.payload? {
                    CodexEventPayload::TokenCount(event) => event.info?.total_token_usage,
                    CodexEventPayload::Other => None,
                },
                CodexEnergyLine::Other => None,
            }
        })
        .next_back()
}

// ---- Price table -----------------------------------------------------------

/// Per-model codex billing rates in USD per million tokens.
///
/// Codex-shaped (input / cached-input / output), unlike the claude-shaped
/// four-rate `claudion::PricingModel` (which distinguishes cache creation
/// from cache read). Reasoning tokens bill at the output rate, so no
/// reasoning rate exists.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // the unit suffix is the load-bearing part of each name
pub struct CodexModelPrice {
    /// Cost per million fresh (non-cached) input tokens.
    pub input_per_mtok: f64,
    /// Cost per million cached-input (prompt-cache read) tokens.
    pub cached_input_per_mtok: f64,
    /// Cost per million output tokens (reasoning included).
    pub output_per_mtok: f64,
}

/// The codex price table — **data, not scattered constants** (claudion
/// precedent). One row per priced model id, exactly as the id appears on the
/// `turn_context` record.
///
/// Rates are USD per million tokens as published by `OpenAI` (verified
/// 2026-07-19). gpt-5 / gpt-5-codex share the gpt-5 rate card.
const CODEX_PRICE_TABLE: &[(&str, CodexModelPrice)] = &[
    (
        "gpt-5.6-sol",
        CodexModelPrice {
            input_per_mtok: 5.0,
            cached_input_per_mtok: 0.50,
            output_per_mtok: 30.0,
        },
    ),
    (
        "gpt-5.6-terra",
        CodexModelPrice {
            input_per_mtok: 2.50,
            cached_input_per_mtok: 0.25,
            output_per_mtok: 15.0,
        },
    ),
    (
        "gpt-5.6-luna",
        CodexModelPrice {
            input_per_mtok: 1.0,
            cached_input_per_mtok: 0.10,
            output_per_mtok: 6.0,
        },
    ),
    (
        "gpt-5",
        CodexModelPrice {
            input_per_mtok: 1.25,
            cached_input_per_mtok: 0.125,
            output_per_mtok: 10.0,
        },
    ),
    (
        "gpt-5-codex",
        CodexModelPrice {
            input_per_mtok: 1.25,
            cached_input_per_mtok: 0.125,
            output_per_mtok: 10.0,
        },
    ),
];

/// Look up the billing rates for a realized codex model id (exact match on
/// the id as reported by `turn_context`).
///
/// **Honest floor:** an unknown or unpriced model returns `None` — the caller
/// shows real token counts and leaves COST as `—`. A fabricated rate would be
/// worse than an absent one.
#[must_use]
pub fn codex_price_for(model: &str) -> Option<CodexModelPrice> {
    CODEX_PRICE_TABLE
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, price)| *price)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `token_count` line captured verbatim from a 2026-07 codex
    /// session (`rollout-2026-07-19T03-29-45-….jsonl`), including the
    /// `rate_limits` sibling the parser must tolerate.
    const REAL_TOKEN_COUNT_LINE: &str = r#"{"timestamp":"2026-07-19T01:36:29.746Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2217412,"cached_input_tokens":2139392,"output_tokens":8285,"reasoning_output_tokens":2400,"total_tokens":2225697},"last_token_usage":{"input_tokens":77145,"cached_input_tokens":76544,"output_tokens":234,"reasoning_output_tokens":39,"total_tokens":77379},"model_context_window":258400},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":3.0,"window_minutes":10080,"resets_at":1784961785},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"individual_limit":null,"plan_type":"pro","rate_limit_reached_type":null}}}"#;

    #[test]
    fn parses_real_token_count_line() {
        let usage = codex_token_usage_from_session(REAL_TOKEN_COUNT_LINE).unwrap();
        assert_eq!(usage.input_tokens, 2_217_412);
        assert_eq!(usage.cached_input_tokens, 2_139_392);
        assert_eq!(usage.output_tokens, 8_285);
        assert_eq!(usage.reasoning_output_tokens, 2_400);
        assert_eq!(usage.total_tokens, 2_225_697);
    }

    #[test]
    fn last_token_count_line_wins() {
        // Counters are cumulative: an earlier, smaller total must be
        // superseded by the final line, with no summation.
        let jsonl = concat!(
            r#"{"timestamp":"t","type":"session_meta","payload":{"cwd":"/x","session_id":"s"}}"#,
            "\n",
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started"}}"#,
            "\n",
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10,"reasoning_output_tokens":4,"total_tokens":110}}}}"#,
            "\n",
            r#"{"timestamp":"t","type":"turn_context","payload":{"model":"gpt-5.6-terra"}}"#,
            "\n",
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":600,"output_tokens":80,"reasoning_output_tokens":20,"total_tokens":980}}}}"#,
        );
        let usage = codex_token_usage_from_session(jsonl).unwrap();
        assert_eq!(usage.input_tokens, 900);
        assert_eq!(usage.cached_input_tokens, 600);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.total_tokens, 980);
    }

    #[test]
    fn counterless_token_count_does_not_clobber_a_seen_total() {
        // A trailing rate-limit-only token_count (no `info`, or info without
        // total_token_usage) must not erase the real total read earlier.
        let jsonl = concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":110}}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
        );
        let usage = codex_token_usage_from_session(jsonl).unwrap();
        assert_eq!(usage.input_tokens, 100);
    }

    #[test]
    fn session_without_counters_is_none_not_zero() {
        // Honest floor: no counters reported → None, never a fabricated 0.
        let jsonl = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/x"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message"}}"#,
        );
        assert!(codex_token_usage_from_session(jsonl).is_none());
    }

    #[test]
    fn lenient_on_garbage_and_unknown_types() {
        let jsonl = concat!(
            "not json at all\n",
            r#"{"type":"totally_new_record_kind","payload":{"x":1}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"brand_new_event_kind"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":7,"cached_input_tokens":0,"output_tokens":3,"reasoning_output_tokens":1,"total_tokens":10}}}}"#,
        );
        let usage = codex_token_usage_from_session(jsonl).unwrap();
        assert_eq!(usage.total_tokens, 10);
    }

    // ---- Pricing ----------------------------------------------------------

    #[test]
    fn price_table_has_the_gpt_56_tiers() {
        let sol = codex_price_for("gpt-5.6-sol").unwrap();
        assert_eq!(sol.input_per_mtok, 5.0);
        assert_eq!(sol.cached_input_per_mtok, 0.50);
        assert_eq!(sol.output_per_mtok, 30.0);

        let terra = codex_price_for("gpt-5.6-terra").unwrap();
        assert_eq!(terra.input_per_mtok, 2.50);
        assert_eq!(terra.output_per_mtok, 15.0);

        assert!(codex_price_for("gpt-5.6-luna").is_some());
        assert!(codex_price_for("gpt-5-codex").is_some());
    }

    #[test]
    fn unknown_model_has_no_price() {
        // Honest floor: tokens computable, cost None — never fabricated.
        assert!(codex_price_for("gpt-7-hypothetical").is_none());
        assert!(codex_price_for("").is_none());
    }

    #[test]
    fn cost_splits_cached_from_fresh_input() {
        // 1M fresh input + 1M cached + 100k output on terra:
        // 1.0×$2.50 + 1.0×$0.25 + 0.1×$15 = $4.25
        let usage = CodexTokenUsage {
            input_tokens: 2_000_000,
            cached_input_tokens: 1_000_000,
            output_tokens: 100_000,
            reasoning_output_tokens: 40_000,
            total_tokens: 2_100_000,
        };
        let price = codex_price_for("gpt-5.6-terra").unwrap();
        let cost = usage.cost_usd(&price);
        assert!((cost - 4.25).abs() < 1e-9);
    }

    #[test]
    fn reasoning_tokens_are_not_double_billed() {
        // Reasoning is a subset of output: two usages with the same output
        // total but different reasoning shares cost the same.
        let price = codex_price_for("gpt-5.6-sol").unwrap();
        let base = CodexTokenUsage {
            input_tokens: 1_000,
            cached_input_tokens: 0,
            output_tokens: 1_000,
            reasoning_output_tokens: 0,
            total_tokens: 2_000,
        };
        let heavy_reasoning = CodexTokenUsage {
            reasoning_output_tokens: 900,
            ..base
        };
        assert!((base.cost_usd(&price) - heavy_reasoning.cost_usd(&price)).abs() < 1e-12);
    }

    #[test]
    fn malformed_cached_exceeding_input_saturates() {
        let usage = CodexTokenUsage {
            input_tokens: 10,
            cached_input_tokens: 999,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 10,
        };
        assert_eq!(usage.uncached_input_tokens(), 0);
    }

    #[test]
    fn real_session_total_prices_end_to_end() {
        // Compose the two pure pieces the probe will chain: parse the real
        // line, price it as sol. Fresh input 78,020 × $5 + cached 2,139,392
        // × $0.50 + output 8,285 × $30 = $0.39010 + $1.069696 + $0.24855.
        let usage = codex_token_usage_from_session(REAL_TOKEN_COUNT_LINE).unwrap();
        let price = codex_price_for("gpt-5.6-sol").unwrap();
        let cost = usage.cost_usd(&price);
        let expected = 0.390_10 + 1.069_696 + 0.248_55;
        assert!((cost - expected).abs() < 1e-9);
    }
}
