// SPDX-License-Identifier: AGPL-3.0-only

//! Remote pilot messages of a bound attachment (issue #212, W7b).
//!
//! The cross-machine collaboration contract
//! (`docs/specs/cross-machine-collaboration.md`, §4) lets a bound attachment
//! of a mission send a pilot message (S-SEND), read its own pending messages
//! without consuming them (S-PULL) and acknowledge one of them (S-ACK). The
//! store is the existing pilot mailbox (`pilot-mailbox`), not a second one:
//! an attachment's mailbox is the mailbox whose session id is the attachment
//! id ([`mailbox_of`]), and the sender written into every envelope is that
//! mailbox, never a field of the request.
//!
//! This module decides; it does no I/O. The mailbox applies a decision under
//! its own exclusive lock, so the key check and the pending cap see every
//! committed remote send.

use chrono::{DateTime, TimeDelta, Utc};
use sha2::{Digest, Sha256};

use crate::collaboration::AttachmentId;
use crate::id::{IdError, SessionId};
use crate::pilot_message::PilotMessage;

/// Largest message body S-SEND accepts, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 65_536;

/// Unacknowledged messages one attachment may have sent at once (S-SEND).
pub const MAX_PENDING_PER_ATTACHMENT: usize = 1_000;

/// Largest page of messages S-PULL returns.
pub const PULL_PAGE: usize = 100;

/// Largest S-PULL response, in bytes.
pub const PULL_RESPONSE_BYTES: usize = 1_048_576;

/// Bytes of [`PULL_RESPONSE_BYTES`] kept for the response envelope (schema
/// version, mission, receipt time, cursor), so that the listed messages
/// never push the whole response past its bound.
pub const PULL_ENVELOPE_RESERVE: usize = 4_096;

/// Bytes available to the listed messages of one S-PULL page.
pub const PULL_ITEM_BUDGET: usize = PULL_RESPONSE_BYTES - PULL_ENVELOPE_RESERVE;

/// Why a remote pilot mailbox operation is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemoteMailboxError {
    /// The body is larger than [`MAX_MESSAGE_BYTES`].
    #[error("message larger than {MAX_MESSAGE_BYTES} bytes")]
    PayloadTooLarge,
    /// The recipient is not a pilot of the caller's mission.
    #[error("the recipient is not a pilot of this mission")]
    NotAMember,
    /// The key already names a message with other content.
    #[error("the key already names a different message")]
    KeyCollision,
    /// The attachment already has [`MAX_PENDING_PER_ATTACHMENT`]
    /// unacknowledged messages.
    #[error("the attachment already has {MAX_PENDING_PER_ATTACHMENT} unacknowledged messages")]
    PendingLimit,
    /// The key names no envelope in the caller's mailbox.
    #[error("no envelope with this key in the attachment's mailbox")]
    UnknownEnvelope,
    /// The requested lifetime cannot be represented as an instant.
    #[error("the message lifetime is out of range")]
    InvalidExpiry,
}

impl RemoteMailboxError {
    /// Stable wire code (contract §6, W7b amendment).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "payload_too_large",
            Self::NotAMember => "not_a_member",
            Self::KeyCollision => "key_collision",
            Self::PendingLimit => "pending_limit",
            Self::UnknownEnvelope => "unknown_envelope",
            Self::InvalidExpiry => "invalid_json_body",
        }
    }

    /// HTTP status of [`Self::code`].
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::PayloadTooLarge => 413,
            Self::NotAMember => 403,
            Self::KeyCollision => 409,
            Self::PendingLimit => 429,
            Self::UnknownEnvelope => 404,
            Self::InvalidExpiry => 400,
        }
    }
}

/// The mailbox of a remote attachment: the pilot mailbox whose session id is
/// the attachment id. Attachment ids are host-issued and unique on the host,
/// so each attachment has exactly one mailbox, whatever its mission.
///
/// # Errors
/// Returns [`IdError`] only if the attachment id were empty, which a parsed
/// [`AttachmentId`] never is.
pub fn mailbox_of(attachment: &AttachmentId) -> Result<SessionId, IdError> {
    SessionId::new(attachment.as_str())
}

/// SHA-256 of `body` as lowercase hex: the digest the CAS stores the body
/// under and the envelope's `payload_hash`.
#[must_use]
pub fn payload_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Refuse a body over [`MAX_MESSAGE_BYTES`].
///
/// # Errors
/// [`RemoteMailboxError::PayloadTooLarge`].
pub fn check_size(body: &[u8]) -> Result<(), RemoteMailboxError> {
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(RemoteMailboxError::PayloadTooLarge);
    }
    Ok(())
}

/// The expiry instant of a message sent at `now` with a lifetime of
/// `ttl_secs`, or `None` for a message that never expires.
///
/// # Errors
/// [`RemoteMailboxError::InvalidExpiry`] when the instant overflows.
pub fn expiry(
    now: DateTime<Utc>,
    ttl_secs: Option<u64>,
) -> Result<Option<DateTime<Utc>>, RemoteMailboxError> {
    let Some(secs) = ttl_secs else {
        return Ok(None);
    };
    i64::try_from(secs)
        .ok()
        .and_then(TimeDelta::try_seconds)
        .and_then(|ttl| now.checked_add_signed(ttl))
        .map(Some)
        .ok_or(RemoteMailboxError::InvalidExpiry)
}

/// The lifetime a stored message was sent with, in whole seconds.
fn ttl_of(message: &PilotMessage) -> Option<i64> {
    message
        .expires_at
        .map(|at| (at - message.created_at).num_seconds())
}

/// What a remote send identifies, apart from its key: the retry identity of
/// a request is the sender, the recipient, the body digest and the lifetime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendIdentity<'a> {
    /// The sending attachment's mailbox.
    pub sender: &'a SessionId,
    /// The recipient's mailbox.
    pub to: &'a SessionId,
    /// Digest of the body ([`payload_digest`]).
    pub payload_hash: &'a str,
    /// Requested lifetime in seconds, if any.
    pub ttl_secs: Option<u64>,
}

/// What an accepted send does to the mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendDecision {
    /// A retry of the send that created this message: write nothing.
    Existing(PilotMessage),
    /// A new message: store the body, then append the envelope.
    Create,
}

/// Decide a remote send, given the message its key already names (if any)
/// and the number of the sender's messages still unacknowledged.
///
/// The key check comes before the cap, so a retry of an accepted send
/// succeeds even when the sender's backlog is full.
///
/// # Errors
/// [`RemoteMailboxError::KeyCollision`] when the key names a message with
/// another sender, recipient, body or lifetime, and
/// [`RemoteMailboxError::PendingLimit`] when `pending` already reaches
/// [`MAX_PENDING_PER_ATTACHMENT`].
pub fn decide_send(
    existing: Option<&PilotMessage>,
    request: &SendIdentity<'_>,
    pending: usize,
) -> Result<SendDecision, RemoteMailboxError> {
    if let Some(message) = existing {
        let ttl = request.ttl_secs.and_then(|secs| i64::try_from(secs).ok());
        let same = &message.from == request.sender
            && &message.to == request.to
            && message.payload_hash == request.payload_hash
            && ttl_of(message) == ttl;
        if same {
            return Ok(SendDecision::Existing(message.clone()));
        }
        return Err(RemoteMailboxError::KeyCollision);
    }
    if pending >= MAX_PENDING_PER_ATTACHMENT {
        return Err(RemoteMailboxError::PendingLimit);
    }
    Ok(SendDecision::Create)
}

/// Cut one S-PULL page from `items`, given in sequence order as
/// `(sequence, item, serialised size)`.
///
/// Takes items while fewer than `limit` are taken and their sizes fit in
/// `budget`; the first item is always taken so a page never stalls. Returns
/// the page and the cursor of the next page: the last sequence taken when an
/// item remains, `None` otherwise. Items are pulled lazily, so at most one
/// item past the page is ever produced.
pub fn cut_page<T>(
    items: impl IntoIterator<Item = (u64, T, usize)>,
    limit: usize,
    budget: usize,
) -> (Vec<T>, Option<u64>) {
    let mut page = Vec::new();
    let mut used = 0usize;
    let mut last = None;
    for (sequence, item, size) in items {
        let fits = used.saturating_add(size) <= budget;
        if page.len() >= limit || (!page.is_empty() && !fits) {
            return (page, last);
        }
        used = used.saturating_add(size);
        last = Some(sequence);
        page.push(item);
    }
    (page, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
    }

    fn stored(ttl: Option<i64>) -> PilotMessage {
        PilotMessage::new(
            sid("att-a"),
            sid("att-b"),
            3,
            "cas/ab/abcd",
            "abcd",
            t0(),
            ttl.map(|s| t0() + TimeDelta::seconds(s)),
        )
    }

    fn request<'a>(sender: &'a SessionId, to: &'a SessionId, ttl: Option<u64>) -> SendIdentity<'a> {
        SendIdentity {
            sender,
            to,
            payload_hash: "abcd",
            ttl_secs: ttl,
        }
    }

    #[test]
    fn the_size_bound_is_inclusive() {
        assert!(check_size(&vec![b'x'; MAX_MESSAGE_BYTES]).is_ok());
        assert_eq!(
            check_size(&vec![b'x'; MAX_MESSAGE_BYTES + 1]),
            Err(RemoteMailboxError::PayloadTooLarge)
        );
    }

    #[test]
    fn a_retry_with_the_same_content_returns_the_stored_message() {
        let (a, b) = (sid("att-a"), sid("att-b"));
        let message = stored(Some(60));
        assert_eq!(
            decide_send(Some(&message), &request(&a, &b, Some(60)), 0),
            Ok(SendDecision::Existing(message.clone()))
        );
        // Even with a full backlog: the key check comes first.
        assert_eq!(
            decide_send(
                Some(&message),
                &request(&a, &b, Some(60)),
                MAX_PENDING_PER_ATTACHMENT
            ),
            Ok(SendDecision::Existing(message))
        );
    }

    #[test]
    fn a_key_reused_with_other_content_collides() {
        let (a, b, c) = (sid("att-a"), sid("att-b"), sid("att-c"));
        let message = stored(Some(60));
        for other in [
            request(&a, &b, Some(61)),
            request(&a, &b, None),
            request(&a, &c, Some(60)),
            request(&c, &b, Some(60)),
            SendIdentity {
                payload_hash: "ffff",
                ..request(&a, &b, Some(60))
            },
        ] {
            assert_eq!(
                decide_send(Some(&message), &other, 0),
                Err(RemoteMailboxError::KeyCollision),
                "{other:?}"
            );
        }
    }

    #[test]
    fn the_pending_cap_refuses_the_thousand_and_first() {
        let (a, b) = (sid("att-a"), sid("att-b"));
        assert_eq!(
            decide_send(None, &request(&a, &b, None), MAX_PENDING_PER_ATTACHMENT - 1),
            Ok(SendDecision::Create)
        );
        assert_eq!(
            decide_send(None, &request(&a, &b, None), MAX_PENDING_PER_ATTACHMENT),
            Err(RemoteMailboxError::PendingLimit)
        );
    }

    #[test]
    fn expiry_is_now_plus_ttl_and_refuses_overflow() {
        assert_eq!(expiry(t0(), None), Ok(None));
        assert_eq!(
            expiry(t0(), Some(90)),
            Ok(Some(t0() + TimeDelta::seconds(90)))
        );
        assert_eq!(
            expiry(t0(), Some(u64::MAX)),
            Err(RemoteMailboxError::InvalidExpiry)
        );
        assert_eq!(
            expiry(t0(), Some(i64::MAX as u64)),
            Err(RemoteMailboxError::InvalidExpiry)
        );
    }

    #[test]
    fn the_digest_is_lowercase_sha256_hex() {
        assert_eq!(
            payload_digest(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn pages_stop_at_the_limit_or_the_budget_and_never_stall() {
        let items = |n: u64, size: usize| (1..=n).map(move |s| (s, s, size));
        assert_eq!(cut_page(items(3, 10), 2, 100), (vec![1, 2], Some(2)));
        assert_eq!(cut_page(items(2, 10), 2, 100), (vec![1, 2], None));
        assert_eq!(cut_page(items(3, 10), 5, 20), (vec![1, 2], Some(2)));
        assert_eq!(cut_page(items(3, 10), 5, 30), (vec![1, 2, 3], None));
        assert_eq!(cut_page(items(2, 50), 5, 20), (vec![1], Some(1)));
        assert_eq!(cut_page(items(0, 10), 5, 20), (vec![], None));
    }

    #[test]
    fn codes_follow_the_contract() {
        let cases = [
            (
                RemoteMailboxError::PayloadTooLarge,
                "payload_too_large",
                413,
            ),
            (RemoteMailboxError::NotAMember, "not_a_member", 403),
            (RemoteMailboxError::KeyCollision, "key_collision", 409),
            (RemoteMailboxError::PendingLimit, "pending_limit", 429),
            (RemoteMailboxError::UnknownEnvelope, "unknown_envelope", 404),
            (RemoteMailboxError::InvalidExpiry, "invalid_json_body", 400),
        ];
        for (error, code, status) in cases {
            assert_eq!((error.code(), error.http_status()), (code, status));
        }
    }
}
