// SPDX-License-Identifier: AGPL-3.0-only

//! Per-adapter capture of the **realized** model — the concrete id an adapter
//! actually ran, read from the fiable side-channel each adapter exposes.
//!
//! # Realization vs intention
//!
//! [`crate::event_v2::EventV2::ModelSelected`] records the *intention*: the pin
//! resolved through the six-rung ladder, minted ex-ante at spawn (before the
//! adapter runs). This module reads the *realization*: the id the adapter's own
//! output names once it is running. The two are epistemically different acts —
//! intention is a bit cosmon *chose*, realization is a bit only the adapter can
//! *report* — and they legitimately differ (unpinned dispatch that still runs a
//! concrete model; a pin the adapter substitutes for a dated/fallback id; a
//! mid-session quota downgrade Opus→Sonnet). See `docs/design/realized-model/`.
//!
//! # Zero-I/O
//!
//! These functions take **already-read bytes** (`&str`) and return parsed model
//! ids — no filesystem, no process, no network. Reading the side-channel from
//! disk (a claude session `*.jsonl`, a codex session file, a provider HTTP
//! body) is the caller's job, in the shell. This keeps the capture honest and
//! testable: the parse is a pure function of the bytes an adapter produced.
//!
//! # Typed, versioned, event-discriminated parsing (F-04)
//!
//! Each parser is driven by a **typed** deserialization keyed on the record's
//! own event/`type` discriminator, not a loose scan for the first `model`
//! field anywhere. That closes the two failure modes the pre-mortem flagged: a
//! codex *configuration* line being mistaken for a *realization*, and a bare
//! empty/whitespace string being logged as an `Observed` id. Every id returned
//! passes through the non-empty [`ModelId`] newtype, so `""` and `"  "` can
//! never reach the event log as a fabricated concrete model.
//!
//! ## Framing strategy
//!
//! All three adapter logs are **newline-delimited JSON** (one record per line):
//! Claude Code stream-json / session `*.jsonl`, codex `rollout-*.jsonl`, and
//! the provider response body is a single JSON object. The parsers therefore
//! split on lines and decode each line independently; a genuinely multi-line
//! (pretty-printed) record is *not* a shape any of these producers emits, and
//! supporting it is explicitly out of scope — declaring the framing is the
//! honest alternative to silently accepting a shape that never occurs.
//!
//! # Fiabilité per adapter (delib-20260718-c70e / feynman)
//!
//! - **claude** — authoritative. The `system`/`init` bootstrap line carries the
//!   session `model`, and each `assistant` turn carries `message.model`.
//!   Per-turn, so a quota fallback shows a *different* id on a later line: the
//!   parser returns the whole trajectory, consecutive duplicates collapsed.
//! - **codex** — best-effort but real, and its record shape is **not** what
//!   this module claimed until 2026-09-11. Measured on a live codex 0.153
//!   session log (2026-09-10): `turn_context` is emitted **once per session**,
//!   at start, carrying `payload.model` and `payload.effort`. It is *not*
//!   "re-emitted whenever the turn context changes" — a mid-session `/model`
//!   or `/effort` switch is recorded as
//!   `{"type":"event_msg","payload":{"type":"thread_settings_applied",
//!   "thread_settings":{"model":…,"reasoning_effort":…}}}` and nothing else.
//!   A parser reading only `turn_context` therefore freezes the realized axis
//!   on the initial pin and reports a switch that happened as a switch that did
//!   not — an ex-post half that silently agrees with the ex-ante one is worth
//!   less than no ex-post half at all. Both records are now read, in log order,
//!   on both axes. Legacy `session_meta` / top-level shapes remain accepted as
//!   a fallback for older codex versions.
//! - **openai / anthropic / mistral** — authoritative. The provider HTTP
//!   response body echoes a top-level `"model"` field cosmon already receives.

use serde::{Deserialize, Serialize};

/// A **non-empty** concrete model id (F-04): a realized model id is only ever
/// constructed from a value that has real content, so `""` or a whitespace-only
/// string can never be logged as an `Observed` realization. This is the
/// structural guard the audit asked for — "a bare `String` is not proof a
/// concrete identifier exists".
///
/// The stored value is trimmed of surrounding whitespace; construction fails
/// (`None`) when nothing remains.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(String);

impl ModelId {
    /// Construct a `ModelId`, trimming surrounding whitespace and rejecting an
    /// empty / whitespace-only value.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_owned()))
        }
    }

    /// Borrow the id as a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the newtype, yielding the owned id string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for ModelId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

/// Where a realized-model observation was read from — per-adapter provenance
/// carried on [`crate::event_v2::EventV2::ModelObserved`] for forensics.
///
/// This is **not** surfaced at the display: `realized` is an *outcome*, not a
/// *choice*, so it carries no source tag in the compact cell (unlike the
/// intention axis, whose `[cli]`/`[config]` tag names where the pin came from).
/// The provenance lives on the event only, so an audit can answer "how did we
/// learn what ran?" without polluting the operator-facing glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelObservationSource {
    /// Parsed from the Claude Code stream-json / session `*.jsonl`
    /// (`system`/`init` model or `message.model` per assistant turn).
    /// Authoritative.
    ClaudeStreamJson,
    /// Parsed from the codex session log — the `turn_context` record's
    /// `payload.model` at session start and any later
    /// `thread_settings_applied` event (with legacy `session_meta` /
    /// top-level fallbacks). Best-effort but real: follows mid-session
    /// settings changes.
    CodexSessionMeta,
    /// Echoed in the provider HTTP response body's top-level `"model"` field
    /// (openai / anthropic / mistral in-process adapters). Authoritative.
    ProviderResponse,
}

impl ModelObservationSource {
    /// A compact, stable tag for logs and forensic tables (never the display).
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::ClaudeStreamJson => "claude_stream_json",
            Self::CodexSessionMeta => "codex_session_meta",
            Self::ProviderResponse => "provider_response",
        }
    }
}

/// A **non-empty** reasoning-effort level as a harness reported it
/// (`"high"`, `"xhigh"`, whatever token that harness uses).
///
/// Structurally identical to [`ModelId`] and for the identical reason: a
/// realized effort is only ever constructed from a value that has real content,
/// so `""` or a whitespace-only string can never be logged as an observation.
/// The two are separate types rather than one shared newtype because they are
/// separate axes — a function that takes an effort must not accept a model id
/// by accident.
///
/// The token is carried **verbatim** (trimmed of surrounding whitespace only).
/// cosmon has no effort vocabulary of its own — see
/// [`crate::harness_settings`] and ADR-177 Decision 6 on why the portable
/// alias is deferred rather than minted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffortLevel(String);

impl EffortLevel {
    /// Construct an `EffortLevel`, trimming surrounding whitespace and
    /// rejecting an empty / whitespace-only value.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_owned()))
        }
    }

    /// Borrow the level as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Collapse consecutive duplicate ids so the returned slice is the *trajectory*
/// of distinct models that ran, in order. `[opus, opus, sonnet, sonnet]` →
/// `[opus, sonnet]`; a stable single-model session → `[opus]`.
///
/// Non-consecutive repeats are preserved (`[opus, sonnet, opus]` stays as-is):
/// the model genuinely changed back, and that is a real trajectory, not noise.
fn collapse_consecutive<T: PartialEq>(ids: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for id in ids {
        if out.last() != Some(&id) {
            out.push(id);
        }
    }
    out
}

// ---- Claude ---------------------------------------------------------------

/// One line of a Claude Code stream-json / session `*.jsonl`, decoded by its
/// `type` discriminator. Unknown record types fall through to [`Self::Other`]
/// so the parser survives Claude Code schema evolution without error.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClaudeLine {
    /// The bootstrap line (`type == "system"`, usually `subtype == "init"`)
    /// carrying the session `model` at top level.
    System(ClaudeSystemLine),
    /// An assistant turn (`type == "assistant"`) whose `message.model` names
    /// the model that produced the turn.
    Assistant(ClaudeAssistantLine),
    /// Any other record type — ignored for realized-model purposes.
    #[serde(other)]
    Other,
}

/// The `system` line's realized-model-bearing fields. Only the `init`
/// bootstrap subtype names the session model; other `system` subtypes
/// (compact-boundary, hook output, …) may carry unrelated `model`-shaped
/// fields, so the parser discriminates on `subtype` (round-3 bonus / F-04).
#[derive(Debug, Deserialize)]
struct ClaudeSystemLine {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// An `assistant` turn's realized-model-bearing fields.
#[derive(Debug, Deserialize)]
struct ClaudeAssistantLine {
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

/// The `message` object nested in an assistant turn.
#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    model: Option<String>,
}

/// Parse the realized-model **trajectory** from a Claude Code stream-json /
/// session `*.jsonl` slice.
///
/// Consults the typed `system`/`init` bootstrap line and every `assistant`
/// turn (`message.model`), in order, collapsing consecutive duplicates — so the
/// result is the ordered trajectory of distinct models: one element for a
/// stable session, two or more when a quota fallback swapped the model mid-run.
/// Lines that are not valid JSON, or whose type carries no model, are skipped.
///
/// Returns an empty vec when no line named a concrete model (a *silent* session
/// — never fabricate an id from the pin). Every element is a non-empty
/// [`ModelId`].
#[must_use]
pub fn realized_models_from_claude_jsonl(content: &str) -> Vec<ModelId> {
    let ids = content.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let raw = match serde_json::from_str::<ClaudeLine>(line).ok()? {
            // Only the `init` bootstrap subtype names the session model; any
            // other `system` subtype is not a realization record.
            ClaudeLine::System(s) if s.subtype.as_deref() == Some("init") => s.model,
            ClaudeLine::Assistant(a) => a.message.and_then(|m| m.model),
            _ => None,
        };
        ModelId::new(raw.as_deref()?)
    });
    collapse_consecutive(ids)
}

// ---- Codex ----------------------------------------------------------------

/// One line of a codex `rollout-*.jsonl`, decoded by its `type` discriminator.
///
/// A live codex session carries the realized model on the `turn_context`
/// record (`payload.model`), emitted **once at session start**, and any later
/// change on a `thread_settings_applied` `event_msg`. Older codex versions used
/// a top-level `model` or a `session_meta` object; both are accepted as a
/// fallback. Unknown record types fall through to [`Self::Other`].
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexLine {
    /// The session-opening context record (`payload.model`, `payload.effort`).
    TurnContext(CodexPayloadHolder),
    /// The legacy session-meta record (`payload.model` or nested `model`).
    SessionMeta(CodexPayloadHolder),
    /// A codex UI event. The only subtype this parser reads is
    /// `thread_settings_applied` — see [`CodexEventPayload`].
    EventMsg(CodexEventMsg),
    /// Any other record type — ignored.
    #[serde(other)]
    Other,
}

/// A codex `event_msg` record, whose own `payload` is a second tagged union.
#[derive(Debug, Deserialize)]
struct CodexEventMsg {
    #[serde(default)]
    payload: Option<CodexEventPayload>,
}

/// The subtypes of a codex `event_msg` payload this parser understands.
///
/// Only `thread_settings_applied` carries a realization; every other subtype
/// (`task_started`, `agent_message`, …) falls through to [`Self::Other`], so
/// the parser survives codex schema growth without error and without
/// mistaking a UI event for an observation.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexEventPayload {
    /// Emitted when the thread's settings change mid-session — the record a
    /// `/model` or `/effort` switch produces.
    ThreadSettingsApplied {
        #[serde(default)]
        thread_settings: Option<CodexThreadSettings>,
    },
    /// Any other event subtype — ignored.
    #[serde(other)]
    Other,
}

/// The `thread_settings` object of a `thread_settings_applied` event.
///
/// Note the field name: codex spells the effort `reasoning_effort` here and
/// `effort` on `turn_context`. Two names for one axis in one log is exactly the
/// kind of fact a parser must carry rather than a reader remember.
#[derive(Debug, Deserialize)]
struct CodexThreadSettings {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

impl CodexEventMsg {
    /// The `thread_settings` of a `thread_settings_applied` event, or `None`
    /// for every other event subtype.
    fn thread_settings(&self) -> Option<&CodexThreadSettings> {
        match self.payload.as_ref()? {
            CodexEventPayload::ThreadSettingsApplied { thread_settings } => {
                thread_settings.as_ref()
            }
            CodexEventPayload::Other => None,
        }
    }
}

/// A codex record wrapping a `payload` object that may name the model.
#[derive(Debug, Deserialize)]
struct CodexPayloadHolder {
    #[serde(default)]
    payload: Option<CodexPayload>,
    /// Legacy top-level `model` on the record itself.
    #[serde(default)]
    model: Option<String>,
}

/// The `payload` object of a codex record.
#[derive(Debug, Deserialize)]
struct CodexPayload {
    #[serde(default)]
    model: Option<String>,
    /// The reasoning effort the turn ran at, as codex's `turn_context` record
    /// reports it (ADR-177 Decision 5, the ex-post half). Read from the same
    /// record the model already comes from — no second log, no second parse.
    #[serde(default)]
    effort: Option<String>,
}

impl CodexPayloadHolder {
    /// The model named by this record, preferring `payload.model` over a legacy
    /// top-level `model`.
    fn model(&self) -> Option<&str> {
        self.payload
            .as_ref()
            .and_then(|p| p.model.as_deref())
            .or(self.model.as_deref())
    }

    /// The reasoning effort named by this record's payload, if any. There is
    /// no legacy top-level fallback: no codex version ever wrote one, and
    /// inventing a shape to be tolerant of is how a parser starts reporting
    /// realizations nobody observed.
    fn effort(&self) -> Option<&str> {
        self.payload.as_ref().and_then(|p| p.effort.as_deref())
    }
}

/// Parse the realized-model **trajectory** from a codex session `*.jsonl`
/// slice, following per-turn context changes.
///
/// Reads the model from each `turn_context` record (`payload.model`) **and**
/// from each `thread_settings_applied` event (`thread_settings.model`) in log
/// order, falling back to a legacy `session_meta` / top-level `model` for older
/// codex logs, and collapses consecutive duplicates. A mid-session `/model`
/// switch — which codex records *only* on the `thread_settings_applied` event —
/// therefore surfaces as a two-element trajectory, exactly like claude.
///
/// Returns an empty vec when no record named a concrete model (the honest floor
/// — the pin then surfaces as *intended, not confirmed*). Every element is a
/// non-empty [`ModelId`].
#[must_use]
pub fn realized_models_from_codex_session(content: &str) -> Vec<ModelId> {
    let ids = content.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let raw = match serde_json::from_str::<CodexLine>(line).ok()? {
            CodexLine::TurnContext(h) | CodexLine::SessionMeta(h) => h.model().map(str::to_owned),
            CodexLine::EventMsg(e) => e
                .thread_settings()
                .and_then(|t| t.model.as_deref())
                .map(str::to_owned),
            CodexLine::Other => None,
        };
        ModelId::new(raw.as_deref()?)
    });
    collapse_consecutive(ids)
}

/// Parse the realized **reasoning-effort** trajectory from a codex session
/// `*.jsonl` slice — the ex-post half of ADR-177 Decision 5, applied to the
/// axis the `reasoning_effort_is_never_inferred` discipline was named after.
///
/// Reads `payload.effort` from each `turn_context` record and
/// `thread_settings.reasoning_effort` from each `thread_settings_applied`
/// event, in log order, collapsing consecutive duplicates — the same
/// trajectory shape [`realized_models_from_codex_session`] returns for the
/// model axis.
///
/// Returns an empty vec when no record named a concrete effort. That is the
/// honest floor: the pin then surfaces as *dispatched, not confirmed*, and the
/// realized axis is **never** back-filled from the pin or the config. Every
/// element is a non-empty [`EffortLevel`].
#[must_use]
pub fn realized_efforts_from_codex_session(content: &str) -> Vec<EffortLevel> {
    let levels = content.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let raw = match serde_json::from_str::<CodexLine>(line).ok()? {
            CodexLine::TurnContext(h) | CodexLine::SessionMeta(h) => h.effort().map(str::to_owned),
            CodexLine::EventMsg(e) => e
                .thread_settings()
                .and_then(|t| t.reasoning_effort.as_deref())
                .map(str::to_owned),
            CodexLine::Other => None,
        };
        EffortLevel::new(raw.as_deref()?)
    });
    collapse_consecutive(levels)
}

// ---- Pure evidence assessment ---------------------------------------------

/// Provider grammar used by an evidence accumulator. Response records and
/// settings records have different meanings and therefore different counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEvidenceGrammar {
    /// Assistant records normally name the model that produced each response.
    Claude,
    /// Settings records can be sparse relative to ordinary response records.
    Codex,
}

/// A bounded explanation of evidence that could not be assessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEvidenceReason {
    /// A recognized assistant response carried no usable model field.
    MissingAssistantModel,
    /// A known model-bearing field carried an empty identifier.
    InvalidModel,
    /// The known non-model placeholder appeared in a model-bearing field.
    PlaceholderModel,
    /// A complete record could not be decoded as the admitted grammar.
    MalformedRecord,
    /// A record discriminator or subtype was outside the admitted grammar.
    UnclassifiedRecord,
    /// Final capture ended with bytes lacking a newline delimiter.
    UnassessedTail,
    /// The shell could not read some source bytes.
    ReadFailure,
    /// The shell discarded a record exceeding its admitted size.
    OversizeRecord,
    /// The shell detected replacement or loss of the assessed input prefix.
    ContinuityLost,
}

/// Explicit shell observation that prevents a complete-coverage claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEvidenceInputLoss {
    /// Source read failed before all relevant bytes could be assessed.
    ReadFailure,
    /// A complete source record was skipped due to a size limit.
    OversizeRecord,
    /// An already-assessed source prefix could not be reconciled.
    ContinuityLost,
}

/// Coverage of the assessed input, separate from the historical trajectory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEvidenceCoverage {
    /// No assessment receipt has been made for this attempt.
    NotAssessed,
    /// No response evidence was assessed; a bootstrap alone does not change it.
    NoResponseEvidence,
    /// Every recognized assistant response had a usable model.
    CompleteRecords,
    /// Settings evidence exists, but per-response confirmation is unavailable.
    SparseSettings,
    /// Some complete input or final tail could not support a coverage claim.
    Degraded(Vec<ModelEvidenceReason>),
}

/// What the latest relevant record actually says about a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatestModelEvidence {
    /// No response or settings report relevant to model identity was seen.
    NoResponse,
    /// A Claude assistant response reported this model.
    ModelReported(ModelId),
    /// A Claude assistant response had no usable model.
    ModelMissing,
    /// A codex settings record reported this model; responses remain unconfirmed.
    SettingsReported(ModelId),
    /// An ordinary response followed the latest settings report, or input was lost.
    Indeterminate,
}

/// Counters whose units follow the selected provider's grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "grammar", rename_all = "snake_case")]
pub enum ModelEvidenceStats {
    /// Claude counts assistant records before trajectory deduplication.
    Claude {
        /// Recognized assistant records, including ones with missing models.
        assistant_records: u64,
        /// Assistant records with usable, non-placeholder model identifiers.
        usable_model_records: u64,
        /// Assistant records carrying the known placeholder.
        placeholder_records: u64,
        /// Complete records that failed decoding or grammar validation.
        malformed_records: u64,
        /// Well-formed records with an unrecognized discriminator or subtype.
        unclassified_records: u64,
    },
    /// Codex counts settings separately from ordinary responses.
    Codex {
        /// Recognized ordinary response items, which need not name a model.
        response_records: u64,
        /// Recognized settings records, including effort-only changes.
        settings_records: u64,
        /// Settings records with a usable model identifier.
        usable_model_records: u64,
        /// Settings records carrying the known placeholder.
        placeholder_records: u64,
        /// Complete records that failed decoding or grammar validation.
        malformed_records: u64,
        /// Well-formed records with an unrecognized discriminator or subtype.
        unclassified_records: u64,
    },
}

/// Pure result at an explicit complete-byte boundary of the supplied input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEvidenceAssessment {
    /// Historical distinct-model trajectory found in the assessed records.
    pub trajectory: Vec<ModelId>,
    /// Provider-specific coverage of the assessed records.
    pub coverage: ModelEvidenceCoverage,
    /// Latest relevant evidence, independent of cumulative coverage.
    pub latest: LatestModelEvidence,
    /// Provider-tagged record counts, never a model-change-event count.
    pub stats: ModelEvidenceStats,
    /// Number of newline-terminated input bytes assessed.
    pub complete_bytes: u64,
    /// Last complete-byte position at which a usable model was reported.
    pub last_usable_model_at: Option<u64>,
    /// Bytes waiting for a newline, or unassessed at final capture.
    pub trailing_bytes: u64,
}

/// Incremental, I/O-free assessment of newline-delimited model evidence.
/// Chunk boundaries do not change a result; only complete records are parsed.
#[derive(Debug, Clone)]
pub struct ModelEvidenceAccumulator {
    grammar: ModelEvidenceGrammar,
    pending: Vec<u8>,
    complete_bytes: u64,
    last_usable_model_at: Option<u64>,
    trajectory: Vec<ModelId>,
    latest: LatestModelEvidence,
    reasons: Vec<ModelEvidenceReason>,
    response_records: u64,
    settings_records: u64,
    usable_model_records: u64,
    placeholder_records: u64,
    malformed_records: u64,
    unclassified_records: u64,
}

impl ModelEvidenceAccumulator {
    /// Begin a new assessment for one provider grammar and input generation.
    #[must_use]
    pub fn new(grammar: ModelEvidenceGrammar) -> Self {
        Self {
            grammar,
            pending: Vec::new(),
            complete_bytes: 0,
            last_usable_model_at: None,
            trajectory: Vec::new(),
            latest: LatestModelEvidence::NoResponse,
            reasons: Vec::new(),
            response_records: 0,
            settings_records: 0,
            usable_model_records: 0,
            placeholder_records: 0,
            malformed_records: 0,
            unclassified_records: 0,
        }
    }

    /// Absorb arbitrary byte chunks; retain an incomplete final record.
    pub fn push(&mut self, bytes: &[u8]) {
        let mut start = 0;
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                self.pending.extend_from_slice(&bytes[start..index]);
                let line = std::mem::take(&mut self.pending);
                self.complete_bytes += line.len() as u64 + 1;
                self.accept_line(&line);
                start = index + 1;
            }
        }
        self.pending.extend_from_slice(&bytes[start..]);
    }

    /// Record source loss observed by the caller without reading source here.
    pub fn note_input_loss(&mut self, loss: ModelEvidenceInputLoss) {
        let reason = match loss {
            ModelEvidenceInputLoss::ReadFailure => ModelEvidenceReason::ReadFailure,
            ModelEvidenceInputLoss::OversizeRecord => ModelEvidenceReason::OversizeRecord,
            ModelEvidenceInputLoss::ContinuityLost => ModelEvidenceReason::ContinuityLost,
        };
        self.reason(reason);
        self.latest = LatestModelEvidence::Indeterminate;
    }

    /// Snapshot only complete records; a torn live tail remains pending.
    #[must_use]
    pub fn assessment(&self) -> ModelEvidenceAssessment {
        let coverage = if self.reasons.is_empty() {
            match self.grammar {
                ModelEvidenceGrammar::Claude if self.response_records > 0 => {
                    ModelEvidenceCoverage::CompleteRecords
                }
                ModelEvidenceGrammar::Codex if self.usable_model_records > 0 => {
                    ModelEvidenceCoverage::SparseSettings
                }
                _ => ModelEvidenceCoverage::NoResponseEvidence,
            }
        } else {
            ModelEvidenceCoverage::Degraded(self.reasons.clone())
        };
        let stats = match self.grammar {
            ModelEvidenceGrammar::Claude => ModelEvidenceStats::Claude {
                assistant_records: self.response_records,
                usable_model_records: self.usable_model_records,
                placeholder_records: self.placeholder_records,
                malformed_records: self.malformed_records,
                unclassified_records: self.unclassified_records,
            },
            ModelEvidenceGrammar::Codex => ModelEvidenceStats::Codex {
                response_records: self.response_records,
                settings_records: self.settings_records,
                usable_model_records: self.usable_model_records,
                placeholder_records: self.placeholder_records,
                malformed_records: self.malformed_records,
                unclassified_records: self.unclassified_records,
            },
        };
        ModelEvidenceAssessment {
            trajectory: self.trajectory.clone(),
            coverage,
            latest: self.latest.clone(),
            stats,
            complete_bytes: self.complete_bytes,
            last_usable_model_at: self.last_usable_model_at,
            trailing_bytes: self.pending.len() as u64,
        }
    }

    /// Finish capture, treating any undelimited bytes as lost evidence.
    #[must_use]
    pub fn finish(mut self) -> ModelEvidenceAssessment {
        if !self.pending.is_empty() {
            self.reason(ModelEvidenceReason::UnassessedTail);
            self.latest = LatestModelEvidence::Indeterminate;
        }
        self.assessment()
    }

    fn reason(&mut self, reason: ModelEvidenceReason) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    fn malformed(&mut self) {
        self.malformed_records += 1;
        self.reason(ModelEvidenceReason::MalformedRecord);
        self.latest = LatestModelEvidence::Indeterminate;
    }

    fn unclassified(&mut self) {
        self.unclassified_records += 1;
        self.reason(ModelEvidenceReason::UnclassifiedRecord);
        self.latest = LatestModelEvidence::Indeterminate;
    }

    fn report_model(&mut self, raw: Option<&str>, response: bool, required: bool) {
        match raw {
            Some("<synthetic>") => {
                if response {
                    self.placeholder_records += 1;
                    self.reason(ModelEvidenceReason::PlaceholderModel);
                    self.latest = LatestModelEvidence::ModelMissing;
                }
            }
            Some(raw) => {
                if let Some(id) = ModelId::new(raw) {
                    if response || self.grammar == ModelEvidenceGrammar::Codex {
                        self.usable_model_records += 1;
                    }
                    self.last_usable_model_at = Some(self.complete_bytes);
                    if self.trajectory.last() != Some(&id) {
                        self.trajectory.push(id.clone());
                    }
                    self.latest = if response {
                        LatestModelEvidence::ModelReported(id)
                    } else if self.grammar == ModelEvidenceGrammar::Claude {
                        LatestModelEvidence::NoResponse
                    } else {
                        LatestModelEvidence::SettingsReported(id)
                    };
                } else {
                    self.reason(ModelEvidenceReason::InvalidModel);
                    self.latest = if response {
                        LatestModelEvidence::ModelMissing
                    } else {
                        LatestModelEvidence::Indeterminate
                    };
                }
            }
            None if required => {
                if response {
                    self.reason(ModelEvidenceReason::MissingAssistantModel);
                    self.latest = LatestModelEvidence::ModelMissing;
                } else {
                    self.reason(ModelEvidenceReason::InvalidModel);
                    self.latest = LatestModelEvidence::Indeterminate;
                }
            }
            None => {}
        }
    }

    fn accept_line(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            self.malformed();
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            self.malformed();
            return;
        };
        match self.grammar {
            ModelEvidenceGrammar::Claude => self.accept_claude(&value),
            ModelEvidenceGrammar::Codex => self.accept_codex(&value),
        }
    }

    fn accept_claude(&mut self, value: &serde_json::Value) {
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("assistant") => {
                self.response_records += 1;
                let raw = value
                    .pointer("/message/model")
                    .and_then(serde_json::Value::as_str);
                if raw.is_none() && value.pointer("/message/model").is_some() {
                    self.reason(ModelEvidenceReason::InvalidModel);
                    self.latest = LatestModelEvidence::ModelMissing;
                } else {
                    self.report_model(raw, true, true);
                }
            }
            Some("system") => match value.get("subtype").and_then(serde_json::Value::as_str) {
                Some("init") => {
                    let raw = value.get("model").and_then(serde_json::Value::as_str);
                    if raw.is_none() && value.get("model").is_some() {
                        self.reason(ModelEvidenceReason::InvalidModel);
                        self.latest = LatestModelEvidence::Indeterminate;
                    } else {
                        self.report_model(raw, false, false);
                    }
                }
                Some("turn_duration" | "compact_boundary" | "stop_hook_summary") => {}
                _ => self.unclassified(),
            },
            Some("user" | "result" | "progress" | "file-history-snapshot" | "queue-operation") => {}
            _ => self.unclassified(),
        }
    }

    fn accept_codex(&mut self, value: &serde_json::Value) {
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("turn_context") => {
                self.settings_records += 1;
                let raw = value
                    .pointer("/payload/model")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.get("model").and_then(serde_json::Value::as_str));
                self.report_model(raw, false, true);
            }
            Some("session_meta") => {
                self.settings_records += 1;
                let raw = value
                    .pointer("/payload/model")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.get("model").and_then(serde_json::Value::as_str));
                if raw.is_none()
                    && (value.pointer("/payload/model").is_some() || value.get("model").is_some())
                {
                    self.reason(ModelEvidenceReason::InvalidModel);
                    self.latest = LatestModelEvidence::Indeterminate;
                } else {
                    self.report_model(raw, false, false);
                }
            }
            Some("event_msg") => match value
                .pointer("/payload/type")
                .and_then(serde_json::Value::as_str)
            {
                Some("thread_settings_applied") => {
                    self.settings_records += 1;
                    let Some(settings) = value
                        .pointer("/payload/thread_settings")
                        .and_then(serde_json::Value::as_object)
                    else {
                        self.malformed();
                        return;
                    };
                    let raw = settings.get("model").and_then(serde_json::Value::as_str);
                    if raw.is_none() && settings.get("model").is_some() {
                        self.reason(ModelEvidenceReason::InvalidModel);
                        self.latest = LatestModelEvidence::Indeterminate;
                    } else {
                        self.report_model(raw, false, false);
                    }
                }
                Some(
                    "task_started" | "task_complete" | "agent_message" | "user_message"
                    | "token_count" | "turn_aborted",
                ) => {}
                _ => self.unclassified(),
            },
            Some("response_item") => {
                match value
                    .pointer("/payload/type")
                    .and_then(serde_json::Value::as_str)
                {
                    Some(
                        "message"
                        | "reasoning"
                        | "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output"
                        | "web_search_call",
                    ) => {
                        self.response_records += 1;
                        self.latest = LatestModelEvidence::Indeterminate;
                    }
                    _ => self.unclassified(),
                }
            }
            _ => self.unclassified(),
        }
    }
}

/// Assess complete and pending Claude bytes without reading a file or clock.
#[must_use]
pub fn assess_claude_model_evidence(bytes: &[u8], final_capture: bool) -> ModelEvidenceAssessment {
    let mut accumulator = ModelEvidenceAccumulator::new(ModelEvidenceGrammar::Claude);
    accumulator.push(bytes);
    if final_capture {
        accumulator.finish()
    } else {
        accumulator.assessment()
    }
}

/// Assess complete and pending codex bytes without reading a file or clock.
#[must_use]
pub fn assess_codex_model_evidence(bytes: &[u8], final_capture: bool) -> ModelEvidenceAssessment {
    let mut accumulator = ModelEvidenceAccumulator::new(ModelEvidenceGrammar::Codex);
    accumulator.push(bytes);
    if final_capture {
        accumulator.finish()
    } else {
        accumulator.assessment()
    }
}

// ---- Provider (openai / anthropic / mistral) ------------------------------

/// The realized-model-bearing field of a provider HTTP response body.
#[derive(Debug, Deserialize)]
struct ProviderResponseModel {
    #[serde(default)]
    model: Option<String>,
}

/// Parse the realized model echoed in a provider HTTP response body's top-level
/// `"model"` field (the openai / anthropic / mistral in-process adapters —
/// cosmon already receives this byte and today discards it).
///
/// `None` when the body is not JSON, carries no `model`, or the `model` is
/// empty/whitespace (never fabricate a placeholder id).
#[must_use]
pub fn realized_model_from_provider_response(body: &str) -> Option<ModelId> {
    let parsed: ProviderResponseModel = serde_json::from_str(body).ok()?;
    ModelId::new(parsed.model.as_deref()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(models: &[&str]) -> Vec<ModelId> {
        models.iter().map(|m| ModelId::new(m).unwrap()).collect()
    }

    fn efforts(levels: &[&str]) -> Vec<EffortLevel> {
        levels
            .iter()
            .map(|e| EffortLevel::new(e).unwrap())
            .collect()
    }

    // ---- ModelId newtype --------------------------------------------------

    #[test]
    fn model_id_rejects_empty_and_whitespace() {
        assert_eq!(ModelId::new(""), None);
        assert_eq!(ModelId::new("   "), None);
        assert_eq!(ModelId::new("\t \n"), None);
        assert_eq!(ModelId::new("  opus  ").unwrap().as_str(), "opus");
    }

    // ---- Claude -----------------------------------------------------------

    #[test]
    fn claude_reads_system_init_line() {
        // The bootstrap line names the model before any assistant turn.
        let jsonl = concat!(
            r#"{"type":"system","subtype":"init","model":"claude-opus-4-8","session_id":"x"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["claude-opus-4-8"])
        );
    }

    #[test]
    fn claude_single_stable_session_yields_one_model() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["claude-opus-4-8"])
        );
    }

    #[test]
    fn claude_init_then_assistant_collapses_consecutive_dup() {
        // system/init names opus, first assistant turn also opus → one element.
        let jsonl = concat!(
            r#"{"type":"system","subtype":"init","model":"claude-opus-4-8"}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-sonnet-5"}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["claude-opus-4-8", "claude-sonnet-5"])
        );
    }

    #[test]
    fn claude_quota_fallback_yields_trajectory() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-sonnet-5","usage":{}}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["claude-opus-4-8", "claude-sonnet-5"])
        );
    }

    #[test]
    fn claude_silent_session_yields_empty() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","usage":{}}}"#,
        );
        assert!(realized_models_from_claude_jsonl(jsonl).is_empty());
    }

    #[test]
    fn claude_empty_model_is_never_observed() {
        // A blank id must not be logged as a concrete realization (F-04).
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"model":""}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"   "}}"#,
        );
        assert!(realized_models_from_claude_jsonl(jsonl).is_empty());
    }

    #[test]
    fn claude_ignores_non_json_and_unknown_types() {
        let jsonl = concat!(
            "not json at all\n",
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8"}}"#,
            "\n",
            "",
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["claude-opus-4-8"])
        );
    }

    #[test]
    fn claude_non_consecutive_repeat_is_preserved() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"model":"opus"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"sonnet"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"opus"}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["opus", "sonnet", "opus"])
        );
    }

    /// Round-3 bonus / F-04: a `system` line whose subtype is NOT `init`
    /// (compact boundary, hook output, …) must not be read as a realization
    /// even if it happens to carry a `model`-shaped field.
    #[test]
    fn claude_non_init_system_subtype_is_ignored() {
        let jsonl = concat!(
            r#"{"type":"system","subtype":"compact_boundary","model":"claude-haiku-4-5"}"#,
            "\n",
            r#"{"type":"system","model":"claude-haiku-4-5"}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8"}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            ids(&["claude-opus-4-8"])
        );
    }

    // ---- Codex ------------------------------------------------------------

    #[test]
    fn codex_reads_turn_context_payload_model() {
        // The REAL codex shape: model on the `turn_context` record's payload,
        // not `session_meta` (the false fixture the old test used).
        let jsonl = concat!(
            r#"{"timestamp":"t","type":"session_meta","payload":{"cwd":"/x","session_id":"s"}}"#,
            "\n",
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started"}}"#,
            "\n",
            r#"{"timestamp":"t","type":"turn_context","payload":{"model":"gpt-5.6-terra","effort":"high"}}"#,
        );
        assert_eq!(
            realized_models_from_codex_session(jsonl),
            ids(&["gpt-5.6-terra"])
        );
    }

    #[test]
    fn codex_follows_mid_session_context_change() {
        // Two turn_context records with different models → a trajectory.
        let jsonl = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-terra"}}"#,
        );
        assert_eq!(
            realized_models_from_codex_session(jsonl),
            ids(&["gpt-5-codex", "gpt-5.6-terra"])
        );
    }

    #[test]
    fn codex_legacy_session_meta_model_fallback() {
        // Older codex logs put the model on session_meta directly.
        let jsonl = r#"{"type":"session_meta","payload":{"model":"gpt-5-codex","id":"abc"}}"#;
        assert_eq!(
            realized_models_from_codex_session(jsonl),
            ids(&["gpt-5-codex"])
        );
    }

    #[test]
    fn codex_config_only_line_is_not_mistaken_for_realization() {
        // A session_meta line with NO model (only cwd/provider) must not yield
        // a realization — the config/intention is not the realization (F-04).
        let jsonl = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/x","model_provider":"openai"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
        );
        assert!(realized_models_from_codex_session(jsonl).is_empty());
    }

    #[test]
    fn codex_thread_settings_applied_continues_the_model_trajectory() {
        // FALSIFIER 4 (task-20260911-345c). Verified on a live codex 0.153
        // session log, 2026-09-10: `turn_context` is emitted ONCE per session,
        // at start. A mid-session `/model` switch is recorded as a
        // `thread_settings_applied` event_msg and NOTHING else. A parser that
        // reads only `turn_context` therefore freezes `realized` on the initial
        // pin and reports a switch that happened as a switch that did not.
        let jsonl = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5-codex","effort":"low"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-6-astra","reasoning_effort":"high"}}}"#,
        );
        assert_eq!(
            realized_models_from_codex_session(jsonl),
            ids(&["gpt-5-codex", "gpt-6-astra"]),
            "a /model switch must extend the trajectory, not vanish"
        );
    }

    #[test]
    fn codex_effort_trajectory_reads_turn_context_then_thread_settings() {
        // FALSIFIER 5 + the ex-post half of ADR-177 Decision 5: `effort` on
        // `turn_context`, `reasoning_effort` on `thread_settings_applied`, in
        // order, consecutive duplicates collapsed.
        let jsonl = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"high"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"high"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-6-astra","reasoning_effort":"xhigh"}}}"#,
        );
        assert_eq!(
            realized_efforts_from_codex_session(jsonl),
            efforts(&["high", "xhigh"])
        );
    }

    #[test]
    fn codex_effort_is_never_fabricated_from_silence() {
        // The honesty floor, applied to the axis the discipline was named
        // after: a session that never reported an effort yields no effort, and
        // the empty string is not a value.
        assert!(realized_efforts_from_codex_session(
            r#"{"type":"turn_context","payload":{"model":"gpt-6-astra"}}"#
        )
        .is_empty());
        assert!(realized_efforts_from_codex_session(
            r#"{"type":"turn_context","payload":{"model":"m","effort":"  "}}"#
        )
        .is_empty());
    }

    #[test]
    fn codex_silent_session_yields_empty() {
        let jsonl = r#"{"type":"response_item","payload":{"type":"message"}}"#;
        assert!(realized_models_from_codex_session(jsonl).is_empty());
    }

    // ---- Provider ---------------------------------------------------------

    #[test]
    fn provider_response_echoes_model() {
        let body = r#"{"id":"chatcmpl-1","model":"gpt-4o-2024-11-20","choices":[]}"#;
        assert_eq!(
            realized_model_from_provider_response(body)
                .unwrap()
                .as_str(),
            "gpt-4o-2024-11-20"
        );
    }

    #[test]
    fn provider_response_without_model_is_none() {
        assert_eq!(
            realized_model_from_provider_response(r#"{"choices":[]}"#),
            None
        );
        assert_eq!(realized_model_from_provider_response("not json"), None);
    }

    #[test]
    fn provider_response_empty_model_is_none() {
        assert_eq!(
            realized_model_from_provider_response(r#"{"model":""}"#),
            None
        );
        assert_eq!(
            realized_model_from_provider_response(r#"{"model":"  "}"#),
            None
        );
    }

    #[test]
    fn observation_source_tags_are_stable() {
        assert_eq!(
            ModelObservationSource::ClaudeStreamJson.tag(),
            "claude_stream_json"
        );
        assert_eq!(
            ModelObservationSource::CodexSessionMeta.tag(),
            "codex_session_meta"
        );
        assert_eq!(
            ModelObservationSource::ProviderResponse.tag(),
            "provider_response"
        );
    }
}
