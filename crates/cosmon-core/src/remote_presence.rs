// SPDX-License-Identifier: AGPL-3.0-only

//! Remote presence of a pilot attachment (issue #212, W7a).
//!
//! The cross-machine collaboration contract
//! (`docs/specs/cross-machine-collaboration.md`, §2 and §4) lets a bound
//! attachment of a mission announce itself (S-ATTACH), refresh a
//! server-observed heartbeat (S-BEAT), withdraw (S-DETACH) and list the other
//! attachments of the same mission (S-PEERS). The `presence-store` is the only
//! writer of these records.
//!
//! A remote attachment is not a local session. [`RemotePresence`] is a record
//! class of its own, tagged by the attachment rather than a session id, and it
//! carries none of a local [`crate::presence::Presence`]'s host facts (working
//! directory, pid, terminal, native session handle) nor any role or lease
//! epoch: S-ATTACH confers no lifecycle authority (O8, ADR-168).
//!
//! This module decides; it does no I/O. The store applies a decision under its
//! own exclusive lock, so the cap and the key check see every committed record.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::collaboration::AttachmentId;
use crate::id::MoleculeId;
use crate::work_message::MessageKey;

/// Schema version written into every [`RemotePresence`] record.
pub const REMOTE_PRESENCE_SCHEMA_VERSION: u32 = 1;

/// Attachments one mission may hold at once (S-ATTACH limit).
pub const MAX_ATTACHMENTS_PER_MISSION: usize = 2;

/// Minimum spacing between two accepted heartbeats of one attachment
/// (S-BEAT limit: one per 10 seconds).
pub const HEARTBEAT_MIN_INTERVAL_SECS: i64 = 10;

/// Largest page of attachments S-PEERS returns.
pub const PEERS_PAGE: usize = 100;

/// Longest display label, in bytes. A label is display metadata and never
/// authority (contract §2).
pub const MAX_LABEL_BYTES: usize = 128;

/// One attachment's remote presence on one mission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePresence {
    /// Record schema version.
    pub schema_version: u32,
    /// Mission the attachment is bound to; fixed by the binding, never by
    /// the request.
    pub mission: MoleculeId,
    /// Host-issued attachment, taken from the admitted binding.
    pub attachment: AttachmentId,
    /// Retry key of the attach request that created this record.
    pub idempotency_key: MessageKey,
    /// Optional display label supplied at attach time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Server time of the accepted attach.
    pub attached_at: DateTime<Utc>,
    /// Server time of the last accepted heartbeat (or the attach).
    pub heartbeat_at: DateTime<Utc>,
}

/// The caller-controlled part of an attach request. Everything else in the
/// record comes from the binding or the server clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachRequest {
    /// Retry key; the same key with the same label returns the same record.
    pub idempotency_key: MessageKey,
    /// Optional display label.
    pub label: Option<String>,
}

/// What an accepted attach does to the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachDecision {
    /// Write this new record.
    Create(RemotePresence),
    /// A retry of the request that created this record: write nothing and
    /// return it unchanged.
    Existing(RemotePresence),
}

/// Why a remote presence operation is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemotePresenceError {
    /// The label is longer than [`MAX_LABEL_BYTES`] or contains a control
    /// character.
    #[error("attachment label is invalid")]
    InvalidLabel,
    /// The attachment is already attached under another key or label.
    #[error("the attachment is already attached by a different request")]
    KeyCollision,
    /// The mission already holds [`MAX_ATTACHMENTS_PER_MISSION`] attachments.
    #[error("the mission already holds its {MAX_ATTACHMENTS_PER_MISSION} attachments")]
    AttachmentLimit,
    /// The previous heartbeat was accepted less than
    /// [`HEARTBEAT_MIN_INTERVAL_SECS`] ago.
    #[error("heartbeat sooner than {HEARTBEAT_MIN_INTERVAL_SECS} seconds after the last one")]
    HeartbeatTooSoon {
        /// Whole seconds until a heartbeat would be accepted.
        retry_after_secs: u64,
    },
    /// The attachment has no presence record on this mission.
    #[error("the attachment is not attached to this mission")]
    NotAttached,
}

impl RemotePresenceError {
    /// Stable wire code (contract §6, 2026-11-20 amendment).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidLabel => "invalid_json_body",
            Self::KeyCollision => "key_collision",
            Self::AttachmentLimit => "attachment_limit",
            Self::HeartbeatTooSoon { .. } => "heartbeat_too_soon",
            Self::NotAttached => "not_attached",
        }
    }

    /// HTTP status of [`Self::code`].
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidLabel => 400,
            Self::KeyCollision | Self::AttachmentLimit => 409,
            Self::HeartbeatTooSoon { .. } => 429,
            Self::NotAttached => 404,
        }
    }
}

fn validate_label(label: Option<&str>) -> Result<(), RemotePresenceError> {
    match label {
        Some(text) if text.len() > MAX_LABEL_BYTES || text.chars().any(char::is_control) => {
            Err(RemotePresenceError::InvalidLabel)
        }
        _ => Ok(()),
    }
}

/// Decide an attach of `attachment` to `mission`, given every record the
/// mission currently holds.
///
/// - A record of the same attachment with the same key and label is a retry:
///   [`AttachDecision::Existing`].
/// - A record of the same attachment with another key or label is
///   [`RemotePresenceError::KeyCollision`]; the stored record is kept.
/// - Otherwise the attach is new and refused with
///   [`RemotePresenceError::AttachmentLimit`] when the mission already holds
///   [`MAX_ATTACHMENTS_PER_MISSION`] other attachments.
///
/// # Errors
/// Returns the refusal described above, or
/// [`RemotePresenceError::InvalidLabel`].
pub fn decide_attach(
    present: &[RemotePresence],
    mission: &MoleculeId,
    attachment: &AttachmentId,
    request: AttachRequest,
    now: DateTime<Utc>,
) -> Result<AttachDecision, RemotePresenceError> {
    validate_label(request.label.as_deref())?;
    let mine = present
        .iter()
        .filter(|record| &record.mission == mission)
        .find(|record| &record.attachment == attachment);
    if let Some(record) = mine {
        if record.idempotency_key == request.idempotency_key && record.label == request.label {
            return Ok(AttachDecision::Existing(record.clone()));
        }
        return Err(RemotePresenceError::KeyCollision);
    }
    let others = present
        .iter()
        .filter(|record| &record.mission == mission)
        .count();
    if others >= MAX_ATTACHMENTS_PER_MISSION {
        return Err(RemotePresenceError::AttachmentLimit);
    }
    Ok(AttachDecision::Create(RemotePresence {
        schema_version: REMOTE_PRESENCE_SCHEMA_VERSION,
        mission: mission.clone(),
        attachment: attachment.clone(),
        idempotency_key: request.idempotency_key,
        label: request.label,
        attached_at: now,
        heartbeat_at: now,
    }))
}

/// Decide a heartbeat of an attached record at server time `now`.
///
/// # Errors
/// Returns [`RemotePresenceError::NotAttached`] without a record, and
/// [`RemotePresenceError::HeartbeatTooSoon`] when the last accepted heartbeat
/// is less than [`HEARTBEAT_MIN_INTERVAL_SECS`] old. A clock that moved
/// backwards also reads as too soon, so no heartbeat is ever recorded earlier
/// than the one before it.
pub fn decide_heartbeat(
    record: Option<&RemotePresence>,
    now: DateTime<Utc>,
) -> Result<RemotePresence, RemotePresenceError> {
    let record = record.ok_or(RemotePresenceError::NotAttached)?;
    let next = record.heartbeat_at + Duration::seconds(HEARTBEAT_MIN_INTERVAL_SECS);
    if now < next {
        let wait = (next - now).num_milliseconds();
        let retry_after_secs = u64::try_from(wait.saturating_add(999) / 1000).unwrap_or(1);
        return Err(RemotePresenceError::HeartbeatTooSoon { retry_after_secs });
    }
    Ok(RemotePresence {
        heartbeat_at: now,
        ..record.clone()
    })
}

/// One page of a mission's attachments, sorted by attachment id, starting
/// strictly after `after`.
///
/// The cursor is the last attachment id of the page, so it stays stable when
/// a record is added or removed between two calls. Returns the page and the
/// cursor of the next page, `None` when this page is the last one.
#[must_use]
pub fn page_peers<'a>(
    present: &'a [RemotePresence],
    mission: &MoleculeId,
    after: Option<&AttachmentId>,
    limit: usize,
) -> (Vec<&'a RemotePresence>, Option<AttachmentId>) {
    let mut rows = present
        .iter()
        .filter(|record| &record.mission == mission)
        .filter(|record| after.is_none_or(|cursor| &record.attachment > cursor))
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| a.attachment.cmp(&b.attachment));
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|record| record.attachment.clone())
    } else {
        None
    };
    (rows, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).single().unwrap()
    }
    fn mission() -> MoleculeId {
        MoleculeId::new("task-20261120-aaaa").unwrap()
    }
    fn att(n: u8) -> AttachmentId {
        AttachmentId::from_random([n; 8])
    }
    fn key(k: &str) -> MessageKey {
        MessageKey::new(k).unwrap()
    }
    fn request(k: &str, label: Option<&str>) -> AttachRequest {
        AttachRequest {
            idempotency_key: key(k),
            label: label.map(str::to_owned),
        }
    }
    fn attached(n: u8, k: &str) -> RemotePresence {
        match decide_attach(&[], &mission(), &att(n), request(k, None), t(0)).unwrap() {
            AttachDecision::Create(record) => record,
            AttachDecision::Existing(_) => panic!("an empty mission has nothing to repeat"),
        }
    }

    #[test]
    fn the_third_attachment_of_a_mission_is_refused_and_a_retry_is_not_counted_twice() {
        let present = vec![attached(1, "k1"), attached(2, "k2")];
        assert_eq!(
            decide_attach(&present, &mission(), &att(3), request("k3", None), t(1)),
            Err(RemotePresenceError::AttachmentLimit)
        );
        // The second attachment retrying its own request still succeeds.
        assert_eq!(
            decide_attach(&present, &mission(), &att(2), request("k2", None), t(1)),
            Ok(AttachDecision::Existing(present[1].clone()))
        );
        // Another mission's records do not count against this one.
        let elsewhere = MoleculeId::new("task-20261120-bbbb").unwrap();
        assert!(matches!(
            decide_attach(&present, &elsewhere, &att(3), request("k3", None), t(1)),
            Ok(AttachDecision::Create(_))
        ));
    }

    #[test]
    fn the_same_key_with_another_body_is_a_collision() {
        let present = vec![attached(1, "k1")];
        assert_eq!(
            decide_attach(&present, &mission(), &att(1), request("k1", Some("laptop")), t(1)),
            Err(RemotePresenceError::KeyCollision)
        );
        assert_eq!(
            decide_attach(&present, &mission(), &att(1), request("other", None), t(1)),
            Err(RemotePresenceError::KeyCollision)
        );
    }

    #[test]
    fn a_label_is_bounded_and_printable() {
        let long = "x".repeat(MAX_LABEL_BYTES + 1);
        assert_eq!(
            decide_attach(&[], &mission(), &att(1), request("k", Some(&long)), t(0)),
            Err(RemotePresenceError::InvalidLabel)
        );
        assert_eq!(
            decide_attach(&[], &mission(), &att(1), request("k", Some("a\nb")), t(0)),
            Err(RemotePresenceError::InvalidLabel)
        );
        let exact = "x".repeat(MAX_LABEL_BYTES);
        assert!(decide_attach(&[], &mission(), &att(1), request("k", Some(&exact)), t(0)).is_ok());
    }

    #[test]
    fn heartbeats_are_spaced_by_ten_seconds_of_server_time() {
        let record = attached(1, "k1");
        assert_eq!(
            decide_heartbeat(Some(&record), t(9)),
            Err(RemotePresenceError::HeartbeatTooSoon {
                retry_after_secs: 1
            })
        );
        assert_eq!(
            decide_heartbeat(Some(&record), t(-5)),
            Err(RemotePresenceError::HeartbeatTooSoon {
                retry_after_secs: 15
            })
        );
        let beat = decide_heartbeat(Some(&record), t(10)).unwrap();
        assert_eq!(beat.heartbeat_at, t(10));
        assert_eq!(beat.attached_at, t(0));
        assert_eq!(
            decide_heartbeat(None, t(10)),
            Err(RemotePresenceError::NotAttached)
        );
    }

    #[test]
    fn peers_page_by_a_stable_attachment_cursor() {
        let present = vec![attached(3, "c"), attached(1, "a"), attached(2, "b")];
        let (page, next) = page_peers(&present, &mission(), None, 2);
        assert_eq!(
            page.iter().map(|r| r.attachment.clone()).collect::<Vec<_>>(),
            vec![att(1), att(2)]
        );
        assert_eq!(next, Some(att(2)));
        let (page, next) = page_peers(&present, &mission(), next.as_ref(), 2);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].attachment, att(3));
        assert_eq!(next, None);
        let (page, next) = page_peers(&present, &mission(), None, 3);
        assert_eq!((page.len(), next), (3, None));
    }

    #[test]
    fn every_refusal_has_a_contract_code() {
        let cases = [
            (RemotePresenceError::InvalidLabel, "invalid_json_body", 400),
            (RemotePresenceError::KeyCollision, "key_collision", 409),
            (RemotePresenceError::AttachmentLimit, "attachment_limit", 409),
            (
                RemotePresenceError::HeartbeatTooSoon {
                    retry_after_secs: 1,
                },
                "heartbeat_too_soon",
                429,
            ),
            (RemotePresenceError::NotAttached, "not_attached", 404),
        ];
        for (error, code, status) in cases {
            assert_eq!((error.code(), error.http_status()), (code, status));
        }
    }
}
