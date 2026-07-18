// SPDX-License-Identifier: AGPL-3.0-only

//! Unified `EventV2` schema — canonical sensor record for post-hoc fleet replay.
//!
//! `EventV2` is the source of truth for any reconstruction of a fleet run: every
//! record carries a monotone sequence number, a UTC timestamp, and an optional
//! `causal_parent` (the sequence of the event that triggered this one). The
//! schema is intentionally richer than the legacy [`crate::event::Event`] —
//! it unifies the disparate ad-hoc JSON shapes historically written to
//! `events.jsonl` into one enum that round-trips through serde.
//!
//! ## Invariants
//!
//! - `seq` is strictly monotone within a single `events.jsonl` file. A writer
//!   reads the last line to resume the sequence; readers can assume
//!   `envelopes[i].seq < envelopes[i+1].seq`.
//! - `timestamp` is UTC ISO8601 and is non-decreasing in practice, but the
//!   sequence number — not the clock — is authoritative for ordering.
//! - `causal_parent` — when present — refers to the `seq` of an earlier event
//!   in the same log. A missing `causal_parent` means the event is root-caused
//!   externally (operator command, timer tick, …).
//!
//! ## Writing
//!
//! All writes flow through [`Envelope::new`] and the
//! `EventLogWriter` adapter (so sequence numbering
//! is centralised). Legacy emission points remain but now also call the
//! writer; during the grace window both shapes coexist in `events.jsonl`.
//!
//! ## Reading
//!
//! [`Envelope::from_line`] parses one JSONL line — legacy lines are detected
//! and coerced best-effort via [`migrate_legacy_line`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::EnumCount;

use crate::expiry::ExpiryPolicy;
use crate::id::{MoleculeId, WorkerId};
use crate::quality_band::QualityBand;
use crate::spawn_seam::LoopOwnership;

/// Wire-side projection of [`LoopOwnership`] (ADR-103).
///
/// `LoopOwnership` is the typed call-site contract — exhaustive matches
/// and [`#[non_exhaustive]`] widening at the type level. The event log
/// is the strictest stability tier in cosmon (replay must keep working
/// across crate revisions); serde-tagged enums on events ossify
/// variant shapes and turn additive widening into a breaking change.
/// `LoopOwnershipTag` carries the discrimination as a string-newtype
/// instead — wire-stable, audit-greppable, never branched on by the
/// runtime.
///
/// The conversion is one-way at the seam:
///
/// ```rust
/// use cosmon_core::event_v2::LoopOwnershipTag;
/// use cosmon_core::spawn_seam::LoopOwnership;
/// assert_eq!(
///     LoopOwnershipTag::from(LoopOwnership::External).as_str(),
///     "external"
/// );
/// assert_eq!(
///     LoopOwnershipTag::from(LoopOwnership::Cosmon).as_str(),
///     "cosmon"
/// );
/// ```
///
/// Same gesture as
/// [`ValidatedAdapterName`](crate::spawn_seam::ValidatedAdapterName) →
/// `adapter_name: String` on the events: typed in Rust, string on the
/// wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoopOwnershipTag(String);

impl LoopOwnershipTag {
    /// Construct a tag from a free-form string (used by the JSONL
    /// migrator path that has no [`LoopOwnership`] in hand).
    ///
    /// Production call sites should flow through
    /// [`From<LoopOwnership>`](#impl-From%3CLoopOwnership%3E-for-LoopOwnershipTag)
    /// instead — the runtime never branches on the tag, so the
    /// validator-driven path is the load-bearing one.
    #[must_use]
    pub fn from_raw(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the tag as a `&str` for audit queries and event-log
    /// projection.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<LoopOwnership> for LoopOwnershipTag {
    fn from(o: LoopOwnership) -> Self {
        match o {
            LoopOwnership::External => Self("external".to_owned()),
            LoopOwnership::Cosmon => Self("cosmon".to_owned()),
        }
    }
}

impl Default for LoopOwnershipTag {
    /// Default to `"external"` — the legacy pre-ADR-103 contract for
    /// `events.jsonl` lines that pre-date the field.
    fn default() -> Self {
        Self("external".to_owned())
    }
}

/// A monotone sequence number assigned by the event log writer.
///
/// The first event in a log is `Seq(0)`. The writer resumes from the last
/// line on open, so `Seq` is unique per `events.jsonl` file but not globally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    /// The sequence that follows this one.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A canonical event envelope — every persisted `EventV2` is wrapped in one of
/// these before being written to the log.
///
/// ## Sequencing
///
/// Two distinct sequence numbers travel on every envelope:
///
/// * `seq` — strictly monotone **per file**. Authoritative ordering across
///   the whole log; the channel by which `causal_parent` references resolve.
/// * `mol_seq` — strictly monotone **per `molecule_id`** (when the event
///   carries one). The local ledger of "what happened to *this* molecule, in
///   what order". Required by ADR-052 invariant I7 (Gödel G5) so that
///   compare-and-append on `(molecule_id, mol_seq)` is observable from a
///   tail-rescan rather than inferred by filtering the global stream.
///
/// `mol_seq` is `None` for events without a molecule association
/// (e.g. [`EventV2::WorkerHeartbeat`]). Old envelopes deserialise without
/// the field.
///
/// ## Emitter header
///
/// Every envelope carries a triple identifying *who* produced the line:
///
/// * [`emitter_kind`](Self::emitter_kind) — coarse role (Cli, Worker,
///   Patrol, Attendant, …).
/// * [`emitter_id`](Self::emitter_id) — opaque scoped id
///   (`"worker:wkr-abc123"`, `"cli:cs-nucleate"`, …).
/// * [`meta_level`](Self::meta_level) — informative reflexivity depth.
///
/// The header is the substrate prerequisite for the attendant causal
/// filter: a patrol that consumes its own output suffers auto-immune
/// dilution unless the requestor side can write
/// `WHERE NOT EXISTS (events e WHERE e.emitter_kind = 'Attendant')`.
/// A causal-filter was chosen
/// over a numeric `meta_level` counter as the structural guard;
/// `meta_level` here is informative only.
///
/// Backward compat: all three fields are `#[serde(default)]` so legacy
/// `events.jsonl` lines deserialise without error — they default to
/// [`EmitterKind::Unknown`] / `""` / `0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Monotone sequence number within the enclosing event log.
    pub seq: Seq,
    /// Per-molecule monotone sequence number. `None` for events without an
    /// associated molecule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mol_seq: Option<Seq>,
    /// Wall-clock time (UTC) when the event was recorded.
    pub timestamp: DateTime<Utc>,
    /// Sequence of the event that causally triggered this one, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causal_parent: Option<Seq>,
    /// Decision-quality band derived from observable signals at the moment
    /// of recording.
    ///
    /// Stamped only on **cs verb** events ([`EventV2::is_verb`]) so the
    /// trace becomes a witness of the operator's physiological state and
    /// no longer invariant to the scribe. `None` for non-verb events
    /// (heartbeats, energy ticks, …) and for legacy envelopes written
    /// before the quality band existed — old envelopes deserialize fine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_band: Option<QualityBand>,
    /// Coarse role of the process that wrote this envelope.
    ///
    /// The structural guard for the attendant causal-filter: a query
    /// of the form `WHERE NOT EXISTS (events WHERE emitter_kind =
    /// 'Attendant')` is what prevents a self-consuming patrol from
    /// diluting its own signal. See [`EmitterKind`] for the catalogue.
    ///
    /// Defaults to [`EmitterKind::Unknown`] for legacy lines.
    #[serde(default)]
    pub emitter_kind: EmitterKind,
    /// Opaque, scoped identifier for the emitter — paired with
    /// [`emitter_kind`](Self::emitter_kind).
    ///
    /// Conventional shapes: `"worker:wkr-abc123"`, `"cli:cs-nucleate"`,
    /// `"formula-step:deep-think:2"`, `"patrol:silence-detect"`. The
    /// id is informative — readers must not parse it as a typed
    /// reference.
    ///
    /// Defaults to `""` for legacy lines.
    #[serde(default)]
    pub emitter_id: String,
    /// Informative reflexivity depth — `0` for first-order observation
    /// of fleet activity, `1` for an attendant acting on those events,
    /// `2` for an audit of the attendant, etc.
    ///
    /// **Not** the structural guard against auto-consumption — that role
    /// is owned by the causal-filter on
    /// [`emitter_kind`](Self::emitter_kind). `meta_level` is preserved
    /// for human reading and informational queries; the runtime does
    /// not enforce monotonicity.
    ///
    /// Defaults to `0` for legacy lines.
    #[serde(default)]
    pub meta_level: u8,
    /// The event payload.
    #[serde(flatten)]
    pub event: EventV2,
}

impl Envelope {
    /// Wrap an event with a caller-supplied sequence number and causal parent.
    ///
    /// The timestamp is stamped to `Utc::now()` so callers cannot accidentally
    /// set a past time and break ordering guarantees.
    ///
    /// `mol_seq` is left as `None`; the canonical writer
    /// (`cosmon_state::event_log::EventLogWriter`) populates it under the
    /// flock by rescanning the tail. Callers building envelopes outside the
    /// writer (tests, in-memory simulators) typically do not need a per-
    /// molecule sequence — they can use [`Self::with_mol_seq`] when they do.
    ///
    /// The emitter header defaults to [`EmitterKind::Unknown`] / `""` /
    /// `0`. The canonical writer overrides this from its set emitter
    /// before calling; in-memory tests that do not care can keep the
    /// default.
    #[must_use]
    pub fn new(seq: Seq, causal_parent: Option<Seq>, event: EventV2) -> Self {
        Self {
            seq,
            mol_seq: None,
            timestamp: Utc::now(),
            causal_parent,
            quality_band: None,
            emitter_kind: EmitterKind::default(),
            emitter_id: String::new(),
            meta_level: 0,
            event,
        }
    }

    /// Wrap an event with both a global and a per-molecule sequence number.
    ///
    /// Used by the canonical event-log writer after it has resolved the
    /// next per-molecule seq under the flock.
    #[must_use]
    pub fn with_mol_seq(
        seq: Seq,
        mol_seq: Option<Seq>,
        causal_parent: Option<Seq>,
        event: EventV2,
    ) -> Self {
        Self {
            seq,
            mol_seq,
            timestamp: Utc::now(),
            causal_parent,
            quality_band: None,
            emitter_kind: EmitterKind::default(),
            emitter_id: String::new(),
            meta_level: 0,
            event,
        }
    }

    /// Wrap an event with a global / per-molecule sequence number **and**
    /// a Kahneman K1 [`QualityBand`].
    ///
    /// Used by the canonical event-log writer for cs verb events
    /// ([`EventV2::is_verb`]); other call sites pass `None` for `quality_band`
    /// (or use [`Self::with_mol_seq`]).
    #[must_use]
    pub fn with_quality_band(
        seq: Seq,
        mol_seq: Option<Seq>,
        causal_parent: Option<Seq>,
        quality_band: Option<QualityBand>,
        event: EventV2,
    ) -> Self {
        Self {
            seq,
            mol_seq,
            timestamp: Utc::now(),
            causal_parent,
            quality_band,
            emitter_kind: EmitterKind::default(),
            emitter_id: String::new(),
            meta_level: 0,
            event,
        }
    }

    /// Wrap an event with full sequencing, quality band, **and** the
    /// emitter header (cosmon-ward §F1).
    ///
    /// Used by the canonical event-log writer once it has resolved every
    /// piece of provenance under the flock. The three header fields are
    /// passed verbatim — no validation is performed (the field-shape
    /// invariants are documented on [`Self::emitter_kind`],
    /// [`Self::emitter_id`], and [`Self::meta_level`]).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_emitter(
        seq: Seq,
        mol_seq: Option<Seq>,
        causal_parent: Option<Seq>,
        quality_band: Option<QualityBand>,
        emitter_kind: EmitterKind,
        emitter_id: String,
        meta_level: u8,
        event: EventV2,
    ) -> Self {
        Self {
            seq,
            mol_seq,
            timestamp: Utc::now(),
            causal_parent,
            quality_band,
            emitter_kind,
            emitter_id,
            meta_level,
            event,
        }
    }

    /// Parse one JSONL line — tries `EventV2` first, then falls back to legacy
    /// coercion.
    ///
    /// # Errors
    ///
    /// Returns `serde_json::Error` if the line is neither valid `EventV2` nor a
    /// recognisable legacy shape.
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        if let Ok(env) = serde_json::from_str::<Self>(line) {
            return Ok(env);
        }
        migrate_legacy_line(line)
    }
}

/// The canonical `EventV2` payload.
///
/// Every variant is `snake_case`-tagged in JSON so the enum discriminator
/// appears as `"type": "..."` at the same level as the envelope fields. The
/// key name `type` is chosen (over `kind`) so legacy writers that used
/// `{"type": "..."}` line shapes can be parsed by the same deserializer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EnumCount)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventV2 {
    /// A molecule was nucleated (created).
    ///
    /// `parent_id` and `blocks` capture DAG edges at nucleation time so the
    /// event log is a sufficient statistic for replay (edges no longer need
    /// to be recovered from `state/`). Both fields are additive: old
    /// `events.jsonl` lines without them deserialize fine.
    MoleculeNucleated {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// The formula used to create it.
        formula_id: String,
        /// The causal parent (e.g. decay parent, orchestrator molecule).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<MoleculeId>,
        /// Downstream molecules this one blocks (outgoing `Blocks` edges).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocks: Vec<MoleculeId>,
    },
    /// A molecule transitioned status.
    MoleculeStatusChanged {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Prior status, `snake_case` string.
        from: String,
        /// New status.
        to: String,
    },
    /// A molecule step completed with measured wall-clock duration.
    MoleculeStepCompleted {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Zero-based index of the step that completed.
        step: usize,
        /// Total number of steps in the formula.
        total: usize,
        /// Wall-clock duration of the step in milliseconds, if measured.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Content-addressed hash of the step's inputs (ADR-043).
        ///
        /// `None` for pre-hash molecules, for steps validated in `MTime`
        /// mode, or for runtimes that did not compute a hash. Backward
        /// compatible: older readers ignore the field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_hash: Option<cosmon_hash::StepHash>,
    },
    /// A molecule reached the `Completed` terminal status.
    MoleculeCompleted {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Wall-clock duration from first dispatch to completion, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Human-readable summary.
        reason: String,
    },
    /// A molecule reached the `Collapsed` terminal status.
    MoleculeCollapsed {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Human-readable reason.
        reason: String,
        /// Categorised collapse kind for `cs errors` aggregation.
        ///
        /// Optional and `#[serde(default)]` so legacy `events.jsonl` lines
        /// without the field deserialize cleanly — the aggregator falls
        /// back to [`CollapseReason::Other`] for those.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<CollapseReason>,
    },
    /// A molecule was marked stuck (needs human intervention).
    MoleculeStuck {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Categorised reason, with free-form fallback for unclassified text.
        reason: StuckReason,
    },
    /// A decay spliced a parent into children (1 → N).
    DecaySpliced {
        /// The parent molecule that was replaced in the DAG.
        parent: MoleculeId,
        /// The child molecules inserted in its place.
        children: Vec<MoleculeId>,
    },
    /// A merge was dispatched (`cs done` began merging a molecule's branch).
    ///
    /// `federation_provenance` is `Some(...)` when the molecule originated
    /// outside cosmon's own ledger (cross-galaxy delegation per ADR-105
    /// I9'). `None` for purely local merges. The field is the
    /// machinery that closes the I9' doctrine-without-machinery gap —
    /// `cs verify --federation` scans the fleet-wide event log for
    /// cross-galaxy events whose `federation_provenance` is missing
    /// and reports them as hard failures.
    ///
    /// Backward compatibility: `#[serde(default,
    /// skip_serializing_if = "Option::is_none")]` means legacy lines
    /// without the field deserialise as `None`, and serialised events
    /// without provenance omit the key entirely.
    MergeDispatched {
        /// The molecule whose branch is being merged.
        molecule: MoleculeId,
        /// The source branch name.
        branch: String,
        /// Federation lineage when the molecule originated in a sister
        /// galaxy. `None` for purely local merges. See
        /// [`crate::federation::FederationLineage`] and ADR-105.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        federation_provenance: Option<crate::federation::FederationLineage>,
    },
    /// A merge completed (successfully or with a conflict).
    ///
    /// `federation_provenance` mirrors the field on
    /// [`Self::MergeDispatched`]: present iff the molecule crossed the
    /// federation boundary. See ADR-105 and
    /// [`crate::federation::FederationLineage`].
    MergeCompleted {
        /// The molecule whose branch was merged.
        molecule: MoleculeId,
        /// The source branch name.
        branch: String,
        /// Categorised outcome. Serializes as `"ok"`, `"conflict"`,
        /// `"error:<detail>"`, or any other free-form string.
        result: MergeResult,
        /// Federation lineage when the molecule originated in a sister
        /// galaxy. `None` for purely local merges. See
        /// [`crate::federation::FederationLineage`] and ADR-105.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        federation_provenance: Option<crate::federation::FederationLineage>,
    },
    /// A worker was spawned by `cs tackle`.
    WorkerSpawned {
        /// The worker's identity.
        worker_id: WorkerId,
        /// The molecule the worker was created to tackle, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        molecule: Option<MoleculeId>,
        /// The tmux session name hosting the worker.
        session_name: String,
        /// Role string (e.g. "polecat", "refinery").
        role: String,
        /// Adapter that actually spawned the worker (ADR-097 / C8).
        ///
        /// Pre-C8, `cs tackle --adapter aider` emitted
        /// [`Self::AdapterSelected`] with `adapter_name = "aider"`
        /// but then routed through the Claude tmux path regardless —
        /// the journal lied to the filesystem. Carrying `adapter_name`
        /// on this passive "worker created at …" event lets a test
        /// cross-reference `adapter_selected.adapter_name ==
        /// worker_spawned.adapter_name`; a mismatch surfaces that
        /// silent-failure mode.
        ///
        /// Default `""` on the V1→V2 legacy parser path keeps old
        /// `events.jsonl` lines round-trippable.
        #[serde(default)]
        adapter_name: String,
        /// Per-Adapter [`LoopOwnership`] axis carried on the wire as a
        /// string newtype (ADR-103). `"external"` when cosmon spawned
        /// an external binary that owns its own loop; `"cosmon"` when
        /// the loop runs in-process inside `cosmon-agent-harness`.
        ///
        /// `#[serde(default)]` keeps `events.jsonl` lines that pre-date
        /// this field round-trippable — the default
        /// [`LoopOwnershipTag::default`] returns `"external"`, the
        /// legacy contract for tmux-pane adapters.
        #[serde(default)]
        loop_ownership: LoopOwnershipTag,
    },
    /// A worker was killed (normally or by purge).
    WorkerKilled {
        /// The worker's identity.
        worker_id: WorkerId,
        /// Human-readable reason.
        reason: String,
    },
    /// Periodic liveness signal from a running worker.
    ///
    /// Emitted every 30–60 seconds by a worker (or its bridge) so the runtime
    /// can distinguish "thinking" from "stuck" without introspecting tmux.
    /// The `activity_hint` is best-effort; consumers should treat `Unknown`
    /// as "worker is alive but activity cannot be classified".
    WorkerHeartbeat {
        /// The worker emitting the heartbeat.
        worker_id: WorkerId,
        /// Wall-clock time (UTC) the heartbeat was produced at the source.
        ts: DateTime<Utc>,
        /// Coarse activity classification.
        activity_hint: ActivityHint,
    },
    /// Patrol detected a running worker that has gone silent — no
    /// `WorkerHeartbeat` for at least `3 ×` the expected interval.
    ///
    /// Emitted by `cs patrol --silence-detect`. The patrol does **not**
    /// kill the worker — it only signals: the molecule is tagged
    /// `temp:frozen` and a `cs notify` push is fired so the operator
    /// hears about the silence in time. Append-only by construction;
    /// this event is the audit record that the gap was observed — the
    /// principle being that the absence of a signal must itself be a
    /// signal.
    WorkerSilenceDetected {
        /// The molecule whose worker fell silent.
        molecule_id: MoleculeId,
        /// Worker the patrol believed to be active. Optional because the
        /// molecule may have lost its `assigned_worker` reference before
        /// the silence check fired.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_id: Option<WorkerId>,
        /// Seconds elapsed since the most recent `WorkerHeartbeat`.
        /// `None` when no heartbeat has ever been recorded for this
        /// molecule (cold start — never observed alive).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        age_since_last_heartbeat_s: Option<u64>,
        /// Threshold in seconds the patrol used to declare silence
        /// (typically `3 ×` the heartbeat cadence).
        threshold_s: u64,
    },
    /// Patrol detected a **blocking dialogue** sitting in a running worker's
    /// pane — a tool-permission prompt, or (the load-bearing case) a Claude
    /// Code spend-/usage-limit dialog nobody was at the keyboard to answer.
    ///
    /// Emitted by `cs patrol --dialogue-scan`. Per the be1e discipline
    /// (ADR-137 §2) the pane text is read only to *surface* this finding; the
    /// `class` is the [`crate::dialogue::DialogueClass`] token
    /// (`permission` / `money_stake` / `unknown`). A `money_stake` is **never**
    /// auto-confirmed — it pages the operator; a `permission` is auto-confirmed
    /// only when the operator opted in. This event is the append-only audit
    /// record of *what the patrol saw and what it did about it*.
    BlockingDialogueDetected {
        /// The running molecule whose pane held the blocking dialogue.
        molecule_id: MoleculeId,
        /// The worker rendering the pane. Optional for parity with the other
        /// patrol events where `assigned_worker` may be absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_id: Option<WorkerId>,
        /// The classified stake — `dialogue::DialogueClass::as_str`.
        class: String,
        /// The action the patrol took — `alerted` / `auto_confirmed` /
        /// `reported` / `canary_red`.
        action: String,
        /// Seconds the molecule had made no progress when the dialogue was
        /// seen (from `updated_at`). `None` if it could not be computed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_seconds: Option<u64>,
    },
    /// A capability-bearing worker paused for an operator decision at an
    /// irreversibility boundary (ADR-123 — operator-block doctrine).
    ///
    /// Emitted by `cs await-operator` — the **only** sanctioned blocking
    /// primitive — **before** the worker yields. This is the
    /// worker-emitted half of the fix: a block must be
    /// *emitted*, never *inferred*, because inner cognition cannot be
    /// observed from outside. The companion derived surface
    /// marker is the tag
    /// [`crate::operator_block::AWAITING_OP_TAG`]; the molecule **stays
    /// `Running`** (ADR-123 Q4 = option (b)).
    ///
    /// Blocking *without* emitting this event is a protocol violation.
    /// The un-emitting case (a worker parked at an off-cosmon
    /// modal) is caught by the external event-age patrol — the
    /// load-bearing suspenders.
    WorkerBlockedOnOperator {
        /// The molecule whose worker is blocked.
        molecule_id: MoleculeId,
        /// The irreversibility boundary that authorised the pause.
        boundary: crate::operator_block::IrreversibleBoundary,
        /// Wall-clock time (UTC) the worker emitted the block.
        since: DateTime<Utc>,
    },
    /// Periodic energy (token) snapshot for a running worker.
    ///
    /// Emitted by the runtime tick loop once per poll interval while the
    /// worker is Active. This is the critical sensor for reconstructing the
    /// timeline graph — every tick contributes one sample to the cost curve.
    EnergyTick {
        /// The worker being sampled.
        worker_id: WorkerId,
        /// Cumulative input tokens across the worker's lifetime.
        input_tokens: u64,
        /// Cumulative output tokens across the worker's lifetime.
        output_tokens: u64,
        /// Cumulative cost in USD.
        cost_usd: f64,
    },
    /// A molecule's TTL fired and an expiry policy was applied (ADR-029).
    ///
    /// Emitted by `cs expire` / `cs patrol --expire` after evaluating a
    /// molecule's `expires_at` against the wall clock. `policy_applied`
    /// records the *effective* policy after the running-molecule
    /// degradation rule (a `Collapse` policy that degraded to `Warn` on
    /// an active molecule records `Warn`).
    Expired {
        /// The molecule whose TTL fired.
        molecule_id: MoleculeId,
        /// Effective policy applied — may differ from the molecule's stored
        /// policy when degradation applied.
        policy_applied: ExpiryPolicy,
    },
    /// A shell gate step started execution.
    ///
    /// Emitted by `cs tackle` when it encounters a formula step with a
    /// `command` field. The command is executed via `sh -c` instead of
    /// launching a Claude worker.
    GateStarted {
        /// The molecule being tackled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// The shell command being executed.
        command: String,
    },
    /// A shell gate step completed successfully (exit code 0).
    GateCompleted {
        /// The molecule being tackled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// Process exit code (always 0 for this variant).
        exit_code: i32,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
    },
    /// A shell gate step failed (non-zero exit code or timeout).
    GateFailed {
        /// The molecule being tackled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// Process exit code, or -1 for timeout/signal kill.
        exit_code: i32,
        /// Tail of stderr output (up to ~500 bytes) for diagnostics.
        stderr_tail: String,
    },
    /// A native step started execution.
    ///
    /// Emitted by `cs tackle` when it encounters a formula step with a
    /// `native` field. The registered Rust function is called directly —
    /// no shell, no tmux.
    NativeStarted {
        /// The molecule being tackled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// The native function registry key.
        native_fn: String,
    },
    /// A native step completed successfully.
    NativeCompleted {
        /// The molecule being tackled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
    },
    /// A native step failed.
    NativeFailed {
        /// The molecule being tackled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// Error message from the native function.
        error: String,
    },
    /// The backlog-sanity guard (ADR-048) was bypassed by an operator
    /// passing `--force-runtime` on `cs tackle` or `cs run`.
    ///
    /// Emitted by the CLI at runtime bootstrap whenever the dirty-backlog
    /// refusal would have fired but the operator elected to proceed.
    /// The event is the durable audit trail required by the ADR: a future
    /// reviewer can reconstruct *which* pendings the runtime walked past
    /// on what root, and which threshold was in effect.
    RuntimeGuardOverride {
        /// Command that emitted the override — `"cs tackle"` or `"cs run"`.
        caller: String,
        /// Root molecule the runtime bootstrap was dispatched onto.
        molecule_id: MoleculeId,
        /// Sediment cardinality observed at the moment of override.
        sediment_count: usize,
        /// Threshold value in force (default 5, env-overridable).
        threshold: usize,
        /// Sample of sediment molecule IDs (up to 5) for context.
        sample: Vec<MoleculeId>,
    },
    /// A BLAKE3 seal was captured over `prompt.md` at nucleation time.
    ///
    /// Soft-contract receipt: the seal itself lives in
    /// `MoleculeData::prompt_seal`; this event records its capture so a
    /// chain walk can confirm *when* the prompt was frozen in the
    /// operator-intent sense. `cs verify` does not enforce on the hot
    /// path — mismatches are reported, never blocked.
    PromptSealed {
        /// The molecule whose prompt was sealed.
        molecule_id: MoleculeId,
        /// BLAKE3 hash as 64-char lowercase hex.
        hash: String,
        /// Wall-clock time the hash was computed.
        sealed_at: DateTime<Utc>,
        /// Byte length of the sealed (canonical) artifact.
        bytes: u64,
        /// Canonical-form recipe used for the hash. ADR-056 — defaults
        /// to `0` (raw) for events emitted before the mint protocol.
        #[serde(default)]
        canonical_version: u8,
    },
    /// A BLAKE3 seal was captured over `briefing.md` at step advance.
    ///
    /// Emitted by `cs evolve` after the briefing has been regenerated for
    /// the next step. Retrospective tooling (`cs verify`) can compare
    /// this seal with the current contents of `briefing.md` to detect a
    /// shadow contract — an edit made *after* the advance that would
    /// silently reshape what the worker sees.
    BriefingSealed {
        /// The molecule whose briefing was sealed.
        molecule_id: MoleculeId,
        /// Zero-based step index this briefing belongs to.
        step: u32,
        /// BLAKE3 hash as 64-char lowercase hex.
        hash: String,
        /// Wall-clock time the hash was computed.
        sealed_at: DateTime<Utc>,
        /// Byte length of the sealed (canonical) artifact.
        bytes: u64,
        /// Canonical-form recipe used for the hash. ADR-056 — defaults
        /// to `0` (raw) for events emitted before the mint protocol.
        #[serde(default)]
        canonical_version: u8,
    },
    /// A BLAKE3 seal was captured over the bootstrap context at step
    /// advance.
    ///
    /// Emitted by `cs evolve` after the agent-harness bootstrap walk
    /// (`AGENTS.md` + `CLAUDE.md` from `work_dir` up to `.git/`) has
    /// produced its fenced concatenation. The seal is a trace of which
    /// project conventions the worker started from. A later `cs verify`
    /// re-runs the walk and compares the hash so cross-worktree
    /// poisoning (a peer worker dropping an `AGENTS.md` between
    /// dispatch and audit) lights up as a shadow-contract failure.
    ///
    /// Companion to [`Self::BriefingSealed`] — same shape, distinct
    /// surface. Bootstrap content lives across multiple files (and may
    /// be empty when the worker runs outside a git checkout), so the
    /// canonical bytes are the concatenated fence-wrapped buffer the
    /// harness actually surfaces to the model.
    BootstrapSealed {
        /// The molecule whose bootstrap context was sealed.
        molecule_id: MoleculeId,
        /// Zero-based step index this bootstrap context belongs to.
        step: u32,
        /// BLAKE3 hash as 64-char lowercase hex.
        hash: String,
        /// Wall-clock time the hash was computed.
        sealed_at: DateTime<Utc>,
        /// Byte length of the sealed (canonical) artifact.
        bytes: u64,
        /// Canonical-form recipe used for the hash. ADR-056 — defaults
        /// to `0` (raw) for events emitted before the mint protocol.
        #[serde(default)]
        canonical_version: u8,
    },
    /// A stress-test prior seal was attested by a witness session
    /// (ADR-085 §3, Layer 2).
    ///
    /// The witness reads `<molecule_dir>/prior.b3` (the BLAKE3 hash) and
    /// `<molecule_dir>/state.json` — it does **not** read `prior.md`,
    /// preserving the structural-independence guarantee of ADR-052
    /// inheritance (one-writer / one-witness). The attestation is the
    /// dispatch precondition for layer 1: `cs tackle` refuses unless a
    /// matching `SealAttested` whose `prior_b3` matches the on-disk seal
    /// has been emitted by a session distinct from the worker's.
    SealAttested {
        /// The molecule whose stress-test prior was attested.
        molecule_id: MoleculeId,
        /// BLAKE3 hash (64-char lowercase hex) of the operator-sealed
        /// `prior.md` — i.e. the contents of `<molecule_dir>/prior.b3`.
        prior_b3: String,
        /// Wall-clock time the operator sealed `prior.md`. Must be
        /// strictly less than the dispatch time enforced by Layer 1.
        sealed_at: DateTime<Utc>,
        /// Tmux session identity of the witness that attested. The
        /// runtime gate refuses an attestation whose session matches the
        /// worker's (cheap heuristic — the hardened `LaunchAgent` path is
        /// deferred per ADR-085 §3).
        witness_id: String,
        /// BLAKE3 hash (64-char lowercase hex) of the canonical
        /// attestation record — what the witness signed, structurally
        /// independent of `prior.md`'s substance.
        attestation_b3: String,
    },
    /// The stress-test seal was bypassed by the operator at nucleation
    /// (ADR-085 §3.5).
    ///
    /// Replaces the historical free-text `dispatch-decision.md` with a
    /// typed event linked to the on-disk
    /// [`BypassReceipt`](crate::molecule_class::BypassReceipt). Every
    /// emission flags the molecule for cross-galaxy escalation on its
    /// next re-run (Layer 3, ADR-085 §4); the per-fleet record is
    /// permanent and cannot be deleted without a new ADR.
    SealBypassed {
        /// The molecule whose seal was bypassed.
        molecule_id: MoleculeId,
        /// BLAKE3 hash (64-char lowercase hex) of the
        /// `bypass-receipt.json` artefact written to the molecule
        /// directory. The receipt is the source of truth; this hash is
        /// the durable audit anchor that survives later edits to the
        /// JSON file (same trace-not-lock discipline as
        /// [`Self::PromptSealed`]).
        bypass_receipt_b3: String,
        /// One-line free-text reason supplied via `--bypass-reason`.
        reason: String,
    },
    /// A stuck molecule was resurrected by `cs resurrect` — a fresh worker
    /// was attached to a wreck with a composed bootstrap prompt.
    ///
    /// Resurrection is an **event, not a status**. The
    /// molecule returns to `Running` as if `cs tackle` had just run; this
    /// event is the sensor trail so ρ̂ and κ can be measured after the fact.
    Resurrected {
        /// The molecule that was resurrected.
        molecule_id: MoleculeId,
        /// The prior tmux session name the molecule was bound to (best
        /// effort — may be absent if the molecule was tackled before
        /// session-name stamping landed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_session: Option<String>,
        /// Byte length of the composed bootstrap prompt injected into the
        /// new worker. Shannon's κ numerator: `κ = A_bytes / H(S_t)`.
        composed_prompt_bytes: u64,
        /// Optional token count attributed to the original (crashed)
        /// worker. Populated when energy data is available; `None` when
        /// the original session's token log is missing. Part of the ρ̂
        /// baseline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        t_orig_tokens: Option<u64>,
        /// Count of prior resurrections for this molecule (k in the
        /// horizon rule). `0` on the first resurrection.
        prior_count: u32,
    },
    /// A completed-but-unmerged molecule was closed by `cs harvest` —
    /// the worker-exit → `cs done` bridge fired.
    ///
    /// Emitted when a transport watchdog (tmux `pane-died` hook) or a
    /// periodic sweep (`cs patrol --harvest`) detected that the molecule
    /// had reached `Completed` with `merged_at = None` and invoked
    /// `cs done` from a sibling shell. `success` records whether the
    /// invoked `cs done` returned exit 0. The event is the audit trail
    /// that answers *who* closed the merge loop when the human didn't.
    Harvested {
        /// The molecule whose completion was harvested.
        molecule_id: MoleculeId,
        /// Whether the spawned `cs done` returned exit 0.
        success: bool,
    },
    /// A typed query step (`[steps.query]`) was evaluated successfully.
    ///
    /// Emitted by `cs tackle` when a formula step with `[steps.query]`
    /// runs to completion. The event records *what was asked of the
    /// store* and *what was bound back* into the molecule's variables —
    /// so a future audit can replay the query without re-reading the
    /// underlying source. Replaces what historically left a silent
    /// `cs --json observe … | jq …` pipe failure with a typed receipt.
    QueryStepEvaluated {
        /// The molecule whose step was evaluated.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// The dot-path expression that was evaluated.
        expr: String,
        /// The resolved source descriptor (e.g. `"state"`,
        /// `"molecule:abc-123"`, `"events"`).
        source: String,
        /// The variable name the result was bound to in the molecule's
        /// `variables` map.
        output_var: String,
        /// The serialised result (JSON-encoded). Truncated to the first
        /// ~512 bytes for the audit trail; the full value lives in
        /// `MoleculeData.variables[output_var]`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        result_preview: String,
    },
    /// A streamed external channel (LLM provider, HTTP API, …) timed out
    /// or stalled. Replaces the historical "8m42s curl-style silent
    /// drop" with a typed event that downstream watchdogs (`cs notify`,
    /// `cs patrol`) can consume.
    ///
    /// Emitted both on a per-checkpoint timeout (when no new tokens have
    /// been observed within the checkpoint window) and on the aggregate
    /// `max_total_minutes` ceiling firing. The runtime distinguishes the
    /// two via the `kind` field so a consumer can decide whether to
    /// retry or escalate.
    ExternalChannelTimeout {
        /// The molecule whose step stalled.
        molecule_id: MoleculeId,
        /// The step identifier within the formula.
        step_id: String,
        /// The provider key (e.g. `"anthropic"`, `"mock"`).
        provider: String,
        /// Stall classification.
        kind: ExternalChannelTimeoutKind,
        /// Seconds elapsed since the last observed progress (token, byte,
        /// or response chunk). `None` when the runtime has no progress
        /// signal at all (cold timeout).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        age_s: Option<u64>,
        /// Cumulative bytes flushed to the output file at the moment of
        /// timeout. Allows a retry to resume from the same prefix.
        bytes_flushed: u64,
        /// Attempt number that just failed (1-based). The runtime may
        /// retry up to `max_retries` times before collapsing the step.
        attempt: u32,
    },
    /// One HTTP invocation through the Remote Pilot Port completed.
    ///
    /// IFBDD measurement of per-tenant traffic on the cosmon API
    /// surface. Emitted by `cosmon-rpp-adapter` at the very end of every
    /// admitted request so the operator can later answer:
    ///
    /// - Per tenant, how many invocations land per backend per day?
    /// - What is the success / failure ratio per backend?
    /// - Which molecules concentrate the traffic?
    ///
    /// `tenant` is the `NucleonId`-as-string of the principal
    /// (V0: `"operator"` for trusted-CLI flows; JWT-mapped nucleon
    /// for remote pilots — kept stringly for the same wire-stability
    /// rationale as [`Self::ExternalChannelTimeout::provider`]).
    ///
    /// `molecule_id` is `None` for routes that do not target a
    /// specific molecule (e.g. `POST /v1/molecules` *before* the
    /// nucleate result is parsed).
    ///
    /// **Observation, not billing.** This event records that a call
    /// happened; it is not a fee, not a quota debit. Billing
    /// decisions await the IFBDD signal.
    InvocationCompleted {
        /// Stringly-typed tenant identity (`"operator"` or
        /// `"jwt:<sub>"` / nucleon-id projection).
        tenant: String,
        /// The molecule the invocation targeted, when one applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        molecule_id: Option<MoleculeId>,
        /// Backend identifier — free-form by design while the
        /// vocabulary settles (`"anthropic"`, `"ollama"`, `"mlx"`,
        /// `"cs-subprocess"`, …).
        backend: String,
        /// Wall-clock latency of the admitted request envelope, in
        /// milliseconds.
        latency_ms: u64,
        /// Whether the response was a 2xx (success) on the wire.
        success: bool,
    },
    /// A worker process was observed to exit — emitted by the tmux
    /// `pane-died` hook the moment the kernel reaps the worker shell.
    ///
    /// ADR-052 invariants I4 + I8 + I10: the probe must *emit* its
    /// observation before acting on it. This event is the kernel-level
    /// witness that the worker process is gone — it arrives before
    /// [`Self::Harvested`] (which records what `cs done` did in response)
    /// and is the only event that closes the "alarm-clock-buzzing-on-
    /// the-empty-apron" gap (dfd8 ghost). Periodic patrol sweeps do NOT
    /// emit this event — they emit probe observations (`Witness`) instead.
    ///
    /// `exit_code` is the wait-status the pane exited with, when tmux
    /// can supply it (`#{pane_dead_status}` format); absent for older
    /// tmux versions or when the code cannot be parsed as an integer.
    /// `reason` is a short tag naming the probe that fired (`pane_died`,
    /// and in future the kernel signal name when tmux exposes it).
    WorkerExited {
        /// The molecule whose worker exited.
        molecule_id: MoleculeId,
        /// Process wait-status reported by tmux, when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Short tag identifying the probe that fired (e.g. `pane_died`).
        reason: String,
    },
    /// A fleet was loaded with an advisory `organization_type` field.
    ///
    /// Pure IFBDD instrumentation: the field is free-form and never
    /// matched on by code. The event exists solely so a future
    /// re-evaluation can read `events.jsonl` and answer empirically
    /// whether (a) ≥3 fleets converge on the same value with the same
    /// operational meaning, or (b) N≥2 distinct human operators with
    /// observable preference divergences emerge. Until either trigger
    /// fires, no Rust enum, no template loading, no `match` on the
    /// value. Re-evaluation is a deliberate operator decision — not a
    /// silent code-path drift.
    ///
    /// `fleet` is the `FleetSpec.name` (string form of the fleet id).
    /// `organization_type` is whatever the operator wrote in
    /// `fleet.toml` — verbatim, no normalisation.
    FleetTyped {
        /// Fleet name (canonical id from `[fleet].id` or legacy
        /// `fleet = "..."`).
        fleet: String,
        /// The advisory operator self-classification. Free-form by
        /// design — never branched on, never enumerated.
        organization_type: String,
        /// Wall-clock time (UTC) the fleet was loaded.
        ts: DateTime<Utc>,
    },
    /// The operator is observed present on a session.
    ///
    /// Emitted by interactive `cs` invocations at the moment the CLI
    /// runs. Carries a `PresenceSource` so a downstream consumer
    /// gating a destructive action can apply the no-cloning theorem:
    /// only `source.is_exogenous() == true`
    /// readouts are valid for that purpose. `Internal` here means
    /// "cosmon's own heartbeat saw the session" and is informative
    /// only.
    ///
    /// `sid` is the cosmon session id of the running shell.
    /// `nucleon_id` and `orbitale_id` are best-effort projections of
    /// the pilot identity (ADR-061 / ADR-063); both are `None` when
    /// the runtime cannot resolve them and the field stays informative.
    /// `phase` records the cognitive substrate label
    /// (`"Biological"` / `"LlmFrontier"`); the value is free-form by
    /// design and never branched on.
    OperatorPresent {
        /// Session id of the cosmon shell that observed the operator.
        sid: String,
        /// Pilot Nucléon id (atomic-nucleon family, ADR-061/063), if
        /// resolvable from the environment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nucleon_id: Option<String>,
        /// Pilot Orbitale id (the trusted device the operator is on),
        /// if resolvable from the environment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        orbitale_id: Option<String>,
        /// Cognitive-substrate label — placeholder by design, never
        /// matched on.
        phase: String,
        /// Wall-clock time (UTC) the observation was recorded.
        ts: DateTime<Utc>,
        /// Which substrate produced the readout. The no-cloning
        /// theorem requires `source.is_exogenous() == true` for
        /// destructive-action gating.
        source: crate::presence_sensor::PresenceSource,
    },
    /// The operator is observed absent on a session.
    ///
    /// Symmetric to [`Self::OperatorPresent`]. `reason` records *why*
    /// the absence was inferred — a session-explicit "operator left"
    /// gesture, a heartbeat timeout, a process crash, or an exogenous
    /// sensor verdict.
    OperatorAbsent {
        /// Session id of the cosmon shell.
        sid: String,
        /// Wall-clock time (UTC) of the last operator activity the
        /// session saw.
        last_seen_ts: DateTime<Utc>,
        /// Free-form classification (`"timeout"`, `"explicit"`,
        /// `"crash"`, `"sensor"`). Never branched on.
        reason: String,
        /// Which substrate produced the readout. See
        /// [`Self::OperatorPresent::source`] for the no-cloning rule.
        source: crate::presence_sensor::PresenceSource,
    },
    /// An operator-emitted *spark* — a request that asks the system
    /// for a verdict. Joined with [`Self::OperatorVerdict`] /
    /// [`Self::OperatorRefused`] / [`Self::OperatorSilent`] by
    /// `spark_id` to derive **latency** (the single most important
    /// liveness property).
    ///
    /// `src` records the channel the spark came in on (`"cli"`,
    /// `"whisper"`, `"chat"`, `"telegram"`, `"voice"`, …). Free-form
    /// while the channel vocabulary settles.
    ///
    /// `content_hash` is a content-addressed digest of the spark
    /// payload (BLAKE3 hex prefix is the conventional shape); it is
    /// **not** the spark text itself — full content lives in
    /// adjacent files (whisper inbox, chat transcripts).
    ///
    /// `ttl_h` is an optional caller-side time-to-live in hours
    /// (e.g. *"answer this within 24h or it is moot"*).
    ///
    /// `mol_ref` ties the spark to a molecule when one applies
    /// (e.g. a `cs ask <id>` invocation).
    OperatorSpark {
        /// Identifier joining this spark to its verdict pair.
        spark_id: String,
        /// Channel the spark arrived on. Free-form.
        src: String,
        /// Content-addressed digest of the spark payload.
        content_hash: String,
        /// Optional caller-side time-to-live, in hours.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_h: Option<u64>,
        /// The molecule the spark targets, when one applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mol_ref: Option<MoleculeId>,
    },
    /// An operator verdict that closes a [`Self::OperatorSpark`].
    ///
    /// `latency_h` is the wall-clock interval from the spark to the
    /// verdict, in hours. Recorded at emission time so a downstream
    /// reader does not have to re-join the stream to derive the
    /// liveness signal. `channel` records *how* the verdict was
    /// delivered (`"cli"` for `cs verdict`, `"whisper"` for an
    /// inbox-level reply, …).
    OperatorVerdict {
        /// Identifier of the spark this verdict closes.
        spark_id: String,
        /// The verdict text — free-form (`"yes"` / `"no"` /
        /// `"plus tard"` / a single line of explanation). Never
        /// branched on.
        verdict: String,
        /// Wall-clock latency from spark to verdict, in hours.
        latency_h: f64,
        /// Channel the verdict was delivered through. Free-form.
        channel: String,
    },
    /// The operator explicitly *refused* a spark — a verdict with
    /// negative semantics that records the refusal reason for later
    /// pattern analysis.
    ///
    /// Emitted in lieu of [`Self::OperatorVerdict`] when the spark
    /// was actively rejected (vs. answered). `latency_h` follows the
    /// same convention as `OperatorVerdict::latency_h`.
    OperatorRefused {
        /// Identifier of the spark that was refused.
        spark_id: String,
        /// Free-form explanation. Never branched on.
        refusal_reason: String,
        /// Wall-clock latency from spark to refusal, in hours.
        latency_h: f64,
    },
    /// An [`Self::OperatorSpark`] that has not been answered after a
    /// patrol-defined window. Emitted by `operator-attention-patrol`
    /// (NOT by the operator) — the patrol *names* the silence on the
    /// trace so a future audit can answer "did the proxy detect this
    /// stall?".
    ///
    /// `escalation_level` is the patrol's count of how many times
    /// this spark has been re-flagged silent in successive sweeps;
    /// the value is free-form by design (the patrol implementation
    /// owns the semantics) and never matched on by the runtime.
    OperatorSilent {
        /// Identifier of the silent spark.
        spark_id: String,
        /// Age of the silence at emission time, in hours.
        age_h: f64,
        /// Patrol-side escalation counter (informative).
        escalation_level: u32,
    },
    /// An operator-signed action — a destructive or otherwise
    /// authoritative gesture the human committed to (`cs done`,
    /// `git push`, `rm`, …).
    ///
    /// Recording the action on the trace is **observation, not
    /// gating**: this iteration ships the signal so a future audit
    /// can decide whether to gate on it. Production-grade gating of
    /// destructive actions on signed events is explicitly deferred
    /// to a separate decision after 2 weeks of trace data.
    ///
    /// `signature_method` is free-form (`"shell"`, `"touch-id"`,
    /// `"yubikey"`, …) so future signing substrates can be added
    /// without changing the schema.
    OperatorSigned {
        /// The action that was signed (`"cs done"`, `"git push"`,
        /// `"rm"`, …). Free-form.
        action: String,
        /// The molecule the action targets, when one applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mol_id: Option<MoleculeId>,
        /// How the action was signed. Free-form.
        signature_method: String,
    },

    /// **ADR-095 / RR-5** — the Resident Runtime performed a
    /// read-decide-write triple on a `.cosmon/state/` file.
    ///
    /// Captures the file's mtime *before* the read and *after* the write,
    /// so a future audit can detect the TOCTOU pattern (the loop read
    /// stale data because a sibling `cs` invocation rewrote the file
    /// between read and write). Emitted from the runtime loop before any
    /// CLI shell-out that depends on the file's content.
    ///
    /// Soft contract — the event records that the runtime *believes* it
    /// performed an atomic transition; an audit comparing the two mtimes
    /// against `events.jsonl` from sibling sessions catches the breach.
    RuntimeReadDecideWrite {
        /// Absolute path of the file that was read and then written.
        path: String,
        /// File mtime (nanoseconds since epoch) the loop observed at read.
        ///
        /// `i64` (not `i128`): `events.jsonl` is JSON and `serde_json`
        /// has no `i128` support, so an `i128` field cannot round-trip —
        /// the event would be unreplayable, defeating its forensic
        /// purpose. `i64` nanoseconds-since-epoch is valid until year
        /// 2262, far beyond any audit horizon.
        pre_read_mtime_ns: i64,
        /// File mtime (nanoseconds since epoch) the loop observed after write.
        /// See [`Self::RuntimeReadDecideWrite::pre_read_mtime_ns`] for why
        /// this is `i64` rather than `i128`.
        post_write_mtime_ns: i64,
        /// One-line decision summary so audits can correlate cause and
        /// effect without parsing the file contents.
        decision: String,
    },

    /// **ADR-095 / RR-5** — the Resident Runtime shelled out to the
    /// transactional core (`cs evolve`, `cs tackle`, `cs done`, …).
    ///
    /// Keyed by `(mol_id, step_n, invocation_uuid)`; a duplicate
    /// `(mol_id, step_n)` with distinct `invocation_uuid` is a detectable
    /// idempotency violation (double `cs evolve`). Emitted *before* the
    /// shell-out so the trace survives an immediate crash of the loop.
    RuntimeShelledOut {
        /// The molecule the shell-out targets.
        molecule_id: MoleculeId,
        /// The verb shelled out — `"cs tackle"`, `"cs done"`,
        /// `"cs evolve"`, `"cs harvest"`, …
        verb: String,
        /// Zero-based step index, when the verb addresses a specific step.
        /// `None` for verbs that operate at molecule granularity
        /// (`cs tackle`, `cs done`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_n: Option<u32>,
        /// Random per-invocation identifier (UUID-shaped hex string).
        /// Two different `invocation_uuid` for the same `(mol_id, step_n)`
        /// is the audit signal for a double-fire.
        invocation_uuid: String,
    },

    /// **ADR-095 / RR-5** — the Resident Runtime dispatched a `cs done`
    /// (merge-back) for a molecule.
    ///
    /// Emitted *before* the `cs done` call. A `MergeDispatched` /
    /// `MergeCompleted` pair on the same molecule with no upstream
    /// `RuntimeMergeDispatched` is the audit signal that some sibling
    /// path merged the branch — a ghost-merge bypass.
    RuntimeMergeDispatched {
        /// The molecule whose branch is being merged.
        molecule_id: MoleculeId,
        /// Random per-invocation identifier so the upstream
        /// `RuntimeShelledOut { verb: "cs done" }` can be correlated.
        invocation_uuid: String,
    },

    /// **ADR-095 / RR-5** — the Resident Runtime claimed a worktree
    /// for a molecule by shelling out `cs tackle`.
    ///
    /// Concurrent `RuntimeWorktreeClaimed` events on the same path with
    /// distinct `invocation_uuid` are the audit signal for a stolen
    /// worktree (two runtimes raced to dispatch the same molecule).
    RuntimeWorktreeClaimed {
        /// The molecule the worktree is being claimed for.
        molecule_id: MoleculeId,
        /// Absolute filesystem path of the worktree the runtime claims.
        worktree_path: String,
        /// Random per-invocation identifier; correlates with the
        /// upstream `RuntimeShelledOut { verb: "cs tackle" }`.
        invocation_uuid: String,
    },

    /// **ADR-097 / WS-1** — an Adapter attempted to spawn a worker for a
    /// molecule.
    ///
    /// Emitted by the Worker-Spawn Port immediately *before* the underlying
    /// `backend.spawn_worker` (or equivalent for a future adapter). The
    /// event is the IFBDD anchor for the five Worker-Spawn silent-failure
    /// modes: an attendant audit query of the form
    /// `select(.event == "WorkerSpawnAttempted")` answers "did the adapter
    /// even try?" without parsing tmux output.
    ///
    /// `adapter_name` is a value of the existing `Adapter` primitive
    /// (ADR-079 §1) carried as a free-form string while the registry
    /// settles. For C2 only `"claude"` is populated.
    ///
    /// `pre_existing_worker` records a tmux-collision: if the adapter
    /// detected an already-running session under the target name before
    /// the spawn, the colliding [`WorkerId`] is recorded here. `None`
    /// is the normal case.
    WorkerSpawnAttempted {
        /// The molecule the worker is being spawned for.
        mol_id: MoleculeId,
        /// The worker identity the adapter intends to register.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// Worktree path the worker will execute in (relative or
        /// absolute, stored verbatim).
        worktree_path: String,
        /// Random per-invocation identifier. Two distinct
        /// `WorkerSpawnAttempted` for the same `(mol_id, worker_id)`
        /// with different `invocation_uuid` is the audit signal for
        /// WS-1's double-spawn pathology.
        invocation_uuid: String,
        /// OS process id observed at spawn time (best-effort; `0` when
        /// the adapter cannot recover it).
        pid: u32,
        /// Optional collision marker: a worker the adapter found
        /// already registered under the target session name before
        /// spawning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pre_existing_worker: Option<WorkerId>,
    },

    /// **ADR-097 / WS-2** — an Adapter probed a worker for liveness.
    ///
    /// Emitted by every liveness check the adapter performs (`check_alive`,
    /// pane-signature inspection, future API handshake). The
    /// `probe_kind` field discriminates *what* was probed; the
    /// `probe_result` field carries the verdict (alive with evidence,
    /// stuck with reason). The pair is the substrate for WS-2's silent-
    /// failure detection: an alive process whose `elapsed_since_last_advance_ms`
    /// keeps growing without the operator noticing.
    AdapterLivenessProbed {
        /// The molecule whose worker was probed.
        mol_id: MoleculeId,
        /// The worker identity that was probed.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// What kind of liveness probe was performed.
        probe_kind: AdapterProbeKind,
        /// The verdict the probe returned.
        probe_result: AdapterProbeResult,
        /// Milliseconds since the most recent observed forward progress
        /// on the worker (token advance, log line, pane diff). Best-
        /// effort; `0` when the adapter has no progress signal.
        elapsed_since_last_advance_ms: u64,
    },

    /// **ADR-097 / WS-3** — an Adapter checked the worker's tmux pane
    /// signature against the registered adapter signature.
    ///
    /// Emitted at the propulsion / whisper perturbation gate (ADR-038):
    /// before sending bytes to a worker pane, the adapter compares the
    /// pane's foreground command against the registered signature for
    /// the adapter (`claude`, `aider`, future entries). A mismatch is
    /// WS-3's silent-failure mode: bytes land in the wrong process.
    ///
    /// `channel` records which perturbation channel asked for the
    /// check — propulsion ($0-byte signal$) or whisper (semantic text).
    /// The variant exists in C2; emit-sites in `readiness.rs` and the
    /// perturbation gates are wired in C3 (PR-2).
    AdapterPaneSignatureChecked {
        /// The molecule whose worker pane was inspected.
        mol_id: MoleculeId,
        /// The worker identity being perturbed.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// The set of foreground-command tokens registered for the
        /// adapter (e.g. `["claude", "claude-code"]`).
        registered_signature: Vec<String>,
        /// The foreground command observed on the pane at probe time.
        observed_command: String,
        /// Whether `observed_command` matched any entry in
        /// `registered_signature`.
        matched: bool,
        /// Which perturbation channel asked for the check.
        channel: PerturbationChannel,
    },

    /// **ADR-097 / WS-4** — an Adapter consumed the molecule's briefing
    /// at worker-bootstrap time.
    ///
    /// Emitted by the spawn path when the adapter reads `briefing.md` to
    /// build the worker's initial prompt. The event records both the
    /// seal *observed* on disk and the seal *recorded* in
    /// `MoleculeData::briefing_seals` so a retrospective audit can
    /// detect WS-4's silent-failure mode: a post-seal edit to
    /// `briefing.md` that silently reshapes the worker's contract.
    ///
    /// `bytes_read` is best-effort: `/proc/self/io` on linux, self-
    /// reported byte count on macOS. The hot path must not fail because
    /// the measurement is unavailable.
    AdapterBriefingConsumed {
        /// The molecule whose briefing was consumed.
        mol_id: MoleculeId,
        /// The worker identity that consumed it.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// Path to the briefing file (relative to the molecule
        /// directory).
        briefing_path: String,
        /// BLAKE3 hash (64-char lowercase hex) of the briefing as
        /// the adapter actually read it.
        briefing_seal_observed: String,
        /// BLAKE3 hash (64-char lowercase hex) recorded in
        /// `MoleculeData::briefing_seals` for the current step.
        briefing_seal_recorded: String,
        /// Bytes read from the briefing file (best-effort).
        bytes_read: u64,
        /// Wall-clock time the briefing was consumed.
        consumed_at: DateTime<Utc>,
    },

    /// **ADR-097 / C6** — `cs tackle` selected a Worker-Spawn Adapter.
    ///
    /// Emitted by `cs tackle` (transactional core) at every invocation,
    /// before the adapter dispatch table is consulted. Records *which*
    /// adapter was chosen and *where* the choice came from
    /// ([`AdapterSelectionSource`]), so a retrospective audit can answer
    /// "did the academy-shim's `--adapter aider` actually route through?"
    /// without correlating against the operator's shell history.
    ///
    /// `role_hint` is a free-form, forensic-only label propagated by a
    /// driver (the academy-shim's `--role researcher` becomes
    /// `role_hint: "researcher"` here). Cosmon does not interpret it;
    /// it exists so the role-of-origin survives the seam between the
    /// driver's vocabulary (roles) and cosmon's vocabulary (adapters).
    AdapterSelected {
        /// The molecule the adapter is being selected for.
        mol_id: MoleculeId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        adapter_name: String,
        /// Wall-clock time the selection happened.
        selected_at: DateTime<Utc>,
        /// Where the selection came from — CLI flag, TOML config,
        /// built-in default, or (reserved) an envelope role.
        selection_source: AdapterSelectionSource,
        /// Forensic-only role-of-origin hint from the driver that
        /// invoked `cs tackle`. `None` for direct operator invocations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_hint: Option<String>,
        /// Per-Adapter [`LoopOwnership`] axis carried on the wire as a
        /// string newtype (ADR-103). `"external"` when cosmon dispatched
        /// to an external binary; `"cosmon"` when the loop runs
        /// in-process inside `cosmon-agent-harness`. The cat-test
        /// `adapter_selected.loop_ownership == worker_spawned.loop_ownership`
        /// surfaces a routing mismatch the way ADR-099 surfaces an
        /// `adapter_name` mismatch.
        ///
        /// `#[serde(default)]` keeps pre-ADR-103 `events.jsonl` lines
        /// round-trippable — the default tag is `"external"`.
        #[serde(default)]
        loop_ownership: LoopOwnershipTag,
    },

    /// **delib-20260704-b476 / C2** — `cs tackle` resolved a per-molecule
    /// **model** pin.
    ///
    /// The model sibling of [`Self::AdapterSelected`], co-minted with the
    /// spawn at the same transactional-core boundary — ex-ante and
    /// deterministic (before the availability probe / fallback chain runs).
    /// Records *which* model was pinned (`None` when nothing pinned one and
    /// the adapter's own default applies — the von-neumann floor) and *where*
    /// the choice came from ([`ModelSelectionSource`]: `flag` / `formula_pin`
    /// / `env_var` / `config` / `global_config` / the `default` floor).
    ///
    /// Before this variant the attribution lived only in a per-molecule
    /// `model-selection.json` file (`record_model_selection`); promoting it to
    /// a typed event on the wire lets the ceiling guard fold over
    /// `events.jsonl` (`ModelSelected` count of strong dispatches, the
    /// `reconcile` idiom — never a mutable counter file) and lets `cs
    /// ensemble` / `cs observe` surface the model + source without parsing a
    /// sidecar file. carnot needs the log-fold, kahneman needs the
    /// observability + ghost-guards, von-neumann tracks the source through the
    /// chain — convergence #4 of the delib.
    ///
    /// The hot path must not fail because telemetry is unhappy: like the other
    /// spawn-time receipts, write errors are swallowed (trace-not-lock).
    ModelSelected {
        /// The molecule the model is being selected for.
        mol_id: MoleculeId,
        /// Adapter the model id is scoped to (a model id only has meaning
        /// inside its adapter). Carried so the `(adapter, model)` bundle is
        /// legible in one line without correlating against the sibling
        /// [`Self::AdapterSelected`].
        adapter_name: String,
        /// The pinned model id, or `None` when nothing pinned a model and the
        /// adapter's own default applies (the safe floor — silence never
        /// resolves to a named strong model).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Where the selection came from — CLI flag, formula-step pin, env
        /// var, per-galaxy / global config, or the `None` floor.
        selection_source: ModelSelectionSource,
        /// Wall-clock time the selection happened (before the spawn probe).
        selected_at: DateTime<Utc>,
    },

    /// **delib-20260718-c70e / realized-model** — an adapter reported the
    /// *concrete* model it actually ran, observed at execution time.
    ///
    /// The ex-post empirical sibling of the ex-ante [`Self::ModelSelected`]:
    /// `ModelSelected` records the *intention* (the pin resolved through the
    /// ladder, minted before the run); `ModelObserved` records the
    /// *realization* (the id the adapter's own output names once it is
    /// running). They **coexist** and neither subsumes the other — an unpinned
    /// dispatch (`ModelSelected.model == None`) can still observe a concrete
    /// realized id, and a pinned dispatch can realize a *different* id (a dated
    /// fallback, or a mid-session quota downgrade Opus→Sonnet). The
    /// retrospective fold
    /// ([`crate::adapter_attribution::AdapterAttribution`]) folds this event
    /// onto a `realized` axis **disjoint** from the intention axis.
    ///
    /// # The honesty invariant, made structural
    ///
    /// The [`model`](Self::ModelObserved::model) field is a **bare `String`**,
    /// never `Option`. Silence — an adapter that cannot report which model ran
    /// (codex/aider today) — is expressed by *not emitting the event at all*,
    /// so "never fabricate a record of execution" is true *by construction*:
    /// there is no `ModelObserved` value that means "ran but unknown". The fold
    /// reads this event and **only** this event for the realized axis; it never
    /// back-fills the realized id from the pin or the config (sibling of the
    /// `reasoning_effort_is_never_inferred` rule).
    ///
    /// # Cadence
    ///
    /// Emitted on the **first** assistant turn that carries a concrete id, and
    /// **re-emitted only on change** (a later turn naming a different id — the
    /// quota-fallback case). Not per-turn, not at teardown. A fold over the
    /// events in append order reconstructs the realized *trajectory*
    /// (`Observed(vec![opus, sonnet])`).
    ///
    /// The hot path must not fail because telemetry is unhappy: like the other
    /// spawn-time receipts, write errors are swallowed (trace-not-lock).
    ModelObserved {
        /// The molecule whose worker reported the model.
        mol_id: MoleculeId,
        /// The worker/dispatch the observation belongs to — the per-attempt
        /// scoping key (delib-20260718-c70e, F-02). A model id is only
        /// meaningful *inside the run that produced it*: after a re-tackle the
        /// fold must attribute a stale observation to the attempt that emitted
        /// it, never to the new one. **Mandatory for new lines** (round-3): the
        /// emit helpers require a concrete `WorkerId`, so `None` occurs only on
        /// legacy lines predating this field (`#[serde(default)]`). The fold
        /// treats such unscoped lines **fail-closed** — they never match an
        /// attempt that recorded a worker boundary, and the dedup never lets
        /// them suppress a scoped observation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_id: Option<WorkerId>,
        /// Adapter the observation is scoped to (a model id only has meaning
        /// inside its adapter), mirroring [`Self::ModelSelected`].
        adapter_name: String,
        /// The concrete model id the adapter reported running — a **bare
        /// `String`**: the event is emitted *only* when a real id was observed,
        /// so this field can never be a fabricated placeholder.
        model: String,
        /// Where the realized id was read from — per-adapter provenance for
        /// forensics. Never surfaced at the display (`realized` is an outcome,
        /// not a choice, so it carries no source tag).
        observed_source: crate::model_realization::ModelObservationSource,
        /// Wall-clock time the observation was recorded.
        observed_at: DateTime<Utc>,
    },

    /// **delib-20260704-b476 / C4** — the fail-closed per-galaxy model-dispatch
    /// ceiling fired: a *strong* model pin was refused because the rolling
    /// window already held `cap` strong dispatches.
    ///
    /// Co-minted with the (down)spawn at the transactional-core boundary, this
    /// is the loud, typed receipt carnot's safety property demands — the
    /// (K+1)th strong dispatch cannot cross the ceiling silently. `action`
    /// records what cosmon did: [`CeilingAction::Downgraded`] (dropped the pin
    /// to the safe floor and spawned economical) or [`CeilingAction::Aborted`]
    /// (refused the spawn entirely), per the operator's
    /// [`on_overflow`](crate::config::ModelBudgetConfig) policy.
    ///
    /// A fold over these events answers "how often did the ceiling bite, and
    /// what did it cost me not to spawn?" without a mutable counter file. The
    /// hot path must not fail because telemetry is unhappy: write errors are
    /// swallowed (trace-not-lock).
    ModelCeilingHit {
        /// The molecule whose strong dispatch was refused.
        mol_id: MoleculeId,
        /// Adapter the refused model id is scoped to.
        adapter_name: String,
        /// The strong model id that was requested and refused/downgraded.
        model: String,
        /// The strong-dispatch count already in the window at decision time
        /// (`>= cap` — that is why the ceiling fired).
        strong_count: u32,
        /// The configured per-galaxy cap `K`.
        cap: u32,
        /// The rolling window width (hours) the count was taken over.
        window_hours: u32,
        /// What cosmon did — downgrade to the floor, or abort the spawn.
        action: CeilingAction,
        /// Wall-clock time the ceiling fired.
        hit_at: DateTime<Utc>,
    },

    /// **Autonomy guard** — `cs tackle` granted a
    /// strict-autonomy worker outbound egress to a remote oracle because the
    /// operator opted in (`--adapter claude` / `openai` / `anthropic` /
    /// `aider`).
    ///
    /// Emitted **before** the worker is spawned, so the egress grant and the
    /// audit record are minted as the *same atom* and cannot diverge: there
    /// is no window in which a worker reaches the network without a matching
    /// `remote_egress_opt_in` line on the wire. The dual of the strict-local
    /// default, where no such line exists because no egress was granted.
    ///
    /// The cutover audit reads this event for C3 (any `remote_egress_opt_in`
    /// in the window means the worker was *not* strict-local) and for the
    /// opt-in seam's provenance.
    RemoteEgressOptIn {
        /// The molecule whose worker was granted remote egress.
        mol_id: MoleculeId,
        /// Adapter name that triggered the opt-in.
        adapter_name: String,
        /// Best-known remote endpoint host the egress was opened to. `None`
        /// when cosmon does not own the adapter's endpoint (e.g. `aider`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_host: Option<String>,
        /// Best-known remote endpoint port. `None` alongside `endpoint_host`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_port: Option<u16>,
        /// Wall-clock time the opt-in was stamped (before spawn).
        opted_in_at: DateTime<Utc>,
    },

    /// **Autonomy guard** — a `deny-external` dispatch could not be
    /// kernel-enforced on this host and degraded to
    /// [`EnforcementMode::Advisory`](crate::egress::EnforcementMode::Advisory).
    ///
    /// Minted by `cs tackle` *before* spawning a strict-local worker on a host
    /// that cannot create the egress-denied network namespace — a macOS dev
    /// host, or a hardened Linux kernel with unprivileged user namespaces
    /// disabled. The degradation is loud by construction: the policy is
    /// recorded but not enforced, and the cutover gate refuses to flip the
    /// hosted-tenant default while any spawn carries this line. This is the
    /// C1-F3 fix (task-20260712-8d2d) — before it, the same situation on a
    /// hardened Linux kernel produced an *opaque, total* failure (every
    /// `exec_command` died with `"shell died during init"`) with no audit
    /// trail, and `enforcement_mode()` still claimed `Netns`.
    EgressUnenforceable {
        /// The molecule whose worker degraded to advisory enforcement.
        mol_id: MoleculeId,
        /// Adapter name of the strict-local worker.
        adapter_name: String,
        /// Operator-facing reason the requested denial could not be enforced.
        reason: String,
        /// Wall-clock time the degradation was stamped (before spawn).
        degraded_at: DateTime<Utc>,
    },

    /// **Local-first fail-policy** — a worker fell back from the local-default
    /// adapter to a remote oracle because the local model **hard-failed**.
    ///
    /// Only *decidable* hard-failures (crash / OOM /
    /// timeout / connection-refused; see [`LocalFailureCause`]) trigger a
    /// fallback. Soft "is the output good?" judgement is undecidable (Rice's
    /// theorem) and is NOT a trigger — strict-fail and silent-fallback are
    /// both rejected.
    ///
    /// **The atom.** This event is minted by `cs tackle` at spawn, in the
    /// *same code block* as the [`RemoteEgressOptIn`](Self::RemoteEgressOptIn)
    /// grant it accompanies. There is no automatic in-loop fallback: the
    /// only path from a local hard-failure to a remote oracle is a conscious
    /// re-`tackle` with a remote `--adapter` *and* `--fallback-from-local
    /// <cause>`, which mints THIS line next to the egress grant. A remote
    /// call carrying a fallback cause therefore can never happen without a
    /// matching loud audit record — **silent fallback is impossible by
    /// construction**, which is the entire point of the fail-policy.
    ///
    /// [`LocalFailureCause`]: crate::egress::LocalFailureCause
    LocalFallback {
        /// The molecule whose worker fell back to a remote oracle.
        mol_id: MoleculeId,
        /// The local adapter that hard-failed (the default that did not
        /// hold — usually `"local"`).
        from_adapter: String,
        /// The remote adapter the operator opted into as the fallback
        /// (e.g. `"claude"`).
        to_adapter: String,
        /// Decidable hard-failure class that justified the fallback, as the
        /// [`LocalFailureCause`](crate::egress::LocalFailureCause) wire token
        /// (`"crash"` / `"oom"` / `"timeout"` / `"connection-refused"` / a
        /// bespoke string for `Other`).
        cause: String,
        /// Wall-clock time the fallback was stamped (before spawn, in the
        /// same atom as the egress grant).
        fell_back_at: DateTime<Utc>,
    },

    /// **Autonomy guard** — positive per-turn evidence
    /// that a turn was produced by local inference.
    ///
    /// The polarity-flipped witness for cutover criterion C1: forgery has no
    /// receipt. Carries the three legs of local-exec proof — in-process FFI
    /// receipt, throughput band, and accelerator load during the turn's
    /// wall-clock window. A `relabel-timing` attack (a remote turn stamped
    /// `local`) produces no FFI receipt and a network-bound throughput, so it
    /// cannot mint a positive receipt.
    LocalExecReceipt {
        /// The molecule the turn belongs to.
        mol_id: MoleculeId,
        /// Zero-based turn index within the worker's agent loop.
        turn: u32,
        /// `true` when the turn was produced by an in-process FFI inference
        /// call (the ground-truth local signature).
        ffi_receipt: bool,
        /// Observed throughput for the turn, tokens/second.
        throughput_tok_s: f64,
        /// Band classification (`"local"` / `"suspect"`) of the throughput.
        throughput_band: String,
        /// Accelerator load (0..1) during the turn's wall-clock window.
        accelerator_load: f64,
        /// Wall-clock time the receipt was observed.
        observed_at: DateTime<Utc>,
    },

    /// **ADR-097 / WS-5** — an Adapter reconciled its handle on a worker
    /// process.
    ///
    /// Emitted at every adapter-side teardown (`kill_session`, harvest,
    /// patrol cleanup). Closes WS-5's silent-failure mode: the adapter
    /// holds the handle long after the underlying process exited, or
    /// releases the handle even though the process is still alive
    /// (orphan). `gap_ms` is the signed delta between
    /// `underlying_exit_observed_at` and `handle_released_at` — negative
    /// when the handle was released before the exit was observed.
    AdapterHandleReconciled {
        /// The molecule whose worker handle is being reconciled.
        mol_id: MoleculeId,
        /// The worker identity whose handle is being reconciled.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// Final state of the adapter-side handle.
        handle_state: AdapterHandleState,
        /// Wall-clock time the adapter observed the underlying process
        /// exit. `None` when the adapter never observed an exit (the
        /// handle is being released defensively).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        underlying_exit_observed_at: Option<DateTime<Utc>>,
        /// Wall-clock time the adapter released its handle.
        handle_released_at: DateTime<Utc>,
        /// Signed millisecond gap between
        /// `underlying_exit_observed_at` and `handle_released_at`.
        /// Positive when the handle outlived the process; negative
        /// when the handle was released before the exit was observed;
        /// `0` when `underlying_exit_observed_at` is `None`.
        gap_ms: i64,
    },

    /// **ADR-100 / R2 wave 2 / SF-1** — an HTTP call to an LLM provider
    /// failed terminally after retry exhaustion (network down, DNS
    /// failure, TLS handshake error, persistent connection refused).
    ///
    /// The Adapter sees the error; the operator does not see *that the
    /// loop tried at all* unless this event lands. Without it, the
    /// failure looks indistinguishable from a wedged worker
    /// (`cs peek` shows alive, no progress; the loop is actually
    /// dead-stick on the network). The five forensic fields
    /// ([`provider_name`](#variant.SF1HttpTransport.field.provider_name),
    /// [`adapter_name`](#variant.SF1HttpTransport.field.adapter_name),
    /// [`model_name`](#variant.SF1HttpTransport.field.model_name),
    /// [`trigger_context`](#variant.SF1HttpTransport.field.trigger_context),
    /// [`recovery_attempted`](#variant.SF1HttpTransport.field.recovery_attempted))
    /// are the cross-variant invariant of the SF taxonomy; the
    /// remaining fields are SF-1-specific (retry count, error class).
    ///
    /// Emitted by Direct-API Adapters (`openai-direct`, `anthropic-direct`,
    /// future entries) at the moment the transport gives up. The Claude
    /// tmux Adapter does not emit this event — the subprocess-CLI sibling
    /// surfaces network failures through its own stderr.
    #[serde(rename = "sf1_http_transport")]
    SF1HttpTransport {
        /// The molecule whose worker hit the transport failure.
        mol_id: MoleculeId,
        /// The worker identity executing the failed call.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        adapter_name: String,
        /// Provider key (e.g. `"openai"`, `"anthropic"`, `"xai"`).
        provider_name: String,
        /// Model identifier as known to the Adapter at call time
        /// (`Some("gpt-4o")`, …). `None` when the Adapter cannot
        /// recover it (e.g. a discovery call that never resolved a
        /// model).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        /// Free-form context label describing *what* the loop was
        /// doing when the failure hit (e.g. `"completion"`,
        /// `"tool_result_followup"`, `"discovery"`). Never branched on.
        trigger_context: String,
        /// Whether the Adapter attempted a recovery (e.g. fallback to
        /// a sibling provider, switch to non-streaming) before giving
        /// up. `false` means the loop terminated on the first failure.
        recovery_attempted: bool,
        /// Number of retries the Adapter exhausted before this event
        /// was emitted. `0` means the first attempt itself failed
        /// terminally.
        retry_count: u32,
        /// Free-form error class (`"dns"`, `"tls"`, `"connection_refused"`,
        /// `"timeout"`, `"tcp_reset"`). Never branched on by the
        /// runtime; preserved for audit triage.
        error_class: String,
    },

    /// **ADR-100 / R2 wave 2 / SF-2** — the provider returned HTTP 429
    /// (or an equivalent quota-exhausted code) and the Adapter
    /// ultimately surrendered after the retry-after budget elapsed.
    ///
    /// Distinct from SF-1: the *transport* succeeded; the *provider*
    /// declined to serve. Without this event the failure mode reads
    /// identically to a wedged worker (the loop is genuinely sleeping
    /// inside `retry-after`, then aborts silently when the cumulative
    /// backoff exceeds budget). This is the silent-rate-limit-backoff
    /// failure mode the event exists to surface.
    #[serde(rename = "sf2_provider_rate_limit")]
    SF2ProviderRateLimit {
        /// The molecule whose worker hit the quota wall.
        mol_id: MoleculeId,
        /// The worker identity executing the call.
        worker_id: WorkerId,
        /// Adapter name.
        adapter_name: String,
        /// Provider key.
        provider_name: String,
        /// Model identifier, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        /// Free-form context label (`"completion"`, `"tool_result"`).
        trigger_context: String,
        /// Whether a recovery was attempted (e.g. switch to a fallback
        /// model, downgrade quality).
        recovery_attempted: bool,
        /// `Retry-After` value (in seconds) the provider returned on
        /// the final 429 — `0` when the header was absent.
        retry_after_secs: u64,
        /// Free-form classification of the quota dimension
        /// (`"rpm"`, `"tpm"`, `"daily"`, `"concurrency"`,
        /// `"organization"`). Never branched on.
        quota_kind: String,
    },

    /// **ADR-100 / R2 wave 2 / SF-3** — the provider returned a 2xx
    /// response, but its JSON body could not be decoded into the
    /// Adapter's schema for that provider (unknown field shape, missing
    /// required key, malformed enum tag).
    ///
    /// This is the silent-failure mode that bites first when a provider
    /// quietly evolves its response shape and the Adapter's deserializer
    /// has no fallback. Without this event, the loop drops the response
    /// and calls `cs evolve` with empty output — the operator only
    /// learns about the drift when downstream behaviour diverges from
    /// expectation.
    #[serde(rename = "sf3_schema_decode_failure")]
    SF3SchemaDecodeFailure {
        /// The molecule whose worker emitted the call.
        mol_id: MoleculeId,
        /// The worker identity that received the malformed response.
        worker_id: WorkerId,
        /// Adapter name.
        adapter_name: String,
        /// Provider key.
        provider_name: String,
        /// Model identifier, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        /// Free-form context label describing the call type.
        trigger_context: String,
        /// Whether the Adapter attempted a recovery (e.g. retry with a
        /// stricter request, fall back to a non-streaming endpoint).
        recovery_attempted: bool,
        /// First ~512 bytes of the underlying serde decode error so an
        /// audit can triage without re-running the call.
        decode_error: String,
        /// Total response body byte length the Adapter received before
        /// the decode failed.
        response_bytes: u64,
    },

    /// **ADR-100 / R2 wave 2 / SF-4** — a tool call invoked by the
    /// agent loop returned a non-zero exit (or equivalent failure
    /// signal), but the Adapter advanced the worker as if the call had
    /// succeeded.
    ///
    /// Without this event the failure mode is the cruellest of the
    /// taxonomy: the loop continues, the model sees a successful tool
    /// result that is silently empty or wrong, the molecule completes
    /// on a hallucinated trail. This is the silent-tool-result-mismatch
    /// pathology: a failing tool call that the loop never notices.
    #[serde(rename = "sf4_tool_call_execution_failure")]
    SF4ToolCallExecutionFailure {
        /// The molecule whose worker fired the failing tool call.
        mol_id: MoleculeId,
        /// The worker identity that executed the tool.
        worker_id: WorkerId,
        /// Adapter name.
        adapter_name: String,
        /// Provider key.
        provider_name: String,
        /// Model identifier, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        /// Free-form context label (`"tool_use"`, `"function_call"`).
        trigger_context: String,
        /// Whether the Adapter attempted a recovery (e.g. retry the
        /// tool, return the error to the model as a `tool_error` block
        /// rather than silently swallowing it).
        recovery_attempted: bool,
        /// Tool name the loop invoked (`"bash"`, `"str_replace_editor"`,
        /// `"web_fetch"`, …).
        tool_name: String,
        /// Process exit code from the failing tool, or `-1` when the
        /// failure surfaced through a non-process signal (timeout,
        /// internal error).
        exit_code: i32,
        /// Tail of the tool's stderr output (up to ~500 bytes) for
        /// triage.
        stderr_tail: String,
    },

    /// **ADR-100 / R2 wave 2 / SF-5** — the assembled prompt exceeded
    /// the model's max context window; the request was silently
    /// truncated (or the provider truncated server-side) and the
    /// response is incomplete.
    ///
    /// Without this event, an `cs evolve` lands with a partial answer
    /// the loop accepted as final. The forensic ask is to record
    /// `input_tokens_estimated` versus `max_context_tokens` at the
    /// moment of overflow so an audit can attribute the drift between
    /// expected and actual output to context truncation rather than
    /// model regression — the silent-context-overflow failure mode.
    #[serde(rename = "sf5_context_overflow")]
    SF5ContextOverflow {
        /// The molecule whose worker hit the cap.
        mol_id: MoleculeId,
        /// The worker identity making the request.
        worker_id: WorkerId,
        /// Adapter name.
        adapter_name: String,
        /// Provider key.
        provider_name: String,
        /// Model identifier, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        /// Free-form context label (`"completion"`,
        /// `"tool_result_followup"`).
        trigger_context: String,
        /// Whether the Adapter attempted a recovery (compaction,
        /// summarisation, drop-oldest-tool-result).
        recovery_attempted: bool,
        /// Adapter-side estimate of input tokens at request time
        /// (tokeniser-derived; best-effort).
        input_tokens_estimated: u64,
        /// Model's advertised max context window in tokens. `0` when
        /// the Adapter could not look the value up at emission time.
        max_context_tokens: u64,
        /// Whether the Adapter applied client-side truncation before
        /// sending. `false` means the provider truncated server-side
        /// (or the request was sent intact and the response was
        /// truncated).
        truncation_applied: bool,
    },

    /// **ADR-100 / R2 wave 2 / SF-6** — `cs tackle` successfully
    /// spawned the worker (the adapter call completed, the agent loop
    /// returned a real artefact on disk, the tmux session is alive),
    /// but a *post-spawn supervision setup step* failed — typically the
    /// kernel-level pane-died hook (`tmux set-hook`) that arms the
    /// worker-exit → `cs harvest` bridge.
    ///
    /// Distinct from SF-1..5 (which are all *LLM provider* failure
    /// modes inside the agent loop): SF-6 is a cosmon-lab
    /// supervision failure that happens *after* the loop has done its
    /// work. The structural nuance is L9-aligned — the work was
    /// performed and must be comptabilised, but the supervision is
    /// missing and must be flagged so the operator does not mistake
    /// the worktree for "a normally-running molecule under hook
    /// surveillance".
    ///
    /// Pre-SF-6, the post-spawn pipeline tore down the worktree, the
    /// branch and the tmux session on any supervision setup failure
    /// (`cleanup_partial_tackle`), which erased the agent's output as
    /// collateral damage. The chronicle `2026-05-18-grok-direct-api-
    /// smoke-result.md` §"Ce qui n'a pas marché" #2-3 documents the
    /// case where `grok-3` wrote a haiku, the supervision setup
    /// failed, and the wipe blew away the only physical evidence the
    /// Direct-API loop had produced anything at all. SF-6 amends the
    /// ADR-052 child #4 hard precondition: the hook is still the
    /// strongly-preferred witness, but its failure no longer
    /// invalidates the worker's already-delivered artefact. The
    /// operator inherits one polling-fallback-only molecule plus a
    /// loud forensic event, rather than a silently-disappeared
    /// worktree.
    ///
    /// Emitted by the `cs tackle` post-spawn pipeline at the moment a
    /// supervision step (currently only `install_pane_died_hook`)
    /// returns an error. The forensic ask is to identify *which* hook
    /// failed (`hook_name`) under which adapter so future cosmon-ward
    /// debugging can attribute drift between expected supervision
    /// coverage and observed reality.
    #[serde(rename = "sf6_supervision_setup_failed")]
    SF6SupervisionSetupFailed {
        /// The molecule whose worker was just spawned and whose
        /// supervision setup failed.
        mol_id: MoleculeId,
        /// The worker identity already alive in tmux at the moment of
        /// the supervision failure.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        /// Preserved so an audit can attribute supervision-setup
        /// failures to a specific adapter family.
        adapter_name: String,
        /// Symbolic name of the supervision hook that failed (currently
        /// only `"pane_died"`; future supervision steps land here too).
        /// Never branched on — preserved for triage.
        hook_name: String,
        /// First ~500 bytes of the underlying error from the
        /// supervision call. Preserved verbatim so a triage can
        /// classify without re-running the call.
        error: String,
    },

    /// **SF-7** — the on-PATH binary that
    /// backs a subprocess Adapter (codex today; future subprocess CLI
    /// siblings later) did not match the version pin the Adapter
    /// requires.
    ///
    /// Distinct from SF-1..SF-5 (LLM-provider failure modes) and from
    /// SF-6 (cosmon-lab supervision setup): SF-7 fires *before*
    /// the spawn — the constructor's three-pillar check
    /// (`.cosmon/adapters/<adapter>.toml` → `<binary> --version` →
    /// equality) refused to build the Adapter. Without this event the
    /// failure is forensically silent: the operator sees only "spawn
    /// failed" on stderr and has to re-run `<binary> --version` by
    /// hand to learn the drift.
    ///
    /// The codex Adapter (`crates/cosmon-transport/src/codex.rs`)
    /// emits this envelope from two stack frames:
    /// 1. **Eager path** — `CodexAdapter::new`/`new_with_config_path`
    ///    detects the mismatch during construction and emits SF-7
    ///    *before* returning the `BinaryVersionMismatch` error;
    /// 2. **Deferred path** — `CodexAdapter::default_for_dispatch`
    ///    constructs without a project root, so the version check
    ///    runs inside `Spawn::spawn` using `SpawnConfig::work_dir`;
    ///    SF-7 lands at the same telemetry sink, one stack-frame
    ///    later. The exact same forensic discipline applies.
    ///
    /// The legacy [`Self::AdapterLivenessProbed`] Stuck-with-`SF-7
    /// binary_version_mismatch` reason prefix is preserved during a
    /// transition window so the existing cat-test query
    /// (`jq 'select(.reason | startswith("SF-7"))'`) still surfaces
    /// the mismatch; new audits should prefer the structured SF-7
    /// variant.
    #[serde(rename = "sf7_binary_version_mismatch")]
    SF7BinaryVersionMismatch {
        /// The molecule whose spawn was refused.
        mol_id: MoleculeId,
        /// The worker identity the Adapter would have spawned.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        /// Preserved so an audit can attribute version-pin drift to a
        /// specific adapter family.
        adapter_name: String,
        /// Name of the binary the Adapter probed (e.g. `"codex"`).
        /// Preserved so the same SF-7 channel can serve future
        /// subprocess CLI siblings without growing a new variant.
        binary_name: String,
        /// The pinned version range declared in
        /// `.cosmon/adapters/<adapter>.toml`. Preserved verbatim,
        /// including the optional Cargo-style `=X.Y.Z` prefix.
        expected_version_range: String,
        /// The version string parsed from `<binary> --version`.
        /// Preserved verbatim — no normalisation, so triage can spot
        /// drift between the binary's self-report and the operator's
        /// expectation.
        actual_version: String,
    },

    /// **ADR-097 / WS-1'** —
    /// terminal partner for [`Self::WorkerSpawnAttempted`] when the
    /// underlying backend spawn returned an error.
    ///
    /// Today a WS-1 emission is followed by `backend.spawn_worker`; if
    /// that call fails the adapter returns `Err(SpawnFailed(_))` and
    /// `cs tackle`'s cleanup path tears the partial setup down — but
    /// **no terminal event lands** on `events.jsonl`. The trail leaves
    /// WS-1 ambiguous between "live but unprobed" and "never alive"
    /// (the TLA+ invariant `I1 — ws1_implies_ws5` is falsified). Emit
    /// this variant from `spawn_*_session` before propagating the
    /// error so every WS-1 has a terminal partner alongside WS-5 and
    /// WS-1''.
    ///
    /// `reason` is a free-form, forensic-only string lifted from the
    /// underlying error. Never branched on; preserved for audit.
    WorkerSpawnFailed {
        /// The molecule the spawn was attempted for.
        mol_id: MoleculeId,
        /// The worker identity the adapter intended to register.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// First ~500 bytes of the underlying spawn error. Preserved
        /// verbatim so triage can attribute the failure (tmux not on
        /// `PATH`, claude binary missing, `work_dir` absent, …) without
        /// re-running the spawn.
        reason: String,
        /// Wall-clock time the spawn returned `Err`.
        failed_at: DateTime<Utc>,
    },

    /// **ADR-097 / WS-1''** —
    /// terminal partner for [`Self::WorkerSpawnAttempted`] when
    /// `cs tackle`'s post-lock read-modify-write race detector
    /// (`crates/cosmon-cli/src/cmd/tackle.rs:627-699`) rolled the
    /// partial spawn back.
    ///
    /// Without this event, a concurrent non-locking writer (`cs tag`,
    /// `cs link`, `cs decay`) that reverts the molecule to Pending
    /// after the fleet lock released will SIGKILL the live worker mid-
    /// startup, remove the `WorkerId` from the fleet, and leave the
    /// audit trail saying "WS-1, full stop." Emit this variant from
    /// the rollback path *before* the `WorkerId` is removed from the
    /// fleet, so telemetry context is preserved.
    ///
    /// `reason` records the observed status the racer left behind
    /// (typically `"pending"` or `"queued"`) so a retrospective audit
    /// can identify the racing writer without parsing the rollback
    /// error string. Forensic-only; never branched on.
    WorkerSpawnRolledBack {
        /// The molecule whose spawn was rolled back.
        mol_id: MoleculeId,
        /// The worker identity whose registration was reverted.
        worker_id: WorkerId,
        /// Adapter name (value of the `Adapter` primitive, ADR-079 §1).
        #[serde(default = "default_adapter_name")]
        adapter_name: String,
        /// Free-form forensic description of the rollback trigger
        /// (`"pending"`, `"queued"`, `"concurrent_writer_reverted"`).
        reason: String,
        /// Wall-clock time the rollback path observed the divergence.
        rolled_back_at: DateTime<Utc>,
    },

    /// **ADR-105 / I9'** —
    /// a chronicle entry was added to the galaxy's chronicle file
    /// (or a sibling chronicle file).
    ///
    /// Carries the federation-provenance machinery that closes the
    /// I9' "doctrine without machinery" gap: when the chronicle text
    /// cites a sister-galaxy artefact (smithy, mailroom, …), the
    /// `cites_galaxies` field enumerates them and
    /// `federation_provenance` MUST carry the typed
    /// [`crate::federation::FederationLineage`] that points to the
    /// source commit. `cs verify --federation` lints cross-galaxy
    /// entries (`cites_galaxies` non-empty after removing `"cosmon"`)
    /// whose `federation_provenance` is `None` and reports them as
    /// hard failures.
    ///
    /// The field is `Option<...>` for the same backward-compatibility
    /// reasons (and the same detection-not-prevention discipline) that
    /// govern the merge events: legacy `events.jsonl` lines pre-date
    /// the field, and the cosmon stance is to log distinctly, not to
    /// refuse the emit. See [`Self::MergeDispatched`] for the merge-
    /// event counterpart.
    ChronicleAdded {
        /// The molecule whose work seeded the chronicle entry, when
        /// the chronicle is anchored to a tracked molecule. `None`
        /// for free-standing chronicle prose (a Feynman moment that
        /// did not flow from a single molecule).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        molecule_id: Option<MoleculeId>,
        /// Path (relative to the galaxy root) of the chronicle file
        /// that received the entry (the galaxy's chronicle file or a
        /// sibling chronicle file).
        chronicle_path: String,
        /// Anchor / heading the entry was inscribed under, when
        /// captured at emission time. Free-form; never branched on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_anchor: Option<String>,
        /// Galaxy aliases cited by the chronicle entry. An entry that
        /// names `smithy` (e.g. *"smithy W3 landed RPP binding"*)
        /// MUST set `cites_galaxies = vec!["smithy"]` so the lint
        /// can detect missing federation provenance.
        ///
        /// `vec!["cosmon"]` (or empty) is the purely local case; the
        /// lint treats `cites_galaxies \ {"cosmon"}` as the cross-
        /// galaxy footprint.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cites_galaxies: Vec<String>,
        /// Federation lineage when the entry references sister-galaxy
        /// content. `None` for purely local entries. See
        /// [`crate::federation::FederationLineage`] and ADR-105 §D3
        /// Oracle B''.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        federation_provenance: Option<crate::federation::FederationLineage>,
    },

    /// **ADR-105 / I9'** —
    /// a new ADR (Architecture Decision Record) was inscribed under
    /// `docs/adr/`.
    ///
    /// Carries the federation-provenance machinery on the same axis
    /// as [`Self::ChronicleAdded`]: when an ADR extends or cites a
    /// sister-galaxy ADR (e.g. cosmon's ADR-049 inheriting
    /// mailroom's cosmon-ward feedback flow), `cites_galaxies`
    /// enumerates the peers and `federation_provenance` MUST be
    /// `Some(...)`. The lint reports cross-galaxy ADRs missing the
    /// field as hard failures.
    ///
    /// The field is `Option<...>` for backward compatibility — the
    /// ADR corpus pre-dates the field. New ADRs that cite peers must
    /// fill it.
    AdrInscribed {
        /// ADR number as inscribed in the filename (e.g. `105` for
        /// `docs/adr/105-i9-prime-federation-provenance.md`). `u32`
        /// because cosmon's ADR corpus is bounded; the wire format
        /// is the lossless integer JSON shape.
        adr_number: u32,
        /// ADR title (the H1 of the file). Free-form prose; never
        /// branched on.
        title: String,
        /// Path (relative to the galaxy root) of the ADR file, e.g.
        /// `"docs/adr/105-i9-prime-federation-provenance.md"`.
        adr_path: String,
        /// Galaxy aliases this ADR cites or inherits from. An ADR
        /// that extends `mailroom/ADR-007` MUST set
        /// `cites_galaxies = vec!["mailroom"]`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cites_galaxies: Vec<String>,
        /// Federation lineage when the ADR references sister-galaxy
        /// content. `None` for purely local ADRs. See ADR-105 §D3.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        federation_provenance: Option<crate::federation::FederationLineage>,
    },

    /// **Config-honoring dispatch** — the Resident
    /// Runtime detected that the on-disk config it sealed at launch no
    /// longer matches what is on disk, and **halted fail-closed** *before*
    /// forming a dispatch.
    ///
    /// This is the typed counterpart of the `prompt_seal` /
    /// `briefing_seals` BLAKE3 trace primitive (§8b of
    /// `docs/architectural-invariants.md`), lifted from the per-molecule
    /// cognitive contract to the runtime's own *launch* contract:
    /// `H = BLAKE3(resolved_config)` is computed at launch and `H'`
    /// recomputed before every dispatch. `H' != H` means the runtime can no
    /// longer *witness its own freshness*, so it refuses the dispatch and
    /// exits non-zero for a fresh supervisor relaunch (Q2b
    /// bounded-ephemeral — never self-repair in place).
    ///
    /// **The seal witnesses config drift only, not binary drift.**
    /// It used to mix in the `cs` binary image, but
    /// `cs done`'s post-merge `just install` reinstalls that binary on
    /// every successful drain — so the runtime tripped this event on its
    /// own success. The binary term was dropped; a reinstall mid-run is the
    /// expected steady state, not drift.
    ///
    /// Forensic-only: it records a *refusal*, not a spec transition. The
    /// load-bearing effect is the **absence** of the dispatch the runtime
    /// would otherwise have formed on a stale snapshot — the silent
    /// wrong-oracle billing is prevented by construction
    /// because the HTTP request is never *formed* (it stops at the
    /// irreversibility boundary), not caught after the fact.
    ConfigDriftDetected {
        /// The seal computed at runtime launch — `blake3:<hex>` over the
        /// resolved config bytes (per-galaxy + global `config.toml`).
        launch_seal: String,
        /// The seal recomputed immediately before the refused dispatch.
        /// Differs from `launch_seal` by construction (that mismatch is
        /// precisely why the runtime halted).
        current_seal: String,
        /// The `cs` verb the runtime refused to shell out (`"tackle"` /
        /// `"done"`). Free-form; never branched on.
        refused_verb: String,
        /// The molecule the refused decision targeted, when the decision
        /// addressed a specific molecule. Carried as a free-form string
        /// (not a [`MoleculeId`]) because the runtime never validates it
        /// — it is halting, not acting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused_molecule: Option<String>,
    },
}

/// Default value for the `adapter_name` field on Worker-Spawn Port
/// events.
///
/// Backwards-compat for `events.jsonl` lines written before C2 (the
/// `EventV2` variants existed in shadow form during transition).
/// Returns `"claude"` because the only adapter shipping today is the
/// claude tmux session.
fn default_adapter_name() -> String {
    "claude".to_owned()
}

impl EventV2 {
    /// Is this event a **cs verb emission** — the operator-level act of
    /// nucleating, tackling, marking done, or collapsing a molecule?
    ///
    /// Verb events carry a [`QualityBand`] in their envelope. Non-verb
    /// events (heartbeats, energy
    /// ticks, gate transitions, …) are observation artefacts of an already
    /// running worker — they are not operator decisions and do not need
    /// the band.
    ///
    /// The four verbs from the briefing map as follows:
    ///
    /// | cs verb     | event variant         |
    /// |-------------|-----------------------|
    /// | `nucleate`  | `MoleculeNucleated`   |
    /// | `tackle`    | `WorkerSpawned`       |
    /// | `done`      | `MoleculeCompleted` and `MergeDispatched` |
    /// | `collapse`  | `MoleculeCollapsed`   |
    ///
    /// `cs stuck` is included as well — it is an operator-level decision
    /// that freezes a molecule and is in the same observational class.
    #[must_use]
    pub const fn is_verb(&self) -> bool {
        matches!(
            self,
            Self::MoleculeNucleated { .. }
                | Self::WorkerSpawned { .. }
                | Self::MoleculeCompleted { .. }
                | Self::MoleculeCollapsed { .. }
                | Self::MergeDispatched { .. }
                | Self::MoleculeStuck { .. }
        )
    }

    /// The molecule this event is about, when one exists.
    ///
    /// Used by the event-log writer to assign a per-molecule sequence number
    /// (`mol_seq`) under the append flock. Returns `None` for events that
    /// describe pure worker lifecycle or runtime telemetry without a
    /// molecule reference (`WorkerKilled`, `WorkerHeartbeat`, `EnergyTick`).
    ///
    /// For [`Self::DecaySpliced`] the parent is returned — the splice is
    /// recorded against the molecule that *was* in the DAG slot, not against
    /// the children that replace it. For [`Self::WorkerSpawned`] the optional
    /// `molecule` field is honoured (workers without a molecule yield `None`).
    #[must_use]
    pub fn molecule_id(&self) -> Option<&MoleculeId> {
        match self {
            Self::MoleculeNucleated { molecule_id, .. }
            | Self::MoleculeStatusChanged { molecule_id, .. }
            | Self::MoleculeStepCompleted { molecule_id, .. }
            | Self::MoleculeCompleted { molecule_id, .. }
            | Self::MoleculeCollapsed { molecule_id, .. }
            | Self::MoleculeStuck { molecule_id, .. }
            | Self::Expired { molecule_id, .. }
            | Self::GateStarted { molecule_id, .. }
            | Self::GateCompleted { molecule_id, .. }
            | Self::GateFailed { molecule_id, .. }
            | Self::NativeStarted { molecule_id, .. }
            | Self::NativeCompleted { molecule_id, .. }
            | Self::NativeFailed { molecule_id, .. }
            | Self::RuntimeGuardOverride { molecule_id, .. }
            | Self::PromptSealed { molecule_id, .. }
            | Self::BriefingSealed { molecule_id, .. }
            | Self::BootstrapSealed { molecule_id, .. }
            | Self::SealAttested { molecule_id, .. }
            | Self::SealBypassed { molecule_id, .. }
            | Self::Resurrected { molecule_id, .. }
            | Self::Harvested { molecule_id, .. }
            | Self::WorkerExited { molecule_id, .. }
            | Self::WorkerSilenceDetected { molecule_id, .. }
            | Self::BlockingDialogueDetected { molecule_id, .. }
            | Self::WorkerBlockedOnOperator { molecule_id, .. }
            | Self::QueryStepEvaluated { molecule_id, .. }
            | Self::ExternalChannelTimeout { molecule_id, .. }
            | Self::RuntimeShelledOut { molecule_id, .. }
            | Self::RuntimeMergeDispatched { molecule_id, .. }
            | Self::RuntimeWorktreeClaimed { molecule_id, .. } => Some(molecule_id),
            Self::WorkerSpawnAttempted { mol_id, .. }
            | Self::WorkerSpawnFailed { mol_id, .. }
            | Self::WorkerSpawnRolledBack { mol_id, .. }
            | Self::AdapterLivenessProbed { mol_id, .. }
            | Self::AdapterPaneSignatureChecked { mol_id, .. }
            | Self::AdapterBriefingConsumed { mol_id, .. }
            | Self::AdapterHandleReconciled { mol_id, .. }
            | Self::AdapterSelected { mol_id, .. }
            | Self::ModelSelected { mol_id, .. }
            | Self::ModelObserved { mol_id, .. }
            | Self::ModelCeilingHit { mol_id, .. }
            | Self::RemoteEgressOptIn { mol_id, .. }
            | Self::EgressUnenforceable { mol_id, .. }
            | Self::LocalFallback { mol_id, .. }
            | Self::LocalExecReceipt { mol_id, .. }
            | Self::SF1HttpTransport { mol_id, .. }
            | Self::SF2ProviderRateLimit { mol_id, .. }
            | Self::SF3SchemaDecodeFailure { mol_id, .. }
            | Self::SF4ToolCallExecutionFailure { mol_id, .. }
            | Self::SF5ContextOverflow { mol_id, .. }
            | Self::SF6SupervisionSetupFailed { mol_id, .. }
            | Self::SF7BinaryVersionMismatch { mol_id, .. } => Some(mol_id),
            Self::DecaySpliced { parent, .. } => Some(parent),
            Self::MergeDispatched { molecule, .. } | Self::MergeCompleted { molecule, .. } => {
                Some(molecule)
            }
            Self::WorkerSpawned { molecule, .. } => molecule.as_ref(),
            Self::InvocationCompleted { molecule_id, .. }
            | Self::ChronicleAdded { molecule_id, .. } => molecule_id.as_ref(),
            Self::OperatorSpark { mol_ref, .. } => mol_ref.as_ref(),
            Self::OperatorSigned { mol_id, .. } => mol_id.as_ref(),
            Self::WorkerKilled { .. }
            | Self::WorkerHeartbeat { .. }
            | Self::EnergyTick { .. }
            | Self::FleetTyped { .. }
            | Self::OperatorPresent { .. }
            | Self::OperatorAbsent { .. }
            | Self::OperatorVerdict { .. }
            | Self::OperatorRefused { .. }
            | Self::OperatorSilent { .. }
            | Self::RuntimeReadDecideWrite { .. }
            | Self::ConfigDriftDetected { .. }
            | Self::AdrInscribed { .. } => None,
        }
    }
}

/// Coarse role of the process that wrote an [`Envelope`] (cosmon-ward §F1).
///
/// Used by attendant-style consumers to write a **causal filter** that
/// excludes their own emissions:
///
/// ```text
/// SELECT … FROM molecules
/// WHERE NOT EXISTS (
///   SELECT 1 FROM events e
///   WHERE e.molecule_id = molecules.id
///     AND e.emitter_kind = 'Attendant'
///     AND e.kind = 'attendant.acted'
///     AND e.ts > NOW() - 24h
/// )
/// ```
///
/// Without this header the natural failure mode is dilution
/// (auto-immune fatigue) — the patrol's events get re-classified as
/// candidates by its own next scan, and signal-to-noise tends to zero.
///
/// `#[non_exhaustive]` so future regimes (e.g. `Migration`,
/// `ImportTool`) can land without breaking exhaustive matches. Wire
/// format is the lowercased variant name (`cli`, `worker`, …); any
/// unrecognised string deserialises back to [`Self::Unknown`] so old
/// readers do not crash on tomorrow's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum EmitterKind {
    /// Direct invocation of the `cs` CLI on the operator's behalf
    /// (`cs nucleate`, `cs done`, `cs collapse`, …). The emitter is
    /// neither a worker nor a patrol — it is a one-shot transactional
    /// command in the operator's foreground.
    Cli,
    /// A long-running worker process executing inside a worktree
    /// (typically a Claude Code session under tmux). Worker-emitted
    /// events are the bulk of fleet activity and are what an
    /// attendant's causal filter is **not** meant to exclude.
    Worker,
    /// A patrol invocation (`cs patrol --silence-detect`,
    /// `cs patrol --propel`, `cs patrol --harvest`, …). External
    /// scheduler is the typical caller.
    Patrol,
    /// A formula step (gate, native, query) emitting on its own
    /// behalf. Distinguished from [`Self::Worker`] when the step
    /// runs in-process rather than spawning a worker.
    FormulaStep,
    /// An attendant — the consumer of the causal filter itself. This
    /// is the variant a self-consuming patrol must look for and
    /// exclude.
    Attendant,
    /// A mission-controller / mission-plan formula run.
    Mission,
    /// An oversee / witness role acting on a molecule.
    Oversee,
    /// A reduce step combining outputs from multiple branches.
    Reduce,
    /// A `cs while` loop iteration.
    While,
    /// A retrospective formula sweeping closed molecules.
    Retrospective,
    /// A pilot session (the operator's interactive cosmon shell or
    /// pilot-pulse heartbeat).
    Pilot,
    /// An operator-initiated state transition (`cs collapse`,
    /// `cs stuck`, `cs harvest`, …) where the human is the
    /// authoritative cause. Distinguished from [`Self::Cli`] for
    /// emissions that record an authoritative decision rather than a
    /// routine command.
    Operator,
    /// Legacy line, unset emitter, or future writer using a
    /// vocabulary this binary does not yet know. The default value;
    /// audit tooling should treat `Unknown` as the absence of a
    /// signal, not a positive classification.
    #[default]
    Unknown,
}

impl EmitterKind {
    /// Wire-form (`snake_case`) string — the same shape serde emits.
    ///
    /// Useful for grep audits and for callers (`cs ensemble --emitter`,
    /// future `cs errors --emitter`) that need a stable string token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Worker => "worker",
            Self::Patrol => "patrol",
            Self::FormulaStep => "formula_step",
            Self::Attendant => "attendant",
            Self::Mission => "mission",
            Self::Oversee => "oversee",
            Self::Reduce => "reduce",
            Self::While => "while",
            Self::Retrospective => "retrospective",
            Self::Pilot => "pilot",
            Self::Operator => "operator",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a `snake_case` wire form. Unknown strings collapse to
    /// [`Self::Unknown`] (matches the serde behaviour for legacy lines).
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "cli" => Self::Cli,
            "worker" => Self::Worker,
            "patrol" => Self::Patrol,
            "formula_step" => Self::FormulaStep,
            "attendant" => Self::Attendant,
            "mission" => Self::Mission,
            "oversee" => Self::Oversee,
            "reduce" => Self::Reduce,
            "while" => Self::While,
            "retrospective" => Self::Retrospective,
            "pilot" => Self::Pilot,
            "operator" => Self::Operator,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for EmitterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EmitterKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EmitterKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self::from_wire(&raw))
    }
}

/// Best-effort coercion of a legacy `events.jsonl` line into an `EventV2`
/// envelope.
///
/// Legacy lines have drifted shapes — some use `type`, some use `kind`, some
/// use neither — but a handful of fields (`timestamp`, `molecule_id`) are
/// reliable across all emitters. This function recognises the common shapes
/// and produces a best-effort envelope; unrecognised shapes return a
/// `serde_json::Error` and the caller can decide whether to skip or log.
///
/// The resulting envelope has `seq = Seq(0)` (unknown) and no causal parent —
/// the migration helper is lossy by design, since the legacy log simply does
/// not carry the sequencing information `EventV2` requires.
///
/// # Errors
///
/// Returns the underlying JSON error if the line cannot even be parsed as an
/// arbitrary JSON object.
#[allow(clippy::too_many_lines)]
pub fn migrate_legacy_line(line: &str) -> Result<Envelope, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(line)?;
    let obj = v.as_object().ok_or_else(invalid_shape)?;

    // Discriminator — legacy writers use both `type` and `kind`.
    let disc = obj
        .get("type")
        .or_else(|| obj.get("kind"))
        .and_then(|v| v.as_str())
        .ok_or_else(invalid_shape)?;

    let timestamp = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc));

    let molecule_id = |key: &str| -> Option<MoleculeId> {
        obj.get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| MoleculeId::new(s).ok())
    };

    let worker_id = |key: &str| -> Option<WorkerId> {
        obj.get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| WorkerId::new(s).ok())
    };

    let reason = |key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };

    let event = match disc {
        "molecule_nucleated" => EventV2::MoleculeNucleated {
            molecule_id: molecule_id("molecule_id").ok_or_else(invalid_shape)?,
            formula_id: obj
                .get("formula_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            parent_id: molecule_id("parent_id"),
            blocks: obj
                .get("blocks")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .filter_map(|s| MoleculeId::new(s).ok())
                        .collect()
                })
                .unwrap_or_default(),
        },
        "molecule_completed" => EventV2::MoleculeCompleted {
            molecule_id: molecule_id("molecule_id").ok_or_else(invalid_shape)?,
            duration_ms: obj.get("duration_ms").and_then(serde_json::Value::as_u64),
            reason: reason("reason"),
        },
        "molecule_collapsed" => EventV2::MoleculeCollapsed {
            molecule_id: molecule_id("molecule_id").ok_or_else(invalid_shape)?,
            reason: reason("reason"),
            kind: obj
                .get("kind")
                .and_then(|v| v.as_str())
                .map(|s| CollapseReason::from(s.to_owned())),
        },
        "molecule_stuck" => EventV2::MoleculeStuck {
            molecule_id: molecule_id("molecule_id").ok_or_else(invalid_shape)?,
            reason: StuckReason::from(reason("reason")),
        },
        "worker_spawned" => EventV2::WorkerSpawned {
            worker_id: worker_id("worker_id").ok_or_else(invalid_shape)?,
            molecule: molecule_id("molecule"),
            session_name: obj
                .get("session_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            role: obj
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            adapter_name: obj
                .get("adapter_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            loop_ownership: obj
                .get("loop_ownership")
                .and_then(|v| v.as_str())
                .map_or_else(LoopOwnershipTag::default, LoopOwnershipTag::from_raw),
        },
        "worker_killed" | "worker_terminated" => EventV2::WorkerKilled {
            worker_id: worker_id("worker_id").ok_or_else(invalid_shape)?,
            reason: reason("reason"),
        },
        _ => return Err(invalid_shape()),
    };

    Ok(Envelope {
        seq: Seq(0),
        mol_seq: None,
        timestamp,
        causal_parent: None,
        quality_band: None,
        emitter_kind: EmitterKind::Unknown,
        emitter_id: String::new(),
        meta_level: 0,
        event,
    })
}

/// Coarse classification of what a worker appears to be doing.
///
/// Workers emit this as part of [`EventV2::WorkerHeartbeat`] to help the
/// runtime distinguish "thinking" (active LLM call) from "stuck" (no
/// progress) without inspecting tmux. `Unknown` is the safe default when
/// Classification of an [`EventV2::ExternalChannelTimeout`] occurrence.
///
/// Two distinct conditions surface as the same event so consumers see one
/// signal instead of two — but the runtime distinguishes them so the
/// recovery decision can fork:
///
/// - [`Self::Checkpoint`] — no progress within the per-checkpoint window.
///   A retry from the on-disk prefix is the canonical response.
/// - [`Self::TotalBudget`] — the aggregate `max_total_minutes` ceiling
///   fired. The step is over; `cs collapse` follows. Retries are not
///   appropriate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalChannelTimeoutKind {
    /// Per-checkpoint stall — no new tokens / bytes for the configured
    /// window. The runtime may retry from the last checkpoint.
    Checkpoint,
    /// Total wall-clock budget exhausted. The step is over.
    TotalBudget,
    /// The provider explicitly aborted the stream (network drop, server
    /// error, rate-limit). Surface for visibility; retry semantics depend
    /// on `attempt < max_retries`.
    ProviderAborted,
}

/// What kind of liveness probe an Adapter performed against a worker
/// (ADR-097 / WS-2).
///
/// Each variant names a distinct *channel* the adapter watched for a
/// liveness signal. Recorded on every [`EventV2::AdapterLivenessProbed`]
/// emission so an audit query can distinguish "tmux says the pane is
/// alive" from "the API handshake succeeded" without re-parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdapterProbeKind {
    /// The adapter inspected the tmux pane's foreground command and
    /// declared the worker alive iff a registered signature matched.
    PaneSignature,
    /// The adapter observed an OS-level process exit (waitpid / tmux
    /// `pane-died` hook) and declared the worker dead.
    ProcessExit,
    /// The adapter observed a token advance on the worker's output
    /// stream within the polling window.
    TokenAdvance,
    /// The adapter completed a round-trip handshake with the worker's
    /// API endpoint (future adapters; not exercised by the claude tmux
    /// path).
    ApiHandshake,
}

/// Verdict returned by an Adapter liveness probe (ADR-097 / WS-2).
///
/// Tagged enum so consumers can distinguish "alive with such-and-such
/// evidence" from "stuck for such-and-such reason" without parsing free-
/// form prose. The string payload is informative and never branched on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum AdapterProbeResult {
    /// The probe confirmed the worker is alive.
    Alive {
        /// Free-form evidence describing how aliveness was established
        /// (e.g. `"pane fg=claude"`, `"tokens advanced 42 since last
        /// probe"`).
        evidence: String,
    },
    /// The probe confirmed the worker is stuck or dead.
    Stuck {
        /// Free-form reason describing why aliveness could not be
        /// established (e.g. `"pane gone"`, `"no token advance for
        /// 120s"`).
        reason: String,
    },
    /// The adapter observed a **recoverable transient failure and retried
    /// it in place** rather than surfacing it as fatal — the worker is
    /// neither dead nor cleanly alive, it is mid-recovery.
    ///
    /// Emitted by the in-process `OpenAI` adapter's `one_turn` retry gate
    /// (delib-20260707-df9b M1) so a mode-C recovery is disk-evaluable
    /// forever: a bench greps `reason` on `events.jsonl` instead of
    /// scraping a tmux pane. The `reason` names which transient class
    /// was retried — `"tool_parse_reinject"` (a spliced tool-call
    /// correction), `"server_error_5xx"` / `"server_error_transport"`
    /// (a paced 5xx / pre-response transport retry), or `"rate_limited"`
    /// (a `Retry-After`-paced 429).
    Retried {
        /// Free-form reason naming the transient class that was retried
        /// (e.g. `"tool_parse_reinject"`, `"server_error_5xx"`).
        reason: String,
    },
}

/// Which perturbation channel asked for a pane-signature check
/// (ADR-038, ADR-097 / WS-3).
///
/// Recorded on every [`EventV2::AdapterPaneSignatureChecked`] emission
/// so an audit can attribute a wrong-process write to either the
/// propulsion channel (0-byte pilot→worker wake-up) or the whisper
/// channel (unbounded pilot→live-worker semantic text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PerturbationChannel {
    /// 0-byte pilot→worker wake-up signal (ADR-038 propulsion).
    Propulsion,
    /// Unbounded pilot→worker semantic text (ADR-038 whisper).
    Whisper,
}

/// Final state of an Adapter-side handle after reconciliation
/// (ADR-097 / WS-5).
///
/// Recorded on every [`EventV2::AdapterHandleReconciled`] emission so an
/// audit can distinguish the three failure modes the WS-5 invariant
/// guards against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdapterHandleState {
    /// The adapter still holds the handle (e.g. the reconciliation is
    /// being recorded mid-teardown).
    Held,
    /// The adapter released the handle and the underlying process exit
    /// was observed cleanly.
    ReleasedClean,
    /// The adapter released the handle but the underlying process
    /// outlived the release (orphan) — or the exit observation raced
    /// with the release and arrived after.
    ReleasedOrphan,
}

/// Where a `cs tackle` Adapter selection came from (ADR-097 / C6).
///
/// Recorded on every [`EventV2::AdapterSelected`] emission so an audit
/// can attribute the choice without correlating against shell history
/// or config snapshots. The variants exhaust the resolution paths the
/// C6 CLI walks (highest priority first; ADR-108):
///
/// 1. [`AdapterSelectionSource::Cli`] — the operator (or a driver
///    shelling out to `cs tackle`) passed `--adapter <name>`.
/// 2. [`AdapterSelectionSource::FormulaStep`] — no flag was passed and the
///    *currently executing formula step* pins `adapter = "<name>"`
///    (per-workflow override). Ranks above
///    every default so a `deep-think` panel can demand frontier reasoning
///    regardless of the operator's blanket preference.
/// 3. [`AdapterSelectionSource::EnvVar`] — no flag and no step pin, and
///    `$COSMON_DEFAULT_ADAPTER` is set non-empty: the operator's
///    session-scoped "right now, everywhere" hammer. Ranks above both
///    config files (it is the explicit live intent) but below the
///    formula-step pin (a correctness need outranks a blanket preference).
/// 4. [`AdapterSelectionSource::Config`] — no flag, no step pin, no env, and
///    the per-galaxy `.cosmon/config.toml::[adapters.default]` resolved
///    the name.
/// 5. [`AdapterSelectionSource::GlobalConfig`] — no flag, no step pin, no
///    env, and the per-galaxy config carried no default, so the global
///    `~/.config/cosmon/config.toml::[adapters.default]` (the operator's
///    machine-wide preference) resolved the name.
/// 6. [`AdapterSelectionSource::Default`] — nothing above resolved, so the
///    built-in `"local"` floor won (the config-undeletable invariant: no
///    config = local autonomy).
/// 7. [`AdapterSelectionSource::EnvelopeRole`] — reserved for a future
///    envelope-driven dispatch path (academy-shim's role → adapter
///    resolution). Not emitted by C6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdapterSelectionSource {
    /// The selection came from a `cs tackle --adapter <flag>` invocation.
    Cli {
        /// The verbatim flag value the operator (or driver) passed.
        flag: String,
    },
    /// The selection came from the currently executing formula step's
    /// `adapter = "<name>"` pin (a per-workflow override).
    /// Ranks above [`Config`](Self::Config) because a
    /// workflow step may legitimately demand a specific adapter regardless
    /// of the galaxy default — but below [`Cli`](Self::Cli), so an explicit
    /// `--adapter` flag still wins.
    FormulaStep {
        /// The formula whose step carried the pin (e.g. `"deep-think"`).
        formula: String,
        /// The step id that pinned the adapter (e.g. `"panel"`).
        step_id: String,
    },
    /// The selection came from the `$COSMON_DEFAULT_ADAPTER` environment
    /// variable. The operator's
    /// session-scoped, machine-wide hammer: it overrides both the
    /// per-galaxy and the global config so a single `export` flips the
    /// default everywhere without touching any `.cosmon/config.toml` —
    /// but it ranks below [`FormulaStep`](Self::FormulaStep), so a step
    /// pinning `adapter = "claude"` (a correctness need) still wins.
    EnvVar {
        /// The environment variable consulted (always
        /// `"COSMON_DEFAULT_ADAPTER"` today; carried verbatim so an audit
        /// reads the origin without recompiling the resolver's knowledge).
        var: String,
    },
    /// The selection came from the per-galaxy `.cosmon/config.toml`.
    Config {
        /// Path to the config file the resolver read.
        path: String,
        /// The TOML key the resolver matched
        /// (e.g. `"adapters.default"`).
        key: String,
    },
    /// The selection came from the global `~/.config/cosmon/config.toml`.
    /// The operator's machine-wide
    /// preference, consulted only when the per-galaxy config declares no
    /// `[adapters.default]` — so a committed per-galaxy default always
    /// wins over the uncommitted machine preference.
    GlobalConfig {
        /// Path to the global config file the resolver read
        /// (`$COSMON_CONFIG_HOME/cosmon/config.toml`, else
        /// `$HOME/.config/cosmon/config.toml`).
        path: String,
    },
    /// No flag, no formula-step pin, and no config: cosmon used its
    /// built-in `"local"` floor — the config-undeletable invariant that
    /// "no config = local autonomy".
    Default {
        /// Free-form explanation of why the fallback fired (e.g.
        /// `"no [adapters] config; using built-in 'local'"`).
        fallback_reason: String,
    },
    /// Reserved for envelope-driven selection (future driver-side
    /// role resolution).
    EnvelopeRole {
        /// The role label carried on the envelope that mapped to an
        /// adapter.
        role: String,
    },
}

/// Where a `cs tackle` **model** selection came from
/// (delib-20260704-b476 C1, the model sibling of [`AdapterSelectionSource`]).
///
/// The model axis is structurally identical to the adapter axis: chosen at
/// the same spawn-time boundary, by the same-shaped six-level resolution
/// chain, highest priority first. Recorded on the future
/// `ModelSelected` event (C2) so a retrospective audit can answer
/// "which model ran for this molecule, and *why*?" without correlating
/// against shell history or config snapshots — and so the safe-default
/// ghost guards (C4) can refuse a *strong* model reachable from anything
/// but a positive per-molecule act (`Flag` / `FormulaPin`).
///
/// The variants exhaust the chain `resolve_model_selection` walks:
///
/// 1. [`ModelSelectionSource::Flag`] — `cs tackle --model <id>` (the
///    operator's in-the-moment choice; always wins).
/// 2. [`ModelSelectionSource::FormulaPin`] — no flag, and the currently
///    executing formula step pins `model = "<id>"` (per-workflow override).
/// 3. [`ModelSelectionSource::EnvVar`] — no flag, no step pin, and a model
///    env var (`$COSMON_DEFAULT_MODEL`, else the legacy `$ANTHROPIC_MODEL`)
///    is set non-empty.
/// 4. [`ModelSelectionSource::Config`] — no flag, no step pin, no env, and
///    the per-galaxy `.cosmon/config.toml::[adapters.<name>].default_model`
///    resolved the id.
/// 5. [`ModelSelectionSource::GlobalConfig`] — as above but the per-galaxy
///    config carried no `default_model`, so the global
///    `~/.config/cosmon/config.toml::[adapters.<name>].default_model` won.
/// 6. [`ModelSelectionSource::Default`] — nothing above resolved, so the
///    **floor is `None`**: cosmon pins no model and the adapter's own
///    default applies (von-neumann's minimax floor — byte-identical to
///    today's no-pin behaviour; a *strong* model is never reachable from
///    this arm).
///
/// Unlike the adapter axis (whose floor is the built-in `"local"`
/// constant), the model floor carries no id — silence resolves to
/// "let the backend decide", never to a named frontier model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelSelectionSource {
    /// The selection came from a `cs tackle --model <id>` invocation.
    Flag {
        /// The verbatim model id the operator (or driver) passed.
        flag: String,
    },
    /// The selection came from the currently executing formula step's
    /// `model = "<id>"` pin (a per-workflow override). Ranks above every
    /// default but below [`Flag`](Self::Flag). Pins do **not** propagate
    /// across nucleation (C4 Ghost D): a child resolves from its own
    /// formula, not its parent's pin.
    FormulaPin {
        /// The formula whose step carried the pin (e.g. `"deep-think"`).
        formula: String,
        /// The step id that pinned the model (e.g. `"panel"`).
        step_id: String,
    },
    /// The selection came from a model environment variable — the
    /// canonical `$COSMON_DEFAULT_MODEL`, or the legacy `$ANTHROPIC_MODEL`
    /// (the carrier the rpp-adapter already exports from `rpp.toml`'s
    /// `claude_model` pin, honoured for backward compatibility). The
    /// operator's session-scoped hammer: it outranks both config files but
    /// ranks below the formula-step pin.
    EnvVar {
        /// The environment variable consulted (`"COSMON_DEFAULT_MODEL"` or
        /// `"ANTHROPIC_MODEL"`), carried verbatim so an audit reads the
        /// origin without recompiling the resolver's knowledge.
        var: String,
    },
    /// The selection came from the per-galaxy `.cosmon/config.toml`
    /// `[adapters.<name>].default_model` (scoped to the resolved adapter —
    /// a model only has meaning inside its adapter).
    Config {
        /// Path to the config file the resolver read.
        path: String,
        /// The TOML key the resolver matched
        /// (e.g. `"adapters.claude.default_model"`).
        key: String,
    },
    /// The selection came from the global
    /// `~/.config/cosmon/config.toml::[adapters.<name>].default_model`,
    /// consulted only when the per-galaxy config declared none.
    GlobalConfig {
        /// Path to the global config file the resolver read.
        path: String,
    },
    /// No flag, no formula-step pin, no env, and no config `default_model`:
    /// cosmon pins **no** model (`None`) and the adapter's own default
    /// applies. The safe floor — silence never resolves to a strong model.
    Default {
        /// Free-form explanation of why the floor fired.
        fallback_reason: String,
    },
}

/// What `cs tackle` did when the fail-closed model-dispatch ceiling fired
/// (delib-20260704-b476 / C4). Carried on [`EventV2::ModelCeilingHit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingAction {
    /// The strong pin was dropped to the safe floor (`None`) and the worker
    /// spawned on the adapter's economical default. The burst continued; the
    /// credit burn stayed bounded.
    Downgraded,
    /// The spawn was refused entirely (`on_overflow = "abort"`) — the operator
    /// must decide before a strong dispatch over budget proceeds.
    Aborted,
}

/// the bridge cannot classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityHint {
    /// Actively processing (LLM call in flight, tool use).
    Thinking,
    /// Blocked on external I/O (network, filesystem, subprocess).
    WaitingIo,
    /// Alive but idle (awaiting input, between steps).
    Idle,
    /// Liveness confirmed but activity cannot be classified.
    Unknown,
}

/// Categorised reason a molecule was marked stuck.
///
/// Serializes as a plain string (`blocker_failed`, `timeout`, …). Any
/// unrecognised string deserializes to [`StuckReason::Other`], so legacy
/// `events.jsonl` lines carrying free-form reasons remain parseable.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StuckReason {
    /// An upstream blocker failed or was collapsed.
    BlockerFailed,
    /// Exceeded time/energy budget.
    Timeout,
    /// Merge conflict the worker could not resolve.
    MergeConflict,
    /// Missing prerequisite (dependency, credential, resource).
    MissingPrerequisite,
    /// Explicit human intervention requested.
    HumanRequested,
    /// Per-molecule [`StepBudget`] circuit breaker fired (THESIS Part XI):
    /// `cs evolve` was attempted on a molecule whose `energy_budget.remaining`
    /// was already at zero. Repair is operator-only — bump the cap, collapse,
    /// or split the work into a fresh molecule. Never silent retry.
    ///
    /// [`StepBudget`]: crate::energy::StepBudget
    EnergyExhausted,
    /// Free-form reason that didn't match any known category.
    Other(String),
}

impl StuckReason {
    /// Render as the canonical on-wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::BlockerFailed => "blocker_failed",
            Self::Timeout => "timeout",
            Self::MergeConflict => "merge_conflict",
            Self::MissingPrerequisite => "missing_prerequisite",
            Self::HumanRequested => "human_requested",
            Self::EnergyExhausted => "energy_exhausted",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl From<String> for StuckReason {
    fn from(s: String) -> Self {
        match s.as_str() {
            "blocker_failed" => Self::BlockerFailed,
            "timeout" => Self::Timeout,
            "merge_conflict" => Self::MergeConflict,
            "missing_prerequisite" => Self::MissingPrerequisite,
            "human_requested" => Self::HumanRequested,
            "energy_exhausted" => Self::EnergyExhausted,
            _ => Self::Other(s),
        }
    }
}

impl Serialize for StuckReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StuckReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(d)?))
    }
}

/// Operator-facing classification of why a molecule collapsed.
///
/// This is the *kind* dimension of `cs collapse`, distinct from
/// [`crate::molecule::CollapseCause`] — the latter is the **repair-oriented**
/// label introduced by ADR-062 to drive `cs peek`'s ghost rendering
/// (rate-limit → wait/rotate; inference-stall → re-prompt). `CollapseReason`
/// is the **failure-oriented** label the operator picks at collapse-time so
/// later aggregation (`cs errors`) can answer "what is breaking my fleet?"
/// without re-parsing free-form prose.
///
/// Wire format: serialized as a plain string (`worker_crashed`,
/// `gate_failed`, …). Any unrecognised string deserializes back as
/// [`CollapseReason::Other`], so legacy `events.jsonl` lines that carried a
/// free-form reason in the `kind` slot remain parseable. Mirrors the
/// [`StuckReason`] precedent immediately above.
///
/// `#[non_exhaustive]` so future variants (e.g. `BudgetExceeded`,
/// `ToolUnreachable`) can land without breaking exhaustive matches.
///
/// # IFBDD lens
///
/// `cs errors --kind` filters by this variant; the structured-vs-`Other`
/// ratio is the headline indicator the operator watches to decide whether
/// the collapse vocabulary covers reality.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CollapseReason {
    /// Worker process died (OOM, panic, signal, tmux session lost).
    /// Repair: restart, investigate the panic / kill signal.
    WorkerCrashed,
    /// A definition-of-done gate failed (build, test, clippy, fmt) and the
    /// worker could not recover. Repair: read the gate output, fix the
    /// regression, optionally split the work.
    GateFailed,
    /// An upstream blocker stalled or itself collapsed, leaving this
    /// molecule unable to proceed. Repair: address the blocker first or
    /// re-shape the DAG.
    BlockerStuck,
    /// Operator manually aborted via `cs collapse` with intent to abandon
    /// (not a worker-side failure).
    ManualAbort,
    /// External resource exhaustion — token quota, rate limit, disk, RAM,
    /// time budget. Repair: wait, rotate credentials, raise the budget.
    ResourceExhausted,
    /// Free-form reason that did not match any known variant. Carries the
    /// raw string so context is preserved across the typed/untyped boundary.
    Other(String),
}

impl CollapseReason {
    /// Render as the canonical on-wire string. For [`Self::Other`] this is
    /// the inner free-form payload, mirroring [`StuckReason::as_str`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::WorkerCrashed => "worker_crashed",
            Self::GateFailed => "gate_failed",
            Self::BlockerStuck => "blocker_stuck",
            Self::ManualAbort => "manual_abort",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Other(s) => s.as_str(),
        }
    }

    /// `true` when this variant is one of the five named categories — i.e.
    /// the operator (or a derivation rule) classified the collapse rather
    /// than letting it fall through as free-form. Used by the IFBDD lens
    /// to compute the structured-coverage ratio.
    #[must_use]
    pub fn is_structured(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl From<String> for CollapseReason {
    fn from(s: String) -> Self {
        match s.as_str() {
            "worker_crashed" => Self::WorkerCrashed,
            "gate_failed" => Self::GateFailed,
            "blocker_stuck" => Self::BlockerStuck,
            "manual_abort" => Self::ManualAbort,
            "resource_exhausted" => Self::ResourceExhausted,
            _ => Self::Other(s),
        }
    }
}

impl std::str::FromStr for CollapseReason {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from(s.to_owned()))
    }
}

impl Serialize for CollapseReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CollapseReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(d)?))
    }
}

/// Categorised outcome of a merge attempt.
///
/// Wire format preserves the legacy string shapes (`"ok"`, `"conflict"`,
/// `"error:<detail>"`) so old logs remain parseable and downstream tooling
/// that reads raw JSON still works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// Merge landed cleanly.
    Ok,
    /// Merge had conflicts.
    Conflict,
    /// Merge errored; detail follows the colon (`"error:<detail>"`).
    Error(String),
    /// Any other free-form result.
    Other(String),
}

impl MergeResult {
    /// Canonical on-wire string form.
    #[must_use]
    pub fn to_wire(&self) -> String {
        match self {
            Self::Ok => "ok".to_owned(),
            Self::Conflict => "conflict".to_owned(),
            Self::Error(d) => format!("error:{d}"),
            Self::Other(s) => s.clone(),
        }
    }
}

impl From<String> for MergeResult {
    fn from(s: String) -> Self {
        match s.as_str() {
            "ok" => Self::Ok,
            "conflict" => Self::Conflict,
            _ => {
                if let Some(detail) = s.strip_prefix("error:") {
                    Self::Error(detail.to_owned())
                } else {
                    Self::Other(s)
                }
            }
        }
    }
}

impl Serialize for MergeResult {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for MergeResult {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(d)?))
    }
}

fn invalid_shape() -> serde_json::Error {
    serde::de::Error::custom("legacy line shape not recognised by EventV2 migration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expiry::ExpiryPolicy;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn wid(s: &str) -> WorkerId {
        WorkerId::new(s).unwrap()
    }

    #[test]
    fn seq_is_monotone() {
        let a = Seq(0);
        let b = a.next();
        assert!(b > a);
        assert_eq!(b.next(), Seq(2));
    }

    #[test]
    fn envelope_roundtrip_through_json() {
        let env = Envelope::new(
            Seq(42),
            Some(Seq(7)),
            EventV2::MoleculeNucleated {
                molecule_id: mid("cs-20260411-abcd"),
                formula_id: "task-work".to_owned(),
                parent_id: None,
                blocks: Vec::new(),
            },
        );
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
        // Envelope flattens the event payload — `type` appears at top level.
        assert!(json.contains("\"type\":\"molecule_nucleated\""));
        assert!(json.contains("\"seq\":42"));
        assert!(json.contains("\"causal_parent\":7"));
    }

    #[test]
    fn adapter_selection_source_new_tiers_serialize_with_honest_tags() {
        // task-20260531-c99e: the env + global tiers must serialise to
        // distinct `source` tags so the `adapter_selected` event records
        // the true origin instead of mislabelling env/global as
        // config/default. The tag is the load-bearing audit key.
        let env = AdapterSelectionSource::EnvVar {
            var: "COSMON_DEFAULT_ADAPTER".to_owned(),
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"source\":\"env_var\""), "{json}");
        assert!(
            json.contains("\"var\":\"COSMON_DEFAULT_ADAPTER\""),
            "{json}"
        );
        let back: AdapterSelectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);

        let global = AdapterSelectionSource::GlobalConfig {
            path: "/home/op/.config/cosmon/config.toml".to_owned(),
        };
        let json = serde_json::to_string(&global).unwrap();
        assert!(json.contains("\"source\":\"global_config\""), "{json}");
        let back: AdapterSelectionSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, global);
    }

    #[test]
    fn model_selection_source_variants_roundtrip_with_honest_tags() {
        // delib-20260704-b476 C1: every model-selection source must serialise
        // to a distinct `source` tag so the future `ModelSelected` event (C2)
        // records the true origin — the tag is the load-bearing audit key the
        // safe-default ghost guards (C4) read to refuse a *strong* model
        // reachable from anything but a positive per-molecule act.
        let cases: Vec<(ModelSelectionSource, &str)> = vec![
            (
                ModelSelectionSource::Flag {
                    flag: "claude-fable-5".to_owned(),
                },
                "\"source\":\"flag\"",
            ),
            (
                ModelSelectionSource::FormulaPin {
                    formula: "deep-think".to_owned(),
                    step_id: "panel".to_owned(),
                },
                "\"source\":\"formula_pin\"",
            ),
            (
                ModelSelectionSource::EnvVar {
                    var: "COSMON_DEFAULT_MODEL".to_owned(),
                },
                "\"source\":\"env_var\"",
            ),
            (
                ModelSelectionSource::Config {
                    path: "/x/.cosmon/config.toml".to_owned(),
                    key: "adapters.claude.default_model".to_owned(),
                },
                "\"source\":\"config\"",
            ),
            (
                ModelSelectionSource::GlobalConfig {
                    path: "/x/.config/cosmon/config.toml".to_owned(),
                },
                "\"source\":\"global_config\"",
            ),
            (
                ModelSelectionSource::Default {
                    fallback_reason: "no pin — adapter default applies".to_owned(),
                },
                "\"source\":\"default\"",
            ),
        ];
        for (source, tag) in cases {
            let json = serde_json::to_string(&source).unwrap();
            assert!(json.contains(tag), "{json} missing {tag}");
            let back: ModelSelectionSource = serde_json::from_str(&json).unwrap();
            assert_eq!(back, source);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // exhaustive list of event variants
    fn every_variant_roundtrips() {
        let events = vec![
            EventV2::MoleculeNucleated {
                molecule_id: mid("cs-20260411-aaaa"),
                formula_id: "f".to_owned(),
                parent_id: Some(mid("cs-20260411-zzzz")),
                blocks: vec![mid("cs-20260411-bbbb")],
            },
            EventV2::MoleculeStatusChanged {
                molecule_id: mid("cs-20260411-aaaa"),
                from: "pending".to_owned(),
                to: "running".to_owned(),
            },
            EventV2::MoleculeStepCompleted {
                molecule_id: mid("cs-20260411-aaaa"),
                step: 1,
                total: 3,
                duration_ms: Some(1234),
                step_hash: None,
            },
            EventV2::MoleculeCompleted {
                molecule_id: mid("cs-20260411-aaaa"),
                duration_ms: Some(9999),
                reason: "ok".to_owned(),
            },
            EventV2::MoleculeCollapsed {
                molecule_id: mid("cs-20260411-aaaa"),
                reason: "boom".to_owned(),
                kind: None,
            },
            EventV2::MoleculeStuck {
                molecule_id: mid("cs-20260411-aaaa"),
                reason: StuckReason::BlockerFailed,
            },
            EventV2::DecaySpliced {
                parent: mid("cs-20260411-aaaa"),
                children: vec![mid("cs-20260411-bbbb"), mid("cs-20260411-cccc")],
            },
            EventV2::MergeDispatched {
                molecule: mid("cs-20260411-aaaa"),
                branch: "feat/task-x".to_owned(),
                federation_provenance: None,
            },
            EventV2::MergeCompleted {
                molecule: mid("cs-20260411-aaaa"),
                branch: "feat/task-x".to_owned(),
                result: MergeResult::Ok,
                federation_provenance: None,
            },
            EventV2::WorkerSpawned {
                worker_id: wid("quartz"),
                molecule: Some(mid("cs-20260411-aaaa")),
                session_name: "cs-task-aaaa".to_owned(),
                role: "polecat".to_owned(),
                adapter_name: "claude".to_owned(),
                loop_ownership: LoopOwnership::External.into(),
            },
            EventV2::WorkerKilled {
                worker_id: wid("quartz"),
                reason: "purge".to_owned(),
            },
            EventV2::WorkerHeartbeat {
                worker_id: wid("quartz"),
                ts: DateTime::parse_from_rfc3339("2026-04-12T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                activity_hint: ActivityHint::Thinking,
            },
            EventV2::EnergyTick {
                worker_id: wid("quartz"),
                input_tokens: 1000,
                output_tokens: 250,
                cost_usd: 0.015,
            },
            EventV2::Expired {
                molecule_id: mid("cs-20260411-aaaa"),
                policy_applied: ExpiryPolicy::Collapse,
            },
            EventV2::GateStarted {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "verify".to_owned(),
                command: "echo hello".to_owned(),
            },
            EventV2::GateCompleted {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "verify".to_owned(),
                exit_code: 0,
                duration_ms: 42,
            },
            EventV2::GateFailed {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "verify".to_owned(),
                exit_code: 1,
                stderr_tail: "not found".to_owned(),
            },
            EventV2::WorkerExited {
                molecule_id: mid("cs-20260411-aaaa"),
                exit_code: Some(0),
                reason: "pane_died".to_owned(),
            },
            EventV2::WorkerExited {
                molecule_id: mid("cs-20260411-aaaa"),
                exit_code: None,
                reason: "pane_died".to_owned(),
            },
            EventV2::WorkerSilenceDetected {
                molecule_id: mid("cs-20260411-aaaa"),
                worker_id: Some(wid("quartz")),
                age_since_last_heartbeat_s: Some(180),
                threshold_s: 90,
            },
            EventV2::WorkerSilenceDetected {
                molecule_id: mid("cs-20260411-aaaa"),
                worker_id: None,
                age_since_last_heartbeat_s: None,
                threshold_s: 90,
            },
            EventV2::WorkerBlockedOnOperator {
                molecule_id: mid("task-20260608-cc44"),
                boundary: crate::operator_block::IrreversibleBoundary::Signature,
                since: DateTime::parse_from_rfc3339("2026-06-08T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::SealAttested {
                molecule_id: mid("delib-20260503-5a74"),
                prior_b3: "0".repeat(64),
                sealed_at: DateTime::parse_from_rfc3339("2026-05-03T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                witness_id: "cs-witness-aabb".to_owned(),
                attestation_b3: "f".repeat(64),
            },
            EventV2::SealBypassed {
                molecule_id: mid("delib-20260503-5a74"),
                bypass_receipt_b3: "a".repeat(64),
                reason: "emergency dispatch — incident triage".to_owned(),
            },
            EventV2::FleetTyped {
                fleet: "twins".to_owned(),
                organization_type: "editorial-board".to_owned(),
                ts: DateTime::parse_from_rfc3339("2026-05-09T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::OperatorPresent {
                sid: "cs-aabbccdd".to_owned(),
                nucleon_id: Some("nucleon-you".to_owned()),
                orbitale_id: Some("orbitale-laptop".to_owned()),
                phase: "Biological".to_owned(),
                ts: DateTime::parse_from_rfc3339("2026-05-09T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                source: crate::presence_sensor::PresenceSource::Ioreg,
            },
            EventV2::OperatorAbsent {
                sid: "cs-aabbccdd".to_owned(),
                last_seen_ts: DateTime::parse_from_rfc3339("2026-05-09T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                reason: "timeout".to_owned(),
                source: crate::presence_sensor::PresenceSource::Internal,
            },
            EventV2::OperatorSpark {
                spark_id: "spark-aaaa".to_owned(),
                src: "cli".to_owned(),
                content_hash: "b3-deadbeef".to_owned(),
                ttl_h: Some(24),
                mol_ref: Some(mid("cs-20260411-aaaa")),
            },
            EventV2::OperatorVerdict {
                spark_id: "spark-aaaa".to_owned(),
                verdict: "yes".to_owned(),
                latency_h: 1.5,
                channel: "cli".to_owned(),
            },
            EventV2::OperatorRefused {
                spark_id: "spark-bbbb".to_owned(),
                refusal_reason: "out of scope".to_owned(),
                latency_h: 0.25,
            },
            EventV2::OperatorSilent {
                spark_id: "spark-cccc".to_owned(),
                age_h: 48.0,
                escalation_level: 2,
            },
            EventV2::OperatorSigned {
                action: "cs done".to_owned(),
                mol_id: Some(mid("cs-20260411-aaaa")),
                signature_method: "shell".to_owned(),
            },
            EventV2::ChronicleAdded {
                molecule_id: Some(mid("cs-20260519-aaaa")),
                chronicle_path: "docs/lore/CHRONICLES.md".to_owned(),
                entry_anchor: Some("federation-machinery".to_owned()),
                cites_galaxies: vec!["smithy".to_owned()],
                federation_provenance: Some(crate::federation::FederationLineage {
                    source_galaxy: "smithy".to_owned(),
                    source_commit: "deadbeef".to_owned(),
                    source_path: std::path::PathBuf::from("docs/lore/2026-05-19.md"),
                    crossed_at: DateTime::parse_from_rfc3339("2026-05-19T10:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                }),
            },
            EventV2::ChronicleAdded {
                molecule_id: None,
                chronicle_path: "docs/lore/CHRONICLES.md".to_owned(),
                entry_anchor: None,
                cites_galaxies: Vec::new(),
                federation_provenance: None,
            },
            EventV2::AdrInscribed {
                adr_number: 105,
                title: "I9' Federation Provenance".to_owned(),
                adr_path: "docs/adr/105-i9-prime-federation-provenance.md".to_owned(),
                cites_galaxies: vec!["smithy".to_owned(), "mailroom".to_owned()],
                federation_provenance: Some(crate::federation::FederationLineage {
                    source_galaxy: "smithy".to_owned(),
                    source_commit: "195ff5aa".to_owned(),
                    source_path: std::path::PathBuf::from("docs/adr/0042.md"),
                    crossed_at: DateTime::parse_from_rfc3339("2026-05-19T10:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                }),
            },
            EventV2::AdrInscribed {
                adr_number: 80,
                title: "Local-only ADR".to_owned(),
                adr_path: "docs/adr/080-local.md".to_owned(),
                cites_galaxies: Vec::new(),
                federation_provenance: None,
            },
            EventV2::ConfigDriftDetected {
                launch_seal: "blake3:aaaa0000aaaa0000".to_owned(),
                current_seal: "blake3:bbbb1111bbbb1111".to_owned(),
                refused_verb: "tackle".to_owned(),
                refused_molecule: Some("task-20260531-ceaf".to_owned()),
            },
            EventV2::RuntimeReadDecideWrite {
                path: "/srv/cosmon/cosmon/.cosmon/state/fleets/default/index.json".to_owned(),
                pre_read_mtime_ns: 1_718_000_000_000_000_000_i64,
                post_write_mtime_ns: 1_718_000_000_123_456_789_i64,
                decision: "advance task-aaaa to step 2/3".to_owned(),
            },
            EventV2::RuntimeShelledOut {
                molecule_id: mid("cs-20260411-aaaa"),
                verb: "cs evolve".to_owned(),
                step_n: Some(1),
                invocation_uuid: "0123456789abcdef0123456789abcdef".to_owned(),
            },
            EventV2::RuntimeShelledOut {
                molecule_id: mid("cs-20260411-aaaa"),
                verb: "cs done".to_owned(),
                step_n: None,
                invocation_uuid: "fedcba9876543210fedcba9876543210".to_owned(),
            },
            EventV2::RuntimeMergeDispatched {
                molecule_id: mid("cs-20260411-aaaa"),
                invocation_uuid: "fedcba9876543210fedcba9876543210".to_owned(),
            },
            EventV2::RuntimeWorktreeClaimed {
                molecule_id: mid("cs-20260411-aaaa"),
                worktree_path: "/srv/cosmon/cosmon/.worktrees/task-aaaa".to_owned(),
                invocation_uuid: "abcdef0123456789abcdef0123456789".to_owned(),
            },
            EventV2::BlockingDialogueDetected {
                molecule_id: mid("cs-20260411-aaaa"),
                worker_id: Some(wid("quartz")),
                class: "money_stake".to_owned(),
                action: "alerted".to_owned(),
                blocked_seconds: Some(300),
            },
            EventV2::NativeStarted {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "reconcile".to_owned(),
                native_fn: "cs.reconcile".to_owned(),
            },
            EventV2::NativeCompleted {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "reconcile".to_owned(),
                duration_ms: 17,
            },
            EventV2::NativeFailed {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "reconcile".to_owned(),
                error: "native fn panicked".to_owned(),
            },
            EventV2::RuntimeGuardOverride {
                caller: "cs run".to_owned(),
                molecule_id: mid("cs-20260411-aaaa"),
                sediment_count: 7,
                threshold: 5,
                sample: vec![mid("cs-20260411-bbbb"), mid("cs-20260411-cccc")],
            },
            EventV2::PromptSealed {
                molecule_id: mid("cs-20260411-aaaa"),
                hash: "a".repeat(64),
                sealed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                bytes: 512,
                canonical_version: 1,
            },
            EventV2::BriefingSealed {
                molecule_id: mid("cs-20260411-aaaa"),
                step: 2,
                hash: "b".repeat(64),
                sealed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                bytes: 1024,
                canonical_version: 0,
            },
            EventV2::BootstrapSealed {
                molecule_id: mid("cs-20260411-aaaa"),
                step: 2,
                hash: "c".repeat(64),
                sealed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                bytes: 2048,
                canonical_version: 0,
            },
            EventV2::Resurrected {
                molecule_id: mid("cs-20260411-aaaa"),
                from_session: Some("cs-task-aaaa".to_owned()),
                composed_prompt_bytes: 4096,
                t_orig_tokens: Some(12345),
                prior_count: 1,
            },
            EventV2::Harvested {
                molecule_id: mid("cs-20260411-aaaa"),
                success: true,
            },
            EventV2::QueryStepEvaluated {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "lookup".to_owned(),
                expr: "state.status".to_owned(),
                source: "state".to_owned(),
                output_var: "status".to_owned(),
                result_preview: "\"running\"".to_owned(),
            },
            EventV2::ExternalChannelTimeout {
                molecule_id: mid("cs-20260411-aaaa"),
                step_id: "panel".to_owned(),
                provider: "anthropic".to_owned(),
                kind: ExternalChannelTimeoutKind::Checkpoint,
                age_s: Some(522),
                bytes_flushed: 8192,
                attempt: 2,
            },
            EventV2::InvocationCompleted {
                tenant: "operator".to_owned(),
                molecule_id: Some(mid("cs-20260411-aaaa")),
                backend: "anthropic".to_owned(),
                latency_ms: 842,
                success: true,
            },
            EventV2::WorkerSpawnAttempted {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                worktree_path: "/x/.worktrees/task-aaaa".to_owned(),
                invocation_uuid: "0123456789abcdef0123456789abcdef".to_owned(),
                pid: 4242,
                pre_existing_worker: Some(wid("basalt")),
            },
            EventV2::AdapterLivenessProbed {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                probe_kind: AdapterProbeKind::PaneSignature,
                probe_result: AdapterProbeResult::Alive {
                    evidence: "pane fg=claude".to_owned(),
                },
                elapsed_since_last_advance_ms: 1500,
            },
            EventV2::AdapterPaneSignatureChecked {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                registered_signature: vec!["claude".to_owned(), "claude-code".to_owned()],
                observed_command: "claude".to_owned(),
                matched: true,
                channel: PerturbationChannel::Whisper,
            },
            EventV2::AdapterBriefingConsumed {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                briefing_path: "briefing.md".to_owned(),
                briefing_seal_observed: "d".repeat(64),
                briefing_seal_recorded: "d".repeat(64),
                bytes_read: 3072,
                consumed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::AdapterSelected {
                mol_id: mid("cs-20260411-aaaa"),
                adapter_name: "claude".to_owned(),
                selected_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                selection_source: AdapterSelectionSource::Cli {
                    flag: "claude".to_owned(),
                },
                role_hint: Some("researcher".to_owned()),
                loop_ownership: LoopOwnership::External.into(),
            },
            EventV2::ModelSelected {
                mol_id: mid("cs-20260411-aaaa"),
                adapter_name: "claude".to_owned(),
                model: Some("claude-opus-4-8".to_owned()),
                selection_source: ModelSelectionSource::Flag {
                    flag: "claude-opus-4-8".to_owned(),
                },
                selected_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::ModelObserved {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: Some(WorkerId::new("worker-aaaa").unwrap()),
                adapter_name: "claude".to_owned(),
                model: "claude-sonnet-5".to_owned(),
                observed_source: crate::model_realization::ModelObservationSource::ClaudeStreamJson,
                observed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::ModelCeilingHit {
                mol_id: mid("cs-20260411-aaaa"),
                adapter_name: "claude".to_owned(),
                model: "claude-opus-4-8".to_owned(),
                strong_count: 5,
                cap: 5,
                window_hours: 24,
                action: CeilingAction::Downgraded,
                hit_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::RemoteEgressOptIn {
                mol_id: mid("cs-20260411-aaaa"),
                adapter_name: "claude".to_owned(),
                endpoint_host: Some("api.anthropic.com".to_owned()),
                endpoint_port: Some(443),
                opted_in_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::EgressUnenforceable {
                mol_id: mid("cs-20260411-aaaa"),
                adapter_name: "local".to_owned(),
                reason: "deny-external cannot be kernel-enforced on this host".to_owned(),
                degraded_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::LocalFallback {
                mol_id: mid("cs-20260411-aaaa"),
                from_adapter: "local".to_owned(),
                to_adapter: "claude".to_owned(),
                cause: "timeout".to_owned(),
                fell_back_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::LocalExecReceipt {
                mol_id: mid("cs-20260411-aaaa"),
                turn: 3,
                ffi_receipt: true,
                throughput_tok_s: 42.5,
                throughput_band: "local".to_owned(),
                accelerator_load: 0.87,
                observed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::AdapterHandleReconciled {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                handle_state: AdapterHandleState::ReleasedClean,
                underlying_exit_observed_at: Some(
                    DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                handle_released_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:01Z")
                    .unwrap()
                    .with_timezone(&Utc),
                gap_ms: 1000,
            },
            EventV2::SF1HttpTransport {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "openai-direct".to_owned(),
                provider_name: "openai".to_owned(),
                model_name: Some("gpt-4o".to_owned()),
                trigger_context: "completion".to_owned(),
                recovery_attempted: false,
                retry_count: 3,
                error_class: "dns".to_owned(),
            },
            EventV2::SF2ProviderRateLimit {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "openai-direct".to_owned(),
                provider_name: "openai".to_owned(),
                model_name: Some("gpt-4o".to_owned()),
                trigger_context: "completion".to_owned(),
                recovery_attempted: true,
                retry_after_secs: 30,
                quota_kind: "rpm".to_owned(),
            },
            EventV2::SF3SchemaDecodeFailure {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "openai-direct".to_owned(),
                provider_name: "openai".to_owned(),
                model_name: None,
                trigger_context: "completion".to_owned(),
                recovery_attempted: false,
                decode_error: "missing field `choices`".to_owned(),
                response_bytes: 640,
            },
            EventV2::SF4ToolCallExecutionFailure {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "openai-direct".to_owned(),
                provider_name: "openai".to_owned(),
                model_name: Some("gpt-4o".to_owned()),
                trigger_context: "tool_use".to_owned(),
                recovery_attempted: true,
                tool_name: "bash".to_owned(),
                exit_code: 127,
                stderr_tail: "command not found".to_owned(),
            },
            EventV2::SF5ContextOverflow {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "openai-direct".to_owned(),
                provider_name: "openai".to_owned(),
                model_name: Some("gpt-4o".to_owned()),
                trigger_context: "completion".to_owned(),
                recovery_attempted: true,
                input_tokens_estimated: 200_000,
                max_context_tokens: 128_000,
                truncation_applied: false,
            },
            EventV2::SF6SupervisionSetupFailed {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                hook_name: "pane_died".to_owned(),
                error: "tmux set-hook failed".to_owned(),
            },
            EventV2::SF7BinaryVersionMismatch {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "codex".to_owned(),
                binary_name: "codex".to_owned(),
                expected_version_range: "=1.2.3".to_owned(),
                actual_version: "1.2.0".to_owned(),
            },
            EventV2::WorkerSpawnFailed {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                reason: "tmux not on PATH".to_owned(),
                failed_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            EventV2::WorkerSpawnRolledBack {
                mol_id: mid("cs-20260411-aaaa"),
                worker_id: wid("quartz"),
                adapter_name: "claude".to_owned(),
                reason: "pending".to_owned(),
                rolled_back_at: DateTime::parse_from_rfc3339("2026-04-11T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
        ];

        // Exhaustiveness guard (C10 test review, review-report.md F2).
        //
        // `events` above is a hand-maintained list, and the event log is the
        // rebuild source of truth (`cosmon-state/src/rebuild.rs`). `EventV2`
        // is `#[non_exhaustive]` with many variants; a new variant forgotten
        // from `events` would pass this test green while its serde shape went
        // untested — surfacing only as a live `events.jsonl` replay
        // corruption. Two guards close that hole:
        //
        // 1. Compile-time: `assert_event_v2_variant_acknowledged` below is an
        //    exhaustive `match` (legal on a `#[non_exhaustive]` enum only from
        //    within its defining crate). Adding variant #N+1 fails to compile
        //    until it is acknowledged there.
        // 2. Runtime (here): the count of *distinct* variants exercised by
        //    `events` must equal `EventV2::COUNT` (derived by `strum`, so it
        //    tracks the enum automatically). A representative that is
        //    acknowledged in the match but never added to `events` reddens
        //    this assert. `HashSet<Discriminant>` dedupes the deliberate
        //    duplicates (e.g. `WorkerExited` appears twice) cleanly.
        let distinct: std::collections::HashSet<_> =
            events.iter().map(std::mem::discriminant).collect();
        assert_eq!(
            distinct.len(),
            EventV2::COUNT,
            "every_variant_roundtrips exercises {} of {} EventV2 variants — \
             a variant is missing a representative in the `events` vec",
            distinct.len(),
            EventV2::COUNT,
        );

        for (i, evt) in events.into_iter().enumerate() {
            let env = Envelope::new(Seq(i as u64), None, evt.clone());
            let json = serde_json::to_string(&env).unwrap();
            let back: Envelope = serde_json::from_str(&json).unwrap();
            assert_eq!(back.event, evt, "roundtrip failed for: {json}");
        }
    }

    /// Compile-time exhaustiveness guard for [`every_variant_roundtrips`].
    ///
    /// This exhaustive `match` (no wildcard arm) is legal because a
    /// `#[non_exhaustive]` enum can still be matched exhaustively from
    /// *within its own defining crate* — exactly here. The moment a new
    /// `EventV2` variant is added the match stops compiling, forcing the
    /// author to acknowledge it, add a representative to the roundtrip
    /// `events` vec, and thereby keep the `EventV2::COUNT` runtime assert
    /// satisfied. Never called; exhaustiveness is checked at compile time
    /// regardless.
    #[allow(dead_code)]
    fn assert_event_v2_variant_acknowledged(e: &EventV2) {
        match e {
            EventV2::MoleculeNucleated { .. }
            | EventV2::MoleculeStatusChanged { .. }
            | EventV2::MoleculeStepCompleted { .. }
            | EventV2::MoleculeCompleted { .. }
            | EventV2::MoleculeCollapsed { .. }
            | EventV2::MoleculeStuck { .. }
            | EventV2::DecaySpliced { .. }
            | EventV2::MergeDispatched { .. }
            | EventV2::MergeCompleted { .. }
            | EventV2::WorkerSpawned { .. }
            | EventV2::WorkerKilled { .. }
            | EventV2::WorkerHeartbeat { .. }
            | EventV2::WorkerSilenceDetected { .. }
            | EventV2::BlockingDialogueDetected { .. }
            | EventV2::WorkerBlockedOnOperator { .. }
            | EventV2::EnergyTick { .. }
            | EventV2::Expired { .. }
            | EventV2::GateStarted { .. }
            | EventV2::GateCompleted { .. }
            | EventV2::GateFailed { .. }
            | EventV2::NativeStarted { .. }
            | EventV2::NativeCompleted { .. }
            | EventV2::NativeFailed { .. }
            | EventV2::RuntimeGuardOverride { .. }
            | EventV2::PromptSealed { .. }
            | EventV2::BriefingSealed { .. }
            | EventV2::BootstrapSealed { .. }
            | EventV2::SealAttested { .. }
            | EventV2::SealBypassed { .. }
            | EventV2::Resurrected { .. }
            | EventV2::Harvested { .. }
            | EventV2::QueryStepEvaluated { .. }
            | EventV2::ExternalChannelTimeout { .. }
            | EventV2::InvocationCompleted { .. }
            | EventV2::WorkerExited { .. }
            | EventV2::FleetTyped { .. }
            | EventV2::OperatorPresent { .. }
            | EventV2::OperatorAbsent { .. }
            | EventV2::OperatorSpark { .. }
            | EventV2::OperatorVerdict { .. }
            | EventV2::OperatorRefused { .. }
            | EventV2::OperatorSilent { .. }
            | EventV2::OperatorSigned { .. }
            | EventV2::RuntimeReadDecideWrite { .. }
            | EventV2::RuntimeShelledOut { .. }
            | EventV2::RuntimeMergeDispatched { .. }
            | EventV2::RuntimeWorktreeClaimed { .. }
            | EventV2::WorkerSpawnAttempted { .. }
            | EventV2::AdapterLivenessProbed { .. }
            | EventV2::AdapterPaneSignatureChecked { .. }
            | EventV2::AdapterBriefingConsumed { .. }
            | EventV2::AdapterSelected { .. }
            | EventV2::ModelSelected { .. }
            | EventV2::ModelObserved { .. }
            | EventV2::ModelCeilingHit { .. }
            | EventV2::RemoteEgressOptIn { .. }
            | EventV2::EgressUnenforceable { .. }
            | EventV2::LocalFallback { .. }
            | EventV2::LocalExecReceipt { .. }
            | EventV2::AdapterHandleReconciled { .. }
            | EventV2::SF1HttpTransport { .. }
            | EventV2::SF2ProviderRateLimit { .. }
            | EventV2::SF3SchemaDecodeFailure { .. }
            | EventV2::SF4ToolCallExecutionFailure { .. }
            | EventV2::SF5ContextOverflow { .. }
            | EventV2::SF6SupervisionSetupFailed { .. }
            | EventV2::SF7BinaryVersionMismatch { .. }
            | EventV2::WorkerSpawnFailed { .. }
            | EventV2::WorkerSpawnRolledBack { .. }
            | EventV2::ChronicleAdded { .. }
            | EventV2::AdrInscribed { .. }
            | EventV2::ConfigDriftDetected { .. } => {}
        }
    }

    /// Focused serde round-trip for the four ADR-095 / RR-5 forensic
    /// runtime events — the hard prerequisite that must round-trip cleanly
    /// before any autonomous dispatch/merge path can be trusted to replay.
    ///
    /// Each variant is the durable answer to one forensic question:
    /// - `RuntimeReadDecideWrite` — was a state transition TOCTOU-clean?
    /// - `RuntimeShelledOut` — did the loop double-fire a `cs` verb?
    /// - `RuntimeMergeDispatched` — did a `cs done` happen without runtime authority (ghost merge)?
    /// - `RuntimeWorktreeClaimed` — did two runtimes race to claim one worktree?
    ///
    /// The dedicated test pins the wire shape (`serialize -> deserialize ->
    /// assert_eq`) independently of the umbrella `every_variant_roundtrips`,
    /// so a regression in any RR-5 variant surfaces with a precise failure.
    #[test]
    fn rr5_forensic_events_roundtrip() {
        let rr5 = vec![
            EventV2::RuntimeReadDecideWrite {
                path: "/abs/.cosmon/state/fleets/default/molecules/task-aaaa/state.json".to_owned(),
                pre_read_mtime_ns: 0_i64,
                post_write_mtime_ns: 1_i64,
                decision: "no-op (idempotent replay)".to_owned(),
            },
            EventV2::RuntimeShelledOut {
                molecule_id: mid("task-20260626-6234"),
                verb: "cs tackle".to_owned(),
                step_n: None,
                invocation_uuid: "deadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
            },
            EventV2::RuntimeMergeDispatched {
                molecule_id: mid("task-20260626-6234"),
                invocation_uuid: "cafebabecafebabecafebabecafebabe".to_owned(),
            },
            EventV2::RuntimeWorktreeClaimed {
                molecule_id: mid("task-20260626-6234"),
                worktree_path: "/abs/.worktrees/task-20260626-6234".to_owned(),
                invocation_uuid: "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f".to_owned(),
            },
        ];

        for evt in rr5 {
            let json = serde_json::to_string(&evt).unwrap();
            let back: EventV2 = serde_json::from_str(&json).unwrap();
            assert_eq!(back, evt, "RR-5 roundtrip failed for: {json}");
        }
    }

    #[test]
    fn config_drift_detected_wire_tag_and_no_molecule() {
        // The variant is a runtime-global forensic receipt: it carries no
        // typed MoleculeId, so `molecule_id()` is `None` even though it
        // names a refused molecule as a free-form string.
        let evt = EventV2::ConfigDriftDetected {
            launch_seal: "blake3:0123456789abcdef".to_owned(),
            current_seal: "blake3:fedcba9876543210".to_owned(),
            refused_verb: "tackle".to_owned(),
            refused_molecule: Some("task-20260531-ceaf".to_owned()),
        };
        assert!(evt.molecule_id().is_none());
        let json = serde_json::to_string(&evt).unwrap();
        // snake_case discriminator under the `type` tag.
        assert!(
            json.contains("\"type\":\"config_drift_detected\""),
            "unexpected wire tag: {json}"
        );
        let back: EventV2 = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn operator_event_wire_tags_are_dotted_path_compatible() {
        // The briefing names them as `operator.present`, `operator.absent`,
        // `operator.spark`, etc. With `rename_all = "snake_case"`, the
        // wire tag is `operator_present` / `operator_absent` / etc. — the
        // dot notation lives in prose / docs / queries; the wire tag is
        // snake_case for serde uniformity. Pin the actual wire tags here
        // so a future rename is a deliberate choice.
        let evt = EventV2::OperatorSpark {
            spark_id: "s".to_owned(),
            src: "cli".to_owned(),
            content_hash: "h".to_owned(),
            ttl_h: None,
            mol_ref: None,
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains(r#""type":"operator_spark""#), "json={json}");
    }

    #[test]
    fn operator_present_carries_presence_source() {
        // No-cloning theorem: an operator.present event MUST carry a
        // PresenceSource so a downstream consumer can apply
        // `is_exogenous()` at decision time.
        use crate::presence_sensor::PresenceSource;
        let evt = EventV2::OperatorPresent {
            sid: "s".to_owned(),
            nucleon_id: None,
            orbitale_id: None,
            phase: "Biological".to_owned(),
            ts: Utc::now(),
            source: PresenceSource::Internal,
        };
        if let EventV2::OperatorPresent { source, .. } = evt {
            assert!(!source.is_exogenous(), "Internal must not be exogenous");
        } else {
            panic!("expected OperatorPresent");
        }
    }

    #[test]
    fn operator_spark_and_signed_carry_molecule_id() {
        // Joining sparks/signed events to a molecule must work via
        // EventV2::molecule_id() so the canonical writer can stamp
        // mol_seq under the flock.
        let spark = EventV2::OperatorSpark {
            spark_id: "s".to_owned(),
            src: "cli".to_owned(),
            content_hash: "h".to_owned(),
            ttl_h: None,
            mol_ref: Some(mid("cs-20260509-aaaa")),
        };
        assert_eq!(
            spark.molecule_id().map(MoleculeId::as_str),
            Some("cs-20260509-aaaa")
        );

        let signed = EventV2::OperatorSigned {
            action: "cs done".to_owned(),
            mol_id: Some(mid("cs-20260509-aaaa")),
            signature_method: "shell".to_owned(),
        };
        assert_eq!(
            signed.molecule_id().map(MoleculeId::as_str),
            Some("cs-20260509-aaaa")
        );

        // Verdict / Refused / Silent / Present / Absent do not have a
        // direct molecule reference.
        let verdict = EventV2::OperatorVerdict {
            spark_id: "s".to_owned(),
            verdict: "yes".to_owned(),
            latency_h: 1.0,
            channel: "cli".to_owned(),
        };
        assert!(verdict.molecule_id().is_none());
    }

    #[test]
    fn migrate_legacy_molecule_nucleated() {
        let line = r#"{"type":"molecule_nucleated","molecule_id":"cs-20260411-aaaa","formula_id":"task-work","timestamp":"2026-04-11T10:00:00Z"}"#;
        let env = Envelope::from_line(line).unwrap();
        match env.event {
            EventV2::MoleculeNucleated {
                molecule_id,
                formula_id,
                ..
            } => {
                assert_eq!(molecule_id.as_str(), "cs-20260411-aaaa");
                assert_eq!(formula_id, "task-work");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn migrate_legacy_worker_terminated_to_killed() {
        let line = r#"{"kind":"worker_terminated","worker_id":"quartz","reason":"timeout","timestamp":"2026-04-11T10:00:00Z"}"#;
        let env = Envelope::from_line(line).unwrap();
        assert!(matches!(env.event, EventV2::WorkerKilled { .. }));
    }

    #[test]
    fn old_molecule_nucleated_json_without_parent_or_blocks_parses() {
        // Simulates an events.jsonl line written before the additive fields
        // were added — must still deserialize cleanly (backward compat).
        let line = r#"{"seq":0,"timestamp":"2026-04-11T10:00:00Z","type":"molecule_nucleated","molecule_id":"cs-20260411-aaaa","formula_id":"task-work"}"#;
        let env: Envelope = serde_json::from_str(line).unwrap();
        match env.event {
            EventV2::MoleculeNucleated {
                parent_id, blocks, ..
            } => {
                assert!(parent_id.is_none());
                assert!(blocks.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn merge_result_wire_forms_roundtrip() {
        for (variant, wire) in [
            (MergeResult::Ok, "ok"),
            (MergeResult::Conflict, "conflict"),
            (MergeResult::Error("boom".into()), "error:boom"),
            (MergeResult::Other("custom".into()), "custom"),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{wire}\""));
            let back: MergeResult = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn stuck_reason_roundtrip_and_fallback() {
        let known = StuckReason::BlockerFailed;
        let json = serde_json::to_string(&known).unwrap();
        assert_eq!(json, "\"blocker_failed\"");
        let back: StuckReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, known);

        // Legacy free-form reason should fall through to Other.
        let legacy: StuckReason = serde_json::from_str("\"network flaked out\"").unwrap();
        assert_eq!(legacy, StuckReason::Other("network flaked out".into()));
    }

    #[test]
    fn migrate_legacy_rejects_unknown_shape() {
        let line = r#"{"foo":"bar"}"#;
        assert!(Envelope::from_line(line).is_err());
    }

    #[test]
    fn worker_silence_detected_reports_molecule_id() {
        let m = mid("cs-20260426-a7e6");
        let ev = EventV2::WorkerSilenceDetected {
            molecule_id: m.clone(),
            worker_id: Some(wid("quartz")),
            age_since_last_heartbeat_s: Some(240),
            threshold_s: 90,
        };
        assert_eq!(ev.molecule_id(), Some(&m));
    }

    #[test]
    fn worker_silence_detected_json_tag_is_snake_case() {
        let ev = EventV2::WorkerSilenceDetected {
            molecule_id: mid("cs-20260426-a7e6"),
            worker_id: None,
            age_since_last_heartbeat_s: None,
            threshold_s: 90,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"type\":\"worker_silence_detected\""),
            "expected worker_silence_detected tag in {json}"
        );
        // Optional fields elided when None.
        assert!(!json.contains("\"worker_id\""));
        assert!(!json.contains("\"age_since_last_heartbeat_s\""));
    }

    #[test]
    fn worker_exited_reports_molecule_id() {
        let m = mid("cs-20260419-8f88");
        let ev = EventV2::WorkerExited {
            molecule_id: m.clone(),
            exit_code: Some(139),
            reason: "pane_died".to_owned(),
        };
        assert_eq!(ev.molecule_id(), Some(&m));
    }

    #[test]
    fn worker_exited_json_tag_is_snake_case() {
        let ev = EventV2::WorkerExited {
            molecule_id: mid("cs-20260419-8f88"),
            exit_code: Some(0),
            reason: "pane_died".to_owned(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"type\":\"worker_exited\""),
            "expected worker_exited tag in {json}"
        );
        assert!(json.contains("\"reason\":\"pane_died\""));
    }

    #[test]
    fn seal_attested_routes_and_tags() {
        let m = mid("delib-20260503-5a74");
        let ev = EventV2::SealAttested {
            molecule_id: m.clone(),
            prior_b3: "0".repeat(64),
            sealed_at: Utc::now(),
            witness_id: "cs-witness-aabb".to_owned(),
            attestation_b3: "f".repeat(64),
        };
        assert_eq!(ev.molecule_id(), Some(&m));
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"type\":\"seal_attested\""),
            "expected seal_attested tag in {json}"
        );
        assert!(json.contains("\"witness_id\":\"cs-witness-aabb\""));
    }

    #[test]
    fn seal_bypassed_routes_and_tags() {
        let m = mid("delib-20260503-5a74");
        let ev = EventV2::SealBypassed {
            molecule_id: m.clone(),
            bypass_receipt_b3: "a".repeat(64),
            reason: "emergency dispatch".to_owned(),
        };
        assert_eq!(ev.molecule_id(), Some(&m));
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"type\":\"seal_bypassed\""),
            "expected seal_bypassed tag in {json}"
        );
        assert!(json.contains("\"reason\":\"emergency dispatch\""));
    }

    // Emitter header tests (cosmon-ward §F1, task-20260509-7210).

    #[test]
    fn emitter_kind_default_is_unknown() {
        assert_eq!(EmitterKind::default(), EmitterKind::Unknown);
    }

    #[test]
    fn emitter_kind_wire_form_roundtrips_for_every_variant() {
        let cases = [
            (EmitterKind::Cli, "cli"),
            (EmitterKind::Worker, "worker"),
            (EmitterKind::Patrol, "patrol"),
            (EmitterKind::FormulaStep, "formula_step"),
            (EmitterKind::Attendant, "attendant"),
            (EmitterKind::Mission, "mission"),
            (EmitterKind::Oversee, "oversee"),
            (EmitterKind::Reduce, "reduce"),
            (EmitterKind::While, "while"),
            (EmitterKind::Retrospective, "retrospective"),
            (EmitterKind::Pilot, "pilot"),
            (EmitterKind::Operator, "operator"),
            (EmitterKind::Unknown, "unknown"),
        ];
        for (variant, wire) in cases {
            assert_eq!(variant.as_str(), wire);
            assert_eq!(EmitterKind::from_wire(wire), variant);
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{wire}\""));
            let back: EmitterKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn emitter_kind_unknown_string_falls_back_to_unknown() {
        // Forward-compatibility: a future writer using a vocabulary this
        // binary does not yet know must not crash the reader.
        let json = "\"some_future_kind\"";
        let parsed: EmitterKind = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, EmitterKind::Unknown);
    }

    #[test]
    fn envelope_with_emitter_serialises_all_three_fields() {
        let env = Envelope::with_emitter(
            Seq(7),
            None,
            None,
            None,
            EmitterKind::Attendant,
            "attendant:silence-watch".to_owned(),
            1,
            EventV2::MoleculeStatusChanged {
                molecule_id: mid("cs-20260509-aaaa"),
                from: "running".to_owned(),
                to: "stuck".to_owned(),
            },
        );
        let json = serde_json::to_string(&env).unwrap();
        assert!(
            json.contains("\"emitter_kind\":\"attendant\""),
            "expected emitter_kind in {json}"
        );
        assert!(
            json.contains("\"emitter_id\":\"attendant:silence-watch\""),
            "expected emitter_id in {json}"
        );
        assert!(
            json.contains("\"meta_level\":1"),
            "expected meta_level in {json}"
        );
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn legacy_envelope_without_emitter_fields_defaults_on_read() {
        // Pre-F1 line — no emitter_kind / emitter_id / meta_level on the
        // wire. Must deserialise cleanly with the documented defaults.
        let line = r#"{"seq":3,"timestamp":"2026-04-11T10:00:00Z","type":"molecule_completed","molecule_id":"cs-20260411-aaaa","duration_ms":1000,"reason":"ok"}"#;
        let env: Envelope = serde_json::from_str(line).unwrap();
        assert_eq!(env.emitter_kind, EmitterKind::Unknown);
        assert_eq!(env.emitter_id, "");
        assert_eq!(env.meta_level, 0);
    }

    #[test]
    fn legacy_line_via_migrate_carries_default_emitter_header() {
        let line = r#"{"type":"molecule_nucleated","molecule_id":"cs-20260411-aaaa","formula_id":"task-work","timestamp":"2026-04-11T10:00:00Z"}"#;
        let env = Envelope::from_line(line).unwrap();
        assert_eq!(env.emitter_kind, EmitterKind::Unknown);
        assert_eq!(env.emitter_id, "");
        assert_eq!(env.meta_level, 0);
    }

    #[test]
    fn envelope_default_constructor_carries_unknown_emitter() {
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::WorkerHeartbeat {
                worker_id: wid("quartz"),
                ts: Utc::now(),
                activity_hint: ActivityHint::Idle,
            },
        );
        assert_eq!(env.emitter_kind, EmitterKind::Unknown);
        assert_eq!(env.emitter_id, "");
        assert_eq!(env.meta_level, 0);
    }

    #[test]
    fn causal_filter_query_pattern_can_be_written_against_emitter_kind() {
        // Documents the cosmon-ward §F1 motivation: an attendant must be
        // able to write a causal filter that excludes its own emissions.
        // We fake "the attendant scans recent events" by collecting them.
        let attendant_envelope = Envelope::with_emitter(
            Seq(0),
            None,
            None,
            None,
            EmitterKind::Attendant,
            "attendant:silence-watch".to_owned(),
            1,
            EventV2::MoleculeStatusChanged {
                molecule_id: mid("cs-20260509-aaaa"),
                from: "running".to_owned(),
                to: "stuck".to_owned(),
            },
        );
        let worker_envelope = Envelope::with_emitter(
            Seq(1),
            None,
            None,
            None,
            EmitterKind::Worker,
            "worker:wkr-abc".to_owned(),
            0,
            EventV2::MoleculeStepCompleted {
                molecule_id: mid("cs-20260509-aaaa"),
                step: 1,
                total: 2,
                duration_ms: Some(500),
                step_hash: None,
            },
        );

        let log = [attendant_envelope, worker_envelope];
        // The attendant's own causal filter: skip events I emitted.
        let candidates: Vec<_> = log
            .iter()
            .filter(|e| e.emitter_kind != EmitterKind::Attendant)
            .collect();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].emitter_kind, EmitterKind::Worker);
    }
}
