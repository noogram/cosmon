// SPDX-License-Identifier: AGPL-3.0-only

//! The pilot-to-pilot message envelope (mission co-pilotage M2).
//!
//! `cs whisper --to-session` already appends a line of text to a peer's log.
//! A line of text is not a message: it has no identity, so a retried delivery
//! is indistinguishable from a second instruction; it has no sequence, so two
//! senders interleave without a readable order; and it has no expiry, so a
//! stale "take over now" reads exactly like a fresh one.
//!
//! [`PilotMessage`] is the envelope that fixes those three, and nothing more.
//! It carries **no payload** — the body is a content-addressed blob and the
//! envelope points at it, which is what keeps the registry line-oriented and
//! `jq`-readable regardless of how large a checkpoint gets (ADR-168 §D5).
//!
//! # The delivery contract (MESSAGE-TRACE)
//!
//! - **At least once.** A reader that dies mid-print has not consumed
//!   anything; the message is still pending on the next read.
//! - **Consumed once.** Consumption is an *acknowledgement keyed by
//!   [`MessageId`]*, not a byte offset. Delivering the same envelope twice
//!   therefore produces one message, and acknowledging twice is a no-op.
//! - **Expiry is visible, not silent.** A message past `expires_at` is
//!   rendered [`MessageState::Expired`] and still listed. Dropping it quietly
//!   would make "the co-pilot never answered" and "the co-pilot's answer
//!   timed out" the same observation.
//!
//! The byte-cursor channel (`<sid>.log` + `<sid>.seek`) is untouched and stays
//! what it is: a best-effort text tail for `cs whisper --to-session`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::id::{IdError, SessionId};

/// Identity of one message envelope.
///
/// Two envelopes are the same message iff their ids are equal — that is the
/// whole of the deduplication rule, and it is why the id is a type rather
/// than a `String` field somebody later compares case-insensitively.
///
/// # Examples
///
/// ```
/// use cosmon_core::pilot_message::MessageId;
///
/// let id = MessageId::new("msg-0123456789ab").unwrap();
/// assert_eq!(id.as_str(), "msg-0123456789ab");
/// assert!(MessageId::new("has space").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(String);

impl MessageId {
    /// Build a message id from `raw`.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Empty`] if `raw` is empty and [`IdError::Invalid`]
    /// if it contains whitespace — an id that can hold a newline cannot be
    /// stored one-per-line, and an id that can hold a space cannot be pasted
    /// back into a command unquoted.
    pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(IdError::Empty { kind: "MessageId" });
        }
        if raw.chars().any(char::is_whitespace) {
            return Err(IdError::Invalid {
                kind: "MessageId",
                reason: format!("{raw:?} contains whitespace"),
            });
        }
        Ok(Self(raw))
    }

    /// Derive the id of a message from what makes it that message: the pair
    /// it travels between, its position in the destination's stream, and the
    /// hash of its body.
    ///
    /// Deterministic on purpose. A sender that crashes after computing an id
    /// but before appending it recomputes the same id on retry, so the retry
    /// deduplicates instead of doubling.
    #[must_use]
    pub fn derive(from: &SessionId, to: &SessionId, sequence: u64, payload_hash: &str) -> Self {
        let mut h = Sha256::new();
        h.update(from.as_str().as_bytes());
        h.update(b"\x00");
        h.update(to.as_str().as_bytes());
        h.update(b"\x00");
        h.update(sequence.to_be_bytes());
        h.update(b"\x00");
        h.update(payload_hash.as_bytes());
        let digest = h.finalize();
        let mut hex = String::with_capacity(12);
        for b in digest.iter().take(6) {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
        }
        Self(format!("msg-{hex}"))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a reader should make of a message it is looking at.
///
/// Tri-valued by the same discipline as `cs diverge`: "not yet read" and "read
/// too late to matter" are different facts about the world and are never
/// collapsed into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageState {
    /// Delivered, not yet acknowledged, still within its validity window.
    Queued,
    /// Acknowledged by the destination session.
    Read,
    /// Past `expires_at` and never acknowledged. Still listed — an expired
    /// instruction is evidence, not litter.
    Expired,
}

impl MessageState {
    /// The lowercase wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Read => "read",
            Self::Expired => "expired",
        }
    }
}

/// One pilot-to-pilot message, body excluded.
///
/// # Examples
///
/// ```
/// use chrono::{Duration, Utc};
/// use cosmon_core::id::SessionId;
/// use cosmon_core::pilot_message::{MessageState, PilotMessage};
///
/// let now = Utc::now();
/// let claude = SessionId::new("claude-sid").unwrap();
/// let codex = SessionId::new("codex-sid").unwrap();
/// let msg = PilotMessage::new(claude, codex, 1, "ref", "abcd", now, Some(now + Duration::minutes(5)));
///
/// assert_eq!(msg.state(now, None), MessageState::Queued);
/// assert_eq!(msg.state(now + Duration::hours(1), None), MessageState::Expired);
/// // An acknowledgement wins over the clock: it was read in time.
/// assert_eq!(msg.state(now + Duration::hours(1), Some(now)), MessageState::Read);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotMessage {
    /// Identity — the deduplication key. See [`MessageId::derive`].
    pub id: MessageId,
    /// Session that sent it. A *session*, not an OS username: two pilots on
    /// one host share a username and must not share a sender identity.
    pub from: SessionId,
    /// Destination session.
    pub to: SessionId,
    /// Position in the destination's stream, starting at 1. Monotonic per
    /// inbox, so a reader can detect a gap instead of assuming file order.
    pub sequence: u64,
    /// Where the body lives — a content-addressed blob reference.
    pub payload_ref: String,
    /// SHA-256 of the body, so a reader can tell a corrupted blob from an
    /// absent one without trusting the path.
    pub payload_hash: String,
    /// When the sender minted the envelope.
    pub created_at: DateTime<Utc>,
    /// After this instant an unacknowledged message is [`MessageState::Expired`].
    /// `None` means it never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl PilotMessage {
    /// Compose an envelope, deriving its [`MessageId`] from its content.
    #[must_use]
    pub fn new(
        from: SessionId,
        to: SessionId,
        sequence: u64,
        payload_ref: impl Into<String>,
        payload_hash: impl Into<String>,
        created_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Self {
        let payload_hash = payload_hash.into();
        let id = MessageId::derive(&from, &to, sequence, &payload_hash);
        Self {
            id,
            from,
            to,
            sequence,
            payload_ref: payload_ref.into(),
            payload_hash,
            created_at,
            expires_at,
        }
    }

    /// Classify this message as of `now`, given the acknowledgement instant
    /// the mailbox recorded for it (`None` if it was never acknowledged).
    ///
    /// An acknowledgement is checked *first*: a message read before it
    /// expired stays [`MessageState::Read`] forever, because what happened
    /// does not become untrue when the clock moves.
    #[must_use]
    pub fn state(&self, now: DateTime<Utc>, read_at: Option<DateTime<Utc>>) -> MessageState {
        if read_at.is_some() {
            return MessageState::Read;
        }
        match self.expires_at {
            Some(deadline) if now >= deadline => MessageState::Expired,
            _ => MessageState::Queued,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, 9, 0, 0).unwrap()
    }

    #[test]
    fn an_id_is_a_function_of_the_message_and_of_nothing_else() {
        let a = MessageId::derive(&sid("claude"), &sid("codex"), 7, "deadbeef");
        let b = MessageId::derive(&sid("claude"), &sid("codex"), 7, "deadbeef");
        assert_eq!(a, b, "a retry must recompute the same id");

        // Each component of the tuple actually participates.
        assert_ne!(
            a,
            MessageId::derive(&sid("codex"), &sid("codex"), 7, "deadbeef")
        );
        assert_ne!(
            a,
            MessageId::derive(&sid("claude"), &sid("claude"), 7, "deadbeef")
        );
        assert_ne!(
            a,
            MessageId::derive(&sid("claude"), &sid("codex"), 8, "deadbeef")
        );
        assert_ne!(
            a,
            MessageId::derive(&sid("claude"), &sid("codex"), 7, "cafe")
        );
    }

    // The separator matters: without it, ("ab", "c") and ("a", "bc") hash the
    // same, and two different session pairs collide on one id.
    #[test]
    fn adjacent_fields_cannot_be_confused_for_one_another() {
        let a = MessageId::derive(&sid("ab"), &sid("c"), 1, "h");
        let b = MessageId::derive(&sid("a"), &sid("bc"), 1, "h");
        assert_ne!(a, b);
    }

    #[test]
    fn an_id_may_not_hold_whitespace() {
        assert!(MessageId::new("").is_err());
        assert!(MessageId::new("msg one").is_err());
        assert!(MessageId::new("msg\none").is_err());
        assert!(MessageId::new("msg-ok").is_ok());
    }

    #[test]
    fn a_message_without_a_deadline_never_expires() {
        let m = PilotMessage::new(sid("a"), sid("b"), 1, "ref", "h", now(), None);
        assert_eq!(
            m.state(now() + Duration::days(365), None),
            MessageState::Queued
        );
    }

    #[test]
    fn expiry_is_visible_rather_than_silent() {
        let deadline = now() + Duration::minutes(5);
        let m = PilotMessage::new(sid("a"), sid("b"), 1, "ref", "h", now(), Some(deadline));
        assert_eq!(m.state(now(), None), MessageState::Queued);
        // Exactly at the deadline is already expired — pins the comparator.
        assert_eq!(m.state(deadline, None), MessageState::Expired);
    }

    #[test]
    fn an_acknowledgement_outlives_the_deadline() {
        let deadline = now() + Duration::minutes(5);
        let m = PilotMessage::new(sid("a"), sid("b"), 1, "ref", "h", now(), Some(deadline));
        assert_eq!(
            m.state(deadline + Duration::hours(1), Some(now())),
            MessageState::Read,
        );
    }

    #[test]
    fn json_round_trips() {
        let m = PilotMessage::new(sid("a"), sid("b"), 3, "cas/ab/abcd", "abcd", now(), None);
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("expires_at"), "None deadline is omitted");
        let back: PilotMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
