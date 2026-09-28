// SPDX-License-Identifier: AGPL-3.0-only

//! Allowlisted plan metadata. These account observations never attribute a
//! quota delta to a worker. All clocks and provider bytes are caller supplied.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::{
    id::WorkerId,
    usage::{
        AllowanceWindow, Availability, Freshness, NonNegativeFinite, ObservationProvenance,
        ObservationScope, PlanApplicability, PlanUsage, PlanWindowObservation, SourceUtilization,
        UnavailableReason, Utilization, UtilizationUnit,
    },
};

/// Supported input contracts, separated because their units differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    /// Claude Code status-line stdin, percent units.
    ClaudeStatusLine,
    /// Claude Code rate-limit stream event, fraction units.
    ClaudeStream,
    /// Codex rollout token-count rate limits, percent units.
    CodexRollout,
}

impl PlanSource {
    /// Stable schema label used in sanitized storage and provenance.
    #[must_use]
    pub const fn schema(self) -> &'static str {
        match self {
            Self::ClaudeStatusLine => "claude.statusline.rate_limits.v1",
            Self::ClaudeStream => "claude.rate_limit_event.unified_windows.v1",
            Self::CodexRollout => "codex.token_count.rate_limits.v1",
        }
    }

    fn meters(self) -> [&'static str; 2] {
        match self {
            Self::CodexRollout => ["primary", "secondary"],
            Self::ClaudeStatusLine | Self::ClaudeStream => ["five_hour", "seven_day"],
        }
    }
}

/// A known window that the particular sample could not supply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailablePlanWindow {
    /// Allowlisted provider meter, never provider-controlled free text.
    pub meter: String,
    /// Missing and malformed observations remain distinguishable.
    pub reason: UnavailableReason,
}

/// Sanitized sample persisted independently of token and price observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanObservation {
    /// Exact parser contract used for this sample.
    pub source: PlanSource,
    /// Original ingestion time; reading the stored sample never rewrites it.
    pub captured_at: DateTime<Utc>,
    /// Canonical account context and unavailable worker attribution.
    pub plan: PlanUsage,
    /// Per-window absences, including a missing sibling of a valid window.
    pub unavailable_windows: Vec<UnavailablePlanWindow>,
}

impl PlanObservation {
    /// Reclassify freshness using the supplied clock and maximum age.
    ///
    /// A source without an observation time is at best unknown. Capture age
    /// and elapsed reset still establish staleness. Future times are unknown.
    pub fn refresh(&mut self, now: DateTime<Utc>, max_age: Duration) {
        for window in &mut self.plan.windows {
            let captured = window.provenance.captured_at.unwrap_or(self.captured_at);
            window.freshness = if window.window.resets_at.is_some_and(|t| t <= now)
                || now.signed_duration_since(captured) > max_age
                || window
                    .provenance
                    .observed_at
                    .is_some_and(|t| now.signed_duration_since(t) > max_age)
            {
                Freshness::Stale
            } else if max_age < Duration::zero() || captured > now {
                Freshness::Unknown
            } else if window.provenance.observed_at.is_some_and(|t| t <= now) {
                Freshness::Fresh
            } else {
                Freshness::Unknown
            };
        }
    }
}

/// Injected storage for sanitized worker samples, scoped to one worker attempt
/// by the caller's store root. Implementations must preserve capture time and
/// prevent an older concurrent writer from replacing a newer sample.
pub trait PlanObservationStore {
    /// Adapter-specific persistence failure.
    type Error;
    /// Persist sanitized metadata only.
    ///
    /// # Errors
    /// Returns the adapter's write or validation failure.
    fn save(&self, worker: &WorkerId, sample: &PlanObservation) -> Result<(), Self::Error>;
    /// Read without installing collectors or changing configuration.
    ///
    /// # Errors
    /// Returns the adapter's read or decoding failure.
    fn load(
        &self,
        worker: &WorkerId,
        source: PlanSource,
    ) -> Result<Option<PlanObservation>, Self::Error>;
}

/// Parse supported Claude input, discarding all unrelated fields.
///
/// Unsupported source selection and malformed input yield explicit absence;
/// no absent field is evidence that this account is API-only.
#[must_use]
pub fn claude_plan(raw: &str, source: PlanSource, captured_at: DateTime<Utc>) -> PlanObservation {
    let parsed = serde_json::from_str::<Value>(raw);
    let root = parsed.as_ref().ok();
    let windows = match source {
        PlanSource::ClaudeStatusLine => root.and_then(|r| r.get("rate_limits")),
        PlanSource::ClaudeStream => root
            .filter(|r| r.get("type").and_then(Value::as_str) == Some("rate_limit_event"))
            .and_then(|r| r.pointer("/rate_limit_info/unifiedWindows")),
        PlanSource::CodexRollout => None,
    };
    let reason = if source == PlanSource::CodexRollout {
        UnavailableReason::Unsupported
    } else if parsed.is_err() || root.is_some_and(|r| !r.is_object()) {
        UnavailableReason::MalformedSource
    } else {
        UnavailableReason::NotObserved
    };
    parse_windows(windows, source, None, captured_at, reason)
}

/// Read all supported Codex quota windows independently of token counters.
///
/// A token-only tail cannot freshen quota. A new non-null rate-limit object
/// replaces the previous window set, so a missing sibling is explicit rather
/// than silently carried into a different reset. Invalid partial JSONL tails
/// are ignored. Rollout event time is retained as source time, not read time.
#[must_use]
pub fn codex_plan(content: &str, captured_at: DateTime<Utc>) -> PlanObservation {
    let mut sample = parse_windows(
        None,
        PlanSource::CodexRollout,
        None,
        captured_at,
        UnavailableReason::NotObserved,
    );
    let mut buckets = BTreeMap::new();
    for line in content.lines() {
        let Ok(root) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if root.get("type").and_then(Value::as_str) != Some("event_msg")
            || root.pointer("/payload/type").and_then(Value::as_str) != Some("token_count")
        {
            continue;
        }
        let Some(limits) = root
            .pointer("/payload/rate_limits")
            .filter(|v| !v.is_null())
        else {
            continue;
        };
        let observed = root
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok());
        let mut reading = parse_windows(
            Some(limits),
            PlanSource::CodexRollout,
            observed,
            captured_at,
            UnavailableReason::NotObserved,
        );
        // A bucket identifies a provider meter, never the shared account.
        // Hash it so provider-controlled labels are not persisted verbatim.
        let bucket = limits
            .get("limit_id")
            .and_then(Value::as_str)
            .map(|id| format!("bucket-{:x}", Sha256::digest(id.as_bytes())))
            .unwrap_or_default();
        if !bucket.is_empty() {
            for window in &mut reading.plan.windows {
                window.meter = format!("{bucket}:{}", window.meter);
            }
            for window in &mut reading.unavailable_windows {
                window.meter = format!("{bucket}:{}", window.meter);
            }
        }
        buckets.insert(bucket, reading);
    }
    if !buckets.is_empty() {
        sample.plan.windows.clear();
        sample.unavailable_windows.clear();
        for reading in buckets.into_values() {
            sample.plan.windows.extend(reading.plan.windows);
            sample
                .unavailable_windows
                .extend(reading.unavailable_windows);
        }
        classify_applicability(&mut sample);
    }
    sample
}

fn parse_windows(
    windows: Option<&Value>,
    source: PlanSource,
    observed_at: Option<DateTime<Utc>>,
    captured_at: DateTime<Utc>,
    absent: UnavailableReason,
) -> PlanObservation {
    let mut result = PlanObservation {
        source,
        captured_at,
        plan: PlanUsage {
            applicability: PlanApplicability::Unknown { reason: absent },
            windows: Vec::new(),
            worker_attributed: Availability::Unavailable {
                reason: UnavailableReason::MissingAttributionEvidence,
            },
        },
        unavailable_windows: Vec::new(),
    };
    for meter in source.meters() {
        let value = windows.and_then(|w| w.get(meter)).filter(|v| !v.is_null());
        let parsed = value.map(|v| parse_window(v, meter, source, observed_at, captured_at));
        match parsed {
            Some(Ok(window)) => result.plan.windows.push(window),
            other => result.unavailable_windows.push(UnavailablePlanWindow {
                meter: meter.to_owned(),
                reason: if matches!(other, Some(Err(())))
                    || windows.is_some_and(|v| !v.is_null() && !v.is_object())
                {
                    UnavailableReason::MalformedSource
                } else {
                    absent
                },
            }),
        }
    }
    classify_applicability(&mut result);
    result
}

fn classify_applicability(result: &mut PlanObservation) {
    if !result.plan.windows.is_empty() {
        result.plan.applicability = PlanApplicability::Applicable {
            evidence: result.source.schema().to_owned(),
        };
    } else if result
        .unavailable_windows
        .iter()
        .any(|w| w.reason == UnavailableReason::MalformedSource)
    {
        result.plan.applicability = PlanApplicability::Unknown {
            reason: UnavailableReason::MalformedSource,
        };
    }
}

fn parse_window(
    value: &Value,
    meter: &str,
    source: PlanSource,
    observed_at: Option<DateTime<Utc>>,
    captured_at: DateTime<Utc>,
) -> Result<PlanWindowObservation, ()> {
    let (key, reset_key, unit) = match source {
        PlanSource::ClaudeStatusLine => ("used_percentage", "resets_at", UtilizationUnit::Percent),
        PlanSource::ClaudeStream => ("utilization", "resetsAt", UtilizationUnit::Fraction),
        PlanSource::CodexRollout => ("used_percent", "resets_at", UtilizationUnit::Percent),
    };
    let number = value.get(key).and_then(Value::as_f64).ok_or(())?;
    let original = NonNegativeFinite::new(number, "plan utilization").map_err(|_| ())?;
    // The documented status-line quota domain is 0..=100. Stream
    // utilization explicitly permits overage; do not impose a global clamp.
    if source == PlanSource::ClaudeStatusLine && number > 100.0 {
        return Err(());
    }
    let utilization = match unit {
        UtilizationUnit::Fraction => Utilization::from_fraction(number),
        UtilizationUnit::Percent => Utilization::from_percent(number),
    }
    .map_err(|_| ())?;
    let resets_at = value
        .get(reset_key)
        .filter(|v| !v.is_null())
        .map(|v| {
            let seconds = v.as_i64().filter(|s| *s >= 0).ok_or(())?;
            DateTime::from_timestamp(seconds, 0).ok_or(())
        })
        .transpose()?;
    let duration_minutes = if source == PlanSource::CodexRollout {
        value
            .get("window_minutes")
            .filter(|v| !v.is_null())
            .map(|v| v.as_u64().filter(|n| *n > 0).ok_or(()))
            .transpose()?
    } else {
        Some(if meter == "five_hour" { 300 } else { 10080 })
    };
    Ok(PlanWindowObservation {
        meter: meter.to_owned(),
        window: AllowanceWindow {
            id: None,
            duration_minutes,
            resets_at,
        },
        utilization,
        source_value: SourceUtilization {
            value: original,
            unit,
        },
        source_schema: source.schema().to_owned(),
        scope: ObservationScope::Account,
        provenance: ObservationProvenance {
            source: source.schema().to_owned(),
            provider: Some(
                if source == PlanSource::CodexRollout {
                    "openai"
                } else {
                    "anthropic"
                }
                .to_owned(),
            ),
            observed_at,
            captured_at: Some(captured_at),
            scope: ObservationScope::Account,
        },
        freshness: Freshness::Unknown,
        precision: None,
    })
}
