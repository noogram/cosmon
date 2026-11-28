// SPDX-License-Identifier: AGPL-3.0-only

//! Pure projection of the `OpenCode` 2 session-message usage fields.

use cosmon_core::usage::UsageRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider and terminal refusal attributed to an `OpenCode` worker's session.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpencodeStatus {
    /// Latest observed provider and model, in `OpenCode`'s `provider/model` form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider refusal recorded on the last idle message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<OpencodeRefusal>,
}

impl OpencodeStatus {
    /// Format the worker's canonical counters and native model or refusal.
    #[must_use]
    pub fn format_usage(
        &self,
        usage: Option<&UsageRecord>,
        format: impl Fn(u64) -> String,
        separator: &str,
    ) -> String {
        use std::fmt::Write as _;

        let counters = crate::usage_projection::format_usage_token_columns(usage, format);
        let mut text = format!(
            "{counters}{separator}{}",
            crate::usage_projection::format_api_equivalent(usage.map_or(&[], std::slice::from_ref))
        );
        if let Some(model) = &self.model {
            let _ = write!(text, "{separator}{model}");
        }
        if let Some(refusal) = self.refusal {
            let _ = write!(text, "{separator}{refusal}");
        }
        text
    }
}

/// An explicit provider refusal retained by `OpenCode`, without its raw payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpencodeRefusal {
    /// Whether the provider throttled requests or refused its usage allowance.
    pub kind: OpencodeRefusalKind,
    /// Retry delay in milliseconds, when reported with the error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// Refusal classes that `OpenCode` distinguishes from other provider failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpencodeRefusalKind {
    /// The provider throttled the request.
    RateLimited,
    /// The provider refused an exhausted quota, budget, or plan allowance.
    QuotaExhausted,
}

impl std::fmt::Display for OpencodeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.kind {
            OpencodeRefusalKind::RateLimited => "rate-limited",
            OpencodeRefusalKind::QuotaExhausted => "quota-exhausted",
        })?;
        if let Some(ms) = self.retry_after_ms {
            write!(f, " (retry after {ms}ms)")?;
        }
        Ok(())
    }
}

/// Cumulative measured fields from recorded assistant steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct OpencodeTotals {
    /// Inclusive input, including cache reads and writes.
    pub input: Option<u64>,
    /// Cached input reads.
    pub cache_read: Option<u64>,
    /// Cached input writes.
    pub cache_write: Option<u64>,
    /// Inclusive output, including reasoning.
    pub output: Option<u64>,
    /// Reasoning subset of output.
    pub reasoning: Option<u64>,
    /// `OpenCode`'s USD estimate for the recorded steps.
    pub cost_usd: Option<f64>,
}

fn add_count(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a?.checked_add(b?)
}

impl OpencodeTotals {
    fn from_step(step: &Value) -> Self {
        let tokens = &step["tokens"];
        let cache_read = tokens["cache"]["read"].as_u64();
        let cache_write = tokens["cache"]["write"].as_u64();
        let reasoning = tokens["reasoning"].as_u64();
        Self {
            input: add_count(tokens["input"].as_u64(), add_count(cache_read, cache_write)),
            cache_read,
            cache_write,
            output: add_count(tokens["output"].as_u64(), reasoning),
            reasoning,
            cost_usd: step["cost"].as_f64().filter(|cost| *cost >= 0.0),
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            input: add_count(self.input, other.input),
            cache_read: add_count(self.cache_read, other.cache_read),
            cache_write: add_count(self.cache_write, other.cache_write),
            output: add_count(self.output, other.output),
            reasoning: add_count(self.reasoning, other.reasoning),
            cost_usd: self
                .cost_usd
                .zip(other.cost_usd)
                .map(|(a, b)| a + b)
                .filter(|cost| cost.is_finite()),
        }
    }
}

/// A session's observed usage and operator-facing status.
#[derive(Debug, Default, PartialEq)]
pub struct OpencodeReading {
    /// Cumulative usage. A session without a recorded step has no total.
    pub totals: Option<OpencodeTotals>,
    /// Provider attribution when every recorded step identifies the same provider.
    pub provider: Option<String>,
    /// The latest known model and any terminal provider refusal.
    pub status: OpencodeStatus,
}

#[derive(Deserialize)]
struct ModelRef<'a> {
    #[serde(rename = "providerID")]
    provider: &'a str,
    id: &'a str,
}

impl ModelRef<'_> {
    fn valid(&self) -> bool {
        [self.provider, self.id].iter().all(|piece| {
            !piece.is_empty() && !piece.chars().any(|c| c.is_whitespace() || c.is_control())
        })
    }
}

fn refusal(error: &Value) -> Option<OpencodeRefusal> {
    let kind = match error["type"].as_str()? {
        "provider.rate-limit" => OpencodeRefusalKind::RateLimited,
        "provider.quota"
        | "provider.chatgpt-limit"
        | "provider.free-limit"
        | "provider.budget-limit"
        | "provider.model-budget-limit"
        | "provider.go-limit" => OpencodeRefusalKind::QuotaExhausted,
        _ => return None,
    };
    Some(OpencodeRefusal {
        kind,
        retry_after_ms: error["retryAfterMs"].as_u64(),
    })
}

/// Parse projected session-message rows in ascending sequence order.
///
/// Each row has its SQLite message type and a JSON object with only model,
/// finish, token, cost and typed error fields. An unfinished or failed step
/// without usage is ignored. A completed step with missing counters leaves
/// those categories unknown in the cumulative reading.
#[must_use]
pub fn parse_opencode_session(
    session_model: Option<&str>,
    rows: impl IntoIterator<Item = (String, String)>,
) -> OpencodeReading {
    let mut reading = OpencodeReading::default();
    if let Some(model) = session_model
        .and_then(|json| serde_json::from_str::<ModelRef<'_>>(json).ok())
        .filter(ModelRef::valid)
    {
        reading.status.model = Some(format!("{}/{}", model.provider, model.id));
    }
    for (kind, json) in rows {
        reading.status.refusal = None;
        let Ok(row) = serde_json::from_str::<Value>(&json) else {
            continue;
        };
        if kind == "idle" {
            reading.status.refusal = refusal(&row["error"]);
        }
        if kind != "assistant" {
            continue;
        }
        let model = ModelRef::deserialize(&row["model"])
            .ok()
            .filter(ModelRef::valid);
        if let Some(model) = &model {
            reading.status.model = Some(format!("{}/{}", model.provider, model.id));
        }
        if row["tokens"].is_null()
            && row["finish"]
                .as_str()
                .is_none_or(|finish| finish == "error")
        {
            continue;
        }
        let provider = model.map(|model| model.provider.to_owned());
        let step = OpencodeTotals::from_step(&row);
        if let Some(totals) = reading.totals {
            reading.totals = Some(totals.add(step));
            if reading.provider != provider {
                reading.provider = None;
            }
        } else {
            reading.totals = Some(step);
            reading.provider = provider;
        }
    }
    reading
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ASSISTANT: &str = include_str!("../../cosmon-cli/tests/fixtures/opencode/assistant.json");
    const RATE_LIMIT: &str =
        include_str!("../../cosmon-cli/tests/fixtures/opencode/rate-limited.json");

    fn row(kind: &str, value: &Value) -> (String, String) {
        (kind.to_owned(), value.to_string())
    }

    #[test]
    fn sums_steps_and_does_not_count_an_in_progress_message() {
        let step: Value = serde_json::from_str(ASSISTANT).unwrap();
        let pending = json!({"model": step["model"]});
        let reading = parse_opencode_session(
            None,
            [
                row("assistant", &step),
                row("assistant", &step),
                row("assistant", &pending),
            ],
        );
        let totals = reading.totals.unwrap();
        assert_eq!(totals.input, Some(34));
        assert_eq!(totals.cache_read, Some(10));
        assert_eq!(totals.cache_write, Some(0));
        assert_eq!(totals.output, Some(22));
        assert_eq!(totals.reasoning, Some(14));
        assert_eq!(totals.cost_usd, Some(0.000073));
        assert_eq!(reading.provider.as_deref(), Some("normal"));
        assert_eq!(reading.status.model.as_deref(), Some("normal/fixture"));
    }

    #[test]
    fn incomplete_finished_step_keeps_only_known_totals_and_provider_changes() {
        let first: Value = serde_json::from_str(ASSISTANT).unwrap();
        let partial = json!({
            "model": {"providerID": "other", "id": "next"},
            "finish": "stop",
            "tokens": {"output": 0, "reasoning": 0}
        });
        let reading =
            parse_opencode_session(None, [row("assistant", &first), row("assistant", &partial)]);
        let totals = reading.totals.unwrap();
        assert_eq!(totals.input, None);
        assert_eq!(totals.cache_read, None);
        assert_eq!(totals.cache_write, None);
        assert_eq!(totals.output, Some(11));
        assert_eq!(totals.reasoning, Some(7));
        assert_eq!(totals.cost_usd, None);
        assert_eq!(reading.provider, None);
        assert_eq!(reading.status.model.as_deref(), Some("other/next"));
    }

    #[test]
    fn terminal_refusal_clears_when_a_later_turn_starts() {
        let error: Value = serde_json::from_str(RATE_LIMIT).unwrap();
        let state = json!({"error": error["error"]});
        let recorded = [row("assistant", &error), row("idle", &state)];
        let reading = parse_opencode_session(None, recorded.clone());
        assert_eq!(
            reading.status.refusal,
            Some(OpencodeRefusal {
                kind: OpencodeRefusalKind::RateLimited,
                retry_after_ms: Some(1_000),
            })
        );
        assert!(reading.totals.is_none());
        let resumed =
            parse_opencode_session(None, recorded.into_iter().chain([row("user", &json!({}))]));
        assert_eq!(resumed.status.refusal, None);
    }

    #[test]
    fn formats_an_unavailable_reading_with_its_provider_and_refusal() {
        let error: Value = serde_json::from_str(RATE_LIMIT).unwrap();
        let model = error["model"].to_string();
        let reading = parse_opencode_session(
            Some(&model),
            [row("idle", &json!({"error": error["error"]}))],
        );
        assert_eq!(
            reading.status.format_usage(None, |n| n.to_string(), "  "),
            "-/-/-/-  API equiv. unavailable  limited/fixture  rate-limited (retry after 1000ms)"
        );
    }

    #[test]
    fn rejects_empty_labels_and_terminal_control_characters() {
        for id in ["", "with space", "escape\u{1b}"] {
            let step = json!({"model": {"providerID": "normal", "id": id}, "finish": "stop"});
            let reading = parse_opencode_session(None, [row("assistant", &step)]);
            assert_eq!(reading.status.model, None, "{id:?}");
            assert_eq!(reading.provider, None, "{id:?}");
        }
    }

    #[test]
    fn a_completed_step_without_tokens_invalidates_an_earlier_total() {
        let first: Value = serde_json::from_str(ASSISTANT).unwrap();
        let incomplete = json!({"model": first["model"], "finish": "stop"});
        let reading = parse_opencode_session(
            None,
            [row("assistant", &first), row("assistant", &incomplete)],
        );
        assert_eq!(reading.totals, Some(OpencodeTotals::default()));
        assert_eq!(reading.provider.as_deref(), Some("normal"));
    }

    #[test]
    fn quota_is_distinct_from_transport_and_a_bad_model_is_ignored() {
        let invalid_model = r#"{"providerID":"normal","id":"\nmodel"}"#;
        let transport = json!({"error": {"type": "provider.transport", "status": 429}});
        let reading = parse_opencode_session(Some(invalid_model), [row("idle", &transport)]);
        assert_eq!(reading.status, OpencodeStatus::default());
        let quota = json!({"error": {"type": "provider.quota"}});
        let reading = parse_opencode_session(None, [row("idle", &quota)]);
        assert_eq!(
            reading.status.refusal,
            Some(OpencodeRefusal {
                kind: OpencodeRefusalKind::QuotaExhausted,
                retry_after_ms: None,
            })
        );
    }
}
