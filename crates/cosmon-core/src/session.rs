// SPDX-License-Identifier: AGPL-3.0-only

//! Session carnet note schema with causal-closure support.
//!
//! A [`SessionNote`] is one timestamped entry in the operator carnet
//! (`.cosmon/state/sessions/session-<ts>.md`). The optional
//! [`SessionNote::cause`] field records *how* the note came to exist —
//! direct keyboard input, voice transcription, oracle suggestion, or
//! autonomous agent output — so the carnet can always answer the
//! authorship question: *who (or what) actually produced this note?*
//! (the decidable-authorship admission test of ADR-061).
//!
//! # Why `cause` is load-bearing
//!
//! Without it, `{human typing, apfel-voice author, human-via-apfel
//! transcription}` are indistinguishable in the carnet. The `cause`
//! field generalises Einstein's "via: apfel" proposal into a typed
//! substrate that every future cognitive channel (apfel, Noogram-self,
//! world-model) can plug into without breaking the schema.
//!
//! # Wire format
//!
//! A rendered note looks like:
//!
//! ```text
//! ## 10:00:00 — insight
//! cause: {kind: direct, agent: null, channel: keyboard}
//!
//! body text goes here
//! ```
//!
//! The `cause:` subline is optional. Notes that pre-date this schema
//! (or omit the flags at the CLI) render without it and parse back as
//! [`SessionNote::cause`] = `None` — backward-compatible by
//! construction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How a session note was produced.
///
/// `Direct` — human typed the note directly. `Transcription` — human
/// spoke, an agent transcribed verbatim. `OracleSuggestion` — agent
/// proposed, human committed. `Autonomous` — agent authored without a
/// human turn in the loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CauseKind {
    /// Human typed the note directly.
    Direct,
    /// Human spoke, an agent transcribed verbatim.
    Transcription,
    /// Agent proposed, human accepted.
    OracleSuggestion,
    /// Agent authored the note with no human turn in the loop.
    Autonomous,
}

impl CauseKind {
    /// Returns the lowercase kebab-case spelling used on the wire
    /// (e.g. `"oracle-suggestion"`). Mirrors the serde repr so the
    /// markdown writer does not need to serialise a whole JSON
    /// fragment just to emit one word.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Transcription => "transcription",
            Self::OracleSuggestion => "oracle-suggestion",
            Self::Autonomous => "autonomous",
        }
    }

    /// Parse from the kebab-case spelling. Returns `None` for any
    /// other input — callers may choose to reject the note or fall
    /// back to a default.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(Self::Direct),
            "transcription" => Some(Self::Transcription),
            "oracle-suggestion" => Some(Self::OracleSuggestion),
            "autonomous" => Some(Self::Autonomous),
            _ => None,
        }
    }
}

/// The physical channel a note arrived on.
///
/// `Other(String)` is the extensibility hatch: future substrates
/// (apfel, Noogram-self, world-model, …) can land as `Other("apfel")`
/// without a breaking enum change. The serde representation is a
/// plain string — known variants serialise to their lowercase name,
/// `Other(s)` serialises to `s`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CauseChannel {
    /// Typed into a terminal.
    Keyboard,
    /// Captured via a microphone.
    Voice,
    /// Inbound Matrix message.
    Matrix,
    /// HTTP webhook.
    Webhook,
    /// Any future channel carrying its name verbatim on the wire.
    Other(String),
}

impl CauseChannel {
    /// Wire spelling. Known variants are lowercase English; `Other(s)`
    /// returns `s` verbatim.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Voice => "voice",
            Self::Matrix => "matrix",
            Self::Webhook => "webhook",
            Self::Other(s) => s.as_str(),
        }
    }

    /// Parse from the wire spelling. Known variants map to the named
    /// enum; anything else lands in `Other`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "keyboard" => Self::Keyboard,
            "voice" => Self::Voice,
            "matrix" => Self::Matrix,
            "webhook" => Self::Webhook,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl Serialize for CauseChannel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CauseChannel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s))
    }
}

/// The `cause` triple attached to a [`SessionNote`] or a nucleate
/// event.
///
/// `agent` is `None` for `kind = Direct` (no agent mediated the note)
/// and `Some("apfel-oracle-<host>" | "matrix:@tenant_auditor:hs" | …)` for
/// every other kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    /// How the note was produced.
    pub kind: CauseKind,
    /// Identity of the mediating agent, if any. Expected to be `None`
    /// when `kind = Direct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Physical channel the note arrived on.
    pub channel: CauseChannel,
}

impl Cause {
    /// Render as the inline subline used in the session markdown:
    /// `cause: {kind: <k>, agent: <a-or-null>, channel: <c>}`.
    ///
    /// The format is deliberately YAML-like but written by hand — the
    /// carnet is read by humans and greppable, so a tiny
    /// deterministic encoder beats dragging in a full YAML emitter.
    #[must_use]
    pub fn render_subline(&self) -> String {
        let agent = match &self.agent {
            Some(a) => a.as_str(),
            None => "null",
        };
        format!(
            "cause: {{kind: {kind}, agent: {agent}, channel: {channel}}}",
            kind = self.kind.as_str(),
            channel = self.channel.as_str(),
        )
    }

    /// Parse the inline subline. Returns `None` for any malformed
    /// input — the carnet must tolerate human edits, so parse
    /// failures flow through as "no cause recorded".
    #[must_use]
    pub fn parse_subline(line: &str) -> Option<Self> {
        let rest = line.trim().strip_prefix("cause:")?.trim();
        let inner = rest.strip_prefix('{')?.strip_suffix('}')?;
        let mut kind: Option<CauseKind> = None;
        let mut agent: Option<Option<String>> = None;
        let mut channel: Option<CauseChannel> = None;
        for field in inner.split(',') {
            let (key, value) = field.split_once(':')?;
            let value = value.trim();
            match key.trim() {
                "kind" => kind = CauseKind::parse(value),
                "agent" => {
                    agent = Some(if value == "null" {
                        None
                    } else {
                        Some(value.to_owned())
                    });
                }
                "channel" => channel = Some(CauseChannel::parse(value)),
                _ => {}
            }
        }
        Some(Self {
            kind: kind?,
            agent: agent?,
            channel: channel?,
        })
    }
}

/// One timestamped entry in the operator carnet.
///
/// The struct round-trips through [`SessionNote::render`] and
/// [`SessionNote::parse_block`] so the `cause` field survives writing
/// and reading the markdown file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNote {
    /// When the note was appended.
    pub timestamp: DateTime<Utc>,
    /// Free-form tag rendered next to the timestamp (e.g. `insight`).
    /// An empty string renders as `HH:MM:SS — ` (trailing em-dash
    /// with no label), matching the pre-schema format.
    pub tag: String,
    /// Free-form markdown body.
    pub body: String,
    /// Optional causal-closure triple. `None` means "pre-schema" or
    /// "cause not supplied at capture time". Existing carnets parse
    /// with `cause = None` — backward-compatible by construction.
    pub cause: Option<Cause>,
}

impl SessionNote {
    /// Build a new note with the current `tag` / `body` / optional
    /// cause. Callers compute the timestamp themselves so tests can
    /// pin it.
    #[must_use]
    pub fn new(
        timestamp: DateTime<Utc>,
        tag: impl Into<String>,
        body: impl Into<String>,
        cause: Option<Cause>,
    ) -> Self {
        Self {
            timestamp,
            tag: tag.into(),
            body: body.into(),
            cause,
        }
    }

    /// Render as a markdown block. Always emits a trailing blank line
    /// so successive notes concatenate cleanly.
    #[must_use]
    pub fn render(&self) -> String {
        let hhmmss = self.timestamp.format("%H:%M:%S");
        let tag = self.tag.trim();
        let header = format!("## {hhmmss} — {tag}\n");
        let cause_line = match &self.cause {
            Some(c) => format!("{}\n", c.render_subline()),
            None => String::new(),
        };
        let body = self.body.trim_end();
        format!("{header}{cause_line}\n{body}\n\n")
    }

    /// Parse one note block starting with a `##` header.
    ///
    /// Accepts the timestamp in `HH:MM:SS` form and reconstructs a
    /// full `DateTime<Utc>` by pairing it with `date_hint` — the
    /// session file only stores times, so the caller supplies the day
    /// from the frontmatter's `started_at`. Returns `None` for any
    /// block that does not start with `## `.
    ///
    /// The parser is lenient: a missing `cause:` subline produces
    /// `cause = None`; a malformed `cause:` subline is treated as
    /// body (the carnet must survive human edits).
    #[must_use]
    pub fn parse_block(block: &str, date_hint: DateTime<Utc>) -> Option<Self> {
        let mut lines = block.lines();
        let header = lines.next()?.strip_prefix("## ")?;
        let (hhmmss, tag) = header.split_once(" — ").unwrap_or((header, ""));
        let time = chrono::NaiveTime::parse_from_str(hhmmss.trim(), "%H:%M:%S").ok()?;
        let timestamp = date_hint.date_naive().and_time(time).and_utc();

        let rest: Vec<&str> = lines.collect();
        let (cause, body_lines) = rest.split_first().map_or((None, &rest[..]), |(h, t)| {
            if h.trim_start().starts_with("cause:") {
                let parsed = Cause::parse_subline(h);
                if parsed.is_some() {
                    (parsed, t)
                } else {
                    (None, &rest[..])
                }
            } else {
                (None, &rest[..])
            }
        });

        let body = body_lines.join("\n").trim().to_owned();
        Some(Self {
            timestamp,
            tag: tag.to_owned(),
            body,
            cause,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 23, 10, 30, 15).unwrap()
    }

    #[test]
    fn cause_kind_str_roundtrip() {
        for k in [
            CauseKind::Direct,
            CauseKind::Transcription,
            CauseKind::OracleSuggestion,
            CauseKind::Autonomous,
        ] {
            assert_eq!(CauseKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn cause_channel_known_and_other() {
        assert_eq!(CauseChannel::parse("keyboard"), CauseChannel::Keyboard);
        assert_eq!(CauseChannel::parse("voice"), CauseChannel::Voice);
        assert_eq!(
            CauseChannel::parse("apfel"),
            CauseChannel::Other("apfel".into())
        );
        assert_eq!(CauseChannel::Other("apfel".into()).as_str(), "apfel");
    }

    #[test]
    fn cause_channel_serde_roundtrip() {
        for c in [
            CauseChannel::Keyboard,
            CauseChannel::Voice,
            CauseChannel::Matrix,
            CauseChannel::Webhook,
            CauseChannel::Other("apfel".into()),
        ] {
            let j = serde_json::to_string(&c).unwrap();
            let back: CauseChannel = serde_json::from_str(&j).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn cause_subline_render_direct() {
        let c = Cause {
            kind: CauseKind::Direct,
            agent: None,
            channel: CauseChannel::Keyboard,
        };
        assert_eq!(
            c.render_subline(),
            "cause: {kind: direct, agent: null, channel: keyboard}"
        );
    }

    #[test]
    fn cause_subline_render_oracle() {
        let c = Cause {
            kind: CauseKind::OracleSuggestion,
            agent: Some("apfel-oracle-rococo".into()),
            channel: CauseChannel::Keyboard,
        };
        assert_eq!(
            c.render_subline(),
            "cause: {kind: oracle-suggestion, agent: apfel-oracle-rococo, channel: keyboard}"
        );
    }

    #[test]
    fn cause_subline_parse_roundtrip() {
        let c = Cause {
            kind: CauseKind::Transcription,
            agent: Some("matrix:@tenant_auditor:hs".into()),
            channel: CauseChannel::Voice,
        };
        let line = c.render_subline();
        assert_eq!(Cause::parse_subline(&line), Some(c));
    }

    #[test]
    fn cause_subline_parse_null_agent() {
        let line = "cause: {kind: direct, agent: null, channel: keyboard}";
        let c = Cause::parse_subline(line).unwrap();
        assert_eq!(c.kind, CauseKind::Direct);
        assert_eq!(c.agent, None);
        assert_eq!(c.channel, CauseChannel::Keyboard);
    }

    #[test]
    fn cause_subline_parse_rejects_malformed() {
        assert!(Cause::parse_subline("not a cause line").is_none());
        assert!(Cause::parse_subline("cause: {kind: direct}").is_none());
        assert!(Cause::parse_subline("cause: direct").is_none());
    }

    #[test]
    fn session_note_roundtrip_with_cause() {
        let note = SessionNote::new(
            ts(),
            "insight",
            "carnet is the primitive",
            Some(Cause {
                kind: CauseKind::OracleSuggestion,
                agent: Some("apfel-oracle-rococo".into()),
                channel: CauseChannel::Keyboard,
            }),
        );
        let rendered = note.render();
        assert!(rendered.contains("## 10:30:15 — insight"));
        assert!(rendered.contains(
            "cause: {kind: oracle-suggestion, agent: apfel-oracle-rococo, channel: keyboard}"
        ));
        assert!(rendered.contains("carnet is the primitive"));
        let parsed = SessionNote::parse_block(rendered.trim_end(), ts()).unwrap();
        assert_eq!(parsed, note);
    }

    #[test]
    fn session_note_roundtrip_without_cause() {
        let note = SessionNote::new(ts(), "", "plain note", None);
        let rendered = note.render();
        assert!(!rendered.contains("cause:"));
        let parsed = SessionNote::parse_block(rendered.trim_end(), ts()).unwrap();
        assert_eq!(parsed, note);
    }

    #[test]
    fn session_note_parses_legacy_block_without_cause_field() {
        // A pre-schema block: header + blank line + body, no cause.
        let block = "## 09:15:00 — insight\n\nfirst body";
        let parsed = SessionNote::parse_block(block, ts()).unwrap();
        assert_eq!(parsed.tag, "insight");
        assert_eq!(parsed.body, "first body");
        assert_eq!(parsed.cause, None);
    }

    fn kind_strategy() -> impl Strategy<Value = CauseKind> {
        prop_oneof![
            Just(CauseKind::Direct),
            Just(CauseKind::Transcription),
            Just(CauseKind::OracleSuggestion),
            Just(CauseKind::Autonomous),
        ]
    }

    fn channel_strategy() -> impl Strategy<Value = CauseChannel> {
        prop_oneof![
            Just(CauseChannel::Keyboard),
            Just(CauseChannel::Voice),
            Just(CauseChannel::Matrix),
            Just(CauseChannel::Webhook),
            // `Other(s)` must not collide with a known variant and
            // must not contain the field separators (`,`, `}`) that
            // the inline encoder relies on.
            "[a-z][a-z0-9-]{0,15}"
                .prop_filter("not a known variant", |s| {
                    !matches!(s.as_str(), "keyboard" | "voice" | "matrix" | "webhook")
                })
                .prop_map(CauseChannel::Other),
        ]
    }

    proptest! {
        #[test]
        fn cause_subline_roundtrip_all_variants(
            kind in kind_strategy(),
            agent in prop::option::of("[a-z][a-z0-9@:_-]{0,31}"),
            channel in channel_strategy(),
        ) {
            let c = Cause { kind, agent, channel };
            let line = c.render_subline();
            prop_assert_eq!(Cause::parse_subline(&line), Some(c));
        }
    }
}
