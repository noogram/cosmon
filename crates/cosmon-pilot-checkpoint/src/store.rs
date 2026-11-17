// SPDX-License-Identifier: AGPL-3.0-only

//! The one place in this crate that touches a filesystem.
//!
//! Everything above is I/O-free by construction, which is what lets the whole
//! comparison be tested without a tempdir. This module is the adapter: one JSON
//! file per checkpoint under
//! `<state>/pilot/checkpoints/<mission-id>/<checkpoint-id>.json`, readable with
//! `cat` and `jq`, with no resident process owning it — the mission's v0 scope
//! forbids a broker and the directory does not need one.
//!
//! Two properties are enforced here rather than documented:
//!
//! - **Append-only.** Publishing over an existing id is
//!   [`CheckpointError::AlreadyPublished`], not a silent overwrite. A published
//!   checkpoint is what a relief pilot resumes from and what a finding cites;
//!   rewriting one changes history under a reader that already quoted it.
//! - **Atomic.** A record is written to a temporary file in the same directory
//!   and renamed into place, so a reader polling the directory never sees half
//!   a checkpoint.

use std::path::{Path, PathBuf};

use crate::checkpoint::{PilotCheckpoint, MAX_CHECKPOINT_BYTES, MAX_EVIDENCE_REFS};
use crate::error::CheckpointError;
use crate::id::{CheckpointId, MissionId, SessionId};

/// What [`CheckpointStore::publish`] did.
///
/// Returned instead of a bare path so a caller can tell a first publication
/// from an idempotent retry without a second filesystem probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    /// Where the record lives.
    pub path: PathBuf,
    /// `true` if this call wrote the record, `false` if an identical one was
    /// already there and nothing was written.
    pub written: bool,
}

/// Reads and publishes checkpoints under one root directory.
#[derive(Clone, Debug)]
pub struct CheckpointStore {
    root: PathBuf,
}

impl CheckpointStore {
    /// A store rooted at `<state_dir>/pilot/checkpoints`.
    ///
    /// Takes the cosmon state directory, not the checkpoint directory, so that
    /// a caller cannot accidentally point the store at a sibling registry.
    #[must_use]
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            root: state_dir.as_ref().join("pilot").join("checkpoints"),
        }
    }

    /// The directory holding one mission's checkpoints.
    #[must_use]
    pub fn mission_dir(&self, mission: &MissionId) -> PathBuf {
        self.root.join(mission.as_str())
    }

    /// The path a checkpoint occupies.
    #[must_use]
    pub fn path_of(&self, mission: &MissionId, id: &CheckpointId) -> PathBuf {
        self.mission_dir(mission)
            .join(format!("{}.json", id.as_str()))
    }

    /// Publish a checkpoint under its id, once.
    ///
    /// The id is the idempotency key. Publishing the same id with the same
    /// content again succeeds and writes nothing ([`Publication::written`] is
    /// `false`), so a retry after a lost response or a crash between publication
    /// and cleanup is safe. The same id with different content is refused with
    /// [`CheckpointError::AlreadyPublished`] and the first record stays as it
    /// was: a finding cites a checkpoint by id, so a published record is never
    /// rewritten. Only `created_at` may differ between an original and its
    /// retry ([`PilotCheckpoint::same_content_as`]).
    ///
    /// Before anything is written the checkpoint is bounded: at most
    /// [`MAX_EVIDENCE_REFS`] references, none of them a client path, and at
    /// most [`MAX_CHECKPOINT_BYTES`] serialised.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::TooManyEvidenceRefs`],
    /// [`CheckpointError::ClientPathEvidence`], [`CheckpointError::TooLarge`],
    /// [`CheckpointError::AlreadyPublished`] or [`CheckpointError::Io`].
    pub fn publish(&self, checkpoint: &PilotCheckpoint) -> Result<Publication, CheckpointError> {
        let count = checkpoint.all_evidence().count();
        if count > MAX_EVIDENCE_REFS {
            return Err(CheckpointError::TooManyEvidenceRefs {
                id: checkpoint.id.to_string(),
                count,
                limit: MAX_EVIDENCE_REFS,
            });
        }
        if let Some(reference) = checkpoint.all_evidence().find(|e| e.is_client_path()) {
            return Err(CheckpointError::ClientPathEvidence {
                locator: reference.locator.clone(),
            });
        }

        let body = serde_json::to_vec_pretty(checkpoint).map_err(|e| {
            CheckpointError::Digest(format!(
                "checkpoint {} could not be serialised: {e}",
                checkpoint.id
            ))
        })?;
        if body.len() as u64 > MAX_CHECKPOINT_BYTES {
            return Err(CheckpointError::TooLarge {
                subject: checkpoint.id.to_string(),
                bytes: body.len() as u64,
                limit: MAX_CHECKPOINT_BYTES,
            });
        }

        let dir = self.mission_dir(&checkpoint.mission_id);
        std::fs::create_dir_all(&dir).map_err(|source| CheckpointError::Io {
            path: dir.clone(),
            source,
        })?;
        let path = self.path_of(&checkpoint.mission_id, &checkpoint.id);

        // Same directory as the destination, so the link cannot cross a
        // filesystem boundary. A hard link fails if the target exists, which
        // makes the publication atomic *and* no-clobber: two concurrent
        // publishers of one id cannot both win.
        let tmp = dir.join(format!(".{}.json.tmp", checkpoint.id.as_str()));
        std::fs::write(&tmp, &body).map_err(|source| CheckpointError::Io {
            path: tmp.clone(),
            source,
        })?;
        let linked = std::fs::hard_link(&tmp, &path);
        std::fs::remove_file(&tmp).ok();
        match linked {
            Ok(()) => Ok(Publication {
                path,
                written: true,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = read_record(&path)?;
                if existing.same_content_as(checkpoint) {
                    Ok(Publication {
                        path,
                        written: false,
                    })
                } else {
                    Err(CheckpointError::AlreadyPublished {
                        id: checkpoint.id.to_string(),
                        path,
                    })
                }
            }
            Err(source) => Err(CheckpointError::Io { path, source }),
        }
    }

    /// Load one checkpoint by id.
    ///
    /// Returns `Ok(None)` when it was never published — absence is an ordinary
    /// answer here, and it is what [`crate::compare`] renders as
    /// `INCONCLUSIVE`.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Io`] or [`CheckpointError::Malformed`] if the record
    /// exists but cannot be read.
    pub fn load(
        &self,
        mission: &MissionId,
        id: &CheckpointId,
    ) -> Result<Option<PilotCheckpoint>, CheckpointError> {
        let path = self.path_of(mission, id);
        if !path.exists() {
            return Ok(None);
        }
        read_record(&path).map(Some)
    }

    /// Every checkpoint published for `mission`, oldest first.
    ///
    /// Ordered by `(created_at, id)`: the id breaks ties so two checkpoints
    /// written in the same second still have a total order, and the order does
    /// not depend on how the filesystem happens to enumerate the directory.
    ///
    /// # Errors
    ///
    /// [`CheckpointError::Io`] if the directory cannot be listed, or
    /// [`CheckpointError::Malformed`] if a file in it is not a checkpoint.
    pub fn list(&self, mission: &MissionId) -> Result<Vec<PilotCheckpoint>, CheckpointError> {
        let dir = self.mission_dir(mission);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let entries = std::fs::read_dir(&dir).map_err(|source| CheckpointError::Io {
            path: dir.clone(),
            source,
        })?;

        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| CheckpointError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            // Skip the in-flight temporaries of a concurrent `publish`, and
            // anything an operator dropped in the directory by hand.
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            out.push(read_record(&path)?);
        }

        out.sort_by(|x, y| {
            x.created_at
                .cmp(&y.created_at)
                .then_with(|| x.id.cmp(&y.id))
        });
        Ok(out)
    }

    /// The newest checkpoint `session` published for `mission`, if any.
    ///
    /// This is what a takeover reads: CHECKPOINT-NOT-SCROLLBACK means the
    /// relief pilot resumes from one record, not from a replay of the log.
    ///
    /// # Errors
    ///
    /// As [`CheckpointStore::list`].
    pub fn latest_for(
        &self,
        mission: &MissionId,
        session: &SessionId,
    ) -> Result<Option<PilotCheckpoint>, CheckpointError> {
        Ok(self
            .list(mission)?
            .into_iter()
            .rfind(|c| &c.session_id == session))
    }

    /// The newest checkpoint for one exact publisher session and lease epoch.
    ///
    /// The pair is intentionally inseparable: a session can regain a lease at
    /// a later epoch, and resuming its earlier hand-over would cross an
    /// authority boundary.
    ///
    /// # Errors
    ///
    /// As [`CheckpointStore::list`].
    pub fn latest_for_session_epoch(
        &self,
        mission: &MissionId,
        session: &SessionId,
        epoch: u64,
    ) -> Result<Option<PilotCheckpoint>, CheckpointError> {
        Ok(self
            .list(mission)?
            .into_iter()
            .rfind(|c| &c.session_id == session && c.lease_epoch == epoch))
    }
}

fn read_record(path: &Path) -> Result<PilotCheckpoint, CheckpointError> {
    // Measured before the read, so an oversized or swapped-in record cannot make
    // the store allocate past the contract's limit.
    let len = std::fs::metadata(path)
        .map_err(|source| CheckpointError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if len > MAX_CHECKPOINT_BYTES {
        return Err(CheckpointError::TooLarge {
            subject: path.display().to_string(),
            bytes: len,
            limit: MAX_CHECKPOINT_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|source| CheckpointError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| CheckpointError::Malformed {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("in range")
    }

    fn cp(id: &str, session: &str, secs: i64) -> PilotCheckpoint {
        PilotCheckpoint::new(id, "task-20260731-67f2", session, 1, at(secs)).expect("valid ids")
    }

    #[test]
    fn a_published_checkpoint_reads_back_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let checkpoint = cp("cp-1", "sess-a", 100);

        store.publish(&checkpoint).unwrap();
        let back = store
            .load(&checkpoint.mission_id, &checkpoint.id)
            .unwrap()
            .unwrap();
        assert_eq!(back, checkpoint);
    }

    #[test]
    fn a_legacy_v1_record_without_schema_loads_as_v1_without_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let checkpoint = cp("legacy", "sess-a", 100);
        let path = store.path_of(&checkpoint.mission_id, &checkpoint.id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut record = serde_json::to_value(&checkpoint).unwrap();
        record.as_object_mut().unwrap().remove("schema");
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

        let loaded = store
            .load(&checkpoint.mission_id, &checkpoint.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.schema, crate::CHECKPOINT_SCHEMA_V1);
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("\"schema\""),
            "loading is not a migration or rewrite"
        );
    }

    #[test]
    fn republishing_the_same_content_is_a_no_op_even_with_a_later_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let first = store.publish(&cp("cp-1", "sess-a", 100)).unwrap();
        assert!(first.written);
        let before = std::fs::read(&first.path).unwrap();

        let retry = store.publish(&cp("cp-1", "sess-a", 999)).unwrap();
        assert!(!retry.written);
        assert_eq!(before, std::fs::read(&retry.path).unwrap());
    }

    #[test]
    fn republishing_the_same_id_with_other_content_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        store.publish(&cp("cp-1", "sess-a", 100)).unwrap();

        let mut other = cp("cp-1", "sess-a", 100);
        other.open_risks.push("changed".to_owned());
        assert!(matches!(
            store.publish(&other),
            Err(CheckpointError::AlreadyPublished { .. })
        ));
    }

    #[test]
    fn a_checkpoint_citing_a_client_path_is_refused_before_anything_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mut checkpoint = cp("cp-1", "sess-a", 100);
        checkpoint
            .evidence_refs
            .push(crate::EvidenceRef::pinned("/home/me/notes.md", "ab"));
        assert!(matches!(
            store.publish(&checkpoint),
            Err(CheckpointError::ClientPathEvidence { .. })
        ));
        assert!(!store.mission_dir(&checkpoint.mission_id).exists());
    }

    #[test]
    fn latest_for_session_epoch_never_falls_back_to_another_epoch_or_session() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mission = MissionId::new("task-20260731-67f2").unwrap();
        let old = cp("old", "holder", 100);
        let mut current = cp("current", "holder", 200);
        current.lease_epoch = 2;
        let mut copilot = cp("later-copilot", "copilot", 300);
        copilot.lease_epoch = 2;
        store.publish(&old).unwrap();
        store.publish(&current).unwrap();
        store.publish(&copilot).unwrap();

        assert_eq!(
            store
                .latest_for_session_epoch(&mission, &SessionId::new("holder").unwrap(), 2)
                .unwrap()
                .unwrap()
                .id
                .as_str(),
            "current"
        );
        assert!(store
            .latest_for_session_epoch(&mission, &SessionId::new("holder").unwrap(), 3)
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_unpublished_checkpoint_is_none_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mission = MissionId::new("task-20260731-67f2").unwrap();
        let id = CheckpointId::new("never-written").unwrap();
        assert_eq!(store.load(&mission, &id).unwrap(), None);
        assert_eq!(store.list(&mission).unwrap(), Vec::new());
    }

    #[test]
    fn latest_for_picks_the_newest_of_that_session_only() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mission = MissionId::new("task-20260731-67f2").unwrap();

        store.publish(&cp("cp-1", "sess-a", 100)).unwrap();
        store.publish(&cp("cp-3", "sess-b", 300)).unwrap();
        store.publish(&cp("cp-2", "sess-a", 200)).unwrap();

        let latest = store
            .latest_for(&mission, &SessionId::new("sess-a").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(latest.id.as_str(), "cp-2");
    }

    #[test]
    fn a_stray_file_in_the_directory_is_ignored_not_parsed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let checkpoint = cp("cp-1", "sess-a", 100);
        store.publish(&checkpoint).unwrap();

        let dir = store.mission_dir(&checkpoint.mission_id);
        std::fs::write(dir.join("README.md"), "operator note").unwrap();
        std::fs::write(dir.join(".cp-9.json.tmp"), "half a record").unwrap();

        assert_eq!(store.list(&checkpoint.mission_id).unwrap().len(), 1);
    }
}
