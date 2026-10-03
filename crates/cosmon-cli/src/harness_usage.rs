// SPDX-License-Identifier: AGPL-3.0-only

//! Usage capture for the in-process direct arms, through the canonical schema.
//!
//! Each answered request reaches [`HarnessUsageRecorder`](crate::harness_usage::HarnessUsageRecorder) as one per-request
//! [`UsageSample`](cosmon_agent_harness::spine::UsageSample). The recorder folds the samples of one worker attempt into a
//! cumulative history and publishes the result as
//! [`EventV2::UsageObserved`](cosmon_core::event_v2::EventV2::UsageObserved): the same record, history identity and
//! unavailability vocabulary every other usage producer uses, so the existing
//! projections read it without a second schema.
//!
//! Three rules shape the fold:
//!
//! - A category is a measured cumulative total only while every sample
//!   reported it. One sample that omitted it, or reported a value the schema
//!   cannot hold, makes the category unavailable from then on; it is never
//!   counted as zero and never published as a total it is not.
//! - A sample is identified by its provider response id, so a replayed receipt
//!   leaves the totals where they were.
//! - Requested and served model stay apart. Counters are paired with the model
//!   the response reported; a response that did not report one is never
//!   attributed to the requested name.
//!
//! The price of a record comes from the existing bundled reference tariff
//! through [`value_model_segments`](cosmon_core::price_manifest::value_model_segments), so this module adds no price table and a
//! gap in the counters shows up as partial coverage, not as a complete amount.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::Utc;
use cosmon_agent_harness::spine::{ReportedCount, ReportedUsage, UsageSample, UsageSink};
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::WorkerId;
use cosmon_core::price_manifest::{bundled_price_manifest, value_model_segments};
use cosmon_core::usage::{
    ApiEquivalent, Availability, ModelUsageSegment, ObservationProvenance, ObservationScope,
    PlanApplicability, PlanUsage, TokenCount, TokenUsage, UnavailableReason, UsageHistory,
    UsageObservationId, UsageRecord, UsageSubject, UsageValidationError,
    CURRENT_USAGE_SCHEMA_VERSION,
};

/// Stable source name carried by every record this module produces.
pub const USAGE_SOURCE: &str = "direct_harness_response";

/// History id of one in-process worker attempt.
///
/// It carries the molecule, worker and the attempt's own invocation id, so a
/// re-tackle starts a new cumulative history. Turn evidence uses the same id,
/// which is what ties an attempt's checkpoints to its usage records.
#[must_use]
pub fn attempt_history_id(
    molecule: &cosmon_core::id::MoleculeId,
    worker: &WorkerId,
    invocation: &str,
) -> String {
    format!(
        "harness/{}/{}/{invocation}",
        molecule.as_str(),
        worker.as_str()
    )
}

/// What happened to one delivered sample.
#[derive(Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    /// The sample was folded in and its cumulative record was written.
    Persisted,
    /// The sample's identity had been folded in before; nothing changed.
    Duplicate,
    /// The sample was folded in but its record was not written to the ledger.
    /// The next record carries the same cumulative totals, so only the
    /// intermediate observation is missing.
    NotPersisted(String),
    /// The cumulative record failed schema validation and was not written.
    Invalid(String),
}

/// One cumulative category: a measured total, or the reason it is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cumulative {
    Measured(u64),
    Unavailable(UnavailableReason),
}

impl Cumulative {
    /// Fold one sample's category into the running total.
    ///
    /// Unavailability is sticky: once a sample is missing or malformed the total
    /// is no longer a total. A malformed value outranks a missing one because it
    /// is the more specific fact.
    fn add(self, sample: ReportedCount) -> Self {
        match (self, sample) {
            (Self::Measured(total), ReportedCount::Measured(n)) => total.checked_add(n).map_or(
                Self::Unavailable(UnavailableReason::MalformedSource),
                Self::Measured,
            ),
            (Self::Measured(_), ReportedCount::NotReported) => {
                Self::Unavailable(UnavailableReason::NotObserved)
            }
            (Self::Unavailable(UnavailableReason::MalformedSource), _)
            | (Self::Measured(_) | Self::Unavailable(_), ReportedCount::Malformed) => {
                Self::Unavailable(UnavailableReason::MalformedSource)
            }
            (unavailable @ Self::Unavailable(_), _) => unavailable,
        }
    }

    const fn token_count(self) -> TokenCount {
        match self {
            Self::Measured(tokens) => TokenCount::Measured { tokens },
            Self::Unavailable(reason) => TokenCount::Unavailable { reason },
        }
    }
}

/// Cumulative counters for one scope: the whole history, or one served model.
#[derive(Debug, Clone, Copy)]
struct Counters {
    input: Cumulative,
    cached: Cumulative,
    cache_write: Cumulative,
    output: Cumulative,
    reasoning: Cumulative,
}

impl Counters {
    const fn zero() -> Self {
        let zero = Cumulative::Measured(0);
        Self {
            input: zero,
            cached: zero,
            cache_write: zero,
            output: zero,
            reasoning: zero,
        }
    }

    fn add(&mut self, usage: &ReportedUsage) {
        self.input = self.input.add(usage.input_tokens);
        self.cached = self.cached.add(usage.cached_input_tokens);
        self.cache_write = self.cache_write.add(usage.cache_write_tokens);
        self.output = self.output.add(usage.output_tokens);
        self.reasoning = self.reasoning.add(usage.reasoning_output_tokens);
    }
}

/// Demote a subset counter that contradicts its total to malformed.
///
/// The schema requires cache and reasoning counters to be subsets of the input
/// and output totals. A response that breaks that is reporting something the
/// schema cannot hold, so the offending subset is marked malformed instead of
/// letting the whole cumulative record fail validation.
fn normalized(mut usage: ReportedUsage) -> ReportedUsage {
    let ReportedUsage {
        input_tokens,
        cached_input_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_output_tokens,
    } = usage;
    if let (ReportedCount::Measured(input), ReportedCount::Measured(cached)) =
        (input_tokens, cached_input_tokens)
    {
        if cached > input {
            usage.cached_input_tokens = ReportedCount::Malformed;
        }
    }
    if let (ReportedCount::Measured(input), ReportedCount::Measured(write)) =
        (input_tokens, cache_write_tokens)
    {
        if write > input {
            usage.cache_write_tokens = ReportedCount::Malformed;
        }
    }
    if let (
        ReportedCount::Measured(input),
        ReportedCount::Measured(cached),
        ReportedCount::Measured(write),
    ) = (
        input_tokens,
        usage.cached_input_tokens,
        usage.cache_write_tokens,
    ) {
        if cached.saturating_add(write) > input {
            usage.cache_write_tokens = ReportedCount::Malformed;
        }
    }
    if let (ReportedCount::Measured(output), ReportedCount::Measured(reasoning)) =
        (output_tokens, reasoning_output_tokens)
    {
        if reasoning > output {
            usage.reasoning_output_tokens = ReportedCount::Malformed;
        }
    }
    usage
}

#[derive(Debug)]
struct State {
    seen: BTreeSet<String>,
    samples: u64,
    totals: Counters,
    per_model: BTreeMap<String, Counters>,
    /// Samples whose response did not name the model that served it.
    unpaired_samples: u64,
    provider: &'static str,
    latest: Option<UsageRecord>,
}

/// Folds per-request samples into one worker attempt's cumulative history and
/// writes each step to the event ledger.
#[derive(Debug)]
pub struct HarnessUsageRecorder {
    events_path: PathBuf,
    worker_id: WorkerId,
    history_id: String,
    attempt: Option<u32>,
    state: Mutex<State>,
}

impl HarnessUsageRecorder {
    /// Create a recorder for one worker attempt.
    ///
    /// `history_id` names the cumulative history and must differ between
    /// attempts: records that share it are one history, and the projections keep
    /// only the latest of them.
    #[must_use]
    pub fn new(
        state_dir: &Path,
        worker_id: WorkerId,
        history_id: impl Into<String>,
        attempt: Option<u32>,
    ) -> Self {
        Self {
            events_path: cosmon_state::event_log::resolve_events_log_path(state_dir),
            worker_id,
            history_id: history_id.into(),
            attempt,
            state: Mutex::new(State {
                seen: BTreeSet::new(),
                samples: 0,
                totals: Counters::zero(),
                per_model: BTreeMap::new(),
                unpaired_samples: 0,
                provider: "unknown",
                latest: None,
            }),
        }
    }

    /// The most recent cumulative record, if any sample has been folded in.
    #[must_use]
    pub fn latest(&self) -> Option<UsageRecord> {
        self.lock().latest.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fold one answered request into the history and write the new
    /// cumulative record.
    pub fn record_sample(&self, sample: &UsageSample) -> RecordOutcome {
        let mut state = self.lock();
        let key = sample.response_id.as_deref().map_or_else(
            || format!("sample:{}", state.samples + 1),
            |id| format!("response:{id}"),
        );
        if !state.seen.insert(key.clone()) {
            return RecordOutcome::Duplicate;
        }
        state.samples += 1;
        state.provider = sample.provider;
        let usage = normalized(sample.usage);
        state.totals.add(&usage);
        match sample
            .served_model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
        {
            Some(model) => state
                .per_model
                .entry(model.to_owned())
                .or_insert_with(Counters::zero)
                .add(&usage),
            None => state.unpaired_samples += 1,
        }

        let record = match self.build_record(&state, &key) {
            Ok(record) => record,
            Err(err) => return RecordOutcome::Invalid(err.to_string()),
        };
        state.latest = Some(record.clone());
        match cosmon_state::event_log::emit_one(
            &self.events_path,
            EventV2::UsageObserved {
                usage: Box::new(record),
            },
            None,
        ) {
            Ok(_) => RecordOutcome::Persisted,
            Err(err) => RecordOutcome::NotPersisted(err.to_string()),
        }
    }

    fn build_record(&self, state: &State, key: &str) -> Result<UsageRecord, UsageValidationError> {
        let provenance = ObservationProvenance {
            source: USAGE_SOURCE.to_owned(),
            provider: Some(state.provider.to_owned()),
            observed_at: None,
            captured_at: Some(Utc::now()),
            scope: ObservationScope::Worker,
        };
        let segments: Vec<ModelUsageSegment> = state
            .per_model
            .iter()
            .map(|(model, c)| ModelUsageSegment {
                model: model.clone(),
                input_tokens: c.input.token_count(),
                cached_input_tokens: c.cached.token_count(),
                cache_write_tokens: c.cache_write.token_count(),
                cache_write_5m_tokens: None,
                cache_write_1h_tokens: None,
                output_tokens: c.output.token_count(),
                reasoning_output_tokens: c.reasoning.token_count(),
            })
            .collect();
        let all_paired = state.unpaired_samples == 0 && !segments.is_empty();
        let api_equivalent = Self::price(&segments, all_paired, &provenance);
        let model_segments = if all_paired {
            Availability::Available { value: segments }
        } else {
            Availability::Unavailable {
                reason: UnavailableReason::NotObserved,
            }
        };
        let totals = &state.totals;
        let record = UsageRecord {
            schema_version: CURRENT_USAGE_SCHEMA_VERSION,
            observation_id: UsageObservationId::Known {
                id: format!("{}/{key}", self.history_id),
            },
            subject: UsageSubject {
                worker_id: self.worker_id.clone(),
                attempt: self.attempt,
                history: UsageHistory::Known {
                    id: self.history_id.clone(),
                },
            },
            tokens: TokenUsage {
                input_tokens: totals.input.token_count(),
                cached_input_tokens: totals.cached.token_count(),
                cache_write_tokens: totals.cache_write.token_count(),
                output_tokens: totals.output.token_count(),
                reasoning_output_tokens: totals.reasoning.token_count(),
                model_segments,
                provenance,
            },
            api_equivalent,
            plan: PlanUsage {
                applicability: PlanApplicability::Unknown {
                    reason: UnavailableReason::NotObserved,
                },
                windows: Vec::new(),
                worker_attributed: Availability::Unavailable {
                    reason: UnavailableReason::NotObserved,
                },
            },
        };
        record.validate()?;
        Ok(record)
    }

    /// Price the counters with the bundled reference tariff, or say why not.
    ///
    /// No segment means no model to price against; an unknown model or a
    /// counter gap is reported by the valuation itself as partial coverage.
    fn price(
        segments: &[ModelUsageSegment],
        all_paired: bool,
        provenance: &ObservationProvenance,
    ) -> ApiEquivalent {
        if segments.is_empty() {
            return ApiEquivalent::Unavailable {
                reason: UnavailableReason::NotObserved,
            };
        }
        let Ok(manifest) = bundled_price_manifest() else {
            return ApiEquivalent::Unavailable {
                reason: UnavailableReason::MissingPricingCategory,
            };
        };
        let Some(card) = manifest.current_card() else {
            return ApiEquivalent::Unavailable {
                reason: UnavailableReason::MissingPricingCategory,
            };
        };
        value_model_segments(card, segments, all_paired, provenance.clone())
    }
}

impl UsageSink for HarnessUsageRecorder {
    fn record(&self, sample: UsageSample) {
        match self.record_sample(&sample) {
            RecordOutcome::Persisted | RecordOutcome::Duplicate => {}
            RecordOutcome::NotPersisted(reason) => tracing::warn!(
                target: "cosmon::harness_usage",
                reason,
                "usage observation was not written to the ledger"
            ),
            RecordOutcome::Invalid(reason) => tracing::warn!(
                target: "cosmon::harness_usage",
                reason,
                "usage observation failed validation and was dropped"
            ),
        }
    }
}
