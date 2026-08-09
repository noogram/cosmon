// SPDX-License-Identifier: Apache-2.0

//! Did the provider itself say the last turn failed in transport?
//!
//! This module answers exactly one question about a session, and it is the
//! narrowest question in the crate: **is the most recent assistant turn a
//! typed API-error record?** Nothing else — not "does the worker look idle",
//! not "has it been quiet a while", not "does the pane show an error". Those
//! are inferences. This is a flag the provider wrote down.
//!
//! # Why the distinction is the whole point
//!
//! Cosmon already had a patrol that re-engaged apparently-idle workers, and it
//! is disabled — for a correct reason: *a worker that is thinking is not a
//! worker that is stuck*, and a trigger keyed on apparent idleness nudged a
//! healthy one into a livelock that burned $151 (task-20260720-8b63,
//! 2026-07-23). Any replacement has to key on something that is **true or
//! false about the world**, not on something that merely looks a certain way.
//!
//! Claude's session journal carries such a fact. When a request dies in
//! transport, the CLI appends an assistant record with
//! `"isApiErrorMessage": true` and leaves the session parked at the prompt with
//! nothing running. The worker is not thinking; the turn is over and no turn
//! replaced it. The flag is typed, it is written by the provider, and it cannot
//! be produced by anything the worker says.
//!
//! # The trap this module is shaped around
//!
//! The rendered sentence — *"API Error: Response stalled mid-stream…"* — is not
//! the signal, and matching it would be the be1e SEV-1 all over again: a guard
//! that grepped tmux panes for a phrase killed healthy workers, because on
//! three lines carrying that phrase two were *humans quoting it*. The same
//! holds inside the journal: a `user` record can contain the exact sentence
//! (an operator pasting the error, a brief describing this very mechanism) and
//! it normalises to [`SessionEventKind::UserMessage`], which has no
//! `api_error` field to set. Use versus mention is settled by the record's
//! *type*, structurally, and not by reading its text.
//!
//! # Failure direction
//!
//! Every uncertainty resolves to [`ApiStall::flagged`] `== false`:
//! an unreadable log, a log whose tail holds no assistant turn at all, a
//! provider that publishes no such flag. Unknown is never "stalled", because
//! the only consumer of a `true` here speaks into a live worker's terminal.

use chrono::{DateTime, Utc};

use crate::cursor::read_tail_lines;
use crate::error::ProbeError;
use crate::event::SessionEventKind;
use crate::port::{ProviderSessionRef, SessionProbe};

/// How much of a session log's end [`last_assistant_api_error`] reads.
///
/// A Claude journal reaches tens of megabytes over a long molecule while the
/// records that answer this question are the last few. 256 KiB comfortably
/// spans several turns — including the large tool-result records that sit
/// between them — and keeps one patrol tick over a fleet at a few hundred
/// kilobytes of I/O instead of a full sweep of every session on the host.
pub const TAIL_WINDOW_BYTES: u64 = 256 * 1024;

/// What the end of a session log says about its last model turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApiStall {
    /// The last assistant turn in the window carries the provider's typed
    /// transport-failure flag. `false` also covers *no assistant turn seen* and
    /// *provider does not publish the flag* — see the module docs on failure
    /// direction.
    pub flagged: bool,
    /// When that turn was recorded, if it carried a timestamp. Lets a caller
    /// report *how long* a session has been parked on the error without
    /// re-reading the log.
    pub at: Option<DateTime<Utc>>,
    /// Byte offset of the record, so a finding can cite the exact line a human
    /// can go read.
    pub offset: Option<u64>,
}

/// Read the tail of `session` and report on its **last** assistant turn.
///
/// "Last" is load-bearing: an API error three turns ago that the session
/// recovered from is not a stall, and a session whose most recent record is an
/// ordinary model turn is working. Only the final assistant record counts, and
/// records of every other kind (user turns, tool results, quota readings) are
/// skipped over rather than treated as evidence either way.
///
/// # Errors
///
/// [`ProbeError::Io`] when the log exists but cannot be read. A caller that
/// prefers "unknown ⇒ not stalled" can map the error to
/// [`ApiStall::default()`]; this signature keeps the distinction available
/// rather than deciding it here.
pub fn last_assistant_api_error(
    probe: &dyn SessionProbe,
    session: &ProviderSessionRef,
) -> Result<ApiStall, ProbeError> {
    let lines = read_tail_lines(&session.source_locator, TAIL_WINDOW_BYTES)?;
    for line in lines.iter().rev() {
        let ev = probe.normalize(line);
        if let SessionEventKind::AssistantMessage { api_error, .. } = ev.kind {
            return Ok(ApiStall {
                flagged: api_error,
                at: ev.at,
                offset: Some(ev.offset),
            });
        }
    }
    Ok(ApiStall::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeProbe;
    use crate::selector::{NativeSessionId, ProviderName};
    use std::path::Path;

    /// The exact sentence the operator saw on two frozen workers on
    /// 2026-08-09 — used below *as text a user quotes*, never as a matcher.
    const PHRASE: &str = "API Error: Response stalled mid-stream. \
                          The response above may be incomplete.";

    fn session(path: &Path) -> ProviderSessionRef {
        ProviderSessionRef {
            provider: ProviderName::new("claude").unwrap(),
            native_session_id: NativeSessionId::new("s1").unwrap(),
            repo_identity: None,
            cwd: None,
            source_locator: path.to_path_buf(),
            display_name: None,
            started_at: None,
            last_observed_at: None,
        }
    }

    fn write_log(dir: &Path, lines: &[String]) -> std::path::PathBuf {
        let path = dir.join("s1.jsonl");
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        path
    }

    /// A first record so no line under test sits at offset 0 (where the Claude
    /// adapter reads the session envelope rather than the record's own type).
    fn envelope() -> String {
        r#"{"type":"user","sessionId":"s1","cwd":"/w","gitBranch":"feat/x","message":{"content":"go"}}"#
            .to_owned()
    }

    fn user_quoting_the_phrase() -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-09T10:00:00Z",
            "message": {"content": format!("the worker froze on: {PHRASE}")}
        })
        .to_string()
    }

    fn assistant_api_error() -> String {
        serde_json::json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "timestamp": "2026-08-09T10:05:00Z",
            "message": {"model": "<synthetic>", "content": [{"type": "text", "text": PHRASE}]}
        })
        .to_string()
    }

    fn assistant_working() -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-09T10:06:00Z",
            "message": {"model": "claude-opus-5", "content": [{"type": "text", "text": "on it"}]}
        })
        .to_string()
    }

    /// The propelling case: the provider itself typed the last turn as a
    /// transport failure.
    #[test]
    fn a_typed_api_error_on_the_last_assistant_turn_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let log = write_log(tmp.path(), &[envelope(), assistant_api_error()]);
        let stall =
            last_assistant_api_error(&ClaudeProbe::new("/x").unwrap(), &session(&log)).unwrap();
        assert!(stall.flagged);
        assert!(stall.at.is_some());
        assert!(stall.offset.is_some());
    }

    /// The be1e case, and the reason this module exists: the **same sentence**,
    /// this time in a user turn that quotes it. Nothing is flagged — the
    /// distinction is the record's type, not its text.
    #[test]
    fn the_same_sentence_quoted_by_a_user_is_not_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let log = write_log(
            tmp.path(),
            &[envelope(), assistant_working(), user_quoting_the_phrase()],
        );
        let stall =
            last_assistant_api_error(&ClaudeProbe::new("/x").unwrap(), &session(&log)).unwrap();
        assert!(
            !stall.flagged,
            "a user quoting the error is a mention, never a stall"
        );
    }

    /// A session that hit the error and then recovered is working, not stalled:
    /// only the *last* assistant turn is evidence.
    #[test]
    fn an_error_the_session_recovered_from_is_not_a_stall() {
        let tmp = tempfile::tempdir().unwrap();
        let log = write_log(
            tmp.path(),
            &[envelope(), assistant_api_error(), assistant_working()],
        );
        let stall =
            last_assistant_api_error(&ClaudeProbe::new("/x").unwrap(), &session(&log)).unwrap();
        assert!(!stall.flagged);
    }

    #[test]
    fn a_log_with_no_assistant_turn_reports_nothing_rather_than_a_stall() {
        let tmp = tempfile::tempdir().unwrap();
        let log = write_log(tmp.path(), &[envelope(), user_quoting_the_phrase()]);
        let stall =
            last_assistant_api_error(&ClaudeProbe::new("/x").unwrap(), &session(&log)).unwrap();
        assert_eq!(stall, ApiStall::default());
    }
}
