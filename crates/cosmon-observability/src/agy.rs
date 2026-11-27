// SPDX-License-Identifier: AGPL-3.0-only

//! Antigravity's receipt counters and quota diagnostics.

use cosmon_core::run_state::GhostKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Counters as reported by Antigravity. A missing value is distinct from a
/// reported zero, and cached input is reported separately from input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgyTokenCounts {
    /// Input excluding cache reads.
    #[serde(rename = "input_tokens")]
    pub input: Option<u64>,
    /// Input read from the cache.
    #[serde(rename = "cache_read_tokens")]
    pub cache_read: Option<u64>,
    /// Output tokens.
    #[serde(rename = "output_tokens")]
    pub output: Option<u64>,
    /// Thinking tokens.
    #[serde(rename = "thinking_tokens")]
    pub thinking: Option<u64>,
}

impl AgyTokenCounts {
    /// Render the provider's four counters in the fleet's column order.
    #[must_use]
    pub fn columns(&self, format: impl Fn(u64) -> String) -> String {
        [self.input, self.cache_read, self.output, self.thinking]
            .map(|count| count.map_or_else(|| "-".to_owned(), &format))
            .join("/")
    }
}

/// A quota refusal reported by the provider for the terminal result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgyQuotaStop {
    /// Delay printed by the provider at refusal time, if it gave one.
    pub reset_delay: Option<String>,
}

impl std::fmt::Display for AgyQuotaStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(GhostKind::QuotaExhausted.as_str())?;
        if let Some(delay) = &self.reset_delay {
            write!(f, " (resets in {delay})")?;
        }
        Ok(())
    }
}

/// Usage and quota stop reported by Antigravity's latest complete result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgyReceipt {
    /// Number of session starts in the append-only receipt, for usage identity.
    #[serde(skip)]
    pub init_count: u64,
    /// Counts from a complete result, if it included any measured category.
    pub usage: Option<AgyTokenCounts>,
    /// Present when the error result has provider quota evidence.
    pub quota_stop: Option<AgyQuotaStop>,
}

/// Fold Antigravity JSONL frames into one receipt. The reader supplies lines
/// so a large transcript does not need to be held in memory. Mixed stderr and
/// a partly written frame do not erase the last complete result.
#[must_use]
pub fn parse_agy_receipt(lines: impl IntoIterator<Item = impl AsRef<[u8]>>) -> AgyReceipt {
    let mut receipt = AgyReceipt::default();
    let mut pending_quota = None;
    for line in lines {
        let Ok(frame) = serde_json::from_slice::<Value>(line.as_ref()) else {
            continue;
        };
        match frame["event"].as_str() {
            Some("init") => {
                receipt = AgyReceipt {
                    init_count: receipt.init_count.saturating_add(1),
                    ..AgyReceipt::default()
                };
                pending_quota = None;
            }
            Some("error") => {
                let error = &frame["error"];
                pending_quota = (error["status"].as_str() == Some("RESOURCE_EXHAUSTED")
                    || error["code"].as_u64() == Some(429))
                .then(|| AgyQuotaStop {
                    reset_delay: error["message"].as_str().and_then(reset_delay),
                });
            }
            Some("result") if frame["result"].is_object() => {
                let result = &frame["result"];
                receipt.usage = token_counts(&result["usage"]);
                receipt.quota_stop = quota_for_result(result, pending_quota.take());
            }
            _ => {}
        }
    }
    receipt
}

fn token_counts(usage: &Value) -> Option<AgyTokenCounts> {
    let counts = AgyTokenCounts {
        input: usage["input_tokens"].as_u64(),
        cache_read: usage["cache_read_tokens"].as_u64(),
        output: usage["output_tokens"].as_u64(),
        thinking: usage["thinking_tokens"].as_u64(),
    };
    (counts != AgyTokenCounts::default()).then_some(counts)
}

fn quota_for_result(result: &Value, pending: Option<AgyQuotaStop>) -> Option<AgyQuotaStop> {
    if result["status"].as_str() != Some("ERROR") {
        return None;
    }
    let message = result["error"].as_str();
    let mut stop = pending.or_else(|| {
        message
            .filter(|message| message.starts_with("Individual quota reached."))
            .map(|_| AgyQuotaStop { reset_delay: None })
    })?;
    stop.reset_delay = stop.reset_delay.or_else(|| message.and_then(reset_delay));
    Some(stop)
}

fn reset_delay(message: &str) -> Option<String> {
    let (_, tail) = message.split_once("Resets in ")?;
    let delay = tail.split(|c: char| !c.is_ascii_alphanumeric()).next()?;
    let units = ['d', 'h', 'm', 's'];
    let valid = !delay.is_empty()
        && delay.split_inclusive(units).all(|part| {
            let (digits, suffix) = part.split_at(part.len() - 1);
            suffix.starts_with(units)
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
        });
    valid.then(|| delay.to_owned())
}
