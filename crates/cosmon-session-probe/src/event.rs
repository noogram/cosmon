// SPDX-License-Identifier: Apache-2.0

//! The normalised event vocabulary — what a cockpit is allowed to know about a
//! session it did not open.
//!
//! Two constraints shape this enum, and they pull in opposite directions.
//!
//! **The cockpit must not learn a provider's schema.** Mission falsifier 10 is
//! *"adding a third provider requires editing `cs sessions`"*. If a cockpit
//! ever matches on `type == "response_item"` or reaches into
//! `message.usage.cache_creation_input_tokens`, Codex's wire format has leaked
//! into cosmon and the next adapter is a cockpit change. So no variant here
//! names a provider, and the residual [`SessionEventKind::Other`] carries the
//! provider's own record name as an opaque string that exists to be *counted*,
//! not matched.
//!
//! **The content must not cross.** ADR-168 D3.3 refuses copying provider
//! conversations into cosmon; the anonymised traces attached to that ADR are
//! the permanent ceiling. So a message event carries its role, its model and
//! its size — never its text. A co-pilot that wants to know what was said asks
//! the pilot for a checkpoint; it does not read the transcript.

use chrono::{DateTime, Utc};
use claudion::TokenCount;
use serde::{Deserialize, Serialize};

/// Token counters, normalised across providers.
///
/// The two providers count differently and both are honoured rather than
/// averaged:
///
/// - Claude reports `input_tokens` (fresh) + `cache_creation_input_tokens` +
///   `cache_read_input_tokens`, which are disjoint. `input_total` is their sum
///   and `cached_input` is the read portion.
/// - Codex reports `input_tokens` already *including* `cached_input_tokens`.
///   Both map across unchanged.
///
/// What is deliberately lost is the Claude cache-creation split, because no
/// other provider has it. Cost arithmetic that needs it stays where it already
/// lives and is already right — `claudion` for Claude, `cosmon_core::codex_energy`
/// for Codex. This struct is for *watching a session*, not for billing it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    /// All input tokens, cached portion included.
    pub input_total: TokenCount,
    /// The portion of `input_total` served from prompt cache.
    pub cached_input: TokenCount,
    /// All output tokens, reasoning portion included.
    pub output_total: TokenCount,
}

/// Quota telemetry, when a provider publishes it.
///
/// ADR-168's central asymmetry: Codex emits this on every one of its 1 451
/// `token_count` events, Claude emits nothing of the sort — its limit is
/// announced as the error that already happened. The type therefore exists
/// with every field optional, and its absence is a fact the cockpit is
/// expected to render as *unknown*, never as *fine*.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaReading {
    /// Percentage of the window consumed, as the provider reports it.
    pub used_percent: Option<f64>,
    /// Length of the rolling window, in minutes.
    pub window_minutes: Option<u64>,
    /// Unix epoch seconds at which the window resets.
    pub resets_at_epoch: Option<i64>,
    /// Whether the provider says a limit has actually been reached.
    pub limit_reached: bool,
}

/// What a normalised record says happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SessionEventKind {
    /// The session announced itself: its working directory, its git branch,
    /// the model provider behind it. Claude publishes these on every record
    /// and this is the first one seen; Codex publishes them once, in
    /// `session_meta`.
    SessionStarted {
        /// Working directory, read from *inside* the log (never decoded from a
        /// directory name — probe P6).
        cwd: Option<String>,
        /// Git branch at session start, when the provider records one.
        git_branch: Option<String>,
        /// The model provider the session is talking to, when named.
        model_provider: Option<String>,
    },
    /// A human turn. Size only.
    UserMessage {
        /// Characters of content, as a size — the content itself never crosses.
        chars: usize,
    },
    /// A model turn. Size, model, and per-turn usage when the record carries
    /// it.
    AssistantMessage {
        /// The model that produced the turn, when the record names one.
        model: Option<String>,
        /// Characters of content, as a size.
        chars: usize,
        /// Per-turn token usage, when the provider attaches it to the turn.
        usage: Option<TurnUsage>,
        /// The provider itself typed this turn as a **transport failure**, not
        /// as model output: Claude's `isApiErrorMessage: true`.
        ///
        /// This is the one field here that is a *flag the provider set*, not a
        /// measurement of the turn, and it exists for one reason: a stalled
        /// session is otherwise indistinguishable from a thinking one. The flag
        /// is deliberately read from the record's own typed boolean and never
        /// from the rendered sentence — the same text ("Response stalled
        /// mid-stream…") appears verbatim in *user* turns that quote it, and a
        /// guard that keys on the phrase arrests the worker that merely
        /// mentions it (the be1e SEV-1 use/mention trap). A `user` record
        /// carrying the phrase normalises to [`SessionEventKind::UserMessage`]
        /// and can therefore never set this flag, whatever it says.
        api_error: bool,
    },
    /// A token-usage report that is not attached to one turn.
    TokenUsage {
        /// The counters.
        usage: TurnUsage,
        /// `true` when the counters are cumulative over the whole session
        /// (Codex's `total_token_usage`), `false` when they cover one turn.
        /// A consumer that sums cumulative readings double-counts, so the flag
        /// is on the event rather than in a provider-specific convention.
        cumulative: bool,
    },
    /// A quota reading the provider volunteered.
    Quota(QuotaReading),
    /// The session compacted its own context. Relevant to a co-pilot because
    /// what the primary can still see changed without anyone deciding it.
    ContextCompacted,
    /// A record the port recognises as well-formed but does not normalise.
    ///
    /// Carries the provider's own name for it so a cockpit can *count*
    /// unmapped traffic ("38 `world_state`") and an adapter author can see
    /// what is worth mapping next — without the cockpit ever branching on the
    /// value.
    Other {
        /// The provider's own record type name.
        record: String,
    },
    /// A complete line that was not valid JSON.
    ///
    /// Not an error: a live log can contain anything, and a probe that aborts
    /// on one bad line cannot watch a live session. Surfacing it as an event
    /// keeps it countable instead of silent.
    Unparseable,
}

/// One normalised record, addressed by the byte offset it starts at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Byte offset of the source line within its file generation. Stable
    /// enough to cite in a finding, meaningless across a rotation — which is
    /// why [`crate::cursor::Continuity`] reports rotations.
    pub offset: u64,
    /// The record's own timestamp, when it has one.
    pub at: Option<DateTime<Utc>>,
    /// What happened.
    pub kind: SessionEventKind,
}

/// Best-effort character count of a provider message body.
///
/// Handles the two shapes both providers use — a bare string, or an array of
/// content blocks with a `text` field. Returns a size and drops the text on
/// the floor; that is the whole point.
pub(crate) fn content_chars(content: Option<&serde_json::Value>) -> usize {
    match content {
        Some(serde_json::Value::String(s)) => s.chars().count(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .map(|b| {
                b.get("text")
                    .and_then(serde_json::Value::as_str)
                    .map_or(0, |t| t.chars().count())
            })
            .sum(),
        _ => 0,
    }
}

/// Parse an RFC 3339 timestamp field, when present and well-formed.
pub(crate) fn timestamp_at(value: &serde_json::Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_chars_counts_both_shapes_and_keeps_no_text() {
        let bare = serde_json::json!("hello");
        assert_eq!(content_chars(Some(&bare)), 5);

        let blocks = serde_json::json!([{"type":"text","text":"ab"},{"type":"tool_use"}]);
        assert_eq!(content_chars(Some(&blocks)), 2);

        assert_eq!(content_chars(None), 0);
    }

    #[test]
    fn an_event_serialises_without_any_content_field() {
        let ev = SessionEvent {
            offset: 0,
            at: None,
            kind: SessionEventKind::UserMessage { chars: 4096 },
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("UserMessage"));
        assert!(json.contains("4096"));
    }
}
