// SPDX-License-Identifier: AGPL-3.0-only

//! Remote sends and acknowledgments of the `pilot-mailbox` writer (issue
//! #212, W7b).
//!
//! The records are the existing mailbox files: an envelope line in the
//! recipient's `presence/<sid>.inbox.jsonl`, the body in the CAS, an ack line
//! in the reader's `presence/<sid>.inbox.ack.jsonl`. A remote attachment's
//! mailbox is the one whose session id is the attachment id
//! ([`cosmon_core::remote_mailbox::mailbox_of`]).
//!
//! Every remote write takes the mission's exclusive mailbox lock
//! ([`CosmonPath::RemoteMailboxLock`]) and reads the mailboxes under it, so
//! two concurrent sends cannot both pass the key check or the pending cap,
//! and two concurrent acknowledgments write one line. A refusal writes
//! nothing: no envelope, no ack and no CAS blob. Reads take no lock.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};

use chrono::{DateTime, Utc};
use cosmon_core::cas::CasStore;
use cosmon_core::error::CosmonError;
use cosmon_core::id::{MoleculeId, SessionId};
use cosmon_core::paths::CosmonPath;
use cosmon_core::pilot_message::{MessageId, PilotMessage};
use cosmon_core::remote_mailbox::{
    check_size, decide_send, expiry, payload_digest, RemoteMailboxError, SendDecision, SendIdentity,
};
use cosmon_core::work_message::MessageKey;

use crate::cas::FileCas;
use crate::pilot_mailbox::PilotMailbox;

/// Why a remote mailbox operation did not complete.
#[derive(Debug, thiserror::Error)]
pub enum RemoteMailboxStoreError {
    /// The operation is refused by the contract; nothing was written.
    #[error(transparent)]
    Refused(#[from] RemoteMailboxError),
    /// The mailbox could not be read, locked or written; the outcome is
    /// unknown.
    #[error("pilot mailbox unavailable: {0}")]
    Unavailable(String),
}

impl From<std::io::Error> for RemoteMailboxStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Unavailable(error.to_string())
    }
}

impl From<CosmonError> for RemoteMailboxStoreError {
    fn from(error: CosmonError) -> Self {
        Self::Unavailable(error.to_string())
    }
}

/// One remote send. The sender and the mission are not fields: the caller
/// passes the admitted attachment's mailbox and mission separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSend {
    /// Caller-retained retry key.
    pub key: MessageKey,
    /// Recipient mailbox; must be one of the mission's pilots.
    pub to: SessionId,
    /// Message body.
    pub body: String,
    /// Seconds after which an unacknowledged message reads as expired.
    pub ttl_secs: Option<u64>,
}

impl PilotMailbox {
    fn remote_lock(&self, mission: &MoleculeId) -> Result<File, RemoteMailboxStoreError> {
        let path = self
            .state_root()
            .join(CosmonPath::RemoteMailboxLock { mission }.rel());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    }

    /// Send `request` from `sender` on `mission` (S-SEND).
    ///
    /// `recipients` names the mailboxes of the mission's pilots; a recipient
    /// outside it is refused before any mailbox path is built from it. The
    /// key check and the pending count read every mailbox of `recipients`:
    /// the message a key names is looked up there, and the sender's pending
    /// messages are its unacknowledged envelopes there. Returns the envelope
    /// and whether this call wrote it.
    ///
    /// # Errors
    /// Returns the refusal of [`check_size`], [`expiry`] or [`decide_send`],
    /// [`RemoteMailboxError::NotAMember`] for a recipient outside
    /// `recipients`, or an unavailable mailbox.
    pub fn send_remote(
        &self,
        mission: &MoleculeId,
        sender: &SessionId,
        recipients: &BTreeSet<SessionId>,
        request: &RemoteSend,
        now: DateTime<Utc>,
    ) -> Result<(PilotMessage, bool), RemoteMailboxStoreError> {
        check_size(request.body.as_bytes())?;
        if !recipients.contains(&request.to) {
            return Err(RemoteMailboxError::NotAMember.into());
        }
        let expires_at = expiry(now, request.ttl_secs)?;
        let id = MessageId::from_remote_key(mission.as_str(), sender, request.key.as_str());
        let payload_hash = payload_digest(request.body.as_bytes());

        let _lock = self.remote_lock(mission)?;
        let mut existing = None;
        let mut pending = 0usize;
        for recipient in recipients {
            for entry in self.entries(recipient, now)? {
                if entry.message.id == id {
                    existing = Some(entry.message.clone());
                }
                if &entry.message.from == sender && entry.read_at.is_none() {
                    pending += 1;
                }
            }
        }
        let identity = SendIdentity {
            sender,
            to: &request.to,
            payload_hash: &payload_hash,
            ttl_secs: request.ttl_secs,
        };
        match decide_send(existing.as_ref(), &identity, pending)? {
            SendDecision::Existing(message) => Ok((message, false)),
            SendDecision::Create => {
                let cas = FileCas::new(self.state_root().join("cas"));
                let hash = cas.put(request.body.as_bytes())?;
                let message = PilotMessage {
                    id,
                    from: sender.clone(),
                    to: request.to.clone(),
                    sequence: self.next_sequence(&request.to)?,
                    payload_ref: format!("cas/{}/{}", hash.prefix(), hash.as_str()),
                    payload_hash: hash.as_str().to_owned(),
                    created_at: now,
                    expires_at,
                };
                self.deliver(&message)?;
                Ok((message, true))
            }
        }
    }

    /// Acknowledge `id` in the mailbox `reader` on `mission` (S-ACK).
    ///
    /// Returns `true` when this call recorded the acknowledgment and `false`
    /// for a duplicate, which writes nothing.
    ///
    /// # Errors
    /// [`RemoteMailboxError::UnknownEnvelope`] when `id` is not in the
    /// mailbox, or an unavailable mailbox.
    pub fn ack_remote(
        &self,
        mission: &MoleculeId,
        reader: &SessionId,
        id: &MessageId,
        now: DateTime<Utc>,
    ) -> Result<bool, RemoteMailboxStoreError> {
        let _lock = self.remote_lock(mission)?;
        if !self.envelopes(reader)?.iter().any(|e| &e.id == id) {
            return Err(RemoteMailboxError::UnknownEnvelope.into());
        }
        Ok(self.ack(reader, id, now)?)
    }

    /// Body of `message` from the CAS, or `None` when it cannot be read.
    /// Reads only.
    #[must_use]
    pub fn body(&self, message: &PilotMessage) -> Option<Vec<u8>> {
        let hash = cosmon_core::cas::ContentHash::new(message.payload_hash.clone()).ok()?;
        FileCas::new(self.state_root().join("cas")).get(&hash).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use cosmon_core::remote_mailbox::MAX_PENDING_PER_ATTACHMENT;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
    }

    fn mission() -> MoleculeId {
        MoleculeId::new("task-20261121-m000").unwrap()
    }

    fn send(key: &str, to: &str, body: &str) -> RemoteSend {
        RemoteSend {
            key: MessageKey::new(key).unwrap(),
            to: sid(to),
            body: body.to_owned(),
            ttl_secs: None,
        }
    }

    fn peers() -> BTreeSet<SessionId> {
        [sid("att-a"), sid("att-b")].into_iter().collect()
    }

    #[test]
    fn a_send_lands_in_the_recipient_mailbox_from_the_sender() {
        let dir = tempfile::tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let (message, written) = mb
            .send_remote(
                &mission(),
                &sid("att-a"),
                &peers(),
                &send("k1", "att-b", "hi"),
                t0(),
            )
            .unwrap();
        assert!(written);
        assert_eq!(message.from, sid("att-a"));
        assert_eq!(message.sequence, 1);
        assert_eq!(mb.envelopes(&sid("att-b")).unwrap(), vec![message.clone()]);
        assert_eq!(mb.body(&message).unwrap(), b"hi");
    }

    #[test]
    fn a_refused_send_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let outsider = send("k1", "../escape", "hi");
        assert!(matches!(
            mb.send_remote(&mission(), &sid("att-a"), &peers(), &outsider, t0()),
            Err(RemoteMailboxStoreError::Refused(
                RemoteMailboxError::NotAMember
            ))
        ));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_pending_cap_counts_only_the_sender_and_frees_on_ack() {
        let dir = tempfile::tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let a = sid("att-a");
        for n in 0..MAX_PENDING_PER_ATTACHMENT {
            mb.send_remote(
                &mission(),
                &a,
                &peers(),
                &send(&format!("k{n}"), "att-b", "x"),
                t0(),
            )
            .unwrap();
        }
        let over = send("over", "att-b", "x");
        assert!(matches!(
            mb.send_remote(&mission(), &a, &peers(), &over, t0()),
            Err(RemoteMailboxStoreError::Refused(
                RemoteMailboxError::PendingLimit
            ))
        ));
        // Another sender is not held by att-a's backlog.
        mb.send_remote(
            &mission(),
            &sid("att-b"),
            &peers(),
            &send("k0", "att-a", "x"),
            t0(),
        )
        .unwrap();
        // One acknowledgment frees one place.
        let first = mb.envelopes(&sid("att-b")).unwrap().remove(0);
        assert!(mb
            .ack_remote(&mission(), &sid("att-b"), &first.id, t0())
            .unwrap());
        mb.send_remote(&mission(), &a, &peers(), &over, t0())
            .unwrap();
    }

    #[test]
    fn acknowledging_is_idempotent_and_refuses_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mb = PilotMailbox::new(dir.path());
        let (message, _) = mb
            .send_remote(
                &mission(),
                &sid("att-a"),
                &peers(),
                &send("k1", "att-b", "hi"),
                t0(),
            )
            .unwrap();
        assert!(mb
            .ack_remote(&mission(), &sid("att-b"), &message.id, t0())
            .unwrap());
        assert!(!mb
            .ack_remote(&mission(), &sid("att-b"), &message.id, t0())
            .unwrap());
        // The sender cannot acknowledge it: it is not in the sender's mailbox.
        assert!(matches!(
            mb.ack_remote(&mission(), &sid("att-a"), &message.id, t0()),
            Err(RemoteMailboxStoreError::Refused(
                RemoteMailboxError::UnknownEnvelope
            ))
        ));
    }
}
