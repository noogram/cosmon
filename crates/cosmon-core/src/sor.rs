// SPDX-License-Identifier: AGPL-3.0-only

//! Budget-aware Smart Order Routing (SOR) and the authoritative ex-post
//! routing receipt (ADR-152, C3 of `delib-20260711-c6c8`).
//!
//! # Why this module exists
//!
//! ADR-142 places SOR as a **policy** above the launch-time `Incarnation`
//! selector, never a new core primitive: its output must be exactly one partial
//! `Incarnation` entering the single `cs tackle` resolution fold. C1 (ADR-150)
//! shipped the named directional policy that *produces* that partial
//! `Incarnation`; C2 (ADR-151) shipped the monotone criticality fold that says
//! *how much assurance* a subject demands. This module is C3: the pure ranking
//! that, given admissible venues and their observed state, **chooses the seat**
//! — and the durable receipt that records *why*.
//!
//! # Two failures the old surface could not distinguish
//!
//! 1. **Ex-ante events, mutable sidecar.** The existing `AdapterSelected` /
//!    `ModelSelected` events are minted *before* the availability probe, and
//!    `model-selection.json` is a mutable sidecar. Neither is an authoritative
//!    *ex-post* record of the decision the router actually made. A retrospective
//!    audit cannot replay "which venues were considered, what was observed, how
//!    they scored, why this one won". [`RoutingReceipt`] is that record — sealed
//!    with a content hash, appended once, replayed (not recomputed) on restart.
//!
//! 2. **`unreadable → empty` conflated with `absent → zero`.** The strong-model
//!    ceiling folds `events.jsonl` for the in-window strong-dispatch count. If
//!    the log is *unreadable* the old fold returned an empty vector — i.e. a
//!    count of **zero**, which *opens* the budget gate exactly when the evidence
//!    is missing. A budget ceiling must **fail closed**: unknown local history is
//!    [`LocalConsumption::Unavailable`], never `Available(0)`. See
//!    [`crate::model_budget::LocalHistory`] for the corrected fold semantics this
//!    module consumes.
//!
//! # The shape of a decision
//!
//! [`select`] is a **pure, total, deterministic** function:
//!
//! 1. **Hard filter.** Every candidate is checked against the request's hard
//!    constraints ([`admit`]): spawnability + carrier parity, an honoured literal
//!    pin, capacity, *fresh* calibration when the subject is critical, a known
//!    local allowance, and diversity. A rejected candidate is recorded with a
//!    typed [`RejectReason`]; it never silently disappears.
//! 2. **Score.** Survivors get a **versioned integer score** ([`score_candidate`],
//!    [`SCORE_VERSION`]) over quality, headroom, availability, cost, and a
//!    staleness penalty. Integer arithmetic keeps the order total and free of
//!    float-NaN hazards.
//! 3. **Tie-break.** Survivors are ordered by `(score desc, adapter asc, model
//!    asc)` — a **total** order, so the winner is deterministic even on ties.
//! 4. **Receipt or typed refusal.** If no candidate is admissible the result is a
//!    typed [`SorRefusal::NoAdmissibleCandidate`] carrying every reject — the
//!    router **never** falls back to a global provider default. Otherwise the
//!    result is a [`SorDecision`] with the chosen partial `Incarnation` and a
//!    sealed [`RoutingReceipt`].
//!
//! # Local consumption vs external observation
//!
//! The module keeps two provenance classes strictly apart, because they fail
//! differently:
//!
//! - **Local-attributed consumption** ([`LocalConsumption`]) is a fold over *our
//!   own* `events.jsonl`. Its only two honest states are `Available(n)` and
//!   `Unavailable` — there is no "stale" local fold, and `Unavailable` is never
//!   `0`.
//! - **External observations** ([`Observation`]) — quota, price, provider load,
//!   calibration freshness — are *reports about the world* that decay. They carry
//!   a value, a source, an `observed_at`, a TTL, a derived [`ObservationStatus`],
//!   and a content hash, so the receipt records exactly what was believed and how
//!   fresh it was.
//!
//! # Zero I/O
//!
//! Like [`crate::model_budget`] and [`crate::calibration`], this module is pure.
//! `now` is always supplied by the caller; nothing here reads a clock, a file, or
//! a socket. The seam that folds the event log, probes availability, and appends
//! the receipt is the `cs tackle` shell.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::criticality::CriticalityLevel;

/// The scoring-function version. Bumped whenever the *meaning* of a score
/// changes (new term, re-weighting, different normalisation) so two receipts are
/// only score-comparable when their [`CandidateScore::version`] agree — the same
/// discipline [`crate::calibration`] applies to `corpus_rev`.
pub const SCORE_VERSION: u32 = 1;

/// The [`RoutingReceipt`] schema version, bumped on any breaking field change so
/// a replayed receipt is only trusted when its schema matches.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Freshness verdict for one external [`Observation`], derived from its
/// `observed_at` + TTL against the decision instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationStatus {
    /// Observed within its TTL — usable at full weight.
    Fresh,
    /// Observed, but older than its TTL — usable, but penalised and flagged.
    Stale,
    /// Never observed (no report available) — contributes no signal.
    Missing,
    /// The *local* history the observation would fold was unreadable. Distinct
    /// from [`Self::Missing`]: missing is "no evidence exists"; this is "evidence
    /// may exist but we could not read it", and a budget gate must **fail closed**
    /// on it, never treat it as zero.
    HistoryUnavailable,
}

impl ObservationStatus {
    /// Classify an observation from its `observed_at` and `ttl` against `now`.
    /// `None` `observed_at` → [`Self::Missing`]. A non-negative age within `ttl`
    /// is [`Self::Fresh`]; beyond `ttl` is [`Self::Stale`]. A future timestamp
    /// (clock skew) is treated as [`Self::Fresh`] (age clamped to zero).
    #[must_use]
    pub fn classify(observed_at: Option<DateTime<Utc>>, ttl: Duration, now: DateTime<Utc>) -> Self {
        match observed_at {
            None => Self::Missing,
            Some(at) => {
                let age = now.signed_duration_since(at);
                if age <= ttl {
                    Self::Fresh
                } else {
                    Self::Stale
                }
            }
        }
    }

    /// Whether the observation carries a usable value (fresh or stale). Missing
    /// and unavailable carry none.
    #[must_use]
    pub fn has_value(self) -> bool {
        matches!(self, Self::Fresh | Self::Stale)
    }
}

/// Where an external observation came from — the provenance axis the receipt
/// keeps separate from local-attributed consumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationSource {
    /// A live probe of the provider endpoint (availability / load).
    Probe,
    /// A published or configured price / quota table.
    PriceTable,
    /// The judgment-quality calibration snapshot ([`crate::calibration`]).
    Calibration,
    /// An operator-declared config value.
    Config,
}

/// One decaying report about the world, carried through the router and recorded
/// verbatim in the receipt: a value, a source, an `observed_at`, a `TTL`, a
/// derived status, and a content hash.
///
/// The value is `Option` because [`ObservationStatus::Missing`] /
/// [`ObservationStatus::HistoryUnavailable`] carry none. The generic `T` is the
/// axis payload (an integer band for quality / availability / cost).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation<T> {
    /// The observed value, or `None` when the status carries no signal.
    pub value: Option<T>,
    /// Provenance class.
    pub source: ObservationSource,
    /// When the value was observed (`None` when never observed).
    pub observed_at: Option<DateTime<Utc>>,
    /// How long the value stays [`ObservationStatus::Fresh`], in seconds — stored
    /// as a plain integer so the envelope serialises without a chrono newtype.
    pub ttl_secs: i64,
    /// Freshness verdict, derived at decision time.
    pub status: ObservationStatus,
    /// Content hash of the observation (`blake3:<hex>`), so a receipt audit can
    /// detect a silently-rewritten observation.
    pub hash: String,
}

impl<T: Serialize> Observation<T> {
    /// Build a fully-classified observation from a value and its freshness
    /// inputs, computing the derived status and content hash. A `None` value
    /// yields [`ObservationStatus::Missing`].
    #[must_use]
    pub fn observed(
        value: Option<T>,
        source: ObservationSource,
        observed_at: Option<DateTime<Utc>>,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Self {
        let status = if value.is_none() {
            ObservationStatus::Missing
        } else {
            ObservationStatus::classify(observed_at, ttl, now)
        };
        let mut obs = Self {
            value,
            source,
            observed_at,
            ttl_secs: ttl.num_seconds(),
            status,
            hash: String::new(),
        };
        obs.hash = obs.compute_hash();
        obs
    }

    /// An explicitly [`ObservationStatus::Missing`] observation (no signal).
    #[must_use]
    pub fn missing(source: ObservationSource) -> Self {
        let mut obs = Self {
            value: None,
            source,
            observed_at: None,
            ttl_secs: 0,
            status: ObservationStatus::Missing,
            hash: String::new(),
        };
        obs.hash = obs.compute_hash();
        obs
    }

    /// The content hash of everything but the `hash` field itself.
    fn compute_hash(&self) -> String {
        let canonical = serde_json::json!({
            "value": self.value,
            "source": self.source,
            "observed_at": self.observed_at,
            "ttl_secs": self.ttl_secs,
            "status": self.status,
        });
        hash_canonical(&canonical)
    }
}

/// Local-attributed consumption folded from *our own* `events.jsonl` — the
/// budget-headroom signal. Unlike an [`Observation`] it does not decay; it is
/// either a known count or unknown.
///
/// The two states are deliberately asymmetric: `Available(0)` means "we read the
/// log and there were no dispatches" (open budget), whereas [`Self::Unavailable`]
/// means "we could not read the log" — and a budget gate must **fail closed** on
/// the latter. Conflating them is the `unreadable → empty` bug this module
/// exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "count")]
pub enum LocalConsumption {
    /// The event log was read; `n` matching dispatches were counted in-window.
    Available(u32),
    /// The event log could not be read — the count is *unknown*, not zero.
    Unavailable,
}

impl LocalConsumption {
    /// Remaining headroom under `cap`, or `None` when consumption is unknown
    /// (the caller must fail closed). `Available(n)` yields `cap - n` clamped at
    /// zero.
    #[must_use]
    pub fn headroom(self, cap: u32) -> Option<u32> {
        match self {
            Self::Available(n) => Some(cap.saturating_sub(n)),
            Self::Unavailable => None,
        }
    }

    /// Whether local history is known (readable). `false` for
    /// [`Self::Unavailable`].
    #[must_use]
    pub fn is_known(self) -> bool {
        matches!(self, Self::Available(_))
    }
}

/// Calibration freshness for a candidate's `(adapter, model-version)` — the
/// judgment-quality signal, made freshness-aware (the axis
/// [`crate::calibration::CalibrationSnapshot`] gained in this change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationObs {
    /// The model version the snapshot scored, if any.
    pub model_version: Option<String>,
    /// Judgment-quality score in per-mille (0..=1000), `None` when unmeasured.
    pub score_permille: Option<i64>,
    /// When the snapshot was measured.
    pub measured_at: Option<DateTime<Utc>>,
    /// Freshness verdict derived at decision time.
    pub status: ObservationStatus,
}

impl CalibrationObs {
    /// Classify a calibration reading against `now` + `ttl`.
    #[must_use]
    pub fn new(
        model_version: Option<String>,
        score_permille: Option<i64>,
        measured_at: Option<DateTime<Utc>>,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Self {
        let status = if score_permille.is_none() {
            ObservationStatus::Missing
        } else {
            ObservationStatus::classify(measured_at, ttl, now)
        };
        Self {
            model_version,
            score_permille,
            measured_at,
            status,
        }
    }

    /// An unmeasured calibration (no snapshot for this seat).
    #[must_use]
    pub fn missing() -> Self {
        Self {
            model_version: None,
            score_permille: None,
            measured_at: None,
            status: ObservationStatus::Missing,
        }
    }
}

/// One venue the router may choose — a resolved `(adapter, model)` seat plus its
/// hard-constraint flags and observed state.
///
/// The four booleans are *independent* admissibility gates, each with a distinct
/// [`RejectReason`] — not a bag of accidental flags. Bundling them into a
/// sub-struct would only obscure that each is a separately-resolved fact, so the
/// `struct_excessive_bools` lint is waived here deliberately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorCandidate {
    /// Resolved adapter/carrier name.
    pub adapter: String,
    /// Resolved model id, `None` for the adapter's own floor.
    pub model: Option<String>,
    /// Optional effort hint carried into the chosen `Incarnation`.
    pub effort: Option<String>,
    /// Whether the carrier can actually transport this slot to its launch
    /// surface (carrier parity, ADR-150). A model pin on a carrier with no model
    /// carrier is **not** spawnable.
    pub spawnable: bool,
    /// Whether this candidate satisfies an active literal pin. `true` when no pin
    /// is in force. A `false` here means "a pin exists and this seat is not it".
    pub honors_pin: bool,
    /// Whether the venue has capacity right now.
    pub capacity_ok: bool,
    /// Whether this seat meets the diversity requirement (e.g. distinct provider
    /// family from the generator). `true` when no diversity requirement applies.
    pub diversity_ok: bool,
    /// Local-attributed budget consumption for this adapter (fold over our log).
    pub consumption: LocalConsumption,
    /// The per-galaxy strong-dispatch cap for this adapter, if configured.
    pub budget_cap: Option<u32>,
    /// Judgment-quality calibration for this seat.
    pub calibration: CalibrationObs,
    /// External availability / inverse-load, per-mille (higher is better).
    pub availability: Observation<i64>,
    /// External cost band (lower is cheaper). Compared relatively across
    /// candidates.
    pub cost: Observation<i64>,
}

impl SorCandidate {
    /// Stable identity for receipts/diagnostics: `adapter` or `adapter/model`.
    #[must_use]
    pub fn id(&self) -> String {
        match &self.model {
            Some(m) => format!("{}/{m}", self.adapter),
            None => self.adapter.clone(),
        }
    }
}

/// The typed reason a candidate was filtered out at the hard-constraint stage.
/// Every reject is recorded on the receipt; none is silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RejectReason {
    /// The carrier cannot transport this slot (carrier parity / spawnability).
    NotSpawnable,
    /// A literal pin is in force and this seat is not it.
    PinMismatch,
    /// The venue has no capacity.
    CapacityExhausted,
    /// The subject is critical and this seat has no calibration reading.
    MissingCalibrationOnCritical,
    /// The subject is critical and this seat's calibration is stale.
    StaleCalibrationOnCritical,
    /// The subject is critical and local budget history is unreadable — fail
    /// closed rather than route on unknown consumption.
    LocalHistoryUnavailableOnCritical,
    /// The known budget headroom for this adapter is exhausted.
    BudgetExhausted,
    /// The seat does not meet the diversity requirement.
    DiversityRequirementUnmet,
}

impl RejectReason {
    /// A stable, human-auditable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotSpawnable => "not-spawnable",
            Self::PinMismatch => "pin-mismatch",
            Self::CapacityExhausted => "capacity-exhausted",
            Self::MissingCalibrationOnCritical => "missing-calibration-on-critical",
            Self::StaleCalibrationOnCritical => "stale-calibration-on-critical",
            Self::LocalHistoryUnavailableOnCritical => "local-history-unavailable-on-critical",
            Self::BudgetExhausted => "budget-exhausted",
            Self::DiversityRequirementUnmet => "diversity-requirement-unmet",
        }
    }
}

/// The request context the router filters and scores against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorRequest {
    /// The subject (molecule) id.
    pub subject: String,
    /// The subject revision the decision applies to.
    pub revision: String,
    /// Effective criticality (the C2 monotone fold's result).
    pub criticality: CriticalityLevel,
    /// The decisive criticality actors (provenance carried into the receipt).
    pub criticality_actors: Vec<String>,
    /// Digest of the named routing policy/profile that produced the candidates
    /// (`blake3:<hex>` or a stable policy label).
    pub policy_digest: String,
    /// This dispatch attempt (1-based). A restart of the same attempt replays the
    /// existing receipt rather than recomputing.
    pub attempt: u32,
    /// The attempt this decision supersedes, if any.
    pub supersedes: Option<u32>,
}

/// A recorded rejection: candidate id + reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectRecord {
    /// The rejected candidate's [`SorCandidate::id`].
    pub candidate: String,
    /// Why it was rejected.
    pub reason: RejectReason,
}

/// The integer, versioned score of one admissible candidate. Every term is
/// recorded so the receipt shows *how* the winner was chosen, not just that it
/// won.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateScore {
    /// The candidate's [`SorCandidate::id`].
    pub candidate: String,
    /// The scoring-function version ([`SCORE_VERSION`]).
    pub version: u32,
    /// Quality contribution (calibration score × weight).
    pub quality_term: i64,
    /// Headroom contribution (remaining budget × weight).
    pub headroom_term: i64,
    /// Availability contribution.
    pub availability_term: i64,
    /// Cost contribution (negative: cheaper scores higher).
    pub cost_term: i64,
    /// Staleness penalty (subtracted).
    pub stale_penalty: i64,
    /// The total (sum of the terms above).
    pub total: i64,
}

/// Integer weights for the five scoring terms. Tunable and testable; the
/// defaults encode "quality first, then headroom, then availability, then cost".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreWeights {
    /// Weight on calibration quality (per-mille).
    pub quality: i64,
    /// Weight on remaining budget headroom.
    pub headroom: i64,
    /// Weight on availability (per-mille).
    pub availability: i64,
    /// Weight on cost (applied to the negated cost band).
    pub cost: i64,
    /// Flat penalty per stale observation used.
    pub stale_penalty: i64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            quality: 10,
            headroom: 5,
            availability: 3,
            cost: 2,
            stale_penalty: 1_000,
        }
    }
}

/// The chosen partial `Incarnation` — the single output that re-enters the
/// `cs tackle` resolution fold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenIncarnation {
    /// The chosen adapter/carrier.
    pub adapter: String,
    /// The chosen model id, `None` for the adapter floor.
    pub model: Option<String>,
    /// The chosen effort hint, if any.
    pub effort: Option<String>,
}

/// The authoritative, append-once, ex-post record of one routing decision — the
/// payload of the `RoutingDecisionRecorded` / `IncarnationDecided` event.
///
/// It is **sealed** with a content hash ([`Self::seal`]): the hash covers every
/// field but itself, so a later audit can detect a silently-rewritten receipt.
/// Two [`select`] runs over byte-identical inputs produce byte-identical
/// receipts (and hashes) — which is exactly what makes *replay on restart* safe:
/// the consumer re-emits the recorded receipt rather than recomputing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingReceipt {
    /// Receipt schema version ([`RECEIPT_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Scoring-function version ([`SCORE_VERSION`]).
    pub score_version: u32,
    /// Subject (molecule) id.
    pub subject: String,
    /// Subject revision.
    pub revision: String,
    /// This dispatch attempt.
    pub attempt: u32,
    /// The attempt this supersedes, if any.
    pub supersedes: Option<u32>,
    /// Digest of the policy/profile that produced the candidates.
    pub policy_digest: String,
    /// Effective criticality.
    pub criticality: CriticalityLevel,
    /// Decisive criticality actors (provenance).
    pub criticality_actors: Vec<String>,
    /// When the decision was taken (caller-supplied).
    pub decided_at: DateTime<Utc>,
    /// Every candidate considered, by id.
    pub candidates_considered: Vec<String>,
    /// Every rejected candidate + reason.
    pub rejects: Vec<RejectRecord>,
    /// Score breakdown for each admissible candidate.
    pub scores: Vec<CandidateScore>,
    /// The chosen partial `Incarnation`.
    pub chosen: ChosenIncarnation,
    /// Content hash (`blake3:<hex>`) of every field but this one.
    pub receipt_hash: String,
}

impl RoutingReceipt {
    /// Seal the receipt: compute the content hash over every field but
    /// `receipt_hash` and store it. Deterministic in the receipt's contents.
    #[must_use]
    pub fn seal(mut self) -> Self {
        self.receipt_hash = String::new();
        let canonical = serde_json::to_value(&self).unwrap_or(serde_json::Value::Null);
        self.receipt_hash = hash_canonical(&canonical);
        self
    }

    /// Whether the stored hash matches a re-computation over the current
    /// contents — the retrospective tamper check.
    #[must_use]
    pub fn verify_hash(&self) -> bool {
        let mut probe = self.clone();
        probe.receipt_hash = String::new();
        let canonical = serde_json::to_value(&probe).unwrap_or(serde_json::Value::Null);
        hash_canonical(&canonical) == self.receipt_hash
    }
}

/// A successful routing decision: the chosen `Incarnation` and its sealed
/// receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SorDecision {
    /// The chosen partial `Incarnation`.
    pub chosen: ChosenIncarnation,
    /// The sealed, authoritative receipt.
    pub receipt: RoutingReceipt,
}

/// Why the router produced no decision. It never falls back to a global provider
/// default — an empty admissible set is a typed refusal the caller must surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum SorRefusal {
    /// No candidate survived the hard-constraint filter. Every reject is carried
    /// so the operator sees *why* every venue was ineligible.
    #[error("no admissible routing candidate ({} rejected)", .rejects.len())]
    NoAdmissibleCandidate {
        /// The subject id.
        subject: String,
        /// The full reject list.
        rejects: Vec<RejectRecord>,
    },
    /// The candidate list was empty to begin with — nothing was offered.
    #[error("no routing candidates offered for `{subject}`")]
    NoCandidatesOffered {
        /// The subject id.
        subject: String,
    },
}

/// Apply the hard-constraint filter to one candidate. `Ok(())` = admissible;
/// `Err(reason)` = the typed rejection recorded on the receipt.
///
/// The order encodes precedence of *why*: a pin mismatch is reported before
/// capacity, which is reported before the critical-only calibration/budget gates.
/// The critical gates are the fail-closed heart: a critical subject may not route
/// to a seat with missing/stale calibration or unknown local budget history.
pub fn admit(candidate: &SorCandidate, request: &SorRequest) -> Result<(), RejectReason> {
    if !candidate.spawnable {
        return Err(RejectReason::NotSpawnable);
    }
    if !candidate.honors_pin {
        return Err(RejectReason::PinMismatch);
    }
    if !candidate.capacity_ok {
        return Err(RejectReason::CapacityExhausted);
    }
    if !candidate.diversity_ok {
        return Err(RejectReason::DiversityRequirementUnmet);
    }

    let critical = candidate_is_critical(request.criticality);
    if critical {
        match candidate.calibration.status {
            ObservationStatus::Missing | ObservationStatus::HistoryUnavailable => {
                return Err(RejectReason::MissingCalibrationOnCritical);
            }
            ObservationStatus::Stale => {
                return Err(RejectReason::StaleCalibrationOnCritical);
            }
            ObservationStatus::Fresh => {}
        }
        // Fail closed: unknown local budget history on a critical subject is
        // ineligible, never "assume zero used".
        if !candidate.consumption.is_known() {
            return Err(RejectReason::LocalHistoryUnavailableOnCritical);
        }
    }

    // Budget headroom: a *known* exhausted budget is a hard reject; an *unknown*
    // budget is only fatal on critical (handled above) — on non-critical it is
    // permitted but scored conservatively (no headroom bonus).
    if let Some(cap) = candidate.budget_cap {
        if let Some(headroom) = candidate.consumption.headroom(cap) {
            if headroom == 0 {
                return Err(RejectReason::BudgetExhausted);
            }
        }
    }

    Ok(())
}

/// Whether a criticality level demands the fail-closed gates.
#[must_use]
fn candidate_is_critical(level: CriticalityLevel) -> bool {
    level.requires_committee()
}

/// Score one admissible candidate with the given weights. Pure integer
/// arithmetic; a missing observation contributes zero, a stale one is penalised.
#[must_use]
pub fn score_candidate(candidate: &SorCandidate, weights: &ScoreWeights) -> CandidateScore {
    let quality = candidate.calibration.score_permille.unwrap_or(0);
    let quality_term = quality * weights.quality;

    let headroom_term = candidate
        .budget_cap
        .and_then(|cap| candidate.consumption.headroom(cap))
        .map_or(0, |h| i64::from(h) * weights.headroom);

    let availability_term = candidate.availability.value.unwrap_or(0) * weights.availability;

    // Cheaper is better: negate the cost band so a low cost lifts the total.
    let cost_term = -candidate.cost.value.unwrap_or(0) * weights.cost;

    let mut stale_penalty = 0;
    if candidate.availability.status == ObservationStatus::Stale {
        stale_penalty += weights.stale_penalty;
    }
    if candidate.cost.status == ObservationStatus::Stale {
        stale_penalty += weights.stale_penalty;
    }
    if candidate.calibration.status == ObservationStatus::Stale {
        stale_penalty += weights.stale_penalty;
    }

    let total = quality_term + headroom_term + availability_term + cost_term - stale_penalty;

    CandidateScore {
        candidate: candidate.id(),
        version: SCORE_VERSION,
        quality_term,
        headroom_term,
        availability_term,
        cost_term,
        stale_penalty,
        total,
    }
}

/// Run the full SOR decision: hard filter → score → total tie-break → receipt or
/// typed refusal. Pure, total, and deterministic in `(request, candidates,
/// weights, now)`.
///
/// # Errors
/// [`SorRefusal::NoCandidatesOffered`] when `candidates` is empty, or
/// [`SorRefusal::NoAdmissibleCandidate`] when none survives the hard filter. The
/// router never substitutes a global provider default.
pub fn select(
    request: &SorRequest,
    candidates: &[SorCandidate],
    weights: &ScoreWeights,
    now: DateTime<Utc>,
) -> Result<SorDecision, SorRefusal> {
    if candidates.is_empty() {
        return Err(SorRefusal::NoCandidatesOffered {
            subject: request.subject.clone(),
        });
    }

    let considered: Vec<String> = candidates.iter().map(SorCandidate::id).collect();
    let mut rejects = Vec::new();
    let mut admissible = Vec::new();
    for candidate in candidates {
        match admit(candidate, request) {
            Ok(()) => admissible.push(candidate),
            Err(reason) => rejects.push(RejectRecord {
                candidate: candidate.id(),
                reason,
            }),
        }
    }

    if admissible.is_empty() {
        return Err(SorRefusal::NoAdmissibleCandidate {
            subject: request.subject.clone(),
            rejects,
        });
    }

    // Score every survivor, then order by the TOTAL tie-break:
    // (score desc, adapter asc, model asc). A total order → deterministic winner.
    let mut ranked: Vec<(&SorCandidate, CandidateScore)> = admissible
        .iter()
        .map(|c| (*c, score_candidate(c, weights)))
        .collect();
    ranked.sort_by(|(a, sa), (b, sb)| {
        sb.total
            .cmp(&sa.total)
            .then_with(|| a.adapter.cmp(&b.adapter))
            .then_with(|| a.model.cmp(&b.model))
    });

    let winner = ranked[0].0;
    let chosen = ChosenIncarnation {
        adapter: winner.adapter.clone(),
        model: winner.model.clone(),
        effort: winner.effort.clone(),
    };
    let scores: Vec<CandidateScore> = ranked.into_iter().map(|(_, s)| s).collect();

    let receipt = RoutingReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        score_version: SCORE_VERSION,
        subject: request.subject.clone(),
        revision: request.revision.clone(),
        attempt: request.attempt,
        supersedes: request.supersedes,
        policy_digest: request.policy_digest.clone(),
        criticality: request.criticality,
        criticality_actors: request.criticality_actors.clone(),
        decided_at: now,
        candidates_considered: considered,
        rejects,
        scores,
        chosen: chosen.clone(),
        receipt_hash: String::new(),
    }
    .seal();

    Ok(SorDecision { chosen, receipt })
}

/// BLAKE3 content hash of a value over its **canonical** (key-sorted,
/// whitespace-free) serialization, `blake3:<64-hex>` — the same prefix
/// convention as [`crate::avatar`], via the shared [`cosmon_hash`] plumbing so
/// two semantically-identical receipts always hash identically regardless of
/// field order.
#[must_use]
fn hash_canonical(value: &serde_json::Value) -> String {
    match cosmon_hash::hash_value(value) {
        Ok(h) => format!("blake3:{}", h.to_hex()),
        Err(_) => "blake3:unhashable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn fresh_avail(v: i64) -> Observation<i64> {
        Observation::observed(
            Some(v),
            ObservationSource::Probe,
            Some(now() - Duration::minutes(1)),
            Duration::hours(1),
            now(),
        )
    }

    fn fresh_cost(v: i64) -> Observation<i64> {
        Observation::observed(
            Some(v),
            ObservationSource::PriceTable,
            Some(now() - Duration::minutes(1)),
            Duration::hours(1),
            now(),
        )
    }

    fn fresh_calib(score: i64) -> CalibrationObs {
        CalibrationObs::new(
            Some("v1".into()),
            Some(score),
            Some(now() - Duration::minutes(1)),
            Duration::hours(24),
            now(),
        )
    }

    fn candidate(adapter: &str, model: Option<&str>) -> SorCandidate {
        SorCandidate {
            adapter: adapter.into(),
            model: model.map(str::to_string),
            effort: None,
            spawnable: true,
            honors_pin: true,
            capacity_ok: true,
            diversity_ok: true,
            consumption: LocalConsumption::Available(0),
            budget_cap: Some(10),
            calibration: fresh_calib(800),
            availability: fresh_avail(900),
            cost: fresh_cost(100),
        }
    }

    fn request(level: CriticalityLevel) -> SorRequest {
        SorRequest {
            subject: "task-1".into(),
            revision: "rev-1".into(),
            criticality: level,
            criticality_actors: vec!["operator".into()],
            policy_digest: "policy:default".into(),
            attempt: 1,
            supersedes: None,
        }
    }

    #[test]
    fn status_classify_fresh_stale_missing() {
        let ttl = Duration::hours(1);
        assert_eq!(
            ObservationStatus::classify(Some(now() - Duration::minutes(30)), ttl, now()),
            ObservationStatus::Fresh
        );
        assert_eq!(
            ObservationStatus::classify(Some(now() - Duration::hours(2)), ttl, now()),
            ObservationStatus::Stale
        );
        assert_eq!(
            ObservationStatus::classify(None, ttl, now()),
            ObservationStatus::Missing
        );
        // Future timestamp (clock skew) → fresh, not a negative-age panic.
        assert_eq!(
            ObservationStatus::classify(Some(now() + Duration::minutes(5)), ttl, now()),
            ObservationStatus::Fresh
        );
    }

    #[test]
    fn local_consumption_unavailable_is_not_zero() {
        // The load-bearing distinction: unavailable history yields no headroom,
        // whereas Available(0) yields the full cap.
        assert_eq!(LocalConsumption::Available(0).headroom(10), Some(10));
        assert_eq!(LocalConsumption::Available(3).headroom(10), Some(7));
        assert_eq!(LocalConsumption::Available(20).headroom(10), Some(0)); // saturating
        assert_eq!(LocalConsumption::Unavailable.headroom(10), None);
        assert!(LocalConsumption::Available(0).is_known());
        assert!(!LocalConsumption::Unavailable.is_known());
    }

    #[test]
    fn deterministic_selection_and_receipt_hash() {
        let cands = vec![
            candidate("claude", Some("claude-opus-4-8")),
            candidate("openai", Some("gpt-5")),
        ];
        let req = request(CriticalityLevel::Routine);
        let w = ScoreWeights::default();
        let a = select(&req, &cands, &w, now()).expect("a decision");
        let b = select(&req, &cands, &w, now()).expect("a decision");
        // Byte-identical inputs → byte-identical receipt (replay-safe).
        assert_eq!(a.receipt.receipt_hash, b.receipt.receipt_hash);
        assert!(a.receipt.verify_hash());
        // Both seats are equal on quality/headroom/availability/cost, so the
        // tie-break falls to adapter asc: claude < openai.
        assert_eq!(a.chosen.adapter, "claude");
    }

    #[test]
    fn higher_quality_wins() {
        let mut good = candidate("openai", Some("gpt-5"));
        good.calibration = fresh_calib(950);
        let mut weak = candidate("claude", Some("claude-opus-4-8"));
        weak.calibration = fresh_calib(200);
        let req = request(CriticalityLevel::Routine);
        let dec = select(&req, &[weak, good], &ScoreWeights::default(), now()).unwrap();
        assert_eq!(dec.chosen.adapter, "openai");
    }

    #[test]
    fn missing_calibration_on_critical_is_ineligible() {
        let mut c = candidate("claude", Some("claude-opus-4-8"));
        c.calibration = CalibrationObs::missing();
        let req = request(CriticalityLevel::Security);
        let err = select(&req, &[c], &ScoreWeights::default(), now()).unwrap_err();
        match err {
            SorRefusal::NoAdmissibleCandidate { rejects, .. } => {
                assert_eq!(rejects.len(), 1);
                assert_eq!(
                    rejects[0].reason,
                    RejectReason::MissingCalibrationOnCritical
                );
            }
            SorRefusal::NoCandidatesOffered { .. } => panic!("expected NoAdmissibleCandidate"),
        }
    }

    #[test]
    fn stale_calibration_on_critical_is_ineligible_but_fine_when_routine() {
        let mut c = candidate("claude", Some("claude-opus-4-8"));
        c.calibration = CalibrationObs::new(
            Some("v1".into()),
            Some(800),
            Some(now() - Duration::hours(48)), // older than the 24h ttl
            Duration::hours(24),
            now(),
        );
        assert_eq!(c.calibration.status, ObservationStatus::Stale);
        // Critical: rejected.
        let crit = select(
            &request(CriticalityLevel::Root),
            std::slice::from_ref(&c),
            &ScoreWeights::default(),
            now(),
        )
        .unwrap_err();
        assert!(matches!(crit, SorRefusal::NoAdmissibleCandidate { .. }));
        // Routine: admissible, but the stale penalty applies.
        let routine = select(
            &request(CriticalityLevel::Routine),
            &[c],
            &ScoreWeights::default(),
            now(),
        )
        .unwrap();
        assert!(routine.receipt.scores[0].stale_penalty > 0);
    }

    #[test]
    fn unknown_local_history_fails_closed_on_critical() {
        let mut c = candidate("claude", Some("claude-opus-4-8"));
        c.consumption = LocalConsumption::Unavailable;
        // Critical → fail closed (never assume zero used).
        let crit = select(
            &request(CriticalityLevel::Max),
            std::slice::from_ref(&c),
            &ScoreWeights::default(),
            now(),
        )
        .unwrap_err();
        match crit {
            SorRefusal::NoAdmissibleCandidate { rejects, .. } => assert_eq!(
                rejects[0].reason,
                RejectReason::LocalHistoryUnavailableOnCritical
            ),
            SorRefusal::NoCandidatesOffered { .. } => panic!("expected fail-closed reject"),
        }
        // Routine → admissible, but scored with no headroom bonus.
        let routine = select(
            &request(CriticalityLevel::Routine),
            &[c],
            &ScoreWeights::default(),
            now(),
        )
        .unwrap();
        assert_eq!(routine.receipt.scores[0].headroom_term, 0);
    }

    #[test]
    fn exhausted_known_budget_is_rejected() {
        let mut c = candidate("claude", Some("claude-opus-4-8"));
        c.consumption = LocalConsumption::Available(10); // cap is 10 → headroom 0
        let err = select(
            &request(CriticalityLevel::Routine),
            &[c],
            &ScoreWeights::default(),
            now(),
        )
        .unwrap_err();
        match err {
            SorRefusal::NoAdmissibleCandidate { rejects, .. } => {
                assert_eq!(rejects[0].reason, RejectReason::BudgetExhausted);
            }
            SorRefusal::NoCandidatesOffered { .. } => panic!("expected budget reject"),
        }
    }

    #[test]
    fn not_spawnable_and_pin_mismatch_are_rejected_not_dropped() {
        let mut unspawnable = candidate("opencode", Some("gpt-5"));
        unspawnable.spawnable = false;
        let mut wrong_pin = candidate("claude", Some("claude-opus-4-8"));
        wrong_pin.honors_pin = false;
        let ok = candidate("openai", Some("gpt-5"));
        let dec = select(
            &request(CriticalityLevel::Routine),
            &[unspawnable, wrong_pin, ok],
            &ScoreWeights::default(),
            now(),
        )
        .unwrap();
        assert_eq!(dec.chosen.adapter, "openai");
        // Both bad candidates are recorded, none silently dropped.
        assert_eq!(dec.receipt.rejects.len(), 2);
        let reasons: Vec<_> = dec.receipt.rejects.iter().map(|r| r.reason).collect();
        assert!(reasons.contains(&RejectReason::NotSpawnable));
        assert!(reasons.contains(&RejectReason::PinMismatch));
    }

    #[test]
    fn empty_candidate_list_is_typed_refusal_never_global_default() {
        let err = select(
            &request(CriticalityLevel::Routine),
            &[],
            &ScoreWeights::default(),
            now(),
        )
        .unwrap_err();
        assert!(matches!(err, SorRefusal::NoCandidatesOffered { .. }));
    }

    #[test]
    fn restart_same_attempt_reproduces_identical_receipt() {
        // Replay safety: recomputing the same attempt yields the same sealed
        // receipt, so the consumer can replay rather than re-decide.
        let cands = vec![candidate("claude", Some("claude-opus-4-8"))];
        let req = request(CriticalityLevel::Root);
        let w = ScoreWeights::default();
        let first = select(&req, &cands, &w, now()).unwrap();
        let replay = select(&req, &cands, &w, now()).unwrap();
        assert_eq!(first.receipt, replay.receipt);
        assert_eq!(first.receipt.attempt, 1);
    }

    #[test]
    fn tampered_receipt_fails_hash_verification() {
        let cands = vec![candidate("claude", Some("claude-opus-4-8"))];
        let mut dec = select(
            &request(CriticalityLevel::Routine),
            &cands,
            &ScoreWeights::default(),
            now(),
        )
        .unwrap();
        assert!(dec.receipt.verify_hash());
        // Silently rewrite the chosen adapter without re-sealing.
        dec.receipt.chosen.adapter = "evil".into();
        assert!(!dec.receipt.verify_hash());
    }

    #[test]
    fn observation_missing_has_no_value_and_stable_hash() {
        let m: Observation<i64> = Observation::missing(ObservationSource::Probe);
        assert_eq!(m.status, ObservationStatus::Missing);
        assert!(m.value.is_none());
        let m2: Observation<i64> = Observation::missing(ObservationSource::Probe);
        assert_eq!(m.hash, m2.hash);
    }

    #[test]
    fn score_version_is_recorded_on_every_score() {
        let cands = vec![candidate("claude", Some("claude-opus-4-8"))];
        let dec = select(
            &request(CriticalityLevel::Routine),
            &cands,
            &ScoreWeights::default(),
            now(),
        )
        .unwrap();
        assert!(dec
            .receipt
            .scores
            .iter()
            .all(|s| s.version == SCORE_VERSION));
        assert_eq!(dec.receipt.score_version, SCORE_VERSION);
    }
}
