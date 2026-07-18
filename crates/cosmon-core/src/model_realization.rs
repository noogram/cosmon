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
//! disk (a claude session `*.jsonl`, a codex session-meta file, a provider HTTP
//! body) is the caller's job, in the shell. This keeps the capture honest and
//! testable: the parse is a pure function of the bytes an adapter produced.
//!
//! # Fiabilité per adapter (delib-20260718-c70e / feynman)
//!
//! - **claude** — authoritative. Each assistant turn in the stream-json /
//!   session `*.jsonl` carries a top-level `message.model`. Per-turn, so a quota
//!   fallback shows a *different* id on a later line: the parser returns the
//!   whole trajectory, consecutive duplicates collapsed.
//! - **codex** — best-effort. The model lives in the session-meta line
//!   (`~/.codex/sessions/*.jsonl`), keyed `model` on a `session_meta` /
//!   `SessionMeta` record. Single value; no per-turn trajectory today.
//! - **openai / anthropic / mistral** — authoritative. The provider HTTP
//!   response body echoes a top-level `"model"` field cosmon already receives.

use serde::{Deserialize, Serialize};

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
    /// (`message.model`, per assistant turn). Authoritative.
    ClaudeStreamJson,
    /// Parsed from the `codex` session-meta record (`model` field).
    /// Best-effort — single value, no per-turn trajectory today.
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
fn collapse_consecutive(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if out.last() != Some(&id) {
            out.push(id);
        }
    }
    out
}

/// Parse the realized-model **trajectory** from a Claude Code stream-json /
/// session `*.jsonl` slice.
///
/// Each line is one JSON object; assistant turns carry `message.model`. The
/// parser is loose (it ignores lines that are not assistant turns, and lines
/// that fail to parse as JSON) so it survives Claude Code schema evolution —
/// only `type == "assistant"` and `message.model` are consulted. Consecutive
/// duplicate ids are collapsed, so the result is the ordered trajectory of
/// distinct models: one element for a stable session, two or more when a quota
/// fallback swapped the model mid-run.
///
/// Returns an empty vec when no assistant turn named a model (a *silent*
/// session — never fabricate an id from the pin).
#[must_use]
pub fn realized_models_from_claude_jsonl(content: &str) -> Vec<String> {
    let ids = content.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(serde_json::Value::as_str) != Some("assistant") {
            return None;
        }
        value
            .get("message")?
            .get("model")?
            .as_str()
            .map(String::from)
    });
    collapse_consecutive(ids)
}

/// Parse the realized model from a `codex` session `*.jsonl` slice.
///
/// codex writes a session-meta record early in the log carrying the model id.
/// Its exact shape has drifted across codex versions, so the parser is
/// deliberately permissive: it accepts a top-level `model`, or a `model` nested
/// under a `session_meta` / `payload` object, on any line — returning the first
/// one it finds. Best-effort: `None` when no line names a model (the honest
/// floor — the pin then surfaces as *intended, not confirmed*).
#[must_use]
pub fn realized_model_from_codex_session(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(model) = extract_codex_model(&value) {
            return Some(model);
        }
    }
    None
}

/// Pull a `model` string out of a codex line, trying the shapes codex has used:
/// top-level, or nested under `session_meta` / `payload`.
fn extract_codex_model(value: &serde_json::Value) -> Option<String> {
    let direct = value.get("model").and_then(serde_json::Value::as_str);
    let nested = ["session_meta", "payload"].into_iter().find_map(|k| {
        value
            .get(k)
            .and_then(|v| v.get("model"))
            .and_then(serde_json::Value::as_str)
    });
    direct.or(nested).map(String::from)
}

/// Parse the realized model echoed in a provider HTTP response body's top-level
/// `"model"` field (the openai / anthropic / mistral in-process adapters —
/// cosmon already receives this byte and today discards it).
///
/// `None` when the body is not JSON or carries no `model` — never fabricate.
#[must_use]
pub fn realized_model_from_provider_response(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_single_stable_session_yields_one_model() {
        // Two assistant turns on the same model → one trajectory element.
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            vec!["claude-opus-4-8".to_string()],
        );
    }

    #[test]
    fn claude_quota_fallback_yields_trajectory() {
        // The case the feature exists to reveal: Opus ran, then a per-turn
        // quota fallback swapped to Sonnet. The trajectory keeps *both*.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-sonnet-5","usage":{}}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            vec!["claude-opus-4-8".to_string(), "claude-sonnet-5".to_string()],
        );
    }

    #[test]
    fn claude_silent_session_yields_empty() {
        // No assistant turn names a model → empty. Never fabricate from a pin.
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","usage":{}}}"#,
        );
        assert!(realized_models_from_claude_jsonl(jsonl).is_empty());
    }

    #[test]
    fn claude_ignores_non_json_and_non_assistant_lines() {
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
            vec!["claude-opus-4-8".to_string()],
        );
    }

    #[test]
    fn codex_reads_top_level_model() {
        let jsonl = concat!(
            r#"{"type":"session_meta","model":"gpt-5-codex","cwd":"/x"}"#,
            "\n",
            r#"{"type":"turn","payload":{}}"#,
        );
        assert_eq!(
            realized_model_from_codex_session(jsonl),
            Some("gpt-5-codex".to_string()),
        );
    }

    #[test]
    fn codex_reads_nested_session_meta_model() {
        let jsonl = r#"{"type":"session_meta","payload":{"model":"gpt-5-codex","id":"abc"}}"#;
        assert_eq!(
            realized_model_from_codex_session(jsonl),
            Some("gpt-5-codex".to_string()),
        );
    }

    #[test]
    fn codex_silent_session_yields_none() {
        let jsonl = r#"{"type":"turn","payload":{"role":"assistant"}}"#;
        assert_eq!(realized_model_from_codex_session(jsonl), None);
    }

    #[test]
    fn provider_response_echoes_model() {
        let body = r#"{"id":"chatcmpl-1","model":"gpt-4o-2024-11-20","choices":[]}"#;
        assert_eq!(
            realized_model_from_provider_response(body),
            Some("gpt-4o-2024-11-20".to_string()),
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

    #[test]
    fn non_consecutive_repeat_is_preserved() {
        // opus → sonnet → opus is a real there-and-back trajectory, not noise.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"model":"opus"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"sonnet"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"opus"}}"#,
        );
        assert_eq!(
            realized_models_from_claude_jsonl(jsonl),
            vec!["opus".to_string(), "sonnet".to_string(), "opus".to_string()],
        );
    }
}
