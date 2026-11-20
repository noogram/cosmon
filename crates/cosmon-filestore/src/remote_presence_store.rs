// SPDX-License-Identifier: AGPL-3.0-only

//! Remote presence records of the `presence-store` writer (issue #212, W7a).
//!
//! One file per bound attachment, `presence/remote/<mission>/<attachment>.json`,
//! decoded from [`CosmonPath::RemotePresence`]. The directory sits below
//! `presence/`, which [`PresenceStore::scan`] reads non-recursively, so a
//! remote attachment never appears to `gc`, patrols or the local session
//! tools as a local session with a pid.
//!
//! Every write takes the mission's exclusive lock
//! ([`CosmonPath::RemotePresenceLock`]) and reads the mission's records under
//! it, so two concurrent attaches cannot both pass the attachment cap. Reads
//! take no lock and create nothing: a listing leaves the store byte-identical.

use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_core::collaboration::AttachmentId;
use cosmon_core::id::MoleculeId;
use cosmon_core::paths::CosmonPath;
use cosmon_core::remote_presence::{
    decide_attach, decide_heartbeat, AttachDecision, AttachRequest, RemotePresence,
    RemotePresenceError,
};

use crate::presence_store::PresenceStore;

/// Why a remote presence operation did not complete.
#[derive(Debug, thiserror::Error)]
pub enum RemotePresenceStoreError {
    /// The operation is refused by the contract; nothing was written.
    #[error(transparent)]
    Refused(#[from] RemotePresenceError),
    /// A stored record fails validation; nothing is repaired silently.
    #[error("remote presence record is corrupt: {0}")]
    Corrupt(String),
    /// The store could not be read, locked or written; the outcome is unknown.
    #[error("remote presence store unavailable: {0}")]
    Io(#[from] std::io::Error),
}

impl PresenceStore {
    /// Path of one attachment's remote presence record.
    #[must_use]
    pub fn remote_presence_path(&self, mission: &MoleculeId, attachment: &AttachmentId) -> PathBuf {
        self.state_root().join(
            CosmonPath::RemotePresence {
                mission,
                attachment,
            }
            .rel(),
        )
    }

    fn remote_dir(&self, mission: &MoleculeId) -> PathBuf {
        // The record path's parent is the mission directory by construction.
        let probe = AttachmentId::from_random([0; 8]);
        let path = self.remote_presence_path(mission, &probe);
        path.parent()
            .map_or_else(|| self.dir().join("remote"), PathBuf::from)
    }

    fn remote_lock(&self, mission: &MoleculeId) -> Result<File, RemotePresenceStoreError> {
        let path = self
            .state_root()
            .join(CosmonPath::RemotePresenceLock { mission }.rel());
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

    /// Every remote presence record of `mission`.
    ///
    /// Takes no lock and creates nothing, so a read leaves the store
    /// byte-identical. Writes are atomic renames, so a record is read whole.
    ///
    /// # Errors
    /// Returns [`RemotePresenceStoreError::Corrupt`] for a record that does
    /// not parse or whose mission or attachment disagrees with its path, and
    /// [`RemotePresenceStoreError::Io`] when the directory cannot be read.
    pub fn remote_presences(
        &self,
        mission: &MoleculeId,
    ) -> Result<Vec<RemotePresence>, RemotePresenceStoreError> {
        let dir = self.remote_dir(mission);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut records = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                // Temporary siblings of an in-flight atomic write, and
                // anything else that is not a record.
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            let attachment = AttachmentId::new(stem)
                .map_err(|_| RemotePresenceStoreError::Corrupt(format!("record name {stem}")))?;
            records.push(self.read_remote(mission, &attachment)?.ok_or_else(|| {
                RemotePresenceStoreError::Corrupt(format!("record {stem} vanished"))
            })?);
        }
        records.sort_by(|a, b| a.attachment.cmp(&b.attachment));
        Ok(records)
    }

    fn read_remote(
        &self,
        mission: &MoleculeId,
        attachment: &AttachmentId,
    ) -> Result<Option<RemotePresence>, RemotePresenceStoreError> {
        let path = self.remote_presence_path(mission, attachment);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: RemotePresence = serde_json::from_slice(&bytes).map_err(|_| {
            RemotePresenceStoreError::Corrupt(format!("record {attachment} does not parse"))
        })?;
        if &record.mission != mission || &record.attachment != attachment {
            return Err(RemotePresenceStoreError::Corrupt(format!(
                "record {attachment} names another mission or attachment"
            )));
        }
        Ok(Some(record))
    }

    fn write_remote(&self, record: &RemotePresence) -> Result<(), RemotePresenceStoreError> {
        let json = serde_json::to_vec_pretty(record)
            .map_err(|error| RemotePresenceStoreError::Corrupt(error.to_string()))?;
        crate::atomic_write_durable(
            &self.remote_presence_path(&record.mission, &record.attachment),
            &json,
        )
        .map_err(|error| RemotePresenceStoreError::Io(std::io::Error::other(error.to_string())))
    }

    /// Attach `attachment` to `mission` (S-ATTACH).
    ///
    /// Returns the record and whether this call wrote it. A retry of the
    /// request that created the record writes nothing.
    ///
    /// # Errors
    /// Returns the refusal of [`decide_attach`], or a corrupt or unavailable
    /// store.
    pub fn attach_remote(
        &self,
        mission: &MoleculeId,
        attachment: &AttachmentId,
        request: AttachRequest,
        now: DateTime<Utc>,
    ) -> Result<(RemotePresence, bool), RemotePresenceStoreError> {
        let _lock = self.remote_lock(mission)?;
        let present = self.remote_presences(mission)?;
        match decide_attach(&present, mission, attachment, request, now)? {
            AttachDecision::Existing(record) => Ok((record, false)),
            AttachDecision::Create(record) => {
                self.write_remote(&record)?;
                Ok((record, true))
            }
        }
    }

    /// Record a server-observed heartbeat of an attached attachment (S-BEAT).
    ///
    /// # Errors
    /// Returns the refusal of [`decide_heartbeat`], or a corrupt or
    /// unavailable store.
    pub fn heartbeat_remote(
        &self,
        mission: &MoleculeId,
        attachment: &AttachmentId,
        now: DateTime<Utc>,
    ) -> Result<RemotePresence, RemotePresenceStoreError> {
        let _lock = self.remote_lock(mission)?;
        let record = self.read_remote(mission, attachment)?;
        let beat = decide_heartbeat(record.as_ref(), now)?;
        self.write_remote(&beat)?;
        Ok(beat)
    }

    /// Remove an attachment's remote presence record (S-DETACH).
    ///
    /// Returns `true` when a record was removed and `false` when there was
    /// none; the `false` case writes nothing, so the call is safe to repeat.
    /// Only the record goes: mailbox, checkpoints, bindings and leases are
    /// other stores and stay as they are.
    ///
    /// # Errors
    /// Returns [`RemotePresenceStoreError::Io`] when the record exists but
    /// cannot be removed.
    pub fn detach_remote(
        &self,
        mission: &MoleculeId,
        attachment: &AttachmentId,
    ) -> Result<bool, RemotePresenceStoreError> {
        let path = self.remote_presence_path(mission, attachment);
        if !path.exists() {
            return Ok(false);
        }
        let _lock = self.remote_lock(mission)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use cosmon_core::work_message::MessageKey;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000, 0).single().unwrap() + Duration::seconds(secs)
    }
    fn mission() -> MoleculeId {
        MoleculeId::new("task-20261120-aaaa").unwrap()
    }
    fn request(key: &str) -> AttachRequest {
        AttachRequest {
            idempotency_key: MessageKey::new(key).unwrap(),
            label: None,
        }
    }

    #[test]
    fn remote_records_stay_out_of_the_local_session_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        let att = AttachmentId::from_random([1; 8]);
        let (record, written) = store
            .attach_remote(&mission(), &att, request("k"), t(0))
            .unwrap();
        assert!(written);
        assert_eq!(store.remote_presences(&mission()).unwrap(), vec![record]);
        assert!(store.scan().unwrap().is_empty());
        assert_eq!(store.gc().unwrap(), 0);
        assert!(store.remote_presence_path(&mission(), &att).exists());
    }

    #[test]
    fn attach_heartbeat_detach_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        let att = AttachmentId::from_random([1; 8]);
        store
            .attach_remote(&mission(), &att, request("k"), t(0))
            .unwrap();
        let (again, written) = store
            .attach_remote(&mission(), &att, request("k"), t(5))
            .unwrap();
        assert!(!written);
        assert_eq!(again.attached_at, t(0));
        assert!(matches!(
            store.heartbeat_remote(&mission(), &att, t(3)),
            Err(RemotePresenceStoreError::Refused(
                RemotePresenceError::HeartbeatTooSoon { .. }
            ))
        ));
        assert_eq!(
            store
                .heartbeat_remote(&mission(), &att, t(10))
                .unwrap()
                .heartbeat_at,
            t(10)
        );
        assert!(store.detach_remote(&mission(), &att).unwrap());
        assert!(!store.detach_remote(&mission(), &att).unwrap());
        assert!(matches!(
            store.heartbeat_remote(&mission(), &att, t(30)),
            Err(RemotePresenceStoreError::Refused(
                RemotePresenceError::NotAttached
            ))
        ));
    }

    #[test]
    fn a_record_that_names_another_attachment_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        let a = AttachmentId::from_random([1; 8]);
        let b = AttachmentId::from_random([2; 8]);
        store
            .attach_remote(&mission(), &a, request("k"), t(0))
            .unwrap();
        fs::copy(
            store.remote_presence_path(&mission(), &a),
            store.remote_presence_path(&mission(), &b),
        )
        .unwrap();
        assert!(matches!(
            store.remote_presences(&mission()),
            Err(RemotePresenceStoreError::Corrupt(_))
        ));
    }

    /// Only "absent" reads as absent: a record or directory that exists but
    /// cannot be read or removed is an unavailable store, never an empty one.
    #[test]
    fn an_unreadable_store_is_an_error_not_an_absence() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        let att = AttachmentId::from_random([1; 8]);
        store
            .attach_remote(&mission(), &att, request("k"), t(0))
            .unwrap();
        let record = store.remote_presence_path(&mission(), &att);
        let mission_dir = record.parent().unwrap().to_path_buf();
        let mode = |path: &std::path::Path, bits| {
            fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
        };

        mode(&record, 0o000);
        let unreadable = store.heartbeat_remote(&mission(), &att, t(60));
        mode(&record, 0o644);
        assert!(
            matches!(unreadable, Err(RemotePresenceStoreError::Io(_))),
            "{unreadable:?}"
        );

        mode(&mission_dir, 0o000);
        let listing = store.remote_presences(&mission());
        mode(&mission_dir, 0o755);
        assert!(
            matches!(listing, Err(RemotePresenceStoreError::Io(_))),
            "{listing:?}"
        );

        mode(&mission_dir, 0o555);
        let detach = store.detach_remote(&mission(), &att);
        mode(&mission_dir, 0o755);
        assert!(
            matches!(detach, Err(RemotePresenceStoreError::Io(_))),
            "{detach:?}"
        );
        assert!(record.exists());
    }

    #[test]
    fn reading_an_unknown_mission_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        assert!(store.remote_presences(&mission()).unwrap().is_empty());
        assert!(!store
            .detach_remote(&mission(), &AttachmentId::from_random([1; 8]))
            .unwrap());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
