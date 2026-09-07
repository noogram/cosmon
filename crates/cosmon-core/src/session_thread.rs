// SPDX-License-Identifier: AGPL-3.0-only

//! Worker session **thread** — the structured, retrievable transcript of what
//! was said inside a worker's session, and whether it is waiting on someone.
//!
//! # The gap this closes (issue #51 follow-up)
//!
//! `GET /v1/molecules/{id}/logs` streams the worker's tmux pane as SSE, line
//! by line, and ends when the session disappears. It is a *live tail of a
//! screen*: no history before the connection, no structure (an unanswered
//! permission prompt and a scrolled-off log line are the same `log.line`), and
//! nothing at all once the worker is gone. The reporters asked for the other
//! thing — *what was said, by whom, in what order*, readable **after** the
//! fact and **without** a live tmux, so a human can see that a molecule is
//! waiting for something rather than working.
//!
//! # Where that text actually lives
//!
//! Not in cosmon's own state. A `cs tackle`d worker is a subprocess agent
//! (`claude`, `codex`) hosted in a tmux pane, and the message thread is
//! written by **that agent**, into its own session log on the host:
//!
//! * **claude** — `~/.claude/projects/{sanitised-cwd}/{sessionId}.jsonl`, one
//!   JSON object per line, `type` ∈ {`user`, `assistant`, `attachment`, …}
//!   with an RFC 3339 `timestamp` and a `message.content` that is either a
//!   string or an array of typed blocks (`text`, `thinking`, `tool_use`,
//!   `tool_result`).
//! * **codex** — `~/.codex/sessions/**/rollout-*.jsonl`, whose
//!   `response_item` lines carry `payload.role` and a `content` array of
//!   `input_text` / `output_text` blocks.
//!
//! cosmon already *resolves* both of those files — `cs peek` and `cs ensemble`
//! read them for token accounting and realized-model capture
//! (`cosmon_cli::energy_probe`) — but has never parsed the **message text**
//! out of either. That is what this module adds, as pure text-in /
//! entries-out functions so the projection is an executable spec, testable
//! without a live agent.
//!
//! The per-molecule `events.jsonl` and `log.md` are *not* the thread: they
//! record lifecycle transitions and injection digests (`input_len`,
//! `input_digest`), never the words.
//!
//! # Why the pane stays as a source
//!
//! A transcript file only exists for the subprocess adapters. An in-process
//! provider adapter writes none, and a session resolved from a recorded cwd
//! can be missing after a machine move. So the pane scrollback remains the
//! last-resort source — with its limits named on the wire
//! ([`ThreadSource::TmuxScrollback`]) rather than silently passed off as a
//! transcript.
//!
//! # Waiting is surfaced, never acted on
//!
//! [`waiting_from_entries`] answers *does the tail of this thread look like an
//! unanswered prompt?* using [`crate::dialogue::classify_pane`] — the
//! classifier `cs patrol --dialogue-scan` already relies on — plus the
//! control-plane `await-operator` signal. No new regex is minted here: the
//! marker vocabulary lives in one place or it drifts in two.
//!
//! The ADR-137 §2 discipline applies unchanged. Agent-authored text is an
//! adversarial channel: a worker can print the exact glyphs of any rule meant
//! to police it. So this verdict is **read-only evidence for a human** — the
//! route that serves it writes nothing back into the session, and no
//! autonomous mutation is keyed off it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dialogue::{classify_pane, DialogueClass};

/// Who produced one entry of the thread.
///
/// Three values because three parties speak in a worker session, and a reader
/// scanning for "is anyone waiting on me?" needs to tell them apart: the agent
/// ([`Self::Worker`]), whoever typed or injected a prompt into it
/// ([`Self::Operator`] — the dispatcher's briefing, or a human at the pane),
/// and the machinery around both ([`Self::System`] — tool results, hooks,
/// harness preamble).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadOrigin {
    /// The agent itself — assistant text, its reasoning, its tool calls.
    Worker,
    /// A prompt that entered the session from outside the agent: the
    /// dispatcher's briefing, a `cs whisper`, or a human typing into the pane.
    /// This is the origin the reporters' "paused it by typing into it" case
    /// shows up as.
    Operator,
    /// Machinery: tool results, hook output, harness/developer preamble. Not
    /// something a party *said*, but part of the ordered thread.
    System,
}

impl ThreadOrigin {
    /// Stable kebab/snake wire token, so a client can filter without knowing
    /// the Rust enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Operator => "operator",
            Self::System => "system",
        }
    }
}

/// One entry of the thread: an ordinal, a timestamp when the source knows one,
/// an origin, and the text.
///
/// The ordinal is assigned over the **whole** thread before any tail/limit is
/// applied, so a client that asks for the last three entries still learns
/// *which* three they were and can detect a gap on the next poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadEntry {
    /// 1-based position in the full thread. Stable for a given source read.
    pub ordinal: u64,
    /// When the entry was written, when the source records it. `None` for the
    /// pane scrollback, which carries no per-line time — that absence is the
    /// honest answer, not a fabricated `now`.
    pub at: Option<DateTime<Utc>>,
    /// Who produced it.
    pub origin: ThreadOrigin,
    /// The text, truncated to [`MAX_ENTRY_CHARS`].
    pub text: String,
}

/// Where the entries came from. Always named on the wire, because "no history
/// before you connected" and "the full transcript" are different products and
/// a reader must not have to guess which one they got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThreadSource {
    /// The claude agent's own session log — full history, timestamped,
    /// readable long after the worker exited.
    ClaudeTranscript,
    /// The codex agent's rollout log — same properties, different schema.
    CodexRollout,
    /// A capture of the live tmux pane. A *snapshot of a screen*: bounded by
    /// the scrollback, untimestamped, and unavailable once the session is
    /// gone.
    TmuxScrollback,
    /// Nothing was retrievable: no transcript file resolved and no live pane
    /// answered. An explicit value, never an empty success.
    None,
}

impl ThreadSource {
    /// Stable wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeTranscript => "claude-transcript",
            Self::CodexRollout => "codex-rollout",
            Self::TmuxScrollback => "tmux-scrollback",
            Self::None => "none",
        }
    }

    /// Does this source carry history from before the reader connected?
    ///
    /// `true` for the two transcript files, `false` for the pane and for
    /// nothing at all. Exposed so the route can state the limitation in the
    /// response instead of leaving the client to infer it from the tag.
    #[must_use]
    pub const fn is_retrospective(self) -> bool {
        matches!(self, Self::ClaudeTranscript | Self::CodexRollout)
    }
}

/// Encode a filesystem path the way the claude agent names its per-project
/// session directory: every non-alphanumeric byte becomes `-`.
///
/// One definition, two callers. `cs peek`'s energy probe resolves the same
/// `projects/{sanitised-cwd}/` directory to price a worker's tokens, and the
/// session-thread route resolves it to read the worker's words. A format
/// contract with two copies is a format contract that drifts on the day the
/// agent changes it.
#[must_use]
pub fn sanitise_agent_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Per-entry text cap. A single agent turn can carry a whole file; a thread of
/// them would make the response unbounded, and an unbounded read route is a
/// denial-of-service surface. Truncation is marked with `…` so a reader can
/// tell a cut entry from a short one.
pub const MAX_ENTRY_CHARS: usize = 8_000;

/// How many trailing entries [`waiting_from_entries`] hands the classifier by
/// default. Matches `cs patrol --dialogue-lines` (40): the live prompt sits at
/// the very end of a thread, and a wider window only invites a stale marker
/// from earlier in the run.
pub const WAITING_SCAN_ENTRIES: usize = 40;

/// Truncate to [`MAX_ENTRY_CHARS`], marking the cut.
fn cap(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.chars().count() <= MAX_ENTRY_CHARS {
        return trimmed.to_owned();
    }
    let kept: String = trimmed.chars().take(MAX_ENTRY_CHARS).collect();
    format!("{kept}…")
}

/// Push an entry unless its text is empty after capping — an empty line in a
/// transcript is structure, not speech, and padding the thread with blanks
/// makes `--tail 3` return three nothings.
fn push_entry(
    out: &mut Vec<ThreadEntry>,
    at: Option<DateTime<Utc>>,
    origin: ThreadOrigin,
    text: &str,
) {
    let text = cap(text);
    if text.is_empty() {
        return;
    }
    let ordinal = out.len() as u64 + 1;
    out.push(ThreadEntry {
        ordinal,
        at,
        origin,
        text,
    });
}

/// Parse an RFC 3339 timestamp field, tolerating its absence.
fn parse_at(value: &serde_json::Value, key: &str) -> Option<DateTime<Utc>> {
    let raw = value.get(key)?.as_str()?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Render one typed content block of a claude message into thread text.
///
/// `text` and `thinking` hand back their body. A `tool_use` becomes a compact
/// `[tool: <name>]` marker — the thread should show *that* the agent reached
/// for a tool without inlining its whole argument payload. A `tool_result`
/// hands back its textual content, which is what a reader scanning for "what
/// did that command answer" is looking for.
fn claude_block_text(block: &serde_json::Value) -> Option<String> {
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("text" | "thinking") => block
            .get("text")
            .or_else(|| block.get("thinking"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        Some("tool_use") => {
            let name = block
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            Some(format!("[tool: {name}]"))
        }
        Some("tool_result") => Some(flatten_text(block.get("content")?)),
        _ => None,
    }
}

/// Flatten a `content` value that may be a bare string or an array of blocks
/// carrying `text` fields. Used for `tool_result` payloads, which appear in
/// both shapes across agent versions.
fn flatten_text(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    let Some(items) = content.as_array() else {
        return String::new();
    };
    items
        .iter()
        .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a **claude** agent session log (`~/.claude/projects/…/<id>.jsonl`)
/// into an ordered thread.
///
/// Pure and total: an unparseable line is skipped rather than failing the
/// read. A partially-written last line — the normal state of a log being
/// appended to by a live worker — therefore costs one entry, never the whole
/// thread.
///
/// Origin mapping, and why:
///
/// * `assistant` → [`ThreadOrigin::Worker`]. The agent speaking.
/// * `user` whose content is a **string** → [`ThreadOrigin::Operator`]. That
///   is a prompt someone put into the session: the dispatcher's briefing, or a
///   human typing at the pane.
/// * `user` whose content is an array of `tool_result` blocks →
///   [`ThreadOrigin::System`]. Nobody said it; the harness fed it back.
/// * everything else with renderable text (attachments, hook output) →
///   [`ThreadOrigin::System`].
#[must_use]
pub fn parse_claude_transcript(jsonl: &str) -> Vec<ThreadEntry> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let at = parse_at(&value, "timestamp");
        let kind = value.get("type").and_then(serde_json::Value::as_str);
        let Some(message) = value.get("message") else {
            // Non-message lines (`mode`, `last-prompt`, snapshots) carry no
            // thread text. An `attachment` does, through `rendered`.
            if kind == Some("attachment") {
                if let Some(items) = value.get("rendered").and_then(serde_json::Value::as_array) {
                    for item in items {
                        if let Some(text) = item.get("content").and_then(serde_json::Value::as_str)
                        {
                            push_entry(&mut out, at, ThreadOrigin::System, text);
                        }
                    }
                }
            }
            continue;
        };
        let role = message.get("role").and_then(serde_json::Value::as_str);
        let content = message.get("content");
        match (role, content) {
            (Some("assistant"), Some(c)) => {
                for block in c.as_array().map(Vec::as_slice).unwrap_or_default() {
                    if let Some(text) = claude_block_text(block) {
                        push_entry(&mut out, at, ThreadOrigin::Worker, &text);
                    }
                }
            }
            (Some("user"), Some(c)) if c.is_string() => {
                push_entry(
                    &mut out,
                    at,
                    ThreadOrigin::Operator,
                    c.as_str().unwrap_or_default(),
                );
            }
            (Some("user"), Some(c)) => {
                for block in c.as_array().map(Vec::as_slice).unwrap_or_default() {
                    if let Some(text) = claude_block_text(block) {
                        push_entry(&mut out, at, ThreadOrigin::System, &text);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse a **codex** rollout log (`~/.codex/sessions/**/rollout-*.jsonl`) into
/// an ordered thread.
///
/// Same totality contract as [`parse_claude_transcript`]. Codex wraps each
/// item as `{"type":"response_item","timestamp":…,"payload":{…}}`; only
/// `message` payloads carry words, and `role` names the party:
/// `assistant` → worker, `user` → operator, `developer`/`system` → system.
/// A `*_tool_call` payload renders as the same compact `[tool: …]` marker the
/// claude parser emits, so a reader sees one vocabulary across adapters.
#[must_use]
pub fn parse_codex_rollout(jsonl: &str) -> Vec<ThreadEntry> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let at = parse_at(&value, "timestamp");
        let Some(payload) = value.get("payload") else {
            continue;
        };
        match payload.get("type").and_then(serde_json::Value::as_str) {
            Some("message") => {
                let origin = match payload.get("role").and_then(serde_json::Value::as_str) {
                    Some("assistant") => ThreadOrigin::Worker,
                    Some("user") => ThreadOrigin::Operator,
                    _ => ThreadOrigin::System,
                };
                let text = payload.get("content").map(flatten_text).unwrap_or_default();
                push_entry(&mut out, at, origin, &text);
            }
            Some(kind) if kind.ends_with("tool_call") || kind == "function_call" => {
                let name = payload
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                push_entry(
                    &mut out,
                    at,
                    ThreadOrigin::Worker,
                    &format!("[tool: {name}]"),
                );
            }
            _ => {}
        }
    }
    out
}

/// Project a captured tmux pane into thread entries — one per non-empty line,
/// in order.
///
/// Every entry is [`ThreadOrigin::Worker`] and carries no timestamp, and both
/// facts are deliberate: a screen capture cannot attribute a line to a party
/// or date it. Guessing either would be the exact failure the typed thread
/// exists to avoid.
#[must_use]
pub fn entries_from_pane(pane: &str) -> Vec<ThreadEntry> {
    let mut out = Vec::new();
    for line in pane.lines() {
        push_entry(&mut out, None, ThreadOrigin::Worker, line);
    }
    out
}

/// Whether the end of a thread looks like an **unanswered prompt**, and on
/// what evidence.
///
/// Two independent inputs, kept separate on purpose:
///
/// * [`Self::awaiting_operator`] is a **control-plane** fact — the
///   `awaiting-operator` tag / `blocked_on.json` proof a worker writes when it
///   deliberately stops for a human. Not forgeable by pane text.
/// * [`Self::class`] is a **text** reading of the tail, from
///   [`crate::dialogue::classify_pane`]. Forgeable by construction, which is
///   why it only ever surfaces a finding to a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingVerdict {
    /// The single field a reader consumes: does this thread look like it is
    /// waiting on someone? True when either input says so.
    pub waiting: bool,
    /// The classified stake of the blocking dialogue, if any.
    pub class: DialogueClass,
    /// The line that fired the classification, kept so the operator sees
    /// *why* without re-reading the thread.
    pub evidence: Option<String>,
    /// The control-plane half: the worker declared a stop for the operator.
    pub awaiting_operator: bool,
}

/// Decide [`WaitingVerdict`] from the tail of a thread plus the control-plane
/// `await-operator` signal.
///
/// `scan` bounds how many trailing entries are handed to the classifier
/// ([`WAITING_SCAN_ENTRIES`] is the calibrated default). The entries are
/// joined newline-wise and passed to [`classify_pane`] verbatim — this
/// function mints **no** markers of its own, so the vocabulary that decides
/// "permission prompt" stays in exactly one module.
#[must_use]
pub fn waiting_from_entries(
    entries: &[ThreadEntry],
    scan: usize,
    awaiting_operator: bool,
) -> WaitingVerdict {
    let start = entries.len().saturating_sub(scan);
    let text = entries[start..]
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let scan = classify_pane(&text);
    WaitingVerdict {
        waiting: awaiting_operator || scan.class != DialogueClass::None,
        class: scan.class,
        evidence: scan.evidence,
        awaiting_operator,
    }
}

/// The last `n` entries, ordinals preserved. `n == 0` yields nothing;
/// `n >= len` yields everything.
#[must_use]
pub fn tail(entries: &[ThreadEntry], n: usize) -> &[ThreadEntry] {
    &entries[entries.len().saturating_sub(n)..]
}

/// A half-open window `[offset, offset + limit)` over the thread, for the
/// paginated read. An `offset` past the end yields nothing rather than an
/// error: a client walking forward off the end of a growing thread is doing
/// the right thing, not making a mistake.
#[must_use]
pub fn window(entries: &[ThreadEntry], offset: usize, limit: usize) -> &[ThreadEntry] {
    let start = offset.min(entries.len());
    let end = start.saturating_add(limit).min(entries.len());
    &entries[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two lines of a real claude session log, reduced to the fields the
    /// parser reads. The briefing is an operator turn; the reply is a worker
    /// turn; both are timestamped.
    const CLAUDE: &str = r##"{"type":"user","timestamp":"2026-09-07T18:50:05.678Z","message":{"role":"user","content":"# Autonomous work mode"}}
{"type":"assistant","timestamp":"2026-09-07T18:50:07.475Z","message":{"role":"assistant","content":[{"type":"text","text":"Starting with the conventions."}]}}
{"type":"assistant","timestamp":"2026-09-07T18:50:08.823Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-09-07T18:50:13.908Z","message":{"role":"user","content":[{"type":"tool_result","content":"Cargo.toml"}]}}"##;

    #[test]
    fn claude_transcript_orders_and_attributes_the_thread() {
        let t = parse_claude_transcript(CLAUDE);
        assert_eq!(t.len(), 4);
        assert_eq!(
            t.iter().map(|e| e.origin).collect::<Vec<_>>(),
            vec![
                ThreadOrigin::Operator,
                ThreadOrigin::Worker,
                ThreadOrigin::Worker,
                ThreadOrigin::System,
            ]
        );
        assert_eq!(
            t.iter().map(|e| e.ordinal).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(t[0].text, "# Autonomous work mode");
        assert_eq!(t[2].text, "[tool: Bash]");
        assert_eq!(t[3].text, "Cargo.toml");
        assert!(t[1].at.is_some(), "a transcript entry is timestamped");
    }

    #[test]
    fn claude_transcript_survives_a_torn_last_line() {
        // A live worker is appending; the last line is half-written. The
        // thread must still parse — that is the normal read, not an edge case.
        let torn = format!("{CLAUDE}\n{{\"type\":\"assist");
        assert_eq!(parse_claude_transcript(&torn).len(), 4);
    }

    #[test]
    fn claude_attachment_renders_as_system() {
        let line = r#"{"type":"attachment","timestamp":"2026-09-07T18:50:04.738Z","rendered":[{"content":"<system-reminder>hook ok</system-reminder>"}]}"#;
        let t = parse_claude_transcript(line);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].origin, ThreadOrigin::System);
    }

    const CODEX: &str = r##"{"type":"response_item","timestamp":"2026-09-07T16:34:29.744Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# Autonomous work mode"}]}}
{"type":"response_item","timestamp":"2026-09-07T16:34:36.465Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Reading the conventions."}]}}
{"type":"response_item","timestamp":"2026-09-07T16:34:41.610Z","payload":{"type":"custom_tool_call","name":"shell"}}
{"type":"response_item","timestamp":"2026-09-07T16:34:29.738Z","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"harness preamble"}]}}"##;

    #[test]
    fn codex_rollout_orders_and_attributes_the_thread() {
        let t = parse_codex_rollout(CODEX);
        assert_eq!(
            t.iter().map(|e| e.origin).collect::<Vec<_>>(),
            vec![
                ThreadOrigin::Operator,
                ThreadOrigin::Worker,
                ThreadOrigin::Worker,
                ThreadOrigin::System,
            ]
        );
        assert_eq!(t[2].text, "[tool: shell]");
    }

    #[test]
    fn pane_entries_are_untimestamped_worker_lines_in_order() {
        let t = entries_from_pane("first\n\nsecond\nthird\n");
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].text, "first");
        assert_eq!(t[2].text, "third");
        assert!(t.iter().all(|e| e.at.is_none()));
        assert!(t.iter().all(|e| e.origin == ThreadOrigin::Worker));
    }

    #[test]
    fn sanitise_agent_path_matches_the_agent_encoding() {
        assert_eq!(
            sanitise_agent_path("/Users/e/dev/projects/cosmon"),
            "-Users-e-dev-projects-cosmon"
        );
    }

    #[test]
    fn source_tokens_and_retrospection_are_pinned() {
        assert_eq!(ThreadSource::ClaudeTranscript.as_str(), "claude-transcript");
        assert_eq!(ThreadSource::CodexRollout.as_str(), "codex-rollout");
        assert_eq!(ThreadSource::TmuxScrollback.as_str(), "tmux-scrollback");
        assert_eq!(ThreadSource::None.as_str(), "none");
        assert!(ThreadSource::ClaudeTranscript.is_retrospective());
        assert!(!ThreadSource::TmuxScrollback.is_retrospective());
        assert!(!ThreadSource::None.is_retrospective());
    }

    #[test]
    fn waiting_fires_on_a_permission_prompt() {
        let t = entries_from_pane(
            "cosmon wants to run `ls`\nDo you want to proceed?\n ❯ 1. Yes\n   3. No",
        );
        let v = waiting_from_entries(&t, WAITING_SCAN_ENTRIES, false);
        assert!(v.waiting);
        assert_eq!(v.class, DialogueClass::Permission);
        assert!(v.evidence.is_some());
    }

    #[test]
    fn waiting_is_silent_on_a_plain_running_log() {
        let t = entries_from_pane("Compiling cosmon-core v0.2.1\nRunning 12 tests\ntest ok");
        let v = waiting_from_entries(&t, WAITING_SCAN_ENTRIES, false);
        assert!(!v.waiting);
        assert_eq!(v.class, DialogueClass::None);
        assert!(v.evidence.is_none());
    }

    #[test]
    fn waiting_fires_on_the_control_plane_signal_alone() {
        // No prompt in the text at all: the worker declared its own stop.
        // The forgeable channel says nothing; the unforgeable one decides.
        let t = entries_from_pane("nothing to see here");
        let v = waiting_from_entries(&t, WAITING_SCAN_ENTRIES, true);
        assert!(v.waiting);
        assert!(v.awaiting_operator);
        assert_eq!(v.class, DialogueClass::None);
    }

    #[test]
    fn waiting_over_an_empty_thread_is_false() {
        let v = waiting_from_entries(&[], WAITING_SCAN_ENTRIES, false);
        assert!(!v.waiting);
    }

    #[test]
    fn tail_keeps_the_last_n_with_their_true_ordinals() {
        let t = entries_from_pane("a\nb\nc\nd\ne");
        let last3 = tail(&t, 3);
        assert_eq!(last3.len(), 3);
        assert_eq!(
            last3.iter().map(|e| e.ordinal).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert_eq!(last3[0].text, "c");
        assert_eq!(tail(&t, 0).len(), 0);
        assert_eq!(tail(&t, 99).len(), 5);
    }

    #[test]
    fn window_clamps_rather_than_failing_past_the_end() {
        let t = entries_from_pane("a\nb\nc");
        assert_eq!(window(&t, 1, 2).len(), 2);
        assert_eq!(window(&t, 2, 10).len(), 1);
        assert_eq!(window(&t, 99, 10).len(), 0);
    }

    #[test]
    fn entry_text_is_capped_and_the_cut_is_marked() {
        let long = "x".repeat(MAX_ENTRY_CHARS + 500);
        let t = entries_from_pane(&long);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text.chars().count(), MAX_ENTRY_CHARS + 1);
        assert!(t[0].text.ends_with('…'));
    }
}
