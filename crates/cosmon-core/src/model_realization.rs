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
//! - **codex** — best-effort but real. A live codex session writes the model on
//!   the **`turn_context`** record (`payload.model`), re-emitted whenever the
//!   turn context changes — so the parser follows the trajectory, not just the
//!   first value. Legacy `session_meta` / top-level shapes are accepted as a
//!   fallback for older codex versions.
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
    /// Parsed from the codex session log — primarily the `turn_context`
    /// record's `payload.model` (with legacy `session_meta` / top-level
    /// fallbacks). Best-effort but real: follows per-turn context changes.
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

/// Collapse consecutive duplicate ids so the returned slice is the *trajectory*
/// of distinct models that ran, in order. `[opus, opus, sonnet, sonnet]` →
/// `[opus, sonnet]`; a stable single-model session → `[opus]`.
///
/// Non-consecutive repeats are preserved (`[opus, sonnet, opus]` stays as-is):
/// the model genuinely changed back, and that is a real trajectory, not noise.
fn collapse_consecutive(ids: impl IntoIterator<Item = ModelId>) -> Vec<ModelId> {
    let mut out: Vec<ModelId> = Vec::new();
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

/// The `system`/`init` bootstrap line's realized-model-bearing fields.
#[derive(Debug, Deserialize)]
struct ClaudeSystemLine {
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
            ClaudeLine::System(s) => s.model,
            ClaudeLine::Assistant(a) => a.message.and_then(|m| m.model),
            ClaudeLine::Other => None,
        };
        ModelId::new(raw.as_deref()?)
    });
    collapse_consecutive(ids)
}

// ---- Codex ----------------------------------------------------------------

/// One line of a codex `rollout-*.jsonl`, decoded by its `type` discriminator.
///
/// A live codex session carries the realized model on the `turn_context`
/// record (`payload.model`), re-emitted on context change. Older codex versions
/// used a top-level `model` or a `session_meta` object; both are accepted as a
/// fallback. Unknown record types fall through to [`Self::Other`].
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexLine {
    /// The authoritative per-turn context record (`payload.model`).
    TurnContext(CodexPayloadHolder),
    /// The legacy session-meta record (`payload.model` or nested `model`).
    SessionMeta(CodexPayloadHolder),
    /// Any other record type — ignored.
    #[serde(other)]
    Other,
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
}

/// Parse the realized-model **trajectory** from a codex session `*.jsonl`
/// slice, following per-turn context changes.
///
/// Reads the model from each `turn_context` record (`payload.model`) in order,
/// falling back to a legacy `session_meta` / top-level `model` for older codex
/// logs, and collapses consecutive duplicates. A mid-session model change in
/// codex therefore surfaces as a two-element trajectory, exactly like claude.
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
            CodexLine::Other => None,
        };
        ModelId::new(raw.as_deref()?)
    });
    collapse_consecutive(ids)
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
