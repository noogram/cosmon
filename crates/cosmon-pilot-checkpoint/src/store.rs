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
//! - **Append-only.** A retry with the same content leaves the record intact.
//!   Different content under the same id returns [`CheckpointError::AlreadyPublished`].
//! - **Atomic.** Each publisher writes to its own temporary file in the same
//!   directory and installs the complete record only if that id is still free.

use std::fs::File;
use std::io::{Read, Write as _};
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

        // Each publisher writes a separate inode before attempting to install it.
        let mut builder = tempfile::Builder::new();
        builder.prefix(".checkpoint-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Preserve the permissions of a normal record creation, subject to umask.
            builder.permissions(std::fs::Permissions::from_mode(0o666));
        }
        let mut tmp = builder
            .tempfile_in(&dir)
            .map_err(|source| CheckpointError::Io { path: dir, source })?;
        tmp.write_all(&body).map_err(|source| CheckpointError::Io {
            path: tmp.path().to_path_buf(),
            source,
        })?;
        match tmp.persist_noclobber(&path) {
            Ok(_) => Ok(Publication {
                path,
                written: true,
            }),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
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
            Err(e) => Err(CheckpointError::Io {
                path,
                source: e.error,
            }),
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
    let io_error = |source| CheckpointError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(io_error)?;
    let len = file.metadata().map_err(io_error)?.len();
    let bytes = read_record_bytes(path, file, len)?;
    serde_json::from_slice(&bytes).map_err(|source| CheckpointError::Malformed {
        path: path.to_path_buf(),
        source,
    })
}

fn read_record_bytes(path: &Path, source: impl Read, len: u64) -> Result<Vec<u8>, CheckpointError> {
    let too_large = |bytes| CheckpointError::TooLarge {
        subject: path.display().to_string(),
        bytes,
        limit: MAX_CHECKPOINT_BYTES,
    };
    if len > MAX_CHECKPOINT_BYTES {
        return Err(too_large(len));
    }
    // A record can grow after the metadata check. Read one extra byte to
    // distinguish an exact-limit record from a truncated oversized record.
    let mut bytes = Vec::new();
    source
        .take(MAX_CHECKPOINT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| CheckpointError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
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
    fn concurrent_publishers_keep_the_winners_content() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        for round in 0..8 {
            let id = format!("cp-concurrent-{round}");
            let candidates: Vec<_> = ('a'..='h')
                .map(|label| {
                    let mut checkpoint = cp(&id, "sess-a", 100);
                    checkpoint
                        .open_risks
                        .push(label.to_string().repeat(200_000));
                    checkpoint
                })
                .collect();
            let barrier = std::sync::Barrier::new(candidates.len());
            let results = std::thread::scope(|scope| {
                let publishers: Vec<_> = candidates
                    .iter()
                    .map(|checkpoint| {
                        let (barrier, store) = (&barrier, &store);
                        scope.spawn(move || {
                            barrier.wait();
                            store.publish(checkpoint)
                        })
                    })
                    .collect();
                publishers
                    .into_iter()
                    .map(|publisher| publisher.join().unwrap())
                    .collect::<Vec<_>>()
            });
            let mut winners = Vec::new();
            for (checkpoint, result) in candidates.iter().zip(results) {
                match result {
                    Ok(publication) if publication.written => winners.push(checkpoint),
                    Err(CheckpointError::AlreadyPublished { .. }) => {}
                    outcome => panic!("unexpected publication outcome: {outcome:?}"),
                }
            }
            let [winner] = winners.as_slice() else {
                panic!("expected exactly one publication, saw {}", winners.len());
            };
            let saved = store.load(&winner.mission_id, &winner.id).unwrap().unwrap();
            assert!(
                saved == **winner,
                "winner's record changed in round {round}"
            );
        }
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
    fn the_reference_limit_combines_checkpoint_hypothesis_and_next_action_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mut checkpoint = cp("cp-refs", "sess-a", 100);
        let reference = crate::EvidenceRef::new("artifact/notes");
        checkpoint.evidence_refs = vec![reference.clone(); 98];
        for claims in [
            &mut checkpoint.current_hypotheses,
            &mut checkpoint.intended_next_actions,
        ] {
            claims.push(
                crate::Claim::new("claim-1", "cursor", crate::Stance::Affirm, "byte offsets")
                    .unwrap()
                    .with_evidence([reference.clone()]),
            );
        }
        assert!(store.publish(&checkpoint).unwrap().written);
        checkpoint.intended_next_actions[0].evidence.push(reference);
        assert!(matches!(
            store.publish(&checkpoint),
            Err(CheckpointError::TooManyEvidenceRefs {
                count: 101,
                limit: 100,
                ..
            })
        ));
    }

    #[test]
    fn publication_and_reads_apply_the_exact_byte_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mut checkpoint = cp("cp-bound", "sess-a", 100);
        checkpoint.open_risks.push(String::new());
        let overhead = serde_json::to_vec_pretty(&checkpoint).unwrap().len();
        checkpoint.open_risks[0] = "x".repeat(262_144 - overhead);

        let publication = store.publish(&checkpoint).unwrap();
        let bytes = std::fs::read(&publication.path).unwrap();
        assert_eq!(bytes.len(), 262_144);
        assert_eq!(
            store.list(&checkpoint.mission_id).unwrap(),
            [checkpoint.clone()]
        );
        assert_eq!(
            store
                .load(&checkpoint.mission_id, &checkpoint.id)
                .unwrap()
                .as_ref(),
            Some(&checkpoint)
        );

        checkpoint.open_risks[0].push('x');
        assert!(matches!(
            store.publish(&checkpoint),
            Err(CheckpointError::TooLarge {
                bytes: 262_145,
                limit: 262_144,
                ..
            })
        ));
        assert_eq!(std::fs::read(&publication.path).unwrap(), bytes);

        // Trailing JSON whitespace is valid data but still counts toward the limit.
        let mut enlarged = bytes;
        enlarged.push(b' ');
        std::fs::write(publication.path, enlarged).unwrap();
        assert!(matches!(
            store.load(&checkpoint.mission_id, &checkpoint.id),
            Err(CheckpointError::TooLarge {
                bytes: 262_145,
                limit: 262_144,
                ..
            })
        ));
        assert!(matches!(
            store.list(&checkpoint.mission_id),
            Err(CheckpointError::TooLarge {
                bytes: 262_145,
                limit: 262_144,
                ..
            })
        ));
    }

    #[test]
    fn a_record_that_grows_after_its_size_check_is_still_refused() {
        let mut input = std::io::Cursor::new(vec![b' '; 262_146]);
        let result = read_record_bytes(Path::new("checkpoint.json"), &mut input, 262_144);
        assert!(matches!(
            result,
            Err(CheckpointError::TooLarge {
                bytes: 262_145,
                limit: 262_144,
                ..
            })
        ));
        assert_eq!(
            input.position(),
            262_145,
            "stop reading at the first excess byte"
        );
    }

    #[cfg(unix)]
    #[test]
    fn installation_io_errors_keep_the_destination_and_underlying_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let long_id = "a".repeat(512);
        let checkpoint = cp(&long_id, "sess-a", 100);
        let expected_path = store.path_of(&checkpoint.mission_id, &checkpoint.id);
        let result = store.publish(&checkpoint);
        assert!(
            matches!(result, Err(CheckpointError::Io { ref path, ref source }) if *path == expected_path && source.kind() == std::io::ErrorKind::InvalidFilename),
            "{result:?}"
        );
        assert_eq!(
            std::fs::read_dir(store.mission_dir(&checkpoint.mission_id))
                .unwrap()
                .count(),
            0
        );
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
