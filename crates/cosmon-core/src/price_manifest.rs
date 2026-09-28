// SPDX-License-Identifier: AGPL-3.0-only

//! Reviewed reference tariffs and pure API-equivalent valuation.
//!
//! The bundled JSON manifest is an offline input, never a runtime pricing
//! service. Callers may select an older card explicitly; changing the current
//! revision does not rewrite historical valuations. Model lookup is exact,
//! apart from aliases explicitly listed in the manifest.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::usage::{
    ApiEquivalent, ModelUsageSegment, NonNegativeFinite, ObservationProvenance, PricingBasis,
    PricingCoverage, UnavailableReason,
};

const MANIFEST_JSON: &str = include_str!("../data/usage-prices.json");
const MANIFEST_SHA256: &str = include_str!("../data/usage-prices.json.sha256");
const MANIFEST_SCHEMA_VERSION: u16 = 1;

/// Review ownership and events that require a tariff check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenancePolicy {
    /// Team role accountable for reviewed manifest changes.
    pub owner: String,
    /// Maximum interval between source reviews.
    pub review_cadence_days: u16,
    /// Named events that require an out-of-cycle review.
    pub review_triggers: Vec<String>,
}

/// Versioned collection of immutable reference rate cards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceManifest {
    schema_version: u16,
    current_revision: String,
    maintenance: MaintenancePolicy,
    cards: Vec<PriceCard>,
}

impl PriceManifest {
    /// Return the manifest maintenance contract.
    #[must_use]
    pub const fn maintenance(&self) -> &MaintenancePolicy {
        &self.maintenance
    }

    /// Return the revision selected for new valuations.
    #[must_use]
    pub fn current_revision(&self) -> &str {
        &self.current_revision
    }

    /// Select the card used for new valuations.
    #[must_use]
    pub fn current_card(&self) -> Option<&PriceCard> {
        self.card(&self.current_revision)
    }

    /// Select one immutable historical card by exact revision.
    #[must_use]
    pub fn card(&self, revision: &str) -> Option<&PriceCard> {
        self.cards.iter().find(|card| card.revision == revision)
    }

    fn validate(&self) -> Result<(), PriceManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(PriceManifestError::UnsupportedSchema {
                found: self.schema_version,
                supported: MANIFEST_SCHEMA_VERSION,
            });
        }
        if self.maintenance.owner.trim().is_empty()
            || self.maintenance.review_cadence_days == 0
            || self.maintenance.review_triggers.is_empty()
        {
            return Err(PriceManifestError::InvalidMaintenancePolicy);
        }
        if self.current_card().is_none() {
            return Err(PriceManifestError::MissingCurrentRevision(
                self.current_revision.clone(),
            ));
        }

        let mut revisions = BTreeSet::new();
        for card in &self.cards {
            if card.revision.trim().is_empty() || !revisions.insert(card.revision.as_str()) {
                return Err(PriceManifestError::DuplicateOrEmptyRevision(
                    card.revision.clone(),
                ));
            }
            card.validate()?;
        }
        Ok(())
    }
}

/// One immutable comparison basis and its exact model rates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCard {
    revision: String,
    verified_at: NaiveDate,
    comparison: String,
    region: String,
    service_tier: String,
    entries: Vec<ModelRate>,
}

impl PriceCard {
    /// Return this immutable card's revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Date on which maintainers checked every source in this card.
    #[must_use]
    pub const fn verified_at(&self) -> NaiveDate {
        self.verified_at
    }

    /// Exact lookup by provider model id or a documented manifest alias.
    #[must_use]
    pub fn rate_for(&self, model: &str) -> Option<&ModelRate> {
        self.entries
            .iter()
            .find(|rate| rate.model == model || rate.aliases.iter().any(|alias| alias == model))
    }

    fn basis(&self) -> PricingBasis {
        PricingBasis::RateCard {
            revision: self.revision.clone(),
            source: format!(
                "embedded:usage-prices.json#{}?sha256={}",
                self.revision,
                MANIFEST_SHA256.trim()
            ),
            verified_at: self.verified_at,
            comparison: self.comparison.clone(),
        }
    }

    fn validate(&self) -> Result<(), PriceManifestError> {
        if self.comparison.trim().is_empty()
            || self.region.trim().is_empty()
            || self.service_tier.trim().is_empty()
            || self.entries.is_empty()
        {
            return Err(PriceManifestError::InvalidCard(self.revision.clone()));
        }
        let mut keys = BTreeSet::new();
        for entry in &self.entries {
            for key in std::iter::once(&entry.model).chain(entry.aliases.iter()) {
                if key.trim().is_empty() || !keys.insert(key.as_str()) {
                    return Err(PriceManifestError::DuplicateOrEmptyModel(key.clone()));
                }
            }
            entry.validate()?;
        }
        Ok(())
    }
}

/// Exact model identity, source, applicability, and token rates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRate {
    /// Provider family named by the official source.
    pub provider: String,
    /// Exact canonical model id.
    pub model: String,
    /// Exact additional ids documented by the provider.
    pub aliases: Vec<String>,
    /// Official page supporting this row.
    pub source_url: String,
    /// Published effective date, when the source states one.
    pub effective_date: Option<NaiveDate>,
    /// Context range to which these rates apply.
    pub context_band: String,
    /// Standard token rates in USD per million tokens.
    pub rates_usd_per_million_tokens: TokenRates,
}

impl ModelRate {
    fn validate(&self) -> Result<(), PriceManifestError> {
        if self.provider.trim().is_empty()
            || self.model.trim().is_empty()
            || self.source_url.trim().is_empty()
            || self.context_band.trim().is_empty()
        {
            return Err(PriceManifestError::InvalidModel(self.model.clone()));
        }
        for rate in [
            Some(self.rates_usd_per_million_tokens.input),
            self.rates_usd_per_million_tokens.cached_input,
            self.rates_usd_per_million_tokens.cache_write_5m,
            self.rates_usd_per_million_tokens.cache_write_1h,
            Some(self.rates_usd_per_million_tokens.output),
        ]
        .into_iter()
        .flatten()
        {
            if !rate.is_finite() || rate < 0.0 {
                return Err(PriceManifestError::InvalidRate(self.model.clone()));
            }
        }
        Ok(())
    }
}

/// Per-category prices in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenRates {
    /// Fresh input rate.
    pub input: f64,
    /// Cache-read rate, when the model exposes that category.
    pub cached_input: Option<f64>,
    /// Five-minute cache-write rate, when applicable.
    pub cache_write_5m: Option<f64>,
    /// One-hour cache-write rate, when applicable.
    pub cache_write_1h: Option<f64>,
    /// Output rate; reasoning subsets are already included.
    pub output: f64,
}

/// Failure while loading or validating the bundled price manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PriceManifestError {
    /// JSON did not match the manifest schema.
    #[error("invalid price manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The bytes differ from the reviewed sidecar digest.
    #[error("price manifest digest mismatch: expected {expected}, found {found}")]
    DigestMismatch {
        /// Digest recorded beside the manifest.
        expected: String,
        /// Digest computed from the embedded bytes.
        found: String,
    },
    /// This reader does not implement the declared schema.
    #[error("unsupported price manifest schema {found}; supported schema is {supported}")]
    UnsupportedSchema {
        /// Manifest schema that was found.
        found: u16,
        /// Schema implemented by this reader.
        supported: u16,
    },
    /// The manifest points at a card it does not contain.
    #[error("current price revision is absent: {0}")]
    MissingCurrentRevision(String),
    /// Maintenance ownership or cadence is empty.
    #[error("price manifest maintenance owner, cadence, and triggers are required")]
    InvalidMaintenancePolicy,
    /// A card revision is missing or repeated.
    #[error("price card revision is empty or duplicated: {0}")]
    DuplicateOrEmptyRevision(String),
    /// Required card metadata is missing.
    #[error("price card metadata is incomplete: {0}")]
    InvalidCard(String),
    /// A model id or documented alias is missing or repeated within one card.
    #[error("price model id or alias is empty or duplicated: {0}")]
    DuplicateOrEmptyModel(String),
    /// Required model metadata is missing.
    #[error("price model metadata is incomplete: {0}")]
    InvalidModel(String),
    /// A rate is negative or non-finite.
    #[error("price model has an invalid numeric rate: {0}")]
    InvalidRate(String),
}

/// Parse and validate the embedded, digest-pinned tariff history.
///
/// # Errors
///
/// Returns a typed error when the checked-in JSON, its digest, or a semantic
/// invariant is invalid. No filesystem, clock, or network access occurs.
pub fn bundled_price_manifest() -> Result<PriceManifest, PriceManifestError> {
    let expected = MANIFEST_SHA256.trim().to_ascii_lowercase();
    let found = format!("{:x}", Sha256::digest(MANIFEST_JSON.as_bytes()));
    if expected != found {
        return Err(PriceManifestError::DigestMismatch { expected, found });
    }
    let manifest: PriceManifest = serde_json::from_str(MANIFEST_JSON)?;
    manifest.validate()?;
    Ok(manifest)
}

/// Value exact model segments under one selected card.
///
/// `segments_complete` states whether the caller could pair every observed
/// token with a realized model. A false value preserves the known subtotal but
/// marks the missing model partition. Unknown models and ambiguous cache-write
/// duration are never treated as free. Reasoning tokens are informational
/// because they are already a subset of output tokens.
#[must_use]
pub fn value_model_segments(
    card: &PriceCard,
    segments: &[ModelUsageSegment],
    segments_complete: bool,
    provenance: ObservationProvenance,
) -> ApiEquivalent {
    let mut amount = 0.0;
    let mut priced_any = false;
    let mut missing = BTreeSet::new();

    if !segments_complete {
        missing.insert("unattributed_model_segment".to_owned());
    }

    for segment in segments {
        if !valid_reasoning_subset(segment) {
            return ApiEquivalent::Unavailable {
                reason: UnavailableReason::MalformedSource,
            };
        }
        let Some(rate) = card.rate_for(&segment.model) else {
            if segment_has_usage(segment) {
                missing.insert(format!("model:{}", segment.model));
            }
            continue;
        };
        let rates = rate.rates_usd_per_million_tokens;

        match (
            segment.input_tokens.measured(),
            segment.cached_input_tokens.measured(),
            segment.cache_write_tokens.measured(),
        ) {
            (Some(input), Some(cached), Some(cache_write)) => {
                let Some(cached_and_write) = cached.checked_add(cache_write) else {
                    return malformed();
                };
                let Some(fresh) = input.checked_sub(cached_and_write) else {
                    return malformed();
                };
                amount += per_million(fresh, rates.input);
                priced_any = true;
                if cached > 0 {
                    if let Some(cached_rate) = rates.cached_input {
                        amount += per_million(cached, cached_rate);
                    } else {
                        missing.insert(format!("{}:cached_input", segment.model));
                    }
                }
                if cache_write > 0 {
                    match (rates.cache_write_5m, rates.cache_write_1h) {
                        (Some(short), Some(long)) if (short - long).abs() < f64::EPSILON => {
                            amount += per_million(cache_write, short);
                        }
                        (Some(_), Some(_)) => {
                            missing.insert(format!("{}:cache_write_duration", segment.model));
                        }
                        (Some(write), None) | (None, Some(write)) => {
                            amount += per_million(cache_write, write);
                        }
                        (None, None) => {
                            missing.insert(format!("{}:cache_write", segment.model));
                        }
                    }
                }
            }
            _ => {
                missing.insert(format!("{}:input_categories", segment.model));
            }
        }

        if let Some(output) = segment.output_tokens.measured() {
            amount += per_million(output, rates.output);
            priced_any = true;
        } else {
            missing.insert(format!("{}:output", segment.model));
        }
    }

    if !priced_any {
        return ApiEquivalent::Unavailable {
            reason: UnavailableReason::MissingPricingCategory,
        };
    }
    let Ok(amount_usd) = NonNegativeFinite::new(amount, "API-equivalent USD") else {
        return malformed();
    };
    let coverage = if missing.is_empty() {
        PricingCoverage::Complete
    } else {
        PricingCoverage::Partial {
            missing: missing.into_iter().collect(),
        }
    };
    ApiEquivalent::Estimated {
        amount_usd,
        coverage,
        basis: card.basis(),
        provenance,
    }
}

fn valid_reasoning_subset(segment: &ModelUsageSegment) -> bool {
    match (
        segment.reasoning_output_tokens.measured(),
        segment.output_tokens.measured(),
    ) {
        (Some(reasoning), Some(output)) => reasoning <= output,
        _ => true,
    }
}

fn segment_has_usage(segment: &ModelUsageSegment) -> bool {
    [
        segment.input_tokens,
        segment.cached_input_tokens,
        segment.cache_write_tokens,
        segment.output_tokens,
    ]
    .into_iter()
    .any(|count| count.measured().is_none_or(|tokens| tokens > 0))
}

fn per_million(tokens: u64, rate: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let tokens = tokens as f64;
    tokens * rate / 1_000_000.0
}

fn malformed() -> ApiEquivalent {
    ApiEquivalent::Unavailable {
        reason: UnavailableReason::MalformedSource,
    }
}
