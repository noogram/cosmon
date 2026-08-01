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

use crate::checkpoint::PilotCheckpoint;
use crate::error::CheckpointError;
use crate::id::{CheckpointId, MissionId, SessionId};

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

    /// Write `checkpoint` and return where it landed.
    ///
    /// # Errors
    ///
    /// - [`CheckpointError::AlreadyPublished`] if that id already exists.
    /// - [`CheckpointError::Io`] if the directory or file cannot be written.
    pub fn publish(&self, checkpoint: &PilotCheckpoint) -> Result<PathBuf, CheckpointError> {
        let dir = self.mission_dir(&checkpoint.mission_id);
        std::fs::create_dir_all(&dir).map_err(|source| CheckpointError::Io {
            path: dir.clone(),
            source,
        })?;

        let path = self.path_of(&checkpoint.mission_id, &checkpoint.id);
        if path.exists() {
            return Err(CheckpointError::AlreadyPublished {
                id: checkpoint.id.to_string(),
                path,
            });
        }

        let body = serde_json::to_vec_pretty(checkpoint).map_err(|e| {
            CheckpointError::Digest(format!(
                "checkpoint {} could not be serialised: {e}",
                checkpoint.id
            ))
        })?;

        // Same directory as the destination, so the rename cannot cross a
        // filesystem boundary and degrade into a copy.
        let tmp = dir.join(format!(".{}.json.tmp", checkpoint.id.as_str()));
        std::fs::write(&tmp, &body).map_err(|source| CheckpointError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, &path).map_err(|source| CheckpointError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(path)
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
}

fn read_record(path: &Path) -> Result<PilotCheckpoint, CheckpointError> {
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
    fn republishing_the_same_id_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let checkpoint = cp("cp-1", "sess-a", 100);

        store.publish(&checkpoint).unwrap();
        let again = store.publish(&checkpoint);
        assert!(matches!(
            again,
            Err(CheckpointError::AlreadyPublished { .. })
        ));
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
