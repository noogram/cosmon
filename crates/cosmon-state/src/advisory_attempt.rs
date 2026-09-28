// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical filesystem custody for declared advisory attempts.
//!
//! Records and raw responses live under one owning molecule.  Writes are
//! private, locked and atomically renamed; reads never consult provider
//! history.  This adapter is deliberately independent of molecule lifecycle
//! state: its reconstruction result is evidence for an owner, not permission
//! to advance a formula or satisfy a `BlockedBy` edge.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::advisory_attempt::{
    reconstruct_attempts, AdvisoryAttemptError, AdvisoryAttemptId, AdvisoryAttemptRecord,
    AdvisoryReconstruction, AttemptAcceptance, AttemptDisposition, AttemptMutation,
    EffectiveExecution, PersistedAttemptOutput, SpawnObservation,
};
use cosmon_hash::Hash;
use serde::{Deserialize, Serialize};

/// Filesystem result for an idempotent attempt mutation.
pub type AdvisoryStoreMutation = AttemptMutation;

/// Canonical filesystem adapter rooted at an owning molecule directory.
#[derive(Debug, Clone)]
pub struct FileAdvisoryAttemptStore {
    molecule_dir: PathBuf,
}

impl FileAdvisoryAttemptStore {
    /// Bind the adapter to an explicit canonical molecule directory.
    ///
    /// Construction performs no I/O; the first mutation creates only the
    /// `advisory/` subtree.
    #[must_use]
    pub fn new(molecule_dir: PathBuf) -> Self {
        Self { molecule_dir }
    }

    fn advisory_dir(&self) -> PathBuf {
        self.molecule_dir.join("advisory")
    }

    fn attempts_dir(&self) -> PathBuf {
        self.advisory_dir().join("attempts")
    }

    fn record_path(&self, attempt: &AdvisoryAttemptId) -> PathBuf {
        self.attempts_dir()
            .join(format!("{}.json", attempt.as_str()))
    }

    fn lock_path(&self, attempt: &AdvisoryAttemptId) -> PathBuf {
        self.attempts_dir()
            .join(format!("{}.lock", attempt.as_str()))
    }

    fn output_path(&self, record: &AdvisoryAttemptRecord) -> PathBuf {
        self.molecule_dir
            .join(record.declaration.output_path.as_str())
    }

    /// Persist an intent before any provider call.
    ///
    /// Repeating the same declaration and capability observation is a no-op.
    /// Reusing an attempt id or output destination for different content is
    /// refused.
    ///
    /// # Errors
    /// Returns [`AdvisoryStoreError`] for validation, conflict or I/O failure.
    pub fn declare(
        &self,
        record: &AdvisoryAttemptRecord,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError> {
        record.declaration.validate()?;
        fs::create_dir_all(self.attempts_dir())?;
        let scope_lock = private_file(&self.advisory_dir().join("scope.lock"))?;
        fs2::FileExt::lock_exclusive(&scope_lock)?;
        let result: Result<AdvisoryStoreMutation, AdvisoryStoreError> = (|| {
            let path = self.record_path(&record.declaration.attempt_id);
            if path.exists() {
                let existing = self.load(&record.declaration.attempt_id)?.ok_or_else(|| {
                    AdvisoryStoreError::MissingRecord(record.declaration.attempt_id.clone())
                })?;
                return if existing == *record {
                    Ok(AdvisoryStoreMutation::Duplicate)
                } else {
                    Err(AdvisoryStoreError::ConflictingAttempt(
                        record.declaration.attempt_id.clone(),
                    ))
                };
            }
            for existing in self.load_all()? {
                if existing.declaration.output_path == record.declaration.output_path {
                    return Err(AdvisoryStoreError::OutputPathAlreadyClaimed(
                        record.declaration.output_path.as_str().to_owned(),
                    ));
                }
            }
            write_record_atomic(&path, record)?;
            Ok(AdvisoryStoreMutation::Applied)
        })();
        let unlock = fs2::FileExt::unlock(&scope_lock);
        let mutation = result?;
        unlock?;
        Ok(mutation)
    }

    /// Load one attempt record without creating directories or changing it.
    ///
    /// # Errors
    /// Returns an I/O or decoding error. Missing records return `Ok(None)`.
    pub fn load(
        &self,
        attempt: &AdvisoryAttemptId,
    ) -> Result<Option<AdvisoryAttemptRecord>, AdvisoryStoreError> {
        match fs::read(self.record_path(attempt)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(AdvisoryStoreError::from),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Load all canonical attempt records in deterministic id order.
    ///
    /// Directory order has no authority: records are sorted by attempt id and
    /// their explicit dispositions determine the projection.
    ///
    /// # Errors
    /// Returns an I/O or decoding error. An absent attempt directory is empty.
    pub fn load_all(&self) -> Result<Vec<AdvisoryAttemptRecord>, AdvisoryStoreError> {
        let entries = match fs::read_dir(self.attempts_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|part| part.to_str()) == Some("json") {
                paths.push(path);
            }
        }
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let bytes = fs::read(path)?;
                serde_json::from_slice(&bytes).map_err(AdvisoryStoreError::from)
            })
            .collect()
    }

    /// Persist validated effective role, model and permission settings.
    ///
    /// # Errors
    /// Returns a domain validation, record lookup or persistence error.
    pub fn record_effective_execution(
        &self,
        attempt: &AdvisoryAttemptId,
        effective: EffectiveExecution,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError> {
        self.update(attempt, move |record| {
            record.record_effective_execution(effective)
        })
    }

    /// Persist an adapter spawn observation.
    ///
    /// # Errors
    /// Returns a domain conflict, record lookup or persistence error.
    pub fn record_spawn(
        &self,
        attempt: &AdvisoryAttemptId,
        observation: SpawnObservation,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError> {
        self.update(attempt, move |record| record.record_spawn(observation))
    }

    /// Atomically publish complete UTF-8 response bytes, then bind their digest
    /// into the attempt record.
    ///
    /// The bytes-first ordering makes the crash window recoverable: a fresh
    /// process can validate the declared destination and finish the record. A
    /// different pre-existing response is never overwritten.
    ///
    /// # Errors
    /// Returns a size, UTF-8, conflict, record lookup or persistence error.
    pub fn publish_output(
        &self,
        attempt: &AdvisoryAttemptId,
        bytes: &[u8],
        persisted_at: DateTime<Utc>,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError> {
        std::str::from_utf8(bytes).map_err(|_| AdvisoryStoreError::UnreadableOutput)?;
        let lock = self.lock(attempt)?;
        let result = (|| {
            let mut record = self
                .load(attempt)?
                .ok_or_else(|| AdvisoryStoreError::MissingRecord(attempt.clone()))?;
            let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if byte_len > record.declaration.limits.max_output_bytes {
                return Err(AdvisoryAttemptError::OutputTooLarge {
                    bytes: byte_len,
                    limit: record.declaration.limits.max_output_bytes,
                }
                .into());
            }
            let digest = Hash::of_bytes(bytes);
            let output_path = self.output_path(&record);
            match fs::read(&output_path) {
                Ok(existing) if Hash::of_bytes(&existing) == digest => {}
                Ok(_) => return Err(AdvisoryStoreError::OutputConflict(attempt.clone())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    write_bytes_atomic(&output_path, bytes)?;
                }
                Err(error) => return Err(error.into()),
            }
            let mutation = record.record_output(PersistedAttemptOutput {
                digest,
                bytes: byte_len,
                persisted_at,
            })?;
            if !matches!(mutation, AdvisoryStoreMutation::Duplicate) {
                write_record_atomic(&self.record_path(attempt), &record)?;
            }
            Ok(mutation)
        })();
        let unlock = fs2::FileExt::unlock(&lock);
        let mutation = result?;
        unlock?;
        Ok(mutation)
    }

    /// Validate durable bytes and persist the owner's acceptance disposition.
    ///
    /// # Errors
    /// Refuses missing or changed bytes, invalid effective settings, unsupported
    /// capability decisions, conflicting dispositions and persistence failures.
    pub fn accept(
        &self,
        attempt: &AdvisoryAttemptId,
        acceptance: AttemptAcceptance,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError> {
        let lock = self.lock(attempt)?;
        let result = (|| {
            let mut record = self
                .load(attempt)?
                .ok_or_else(|| AdvisoryStoreError::MissingRecord(attempt.clone()))?;
            let bytes = fs::read(self.output_path(&record))
                .map_err(|_| AdvisoryStoreError::DurableOutputMissing(attempt.clone()))?;
            let observed = Hash::of_bytes(&bytes);
            if observed != acceptance.artifact_digest {
                return Err(AdvisoryStoreError::DurableOutputDigestMismatch {
                    attempt: attempt.clone(),
                    expected: acceptance.artifact_digest,
                    observed,
                });
            }
            let mutation = record.accept(acceptance)?;
            if !matches!(mutation, AdvisoryStoreMutation::Duplicate) {
                write_record_atomic(&self.record_path(attempt), &record)?;
            }
            Ok(mutation)
        })();
        let unlock = fs2::FileExt::unlock(&lock);
        let mutation = result?;
        unlock?;
        Ok(mutation)
    }

    /// Persist a missing-seat outcome without treating absence as assent.
    ///
    /// # Errors
    /// Returns a domain conflict, record lookup or persistence error.
    pub fn mark_missing(
        &self,
        attempt: &AdvisoryAttemptId,
        reason: String,
        at: DateTime<Utc>,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError> {
        self.update(attempt, move |record| record.mark_missing(reason, at))
    }

    /// Reconstruct accepted, missing and uncertain seats from canonical disk.
    ///
    /// A bytes-without-record crash window is reconciled by digesting the
    /// declared response path. Accepted metadata whose bytes are missing or
    /// changed is reported as invalid and excluded from the accepted view.
    /// Provider history and native ancestry are never consulted.
    ///
    /// # Errors
    /// Returns decoding, I/O, domain conflict or persistence errors.
    pub fn reconstruct(
        &self,
        observed_at: DateTime<Utc>,
    ) -> Result<FileAdvisoryReconstruction, AdvisoryStoreError> {
        let initial = self.load_all()?;
        for record in &initial {
            if record.output.is_none() {
                self.reconcile_published_bytes(&record.declaration.attempt_id, observed_at)?;
            }
        }
        let records = self.load_all()?;
        let mut usable = Vec::with_capacity(records.len());
        let mut invalid_artifacts = Vec::new();
        for mut record in records {
            if let AttemptDisposition::Accepted(acceptance) = &record.disposition {
                let problem = match fs::read(self.output_path(&record)) {
                    Ok(bytes) => {
                        let observed = Hash::of_bytes(&bytes);
                        (observed != acceptance.artifact_digest).then_some(
                            ArtifactIntegrityProblem::DigestMismatch {
                                attempt_id: record.declaration.attempt_id.clone(),
                                expected: acceptance.artifact_digest,
                                observed,
                            },
                        )
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        Some(ArtifactIntegrityProblem::Missing {
                            attempt_id: record.declaration.attempt_id.clone(),
                            expected: acceptance.artifact_digest,
                        })
                    }
                    Err(error) => return Err(error.into()),
                };
                if let Some(problem) = problem {
                    invalid_artifacts.push(problem);
                    record.disposition = AttemptDisposition::Pending;
                }
            }
            usable.push(record);
        }
        let seats = reconstruct_attempts(&usable)?;
        Ok(FileAdvisoryReconstruction {
            seats,
            invalid_artifacts,
        })
    }

    fn reconcile_published_bytes(
        &self,
        attempt: &AdvisoryAttemptId,
        observed_at: DateTime<Utc>,
    ) -> Result<(), AdvisoryStoreError> {
        let lock = self.lock(attempt)?;
        let result = (|| {
            let mut record = self
                .load(attempt)?
                .ok_or_else(|| AdvisoryStoreError::MissingRecord(attempt.clone()))?;
            if record.output.is_some() {
                return Ok(());
            }
            let bytes = match fs::read(self.output_path(&record)) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            std::str::from_utf8(&bytes).map_err(|_| AdvisoryStoreError::UnreadableOutput)?;
            let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            record.record_output(PersistedAttemptOutput {
                digest: Hash::of_bytes(&bytes),
                bytes: byte_len,
                persisted_at: observed_at,
            })?;
            write_record_atomic(&self.record_path(attempt), &record)
        })();
        let unlock = fs2::FileExt::unlock(&lock);
        result?;
        unlock?;
        Ok(())
    }

    fn update<F>(
        &self,
        attempt: &AdvisoryAttemptId,
        mutate: F,
    ) -> Result<AdvisoryStoreMutation, AdvisoryStoreError>
    where
        F: FnOnce(
            &mut AdvisoryAttemptRecord,
        ) -> Result<AdvisoryStoreMutation, AdvisoryAttemptError>,
    {
        let lock = self.lock(attempt)?;
        let result: Result<AdvisoryStoreMutation, AdvisoryStoreError> = (|| {
            let mut record = self
                .load(attempt)?
                .ok_or_else(|| AdvisoryStoreError::MissingRecord(attempt.clone()))?;
            let mutation = mutate(&mut record)?;
            if matches!(
                mutation,
                AdvisoryStoreMutation::Applied | AdvisoryStoreMutation::AppliedLate
            ) {
                write_record_atomic(&self.record_path(attempt), &record)?;
            }
            Ok(mutation)
        })();
        let unlock = fs2::FileExt::unlock(&lock);
        let mutation = result?;
        unlock?;
        Ok(mutation)
    }

    fn lock(&self, attempt: &AdvisoryAttemptId) -> Result<File, AdvisoryStoreError> {
        fs::create_dir_all(self.attempts_dir())?;
        let lock = private_file(&self.lock_path(attempt))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        Ok(lock)
    }
}

/// Parent-loss reconstruction result from verified canonical bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAdvisoryReconstruction {
    /// Accepted, missing and uncertain seat projection.
    pub seats: AdvisoryReconstruction,
    /// Previously accepted metadata whose proof bytes are absent or changed.
    pub invalid_artifacts: Vec<ArtifactIntegrityProblem>,
}

/// Integrity problem that prevents reuse of an accepted disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactIntegrityProblem {
    /// Accepted bytes no longer exist.
    Missing {
        /// Affected attempt.
        attempt_id: AdvisoryAttemptId,
        /// Digest recorded at acceptance.
        expected: Hash,
    },
    /// Present bytes no longer match the accepted digest.
    DigestMismatch {
        /// Affected attempt.
        attempt_id: AdvisoryAttemptId,
        /// Digest recorded at acceptance.
        expected: Hash,
        /// Digest reconstructed from current bytes.
        observed: Hash,
    },
}

/// Persistence and integrity failures for the advisory-attempt adapter.
#[derive(Debug, thiserror::Error)]
pub enum AdvisoryStoreError {
    /// Filesystem operation failed.
    #[error("advisory attempt I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Canonical record could not be decoded.
    #[error("advisory attempt record is malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// Pure contract validation failed.
    #[error(transparent)]
    Domain(#[from] AdvisoryAttemptError),
    /// Requested record does not exist.
    #[error("advisory attempt record {0} is missing")]
    MissingRecord(AdvisoryAttemptId),
    /// Existing record reused the id for different intent content.
    #[error("advisory attempt id {0} is already bound to different content")]
    ConflictingAttempt(AdvisoryAttemptId),
    /// Two attempts declared the same canonical raw-response destination.
    #[error("advisory output path is already claimed: {0}")]
    OutputPathAlreadyClaimed(String),
    /// Existing response bytes differ from the attempted publication.
    #[error("advisory attempt {0} already has different response bytes")]
    OutputConflict(AdvisoryAttemptId),
    /// Response bytes were not readable UTF-8.
    #[error("advisory response is not readable UTF-8")]
    UnreadableOutput,
    /// Acceptance referenced a response absent from canonical custody.
    #[error("durable output for advisory attempt {0} is missing")]
    DurableOutputMissing(AdvisoryAttemptId),
    /// Durable bytes changed between publication and acceptance.
    #[error("durable output digest mismatch for advisory attempt {attempt}: expected {expected}, observed {observed}")]
    DurableOutputDigestMismatch {
        /// Affected attempt.
        attempt: AdvisoryAttemptId,
        /// Acceptance digest.
        expected: Hash,
        /// Current bytes digest.
        observed: Hash,
    },
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_record_atomic(
    path: &Path,
    record: &AdvisoryAttemptRecord,
) -> Result<(), AdvisoryStoreError> {
    let mut bytes = serde_json::to_vec_pretty(record)?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes)?;
    Ok(())
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("advisory path has no parent"))?;
    fs::create_dir_all(parent)?;
    let pending = path.with_extension("pending");
    let mut file = private_file(&pending)?;
    file.set_len(0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&pending, path)?;
    File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use cosmon_core::advisory_attempt::{
        AdvisoryArtifactPath, AdvisoryAttemptDeclaration, AdvisoryAttemptLimits, AdvisoryFallback,
        AdvisoryObservation, AdvisorySeatId, AdvisoryUnavailableReason, AttemptProvenance,
        EffectivePermissionEnvelope, NativeAdvisoryProtocol, NativeAdvisoryTool,
        NativeCapabilityObservation, NativeCapabilityRequest, PermissionEnforcement,
        RequestedExecution, RequestedPermissionEnvelope, ADVISORY_ATTEMPT_SCHEMA_VERSION,
    };
    use tempfile::tempdir;

    use super::*;

    fn observed<T>(value: T) -> AdvisoryObservation<T> {
        AdvisoryObservation::Observed {
            value,
            source: "fixture.v1".to_owned(),
        }
    }

    fn record(attempt: &str, seat: &str, output: &str) -> AdvisoryAttemptRecord {
        let declaration = AdvisoryAttemptDeclaration {
            schema_version: ADVISORY_ATTEMPT_SCHEMA_VERSION,
            attempt_id: AdvisoryAttemptId::new(attempt).unwrap(),
            seat_id: AdvisorySeatId::new(seat).unwrap(),
            required: true,
            assignment_digest: Hash::of_bytes(b"assignment"),
            provenance: AttemptProvenance {
                source_revision: "source-revision".to_owned(),
                input_digest: Hash::of_bytes(b"input"),
                scope_revision: Hash::of_bytes(b"scope"),
            },
            output_path: AdvisoryArtifactPath::new(output).unwrap(),
            requested_execution: RequestedExecution {
                role: "reviewer".to_owned(),
                model: "model-a".to_owned(),
                permissions: RequestedPermissionEnvelope {
                    allowed: BTreeSet::from(["read_sources".to_owned()]),
                    denied: BTreeSet::from(["lifecycle".to_owned()]),
                },
            },
            capability_request: NativeCapabilityRequest {
                protocol: NativeAdvisoryProtocol::V2,
                required_tools: BTreeSet::from([NativeAdvisoryTool::Spawn]),
                fallback: AdvisoryFallback::SeparateMolecule,
            },
            limits: AdvisoryAttemptLimits {
                max_attempts: 1,
                max_concurrency: 1,
                deadline: DateTime::from_timestamp(2_000_000_000, 0).unwrap(),
                max_output_bytes: 4096,
                max_messages: 4,
                max_message_bytes: 4096,
                max_discussion_rounds: 1,
            },
            recorded_at: DateTime::from_timestamp(1_900_000_000, 0).unwrap(),
        };
        AdvisoryAttemptRecord::new(
            declaration,
            NativeCapabilityObservation {
                binary_version: observed("0.157.1".to_owned()),
                realized_model: observed("model-a".to_owned()),
                feature_flags: observed(BTreeMap::from([("multi_agent_v2".to_owned(), true)])),
                tools: observed(BTreeSet::from([NativeAdvisoryTool::Spawn])),
                protocol: observed(NativeAdvisoryProtocol::V2),
            },
        )
        .unwrap()
    }

    fn effective() -> EffectiveExecution {
        EffectiveExecution {
            role: observed("reviewer".to_owned()),
            model: observed("model-a".to_owned()),
            permissions: observed(EffectivePermissionEnvelope {
                allowed: BTreeSet::from(["read_sources".to_owned()]),
                denied: BTreeSet::from(["lifecycle".to_owned()]),
                enforcement: PermissionEnforcement::EffectBoundary,
            }),
        }
    }

    fn acceptance(digest: Hash) -> AttemptAcceptance {
        AttemptAcceptance {
            artifact_digest: digest,
            accepted_by: "owner".to_owned(),
            checks: vec!["digest and rubric".to_owned()],
            limitations: Vec::new(),
            accepted_at: Utc::now(),
        }
    }

    #[test]
    fn intent_is_idempotent_and_output_paths_are_unique() {
        let dir = tempdir().unwrap();
        let store = FileAdvisoryAttemptStore::new(dir.path().to_owned());
        let first = record(
            "architect-1",
            "architect",
            "advisory/responses/architect-1.md",
        );
        assert_eq!(
            store.declare(&first).unwrap(),
            AdvisoryStoreMutation::Applied
        );
        assert_eq!(
            store.declare(&first).unwrap(),
            AdvisoryStoreMutation::Duplicate
        );
        let duplicate_path = record("turing-1", "turing", "advisory/responses/architect-1.md");
        assert!(matches!(
            store.declare(&duplicate_path).unwrap_err(),
            AdvisoryStoreError::OutputPathAlreadyClaimed(_)
        ));
    }

    #[test]
    fn parent_loss_reconciles_bytes_saved_before_record_update() {
        let dir = tempdir().unwrap();
        let store = FileAdvisoryAttemptStore::new(dir.path().to_owned());
        let record = record(
            "architect-1",
            "architect",
            "advisory/responses/architect-1.md",
        );
        let attempt = record.declaration.attempt_id.clone();
        store.declare(&record).unwrap();
        let output = dir.path().join(record.declaration.output_path.as_str());
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"complete response").unwrap();

        let rebuilt = store.reconstruct(Utc::now()).unwrap();
        assert!(rebuilt
            .seats
            .missing_required
            .contains(&record.declaration.seat_id));
        assert!(store.load(&attempt).unwrap().unwrap().output.is_some());
    }

    #[test]
    fn accepted_bytes_survive_fresh_store_reconstruction() {
        let dir = tempdir().unwrap();
        let attempt_record = record(
            "architect-1",
            "architect",
            "advisory/responses/architect-1.md",
        );
        let attempt = attempt_record.declaration.attempt_id.clone();
        let seat = attempt_record.declaration.seat_id.clone();
        let store = FileAdvisoryAttemptStore::new(dir.path().to_owned());
        store.declare(&attempt_record).unwrap();
        store
            .record_effective_execution(&attempt, effective())
            .unwrap();
        store
            .record_spawn(&attempt, SpawnObservation::Acknowledged { at: Utc::now() })
            .unwrap();
        store
            .publish_output(&attempt, b"complete response", Utc::now())
            .unwrap();
        let digest = Hash::of_bytes(b"complete response");
        store.accept(&attempt, acceptance(digest)).unwrap();

        let fresh = FileAdvisoryAttemptStore::new(dir.path().to_owned());
        let rebuilt = fresh.reconstruct(Utc::now()).unwrap();
        assert_eq!(rebuilt.seats.accepted.get(&seat), Some(&digest));
        assert!(rebuilt.seats.missing_required.is_empty());
        assert!(rebuilt.invalid_artifacts.is_empty());
    }

    #[test]
    fn deleted_accepted_output_becomes_missing_with_integrity_finding() {
        let dir = tempdir().unwrap();
        let attempt_record = record(
            "architect-1",
            "architect",
            "advisory/responses/architect-1.md",
        );
        let attempt = attempt_record.declaration.attempt_id.clone();
        let seat = attempt_record.declaration.seat_id.clone();
        let store = FileAdvisoryAttemptStore::new(dir.path().to_owned());
        store.declare(&attempt_record).unwrap();
        store
            .record_effective_execution(&attempt, effective())
            .unwrap();
        store
            .publish_output(&attempt, b"complete response", Utc::now())
            .unwrap();
        let digest = Hash::of_bytes(b"complete response");
        store.accept(&attempt, acceptance(digest)).unwrap();
        fs::remove_file(
            dir.path()
                .join(attempt_record.declaration.output_path.as_str()),
        )
        .unwrap();

        let rebuilt = store.reconstruct(Utc::now()).unwrap();
        assert!(!rebuilt.seats.accepted.contains_key(&seat));
        assert!(rebuilt.seats.missing_required.contains(&seat));
        assert!(matches!(
            rebuilt.invalid_artifacts.as_slice(),
            [ArtifactIntegrityProblem::Missing { .. }]
        ));
    }

    #[test]
    fn unavailable_observations_are_serialized_explicitly() {
        let value: AdvisoryObservation<String> = AdvisoryObservation::Unavailable {
            reason: AdvisoryUnavailableReason::NotObserved,
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("not_observed"));
    }
}
