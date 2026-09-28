// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical usage observations and compatibility decoding.
//!
//! A usage record preserves token counters, an API-equivalent estimate, and
//! plan observations as independent components.  Absence is always typed: an
//! unavailable value is never represented by numeric zero.  The module is
//! deliberately I/O-free.  Adapters read provider data and supply timestamps;
//! the core only validates and normalizes values already in memory.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::id::WorkerId;

/// The usage-record schema understood by this reader.
pub const CURRENT_USAGE_SCHEMA_VERSION: u16 = 1;

/// A finite, non-negative numeric observation.
///
/// This newtype rejects negative values, infinities, and NaN during both
/// construction and deserialization.  Zero remains valid because a measured
/// zero is distinct from an unavailable observation.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct NonNegativeFinite(f64);

impl NonNegativeFinite {
    /// Validate and construct a numeric observation.
    ///
    /// # Errors
    ///
    /// Returns [`UsageValidationError::InvalidNumber`] for a negative or
    /// non-finite value.
    pub fn new(value: f64, field: &'static str) -> Result<Self, UsageValidationError> {
        if value.is_finite() && value >= 0.0 {
            Ok(Self(value))
        } else {
            Err(UsageValidationError::InvalidNumber { field, value })
        }
    }

    /// Return the validated numeric value.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for NonNegativeFinite {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value, "numeric observation").map_err(D::Error::custom)
    }
}

/// Normalized allowance utilization as a fraction (`0.42` means 42%).
///
/// Values greater than one are valid because some provider meters report
/// overage.  Consumers must not clamp such evidence to 100%.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Utilization(NonNegativeFinite);

impl Utilization {
    /// Construct from a fraction.
    ///
    /// # Errors
    ///
    /// Returns an error when `fraction` is negative or non-finite.
    pub fn from_fraction(fraction: f64) -> Result<Self, UsageValidationError> {
        NonNegativeFinite::new(fraction, "utilization").map(Self)
    }

    /// Construct from a percentage (`42.0` means 42%).
    ///
    /// # Errors
    ///
    /// Returns an error when `percent` is negative or non-finite.
    pub fn from_percent(percent: f64) -> Result<Self, UsageValidationError> {
        NonNegativeFinite::new(percent, "utilization percent")?;
        Self::from_fraction(percent / 100.0)
    }

    /// Return the normalized fraction.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for Utilization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::from_fraction(value).map_err(D::Error::custom)
    }
}

/// Why a usage component is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnavailableReason {
    /// The source did not report the component.
    NotObserved,
    /// The adapter or provider does not expose the component.
    Unsupported,
    /// Pricing inputs exist but one or more required categories are missing.
    MissingPricingCategory,
    /// No evidence attributes an account reading to this worker.
    MissingAttributionEvidence,
    /// The source value was malformed.
    MalformedSource,
    /// A legacy record did not carry the evidence required by the new schema.
    LegacyUnknown,
    /// A legacy zero-dollar tick cannot distinguish measured zero from a
    /// historical unknown-to-zero fallback.
    LegacyZeroAmbiguous,
}

/// Availability wrapper that cannot confuse missing evidence with zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Availability<T> {
    /// The value was measured or otherwise supported by the named source.
    Available {
        /// Available value.
        value: T,
    },
    /// The value is unavailable for the stated reason.
    Unavailable {
        /// Evidence-preserving absence reason.
        reason: UnavailableReason,
    },
}

/// Stable identity for one observation sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UsageObservationId {
    /// A producer-supplied identifier suitable for deduplication.
    Known {
        /// Opaque identifier; readers compare it but do not parse it.
        id: String,
    },
    /// A legacy source carried no sample identity.
    LegacyUnknown,
}

/// Identity of the cumulative usage history being sampled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UsageHistory {
    /// A source-stable cumulative history identifier.
    Known {
        /// Opaque history identifier.
        id: String,
    },
    /// A legacy source carried no history identity.
    LegacyUnknown,
}

/// Worker and attempt scope for a usage observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSubject {
    /// Worker whose activity is represented.
    pub worker_id: WorkerId,
    /// One-based worker attempt when the source establishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Cumulative usage-history identity.
    pub history: UsageHistory,
}

/// Attribution scope of a component observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObservationScope {
    /// The source measures this worker attempt.
    Worker,
    /// The source measures one cumulative usage history.
    UsageHistory,
    /// The source measures an account shared by potentially many workers.
    Account,
    /// The source did not establish a scope.
    Unknown,
}

/// Component-specific source and time evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationProvenance {
    /// Stable source/schema name, never a private filesystem path.
    pub source: String,
    /// Provider family reported by the source, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider observation time, when the provider reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// Adapter capture time supplied by the caller, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<DateTime<Utc>>,
    /// Attribution scope established by the source.
    pub scope: ObservationScope,
}

impl ObservationProvenance {
    fn legacy(source: &str, captured_at: Option<DateTime<Utc>>) -> Self {
        Self {
            source: source.to_owned(),
            provider: None,
            observed_at: None,
            captured_at,
            scope: ObservationScope::Unknown,
        }
    }
}

/// One token category, measured or explicitly unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TokenCount {
    /// A measured cumulative token count.  Zero is meaningful here.
    Measured {
        /// Cumulative token count.
        tokens: u64,
    },
    /// The category was not measured.
    Unavailable {
        /// Why the source could not supply the category.
        reason: UnavailableReason,
    },
}

impl TokenCount {
    /// Return the measured count, or `None` when unavailable.
    #[must_use]
    pub const fn measured(self) -> Option<u64> {
        match self {
            Self::Measured { tokens } => Some(tokens),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Cumulative token categories from one source observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Total input tokens, including measured cache subsets.
    pub input_tokens: TokenCount,
    /// Cache-read subset of input tokens.
    pub cached_input_tokens: TokenCount,
    /// Cache-write subset of input tokens when the provider distinguishes it.
    pub cache_write_tokens: TokenCount,
    /// Total output tokens, including the reasoning subset.
    pub output_tokens: TokenCount,
    /// Reasoning subset of output tokens.
    pub reasoning_output_tokens: TokenCount,
    /// Model-scoped counter segments when the source can pair counters with
    /// realized model identity.
    pub model_segments: Availability<Vec<ModelUsageSegment>>,
    /// Source and timestamp evidence for these counters only.
    pub provenance: ObservationProvenance,
}

/// Token counters paired with one realized model segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsageSegment {
    /// Exact model identifier reported by the adapter.
    pub model: String,
    /// Total input tokens in this segment.
    pub input_tokens: TokenCount,
    /// Cache-read subset of segment input.
    pub cached_input_tokens: TokenCount,
    /// Cache-write subset of segment input.
    pub cache_write_tokens: TokenCount,
    /// Five-minute cache-write subset, when the source reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_tokens: Option<u64>,
    /// One-hour cache-write subset, when the source reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_tokens: Option<u64>,
    /// Total output tokens in this segment.
    pub output_tokens: TokenCount,
    /// Reasoning subset of segment output.
    pub reasoning_output_tokens: TokenCount,
}

/// Coverage of an API-equivalent estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PricingCoverage {
    /// Every category needed by the named pricing basis was valued.
    Complete,
    /// A known subtotal; named categories remain unpriced.
    Partial {
        /// Categories excluded from the subtotal.
        missing: Vec<String>,
    },
    /// A legacy amount whose priced categories were not recorded.
    LegacyUnknown,
}

/// Reproducible basis for an API-equivalent estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PricingBasis {
    /// A named, reviewed reference rate card.
    RateCard {
        /// Immutable rate-card revision.
        revision: String,
        /// Public source identifier or URL for the rates.
        source: String,
        /// Date on which the rates were verified.
        verified_at: NaiveDate,
        /// Named comparison policy, such as `standard_list_price`.
        comparison: String,
    },
    /// A legacy amount with no recorded rate version or basis.
    LegacyUnknown,
}

/// API-equivalent USD estimate, independent from plan observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApiEquivalent {
    /// A reproducible estimate or known subtotal.
    Estimated {
        /// Estimated amount in US dollars.
        amount_usd: NonNegativeFinite,
        /// Completeness of the amount.
        coverage: PricingCoverage,
        /// Rate-card or legacy basis.
        basis: PricingBasis,
        /// Source and time evidence for this estimate only.
        provenance: ObservationProvenance,
    },
    /// No defensible amount is available.
    Unavailable {
        /// Why the amount is unavailable.
        reason: UnavailableReason,
    },
}

/// Unit used by the source allowance meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UtilizationUnit {
    /// Fractional source value (`0.42`).
    Fraction,
    /// Percentage source value (`42.0`).
    Percent,
}

/// Original allowance value and unit before normalization.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SourceUtilization {
    /// Original non-negative finite value.
    pub value: NonNegativeFinite,
    /// Unit declared by the source schema.
    pub unit: UtilizationUnit,
}

impl SourceUtilization {
    fn normalized(self) -> f64 {
        match self.unit {
            UtilizationUnit::Fraction => self.value.get(),
            UtilizationUnit::Percent => self.value.get() / 100.0,
        }
    }
}

/// Identity and reset information for an allowance window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AllowanceWindow {
    /// Provider-stable window/reset identity, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Window duration in minutes, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u64>,
    /// Next reset time, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
}

/// Freshness classification supplied by the adapter policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Freshness {
    /// Observation satisfies the adapter's freshness policy.
    Fresh,
    /// Observation is older than the adapter's freshness policy permits.
    Stale,
    /// The observed allowance window has passed its reset time.
    Reset,
    /// No defensible freshness classification is available.
    Unknown,
}

/// One independently identified plan-window observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanWindowObservation {
    /// Provider meter name, such as `primary` or `weekly`.
    pub meter: String,
    /// Window/reset identity.
    pub window: AllowanceWindow,
    /// Canonical utilization fraction; overage above one is preserved.
    pub utilization: Utilization,
    /// Original value and unit for wire fidelity.
    pub source_value: SourceUtilization,
    /// Versioned provider schema that declared the original unit.
    pub source_schema: String,
    /// Account, worker, or unknown attribution scope.
    pub scope: ObservationScope,
    /// Source and timestamps for this window only.
    pub provenance: ObservationProvenance,
    /// Adapter-supplied freshness classification.
    pub freshness: Freshness,
    /// Source precision in normalized fraction units, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<NonNegativeFinite>,
}

/// Whether a plan allowance applies, supported by explicit evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanApplicability {
    /// A plan applies according to the named evidence.
    Applicable {
        /// Human-readable evidence class, not a private binding.
        evidence: String,
    },
    /// Applicability cannot be established.
    Unknown {
        /// Why applicability is unknown.
        reason: UnavailableReason,
    },
    /// Evidence establishes that no plan allowance applies.
    NotApplicable {
        /// Human-readable evidence class.
        evidence: String,
    },
}

/// Worker-attributed share when a source actually supports that claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerPlanConsumption {
    /// Worker-attributed utilization as a fraction.
    pub utilization: Utilization,
    /// Window to which the attribution applies.
    pub window: AllowanceWindow,
    /// Source and timestamps establishing worker scope.
    pub provenance: ObservationProvenance,
}

/// Plan applicability, account context, and independent worker attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanUsage {
    /// Whether a plan applies.
    pub applicability: PlanApplicability,
    /// Independently identified allowance-window observations.
    #[serde(default)]
    pub windows: Vec<PlanWindowObservation>,
    /// Worker-attributed consumption, separate from account occupancy.
    pub worker_attributed: Availability<WorkerPlanConsumption>,
}

/// Versioned canonical usage record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageRecord {
    /// Wire schema version.  Readers fail closed on unsupported versions.
    pub schema_version: u16,
    /// Sample identity used to correlate or deduplicate observations.
    pub observation_id: UsageObservationId,
    /// Worker, attempt, and cumulative-history scope.
    pub subject: UsageSubject,
    /// Token categories and their own provenance.
    pub tokens: TokenUsage,
    /// API-equivalent estimate, independent from plan observations.
    pub api_equivalent: ApiEquivalent,
    /// Plan observations and independent worker attribution.
    pub plan: PlanUsage,
}

#[derive(Deserialize)]
struct UsageRecordWire {
    schema_version: u16,
    observation_id: UsageObservationId,
    subject: UsageSubject,
    tokens: TokenUsage,
    api_equivalent: ApiEquivalent,
    plan: PlanUsage,
}

impl<'de> Deserialize<'de> for UsageRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = UsageRecordWire::deserialize(deserializer)?;
        let record = Self {
            schema_version: wire.schema_version,
            observation_id: wire.observation_id,
            subject: wire.subject,
            tokens: wire.tokens,
            api_equivalent: wire.api_equivalent,
            plan: wire.plan,
        };
        record.validate().map_err(D::Error::custom)?;
        Ok(record)
    }
}

impl UsageRecord {
    /// Validate cross-field schema, coverage, subset, and normalization rules.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error without consulting I/O or a clock.
    pub fn validate(&self) -> Result<(), UsageValidationError> {
        if self.schema_version != CURRENT_USAGE_SCHEMA_VERSION {
            return Err(UsageValidationError::UnsupportedSchema {
                found: self.schema_version,
                supported: CURRENT_USAGE_SCHEMA_VERSION,
            });
        }
        validate_nonempty_identity(&self.observation_id)?;
        validate_nonempty_history(&self.subject.history)?;
        validate_token_usage(&self.tokens)?;
        validate_api_equivalent(&self.api_equivalent)?;
        validate_plan_usage(&self.plan)?;
        Ok(())
    }

    /// Convert an old worker energy budget without inventing missing evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when legacy subset counters contradict their totals.
    pub fn from_legacy_budget(
        worker_id: WorkerId,
        legacy: LegacyEnergyBudget,
    ) -> Result<Self, UsageValidationError> {
        let provenance = ObservationProvenance::legacy("legacy_energy_budget", None);
        let tokens = TokenUsage {
            input_tokens: TokenCount::Measured {
                tokens: legacy.input_tokens,
            },
            cached_input_tokens: TokenCount::Measured {
                tokens: legacy.cached_input_tokens,
            },
            cache_write_tokens: TokenCount::Unavailable {
                reason: UnavailableReason::LegacyUnknown,
            },
            output_tokens: TokenCount::Measured {
                tokens: legacy.output_tokens,
            },
            reasoning_output_tokens: TokenCount::Measured {
                tokens: legacy.reasoning_output_tokens,
            },
            model_segments: Availability::Unavailable {
                reason: UnavailableReason::LegacyUnknown,
            },
            provenance: provenance.clone(),
        };
        let (api_equivalent, plan) = legacy_cost_components(legacy.cost, provenance);
        let record = Self {
            schema_version: CURRENT_USAGE_SCHEMA_VERSION,
            observation_id: UsageObservationId::LegacyUnknown,
            subject: UsageSubject {
                worker_id,
                attempt: None,
                history: UsageHistory::LegacyUnknown,
            },
            tokens,
            api_equivalent,
            plan,
        };
        record.validate()?;
        Ok(record)
    }

    /// Convert an old durable energy tick conservatively.
    ///
    /// A positive legacy amount remains a provenance-unknown estimate.  Zero
    /// becomes [`UnavailableReason::LegacyZeroAmbiguous`] because historical
    /// producers used zero as a fallback for unavailable prices.
    #[must_use]
    pub fn from_legacy_energy_tick(
        worker_id: WorkerId,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
        captured_at: DateTime<Utc>,
    ) -> Self {
        let provenance = ObservationProvenance::legacy("legacy_energy_tick", Some(captured_at));
        let api_equivalent = legacy_reference_amount(cost_usd, provenance.clone());
        Self {
            schema_version: CURRENT_USAGE_SCHEMA_VERSION,
            observation_id: UsageObservationId::LegacyUnknown,
            subject: UsageSubject {
                worker_id,
                attempt: None,
                history: UsageHistory::LegacyUnknown,
            },
            tokens: TokenUsage {
                input_tokens: TokenCount::Measured {
                    tokens: input_tokens,
                },
                cached_input_tokens: TokenCount::Unavailable {
                    reason: UnavailableReason::LegacyUnknown,
                },
                cache_write_tokens: TokenCount::Unavailable {
                    reason: UnavailableReason::LegacyUnknown,
                },
                output_tokens: TokenCount::Measured {
                    tokens: output_tokens,
                },
                reasoning_output_tokens: TokenCount::Unavailable {
                    reason: UnavailableReason::LegacyUnknown,
                },
                model_segments: Availability::Unavailable {
                    reason: UnavailableReason::LegacyUnknown,
                },
                provenance,
            },
            api_equivalent,
            plan: unknown_plan(UnavailableReason::NotObserved),
        }
    }
}

/// Legacy mutually-exclusive cost field accepted by the compatibility reader.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LegacyEnergyCost {
    /// No legacy value was available.
    #[default]
    Unknown,
    /// Unqualified API-reference amount.
    ReferenceUsd {
        /// Legacy USD amount.
        usd: f64,
    },
    /// Legacy primary subscription window.
    Subscription {
        /// Provider plan label, when reported.
        #[serde(default)]
        plan_type: Option<String>,
        /// Used percentage in the primary window.
        used_percent: f64,
        /// Primary window duration, when reported.
        #[serde(default)]
        window_minutes: Option<u64>,
    },
}

/// Old `EnergyBudget` JSON shape accepted during the reader migration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LegacyEnergyBudget {
    /// Cumulative input tokens.
    pub input_tokens: u64,
    /// Cached-input subset; absent on the oldest records.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Cumulative output tokens.
    pub output_tokens: u64,
    /// Reasoning-output subset; absent on the oldest records.
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    /// Mutually-exclusive legacy cost interpretation.
    #[serde(default)]
    pub cost: LegacyEnergyCost,
    /// Legacy context capacity.  Retained for decoding but deliberately not
    /// promoted into allowance usage.
    #[serde(default)]
    pub context_window: Option<u64>,
}

/// Decode either a current usage record or an old energy-budget record.
///
/// Presence of `schema_version` selects the current reader and therefore
/// fails closed on unsupported versions.  A document without that field is
/// interpreted only as the documented legacy energy-budget shape.
///
/// # Errors
///
/// Returns a JSON or validation error; malformed current records never fall
/// back to the more permissive legacy reader.
pub fn decode_usage_record(
    json: &str,
    legacy_worker_id: WorkerId,
) -> Result<UsageRecord, UsageDecodeError> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value.get("schema_version").is_some() {
        return Ok(serde_json::from_value(value)?);
    }
    let legacy: LegacyEnergyBudget = serde_json::from_value(value)?;
    Ok(UsageRecord::from_legacy_budget(legacy_worker_id, legacy)?)
}

/// Failure while validating a usage record.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageValidationError {
    /// A numeric field was negative or non-finite.
    #[error("{field} must be finite and non-negative, got {value}")]
    InvalidNumber {
        /// Field being validated.
        field: &'static str,
        /// Rejected value.
        value: f64,
    },
    /// The reader does not understand the declared schema.
    #[error("unsupported usage schema version {found}; this reader supports {supported}")]
    UnsupportedSchema {
        /// Declared schema version.
        found: u16,
        /// Maximum and exact supported schema version.
        supported: u16,
    },
    /// A required identifier or evidence string was empty.
    #[error("{field} must not be empty")]
    EmptyField {
        /// Empty field.
        field: &'static str,
    },
    /// A subset token counter exceeded its enclosing total.
    #[error("{subset} ({subset_value}) exceeds {total} ({total_value})")]
    InvalidTokenSubset {
        /// Subset category.
        subset: &'static str,
        /// Subset count.
        subset_value: u64,
        /// Total category.
        total: &'static str,
        /// Total count.
        total_value: u64,
    },
    /// Coverage and rate-card evidence contradict one another.
    #[error("pricing coverage and pricing basis are inconsistent")]
    InconsistentPricingEvidence,
    /// A source value does not normalize to the canonical utilization.
    #[error(
        "normalized utilization {normalized} disagrees with source-normalized {source_normalized}"
    )]
    InconsistentUtilization {
        /// Canonical fraction.
        normalized: f64,
        /// Fraction derived independently from the source value and unit.
        source_normalized: f64,
    },
    /// Plan windows were attached to a not-applicable plan.
    #[error("plan marked not applicable cannot carry allowance windows")]
    InconsistentPlanApplicability,
    /// Worker-attributed usage lacked worker-scoped provenance.
    #[error("worker-attributed plan consumption requires worker-scoped provenance")]
    WorkerAttributionWithoutWorkerScope,
}

/// Failure while selecting and decoding a usage wire version.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageDecodeError {
    /// JSON syntax or shape did not match the selected reader.
    #[error("invalid usage JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The decoded record violated a semantic invariant.
    #[error(transparent)]
    Validation(#[from] UsageValidationError),
}

fn validate_nonempty_identity(id: &UsageObservationId) -> Result<(), UsageValidationError> {
    if let UsageObservationId::Known { id } = id {
        if id.trim().is_empty() {
            return Err(UsageValidationError::EmptyField {
                field: "observation_id.id",
            });
        }
    }
    Ok(())
}

fn validate_nonempty_history(history: &UsageHistory) -> Result<(), UsageValidationError> {
    if let UsageHistory::Known { id } = history {
        if id.trim().is_empty() {
            return Err(UsageValidationError::EmptyField {
                field: "subject.history.id",
            });
        }
    }
    Ok(())
}

fn validate_provenance(provenance: &ObservationProvenance) -> Result<(), UsageValidationError> {
    if provenance.source.trim().is_empty() {
        return Err(UsageValidationError::EmptyField {
            field: "provenance.source",
        });
    }
    Ok(())
}

fn validate_token_usage(tokens: &TokenUsage) -> Result<(), UsageValidationError> {
    validate_provenance(&tokens.provenance)?;
    validate_subset(
        "cached_input_tokens",
        tokens.cached_input_tokens,
        "input_tokens",
        tokens.input_tokens,
    )?;
    validate_subset(
        "cache_write_tokens",
        tokens.cache_write_tokens,
        "input_tokens",
        tokens.input_tokens,
    )?;
    validate_subset(
        "reasoning_output_tokens",
        tokens.reasoning_output_tokens,
        "output_tokens",
        tokens.output_tokens,
    )?;
    if let Availability::Available { value: segments } = &tokens.model_segments {
        if segments.is_empty() {
            return Err(UsageValidationError::EmptyField {
                field: "tokens.model_segments",
            });
        }
        for segment in segments {
            if segment.model.trim().is_empty() {
                return Err(UsageValidationError::EmptyField {
                    field: "tokens.model_segments.model",
                });
            }
            validate_subset(
                "model_segments.cached_input_tokens",
                segment.cached_input_tokens,
                "model_segments.input_tokens",
                segment.input_tokens,
            )?;
            validate_subset(
                "model_segments.cache_write_tokens",
                segment.cache_write_tokens,
                "model_segments.input_tokens",
                segment.input_tokens,
            )?;
            validate_subset(
                "model_segments.reasoning_output_tokens",
                segment.reasoning_output_tokens,
                "model_segments.output_tokens",
                segment.output_tokens,
            )?;
        }
    }
    Ok(())
}

fn validate_api_equivalent(value: &ApiEquivalent) -> Result<(), UsageValidationError> {
    let ApiEquivalent::Estimated {
        coverage,
        basis,
        provenance,
        ..
    } = value
    else {
        return Ok(());
    };
    validate_provenance(provenance)?;
    match (coverage, basis) {
        (PricingCoverage::Complete, PricingBasis::RateCard { .. })
        | (PricingCoverage::LegacyUnknown, PricingBasis::LegacyUnknown) => {}
        (PricingCoverage::Partial { missing }, PricingBasis::RateCard { .. }) => {
            if missing.is_empty() || missing.iter().any(String::is_empty) {
                return Err(UsageValidationError::EmptyField {
                    field: "api_equivalent.coverage.missing",
                });
            }
        }
        _ => return Err(UsageValidationError::InconsistentPricingEvidence),
    }
    if let PricingBasis::RateCard {
        revision,
        source,
        comparison,
        ..
    } = basis
    {
        for (field, value) in [
            ("api_equivalent.basis.revision", revision),
            ("api_equivalent.basis.source", source),
            ("api_equivalent.basis.comparison", comparison),
        ] {
            if value.trim().is_empty() {
                return Err(UsageValidationError::EmptyField { field });
            }
        }
    }
    Ok(())
}

fn validate_plan_usage(plan: &PlanUsage) -> Result<(), UsageValidationError> {
    match &plan.applicability {
        PlanApplicability::Applicable { evidence }
        | PlanApplicability::NotApplicable { evidence } => {
            if evidence.trim().is_empty() {
                return Err(UsageValidationError::EmptyField {
                    field: "plan.applicability.evidence",
                });
            }
        }
        PlanApplicability::Unknown { .. } => {}
    }
    if matches!(plan.applicability, PlanApplicability::NotApplicable { .. })
        && !plan.windows.is_empty()
    {
        return Err(UsageValidationError::InconsistentPlanApplicability);
    }
    for window in &plan.windows {
        if window.meter.trim().is_empty() {
            return Err(UsageValidationError::EmptyField {
                field: "plan.windows.meter",
            });
        }
        if window.source_schema.trim().is_empty() {
            return Err(UsageValidationError::EmptyField {
                field: "plan.windows.source_schema",
            });
        }
        validate_provenance(&window.provenance)?;
        if (window.utilization.get() - window.source_value.normalized()).abs() > 1e-12 {
            return Err(UsageValidationError::InconsistentUtilization {
                normalized: window.utilization.get(),
                source_normalized: window.source_value.normalized(),
            });
        }
    }
    if let Availability::Available { value } = &plan.worker_attributed {
        validate_provenance(&value.provenance)?;
        if value.provenance.scope != ObservationScope::Worker {
            return Err(UsageValidationError::WorkerAttributionWithoutWorkerScope);
        }
    }
    Ok(())
}

fn validate_subset(
    subset_name: &'static str,
    subset: TokenCount,
    total_name: &'static str,
    total: TokenCount,
) -> Result<(), UsageValidationError> {
    if let (Some(subset_value), Some(total_value)) = (subset.measured(), total.measured()) {
        if subset_value > total_value {
            return Err(UsageValidationError::InvalidTokenSubset {
                subset: subset_name,
                subset_value,
                total: total_name,
                total_value,
            });
        }
    }
    Ok(())
}

fn unknown_plan(reason: UnavailableReason) -> PlanUsage {
    PlanUsage {
        applicability: PlanApplicability::Unknown { reason },
        windows: Vec::new(),
        worker_attributed: Availability::Unavailable { reason },
    }
}

fn legacy_reference_amount(cost_usd: f64, provenance: ObservationProvenance) -> ApiEquivalent {
    if cost_usd == 0.0 {
        return ApiEquivalent::Unavailable {
            reason: UnavailableReason::LegacyZeroAmbiguous,
        };
    }
    match NonNegativeFinite::new(cost_usd, "legacy cost_usd") {
        Ok(amount_usd) => ApiEquivalent::Estimated {
            amount_usd,
            coverage: PricingCoverage::LegacyUnknown,
            basis: PricingBasis::LegacyUnknown,
            provenance,
        },
        Err(_) => ApiEquivalent::Unavailable {
            reason: UnavailableReason::MalformedSource,
        },
    }
}

fn legacy_cost_components(
    cost: LegacyEnergyCost,
    provenance: ObservationProvenance,
) -> (ApiEquivalent, PlanUsage) {
    match cost {
        LegacyEnergyCost::Unknown => (
            ApiEquivalent::Unavailable {
                reason: UnavailableReason::NotObserved,
            },
            unknown_plan(UnavailableReason::NotObserved),
        ),
        LegacyEnergyCost::ReferenceUsd { usd } => (
            legacy_reference_amount(usd, provenance),
            unknown_plan(UnavailableReason::NotObserved),
        ),
        LegacyEnergyCost::Subscription {
            plan_type,
            used_percent,
            window_minutes,
        } => {
            let window = Utilization::from_percent(used_percent)
                .ok()
                .and_then(|utilization| {
                    let source_value =
                        NonNegativeFinite::new(used_percent, "legacy subscription used_percent")
                            .ok()?;
                    Some(PlanWindowObservation {
                        meter: "legacy_primary".to_owned(),
                        window: AllowanceWindow {
                            id: None,
                            duration_minutes: window_minutes,
                            resets_at: None,
                        },
                        utilization,
                        source_value: SourceUtilization {
                            value: source_value,
                            unit: UtilizationUnit::Percent,
                        },
                        source_schema: "legacy_energy_cost".to_owned(),
                        scope: ObservationScope::Unknown,
                        provenance: provenance.clone(),
                        freshness: Freshness::Unknown,
                        precision: None,
                    })
                });
            let mut evidence = "legacy subscription variant".to_owned();
            if let Some(plan_type) = plan_type.filter(|value| !value.trim().is_empty()) {
                evidence.push_str(": plan label ");
                evidence.push_str(&plan_type);
            }
            let malformed = window.is_none();
            (
                ApiEquivalent::Unavailable {
                    reason: UnavailableReason::NotObserved,
                },
                PlanUsage {
                    applicability: PlanApplicability::Applicable { evidence },
                    windows: window.into_iter().collect(),
                    worker_attributed: Availability::Unavailable {
                        reason: if malformed {
                            UnavailableReason::MalformedSource
                        } else {
                            UnavailableReason::MissingAttributionEvidence
                        },
                    },
                },
            )
        }
    }
}
