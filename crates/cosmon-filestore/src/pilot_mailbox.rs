// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem backend for the pilot mailbox (mission co-pilotage M2).
//!
//! Two append-only files per session, both NDJSON, both `jq`-readable:
//!
//! ```text
//! presence/<sid>.inbox.jsonl       one PilotMessage envelope per line
//! presence/<sid>.inbox.ack.jsonl   one {id, read_at} per line
//! ```
//!
//! # Why not a byte cursor
//!
//! The existing `<sid>.log` + `<sid>.seek` channel keeps its read position as
//! a byte offset, and ADR-168 §D4 records what that costs: a seek past a
//! rotated end swallows the backlog silently (P4), and a seek that lands
//! inside a multi-byte character panics the reader (P5). Both are properties
//! of *offsets*, not of that particular code — an offset is a claim about a
//! file that the file can invalidate without telling anyone.
//!
//! This mailbox therefore has no offset at all. Consumption is an
//! acknowledgement keyed by [`MessageId`], which is a claim about a *message*,
//! and a message cannot be rotated out from under its own id. The two failure
//! modes are not fixed here; they are absent.
//!
//! # Crash semantics
//!
//! - Crash *before* the ack is appended: the message is still pending. The
//!   reader sees it again. **At least once.**
//! - Crash *after* the ack is appended: the message is `read` and is not
//!   re-served. **Consumed once.**
//! - The same envelope delivered twice: [`PilotMailbox::deliver`] returns
//!   `false` the second time and the file gains no line. **Consumed once**,
//!   again — the dedup is on the write side *and* on the read side, because a
//!   duplicate that slips past one of them must still not double-act.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_core::error::CosmonError;
use cosmon_core::id::SessionId;
use cosmon_core::paths::CosmonPath;
use cosmon_core::pilot_message::{MessageId, MessageState, PilotMessage};
use serde::{Deserialize, Serialize};

/// One acknowledgement line in `<sid>.inbox.ack.jsonl`.
///
/// Separate from the envelope on purpose: the inbox is written by *senders*
/// and the ack file by the *receiver*, so each file keeps a single writer even
/// though the conversation has two ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAck {
    /// The message that was consumed.
    pub id: MessageId,
    /// When the destination session consumed it.
    pub read_at: DateTime<Utc>,
}

/// A message as a reader sees it: the envelope plus what the mailbox knows
/// about its fate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEntry {
    /// The envelope as delivered.
    pub message: PilotMessage,
    /// Acknowledgement instant, if the destination has consumed it.
    pub read_at: Option<DateTime<Utc>>,
    /// [`MessageState`] evaluated at the instant the caller asked.
    pub state: MessageState,
}

/// File-backed pilot mailbox. Stateless; every call is a pure function of the
/// on-disk layout, exactly like [`crate::PresenceStore`].
#[derive(Debug, Clone)]
pub struct PilotMailbox {
    /// The cosmon **state root** (`.cosmon/state/`).
    state_root: PathBuf,
}

impl PilotMailbox {
    /// Construct a mailbox over the given cosmon state root.
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    /// Path of a session's envelope file.
    #[must_use]
    pub fn inbox_path(&self, sid: &SessionId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PresenceInbox { session: sid }.rel())
    }

    /// Path of a session's acknowledgement file.
    #[must_use]
    pub fn ack_path(&self, sid: &SessionId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PresenceInboxAck { session: sid }.rel())
    }

    /// Every envelope delivered to `sid`, in file order.
    ///
    /// A line that fails to parse is skipped rather than fatal: one malformed
    /// envelope from a buggy sender must not make a pilot's whole inbox
    /// unreadable. A partial trailing line — a sender killed mid-append —
    /// simply fails to parse and is skipped, and the sender's retry appends
    /// the same envelope again under the same id.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] if the file exists but cannot be
    /// read. A missing file is an empty inbox, not an error.
    pub fn envelopes(&self, sid: &SessionId) -> Result<Vec<PilotMessage>, CosmonError> {
        Ok(read_lines(&self.inbox_path(sid))?
            .iter()
            .filter_map(|l| serde_json::from_str::<PilotMessage>(l).ok())
            .collect())
    }

    /// Acknowledgements recorded for `sid`, keyed by message id.
    ///
    /// The *earliest* ack for an id wins: acknowledging twice must not move
    /// the instant at which the message was consumed.
    ///
    /// # Errors
    ///
    /// As [`Self::envelopes`].
    pub fn acks(&self, sid: &SessionId) -> Result<BTreeMap<MessageId, DateTime<Utc>>, CosmonError> {
        let mut out: BTreeMap<MessageId, DateTime<Utc>> = BTreeMap::new();
        for line in read_lines(&self.ack_path(sid))? {
            if let Ok(a) = serde_json::from_str::<MessageAck>(&line) {
                out.entry(a.id)
                    .and_modify(|prior| {
                        if a.read_at < *prior {
                            *prior = a.read_at;
                        }
                    })
                    .or_insert(a.read_at);
            }
        }
        Ok(out)
    }

    /// Append `message` to its destination's inbox unless an envelope with
    /// the same id is already there.
    ///
    /// Returns `true` if the envelope was written, `false` if it was already
    /// present — so a caller can report "delivered" and "already delivered"
    /// as different things without either being an error.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] on a filesystem failure.
    pub fn deliver(&self, message: &PilotMessage) -> Result<bool, CosmonError> {
        if self
            .envelopes(&message.to)?
            .iter()
            .any(|e| e.id == message.id)
        {
            return Ok(false);
        }
        let line = serde_json::to_string(message).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to serialise pilot message: {e}"),
        })?;
        append_line(&self.inbox_path(&message.to), &line)?;
        Ok(true)
    }

    /// The sequence number the next message to `sid` should carry: one past
    /// the highest already delivered, starting at 1.
    ///
    /// Derived from the file rather than from a counter, because a counter is
    /// a second source of truth that a crash can desynchronise.
    ///
    /// # Errors
    ///
    /// As [`Self::envelopes`].
    pub fn next_sequence(&self, sid: &SessionId) -> Result<u64, CosmonError> {
        Ok(self
            .envelopes(sid)?
            .iter()
            .map(|e| e.sequence)
            .max()
            .unwrap_or(0)
            + 1)
    }

    /// Every envelope in `sid`'s inbox, classified as of `now`, in sequence
    /// order.
    ///
    /// # Errors
    ///
    /// As [`Self::envelopes`].
    pub fn entries(
        &self,
        sid: &SessionId,
        now: DateTime<Utc>,
    ) -> Result<Vec<MailboxEntry>, CosmonError> {
        let acks = self.acks(sid)?;
        let mut out: Vec<MailboxEntry> = self
            .envelopes(sid)?
            .into_iter()
            .map(|message| {
                let read_at = acks.get(&message.id).copied();
                let state = message.state(now, read_at);
                MailboxEntry {
                    message,
                    read_at,
                    state,
                }
            })
            .collect();
        out.sort_by_key(|e| e.message.sequence);
        out
            // A duplicate that slipped past `deliver` (two writers racing on
            // an inbox with no file lock) is collapsed here, so a reader acts
            // once even if the file says twice.
            .dedup_by(|a, b| a.message.id == b.message.id);
        Ok(out)
    }

    /// The entries `sid` has not yet acknowledged — what a reader owes an
    /// answer to. Includes expired ones: see [`MessageState::Expired`].
    ///
    /// # Errors
    ///
    /// As [`Self::envelopes`].
    pub fn pending(
        &self,
        sid: &SessionId,
        now: DateTime<Utc>,
    ) -> Result<Vec<MailboxEntry>, CosmonError> {
        Ok(self
            .entries(sid, now)?
            .into_iter()
            .filter(|e| e.read_at.is_none())
            .collect())
    }

    /// Record that `sid` consumed `id` at `read_at`.
    ///
    /// Idempotent: acknowledging an already-acknowledged message appends a
    /// second line and changes nothing observable, because [`Self::acks`]
    /// keeps the earliest instant. The file is append-only so that a crash
    /// mid-write cannot corrupt a prior acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] on a filesystem failure.
    pub fn ack(
        &self,
        sid: &SessionId,
        id: &MessageId,
        read_at: DateTime<Utc>,
    ) -> Result<(), CosmonError> {
        let line = serde_json::to_string(&MessageAck {
            id: id.clone(),
            read_at,
        })
        .map_err(|e| CosmonError::StateStore {
            reason: format!("failed to serialise ack: {e}"),
        })?;
        append_line(&self.ack_path(sid), &line)
    }

    /// Remove both mailbox files for `sid`. Used by the presence sweep when a
    /// session is garbage-collected.
    ///
    /// Best-effort and idempotent — a missing file is a success.
    pub fn remove(&self, sid: &SessionId) {
        let _ = fs::remove_file(self.inbox_path(sid));
        let _ = fs::remove_file(self.ack_path(sid));
    }
}

/// Read a file into lines, treating "does not exist" as "empty".
fn read_lines(path: &PathBuf) -> Result<Vec<String>, CosmonError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s.lines().map(str::to_owned).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(CosmonError::StateStore {
            reason: format!("failed to read {}: {e}", path.display()),
        }),
    }
}

/// Append one newline-terminated line, creating the parent directory on first
/// write.
fn append_line(path: &PathBuf, line: &str) -> Result<(), CosmonError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| CosmonError::StateStore {
            reason: format!("failed to open {}: {e}", path.display()),
        })?;
    writeln!(f, "{line}").map_err(|e| CosmonError::StateStore {
        reason: format!("failed to append to {}: {e}", path.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use tempfile::tempdir;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, 9, 0, 0).unwrap()
    }

    fn msg(from: &str, to: &str, seq: u64, hash: &str) -> PilotMessage {
        PilotMessage::new(
            sid(from),
            sid(to),
            seq,
            format!("cas/{hash}"),
            hash,
            t0(),
            None,
        )
    }

    #[test]
    fn a_cold_mailbox_is_empty_not_an_error() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        assert!(mb.envelopes(&sid("nobody")).unwrap().is_empty());
        assert!(mb.acks(&sid("nobody")).unwrap().is_empty());
        assert_eq!(mb.next_sequence(&sid("nobody")).unwrap(), 1);
    }

    #[test]
    fn delivery_is_idempotent_on_the_envelope_id() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let m = msg("claude", "codex", 1, "aaaa");

        assert!(mb.deliver(&m).unwrap(), "first delivery writes");
        assert!(!mb.deliver(&m).unwrap(), "second delivery is a no-op");
        assert_eq!(mb.envelopes(&sid("codex")).unwrap().len(), 1);
    }

    // MESSAGE-TRACE: a reader that dies before acknowledging has consumed
    // nothing, and a reader that dies after acknowledging has consumed once.
    #[test]
    fn a_crash_before_the_ack_redelivers_and_after_it_does_not() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let m = msg("claude", "codex", 1, "aaaa");
        mb.deliver(&m).unwrap();

        // Reader #1 reads, then "crashes" — no ack is written.
        assert_eq!(mb.pending(&sid("codex"), t0()).unwrap().len(), 1);
        // Reader #2 sees it again. At least once.
        let pending = mb.pending(&sid("codex"), t0()).unwrap();
        assert_eq!(pending.len(), 1);
        mb.ack(&sid("codex"), &pending[0].message.id, t0()).unwrap();
        // Reader #3 sees nothing. Consumed once.
        assert!(mb.pending(&sid("codex"), t0()).unwrap().is_empty());
    }

    #[test]
    fn acknowledging_twice_keeps_the_first_instant() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let m = msg("claude", "codex", 1, "aaaa");
        mb.deliver(&m).unwrap();
        mb.ack(&sid("codex"), &m.id, t0()).unwrap();
        mb.ack(&sid("codex"), &m.id, t0() + Duration::hours(3))
            .unwrap();
        assert_eq!(mb.acks(&sid("codex")).unwrap()[&m.id], t0());
        assert!(mb.pending(&sid("codex"), t0()).unwrap().is_empty());
    }

    #[test]
    fn an_expired_payload_stays_visible() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let m = PilotMessage::new(
            sid("claude"),
            sid("codex"),
            1,
            "cas/x",
            "x",
            t0(),
            Some(t0() + Duration::minutes(5)),
        );
        mb.deliver(&m).unwrap();

        let late = t0() + Duration::hours(1);
        let pending = mb.pending(&sid("codex"), late).unwrap();
        assert_eq!(pending.len(), 1, "expiry hides nothing");
        assert_eq!(pending[0].state, MessageState::Expired);
    }

    #[test]
    fn sequences_are_monotonic_per_inbox() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        for i in 1..=3 {
            let seq = mb.next_sequence(&sid("codex")).unwrap();
            assert_eq!(seq, i);
            mb.deliver(&msg("claude", "codex", seq, &format!("h{i}")))
                .unwrap();
        }
        // A different destination has its own stream.
        assert_eq!(mb.next_sequence(&sid("claude")).unwrap(), 1);
    }

    // Two pilots writing to each other must not share a stream: the inbox is
    // per-destination, which is what makes the channel bidirectional rather
    // than merely two-ended.
    #[test]
    fn the_channel_runs_both_ways_without_crossing() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        mb.deliver(&msg("claude", "codex", 1, "aaaa")).unwrap();
        mb.deliver(&msg("codex", "claude", 1, "bbbb")).unwrap();

        let to_codex = mb.pending(&sid("codex"), t0()).unwrap();
        let to_claude = mb.pending(&sid("claude"), t0()).unwrap();
        assert_eq!(to_codex.len(), 1);
        assert_eq!(to_claude.len(), 1);
        assert_eq!(to_codex[0].message.from.as_str(), "claude");
        assert_eq!(to_claude[0].message.from.as_str(), "codex");
    }

    #[test]
    fn a_partial_trailing_line_is_skipped_not_fatal() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let m = msg("claude", "codex", 1, "aaaa");
        mb.deliver(&m).unwrap();
        // A sender killed mid-append leaves half a line behind.
        let mut f = OpenOptions::new()
            .append(true)
            .open(mb.inbox_path(&sid("codex")))
            .unwrap();
        write!(f, "{{\"id\":\"msg-trun").unwrap();
        drop(f);

        let got = mb.envelopes(&sid("codex")).unwrap();
        assert_eq!(got.len(), 1, "the good envelope survives the torn one");
        assert_eq!(got[0].id, m.id);
    }

    #[test]
    fn a_duplicate_that_slipped_past_deliver_is_still_read_once() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let m = msg("claude", "codex", 1, "aaaa");
        // Two writers raced: the same line landed twice.
        let line = serde_json::to_string(&m).unwrap();
        append_line(&mb.inbox_path(&sid("codex")), &line).unwrap();
        append_line(&mb.inbox_path(&sid("codex")), &line).unwrap();

        assert_eq!(mb.envelopes(&sid("codex")).unwrap().len(), 2, "file says 2");
        assert_eq!(
            mb.pending(&sid("codex"), t0()).unwrap().len(),
            1,
            "the reader acts once",
        );
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        mb.deliver(&msg("claude", "codex", 1, "aaaa")).unwrap();
        mb.remove(&sid("codex"));
        mb.remove(&sid("codex"));
        assert!(mb.envelopes(&sid("codex")).unwrap().is_empty());
    }

    #[test]
    fn paths_are_siblings_of_the_presence_snapshot() {
        let mb = PilotMailbox::new(PathBuf::from("/tmp/state"));
        assert_eq!(
            mb.inbox_path(&sid("s1")).to_string_lossy(),
            "/tmp/state/presence/s1.inbox.jsonl"
        );
        assert_eq!(
            mb.ack_path(&sid("s1")).to_string_lossy(),
            "/tmp/state/presence/s1.inbox.ack.jsonl"
        );
    }
}
