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
//! [`codex_price_for`] is a compatibility lookup into the versioned manifest.
//! Codex
//! billing shape is input / cached-input / output; **reasoning tokens bill as
//! output** and are already included in `output_tokens`, so they carry no
//! rate of their own. **Honest floor:** a model absent from the table yields
//! `None` — tokens stay computable, cost displays as `—`. Never fabricate a
//! rate.
//!
//! # Model-segment attribution
//!
//! Successive cumulative readings are differenced and paired with the exact
//! active `turn_context.model`. Repeated cumulative snapshots contribute zero,
//! so allowance-only refreshes cannot duplicate usage. If any delta lacks a
//! model or counters regress, the snapshot retains its exact totals and marks
//! model coverage incomplete rather than assigning the whole session to its
//! last model.

use serde::{Deserialize, Serialize};

use crate::price_manifest::bundled_price_manifest;
use crate::usage::{ModelUsageSegment, TokenCount, UnavailableReason};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
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

/// Subscription-limit observation carried by a Codex `token_count` event.
///
/// This is usage of a `ChatGPT` plan allowance, not a monetary charge.  Keeping
/// it typed separately prevents a subscription-backed run from being rendered
/// as the numeric dollar value zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexSubscriptionUsage {
    /// Plan label reported by Codex (for example `pro`).
    pub plan_type: Option<String>,
    /// Share of the primary plan window consumed, in percent.
    pub used_percent: f64,
    /// Length of the primary limit window, when Codex reports it.
    pub window_minutes: Option<u64>,
}

/// Latest cumulative energy observation from one Codex rollout.
///
/// All values come from the same append-only `token_count` stream.  Token
/// subsets remain explicit so observers can display input / cached / output /
/// reasoning without reconstructing them from a lossy total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexEnergySnapshot {
    /// Cumulative token counters from the latest complete reading.
    pub usage: CodexTokenUsage,
    /// Model context-window capacity reported beside the counters.
    pub model_context_window: Option<u64>,
    /// `ChatGPT` subscription allowance use, when present in the rollout.
    pub subscription: Option<CodexSubscriptionUsage>,
    /// Deduplicated cumulative deltas paired with exact realized models.
    pub model_segments: Vec<ModelUsageSegment>,
    /// Whether every cumulative token delta had a realized model identity.
    pub model_segments_complete: bool,
}

impl CodexTokenUsage {
    /// The non-cached portion of the input, billed at the full input rate.
    ///
    /// Returns `None` for malformed subset counters rather than silently
    /// clamping an invalid observation into a priceable one.
    #[must_use]
    pub fn uncached_input_tokens(&self) -> Option<u64> {
        self.input_tokens.checked_sub(self.cached_input_tokens)
    }

    /// Dollar cost of this usage at the given per-model rates.
    ///
    /// `(input − cached) × input_rate + cached × cached_rate +
    /// output × output_rate` — reasoning tokens are already inside
    /// `output_tokens` and are **not** billed again.
    #[must_use]
    pub fn cost_usd(&self, price: &CodexModelPrice) -> Option<f64> {
        if self.reasoning_output_tokens > self.output_tokens {
            return None;
        }
        Some(
            per_mtok(self.uncached_input_tokens()?, price.input_per_mtok)
                + per_mtok(self.cached_input_tokens, price.cached_input_per_mtok)
                + per_mtok(self.output_tokens, price.output_per_mtok),
        )
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
    /// Model identity applying to subsequent cumulative token readings.
    TurnContext(CodexTurnContextHolder),
    /// Any other record type — ignored for energy purposes.
    #[serde(other)]
    Other,
}

/// The `payload` wrapper of a `turn_context` record.
#[derive(Debug, Deserialize)]
struct CodexTurnContextHolder {
    #[serde(default)]
    payload: Option<CodexTurnContext>,
}

/// Model-bearing portion of a `turn_context` payload.
#[derive(Debug, Deserialize)]
struct CodexTurnContext {
    #[serde(default)]
    model: Option<String>,
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
    #[serde(default)]
    rate_limits: Option<CodexRateLimits>,
}

/// The `info` object of a `token_count` event. Only the cumulative
/// `total_token_usage` is read; the per-turn `last_token_usage` is v2
/// material (per-turn cost split) and deliberately not modeled yet.
#[derive(Debug, Deserialize)]
struct CodexTokenCountInfo {
    #[serde(default)]
    total_token_usage: Option<CodexTokenUsage>,
    #[serde(default)]
    model_context_window: Option<u64>,
}

/// Subscription metadata nested beside a Codex token reading.
#[derive(Debug, Deserialize)]
struct CodexRateLimits {
    #[serde(default)]
    primary: Option<CodexRateLimitWindow>,
    #[serde(default)]
    plan_type: Option<String>,
}

/// One plan-limit window reported by Codex.
#[derive(Debug, Deserialize)]
struct CodexRateLimitWindow {
    used_percent: f64,
    #[serde(default)]
    window_minutes: Option<u64>,
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
    codex_energy_from_session(content).map(|snapshot| snapshot.usage)
}

/// Parse the latest complete Codex energy observation from rollout JSONL.
///
/// Token counters, context capacity, and subscription allowance are folded
/// independently: a trailing rate-limit-only event may refresh the allowance
/// without erasing the last real token count.  Malformed and unknown lines are
/// ignored so an append in progress cannot blank an earlier observation.
#[must_use]
pub fn codex_energy_from_session(content: &str) -> Option<CodexEnergySnapshot> {
    let mut usage = None;
    let mut model_context_window = None;
    let mut subscription = None;
    let mut current_model = None;
    let mut previous_usage = None;
    let mut model_segments = Vec::new();
    let mut model_segments_complete = true;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<CodexEnergyLine>(line) else {
            continue;
        };
        let holder = match record {
            CodexEnergyLine::EventMsg(holder) => holder,
            CodexEnergyLine::TurnContext(holder) => {
                if let Some(model) = holder.payload.and_then(|payload| payload.model) {
                    current_model = Some(model);
                }
                continue;
            }
            CodexEnergyLine::Other => continue,
        };
        let Some(CodexEventPayload::TokenCount(event)) = holder.payload else {
            continue;
        };
        if let Some(info) = event.info {
            if let Some(new_usage) = info.total_token_usage {
                match usage_delta(previous_usage, new_usage) {
                    Some(delta) if delta.total_tokens > 0 => {
                        if let Some(model) = current_model.as_deref() {
                            add_model_delta(&mut model_segments, model, delta);
                        } else {
                            model_segments_complete = false;
                        }
                    }
                    Some(_) => {}
                    None => model_segments_complete = false,
                }
                previous_usage = Some(new_usage);
                usage = Some(new_usage);
            }
            if info.model_context_window.is_some() {
                model_context_window = info.model_context_window;
            }
        }
        if let Some(limits) = event.rate_limits {
            if let Some(primary) = limits.primary {
                subscription = Some(CodexSubscriptionUsage {
                    plan_type: limits.plan_type,
                    used_percent: primary.used_percent,
                    window_minutes: primary.window_minutes,
                });
            }
        }
    }

    let usage = usage?;
    model_segments_complete &= segments_match_usage(&model_segments, usage);
    Some(CodexEnergySnapshot {
        usage,
        model_context_window,
        subscription,
        model_segments,
        model_segments_complete,
    })
}

fn usage_delta(
    previous: Option<CodexTokenUsage>,
    current: CodexTokenUsage,
) -> Option<CodexTokenUsage> {
    let previous = previous.unwrap_or_default();
    Some(CodexTokenUsage {
        input_tokens: current.input_tokens.checked_sub(previous.input_tokens)?,
        cached_input_tokens: current
            .cached_input_tokens
            .checked_sub(previous.cached_input_tokens)?,
        output_tokens: current.output_tokens.checked_sub(previous.output_tokens)?,
        reasoning_output_tokens: current
            .reasoning_output_tokens
            .checked_sub(previous.reasoning_output_tokens)?,
        total_tokens: current.total_tokens.checked_sub(previous.total_tokens)?,
    })
}

fn measured(tokens: u64) -> TokenCount {
    TokenCount::Measured { tokens }
}

fn add_model_delta(segments: &mut Vec<ModelUsageSegment>, model: &str, delta: CodexTokenUsage) {
    let index = segments
        .iter()
        .position(|item| item.model == model)
        .unwrap_or_else(|| {
            segments.push(ModelUsageSegment {
                model: model.to_owned(),
                input_tokens: measured(0),
                cached_input_tokens: measured(0),
                cache_write_tokens: measured(0),
                output_tokens: measured(0),
                reasoning_output_tokens: measured(0),
            });
            segments.len() - 1
        });
    let segment = &mut segments[index];
    add_measured(&mut segment.input_tokens, delta.input_tokens);
    add_measured(&mut segment.cached_input_tokens, delta.cached_input_tokens);
    add_measured(&mut segment.output_tokens, delta.output_tokens);
    add_measured(
        &mut segment.reasoning_output_tokens,
        delta.reasoning_output_tokens,
    );
}

fn add_measured(count: &mut TokenCount, delta: u64) {
    let TokenCount::Measured { tokens } = count else {
        *count = TokenCount::Unavailable {
            reason: UnavailableReason::MalformedSource,
        };
        return;
    };
    if let Some(total) = tokens.checked_add(delta) {
        *tokens = total;
    } else {
        *count = TokenCount::Unavailable {
            reason: UnavailableReason::MalformedSource,
        };
    }
}

fn segments_match_usage(segments: &[ModelUsageSegment], usage: CodexTokenUsage) -> bool {
    let sum = |select: fn(&ModelUsageSegment) -> TokenCount| {
        segments
            .iter()
            .map(select)
            .try_fold(0_u64, |total, count| total.checked_add(count.measured()?))
    };
    sum(|segment| segment.input_tokens) == Some(usage.input_tokens)
        && sum(|segment| segment.cached_input_tokens) == Some(usage.cached_input_tokens)
        && sum(|segment| segment.output_tokens) == Some(usage.output_tokens)
        && sum(|segment| segment.reasoning_output_tokens) == Some(usage.reasoning_output_tokens)
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

/// Look up the billing rates for a realized codex model id (exact match on
/// the id as reported by `turn_context`).
///
/// **Honest floor:** an unknown or unpriced model returns `None` — the caller
/// shows real token counts and leaves COST as `—`. A fabricated rate would be
/// worse than an absent one.
#[must_use]
pub fn codex_price_for(model: &str) -> Option<CodexModelPrice> {
    let manifest = bundled_price_manifest().ok()?;
    let rate = manifest.current_card()?.rate_for(model)?;
    Some(CodexModelPrice {
        input_per_mtok: rate.rates_usd_per_million_tokens.input,
        cached_input_per_mtok: rate.rates_usd_per_million_tokens.cached_input?,
        output_per_mtok: rate.rates_usd_per_million_tokens.output,
    })
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
        let snapshot = codex_energy_from_session(REAL_TOKEN_COUNT_LINE).unwrap();
        let usage = snapshot.usage;
        assert_eq!(usage.input_tokens, 2_217_412);
        assert_eq!(usage.cached_input_tokens, 2_139_392);
        assert_eq!(usage.output_tokens, 8_285);
        assert_eq!(usage.reasoning_output_tokens, 2_400);
        assert_eq!(usage.total_tokens, 2_225_697);
        assert_eq!(snapshot.model_context_window, Some(258_400));
        let subscription = snapshot.subscription.unwrap();
        assert_eq!(subscription.plan_type.as_deref(), Some("pro"));
        assert!((subscription.used_percent - 3.0).abs() < f64::EPSILON);
        assert_eq!(subscription.window_minutes, Some(10_080));
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
    fn rate_limit_only_tail_refreshes_subscription_without_erasing_tokens() {
        let jsonl = concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"reasoning_output_tokens":4,"total_tokens":110}},"rate_limits":{"primary":{"used_percent":2.0,"window_minutes":10080},"plan_type":"pro"}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":7.0,"window_minutes":10080},"plan_type":"pro"}}}"#,
        );
        let snapshot = codex_energy_from_session(jsonl).unwrap();
        assert_eq!(snapshot.usage.total_tokens, 110);
        assert_eq!(snapshot.subscription.unwrap().used_percent, 7.0);
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

    #[test]
    fn cumulative_snapshots_are_deduplicated_into_model_segments() {
        let jsonl = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"reasoning_output_tokens":4,"total_tokens":110}}}}"#,
            "\n",
            // An allowance refresh repeats the same cumulative snapshot.
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"reasoning_output_tokens":4,"total_tokens":110}}}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.3-codex"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"cached_input_tokens":100,"output_tokens":30,"reasoning_output_tokens":9,"total_tokens":230}}}}"#,
        );
        let snapshot = codex_energy_from_session(jsonl).unwrap();
        assert!(snapshot.model_segments_complete);
        assert_eq!(snapshot.model_segments.len(), 2);
        assert_eq!(
            snapshot.model_segments[0].input_tokens.measured(),
            Some(100)
        );
        assert_eq!(
            snapshot.model_segments[1].input_tokens.measured(),
            Some(100),
            "second segment is the cumulative delta, not the repeated total"
        );
        assert_eq!(
            snapshot.model_segments[1].cached_input_tokens.measured(),
            Some(20)
        );
        assert_eq!(
            snapshot.model_segments[1].output_tokens.measured(),
            Some(20)
        );
    }

    // ---- Pricing ----------------------------------------------------------

    #[test]
    fn price_lookup_uses_exact_sourced_manifest_ids() {
        let codex = codex_price_for("gpt-5.3-codex").unwrap();
        assert_eq!(codex.input_per_mtok, 1.75);
        assert_eq!(codex.cached_input_per_mtok, 0.175);
        assert_eq!(codex.output_per_mtok, 14.0);

        assert!(codex_price_for("gpt-5-codex").is_some());
        assert!(codex_price_for("gpt-5.6-sol").is_none());
    }

    #[test]
    fn unknown_model_has_no_price() {
        // Honest floor: tokens computable, cost None — never fabricated.
        assert!(codex_price_for("gpt-7-hypothetical").is_none());
        assert!(codex_price_for("").is_none());
    }

    #[test]
    fn cost_splits_cached_from_fresh_input() {
        // 1M fresh input + 1M cached + 100k output on GPT-5.3-Codex:
        // 1.0×$1.75 + 1.0×$0.175 + 0.1×$14 = $3.325.
        let usage = CodexTokenUsage {
            input_tokens: 2_000_000,
            cached_input_tokens: 1_000_000,
            output_tokens: 100_000,
            reasoning_output_tokens: 40_000,
            total_tokens: 2_100_000,
        };
        let price = codex_price_for("gpt-5.3-codex").unwrap();
        let cost = usage.cost_usd(&price).unwrap();
        assert!((cost - 3.325).abs() < 1e-9);
    }

    #[test]
    fn reasoning_tokens_are_not_double_billed() {
        // Reasoning is a subset of output: two usages with the same output
        // total but different reasoning shares cost the same.
        let price = codex_price_for("gpt-5.3-codex").unwrap();
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
        assert_eq!(base.cost_usd(&price), heavy_reasoning.cost_usd(&price));
    }

    #[test]
    fn malformed_cached_exceeding_input_is_unpriceable() {
        let usage = CodexTokenUsage {
            input_tokens: 10,
            cached_input_tokens: 999,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 10,
        };
        assert_eq!(usage.uncached_input_tokens(), None);
        assert_eq!(
            usage.cost_usd(&codex_price_for("gpt-5.3-codex").unwrap()),
            None
        );
    }

    #[test]
    fn real_session_total_prices_end_to_end() {
        // Compose the two pure pieces the probe will chain: parse the real
        // line, price it as GPT-5.3-Codex. The expected value is calculated
        // independently from the official three rates.
        let usage = codex_token_usage_from_session(REAL_TOKEN_COUNT_LINE).unwrap();
        let price = codex_price_for("gpt-5.3-codex").unwrap();
        let cost = usage.cost_usd(&price).unwrap();
        let expected = 0.136_535 + 0.374_393_6 + 0.115_99;
        assert!((cost - expected).abs() < 1e-9);
    }
}
