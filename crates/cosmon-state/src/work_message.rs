// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem custody for messages inside a declared work.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::work_message::{
    admit, fold, AdapterCapability, Admission, Envelope, MessageKey, ObserverId, PutOutcome,
    Receipt, ScopeRevision, Stage, Submission, WorkMessageError, WorkMessageStore, WorkProjection,
    WorkRecords, WorkScope,
};
use cosmon_hash::Hash;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Canonical filesystem adapter rooted at the owning molecule directory.
#[derive(Debug, Clone)]
pub struct FileWorkMessageStore {
    owner_dir: PathBuf,
}

/// Result of rebuilding the work from its canonical directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkReconstruction {
    /// Folded stages, if a scope exists.
    pub projection: Option<WorkProjection>,
    /// Payload defects that the pure fold cannot see.
    pub findings: Vec<PayloadIntegrityFinding>,
}

/// A payload custody defect, kept visible during reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadIntegrityFinding {
    /// An envelope's content address has no file.
    Missing {
        /// Envelope whose payload is missing.
        key: MessageKey,
        /// Admitted content address.
        expected: Hash,
    },
    /// The file differs in digest or length from its envelope.
    DigestMismatch {
        /// Envelope whose payload changed.
        key: MessageKey,
        /// Admitted content address.
        expected: Hash,
        /// Digest of current bytes.
        observed: Hash,
    },
    /// The payload path is a symlink or another non-regular file.
    UnsafePayload {
        /// Envelope with an unsafe payload path.
        key: MessageKey,
    },
    /// A payload was published but no envelope references it.
    OrphanPayload {
        /// Filename of the unreferenced payload.
        digest: String,
    },
}

/// Filesystem, decoding and domain errors from the adapter.
#[derive(Debug, thiserror::Error)]
pub enum WorkStoreError {
    /// Filesystem I/O failed.
    #[error("work message I/O failed: {0}")]
    Io(#[from] io::Error),
    /// A canonical JSON record was malformed.
    #[error("work message JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// The pure contract refused the operation.
    #[error(transparent)]
    Domain(#[from] WorkMessageError),
    /// A submission has no declared scope.
    #[error("work scope is not declared")]
    NoScope,
    /// Supplied bytes differ from the declared content address or length.
    #[error("payload does not match its declared digest or byte length")]
    PayloadMismatch,
    /// An existing content-addressed file is corrupt.
    #[error("existing payload differs from its content address")]
    ExistingPayloadMismatch,
    /// A create-only key already names different metadata.
    #[error("message key {0} is already bound to another envelope")]
    EnvelopeConflict(MessageKey),
    /// A test injected a failure after writing the payload.
    #[cfg(test)]
    #[error("injected failure")]
    Injected,
}

impl FileWorkMessageStore {
    /// Bind to an explicit owner molecule directory without touching disk.
    #[must_use]
    pub fn new(owner_dir: PathBuf) -> Self {
        Self { owner_dir }
    }

    fn work_dir(&self) -> PathBuf {
        self.owner_dir.join("work")
    }
    fn payload_path(&self, digest: &Hash) -> PathBuf {
        self.work_dir().join("payloads").join(digest.to_hex())
    }
    fn envelope_path(&self, key: &MessageKey) -> PathBuf {
        self.work_dir()
            .join("envelopes")
            .join(format!("{key}.json"))
    }
    fn receipt_path(&self, key: &MessageKey) -> PathBuf {
        self.work_dir()
            .join("receipts")
            .join(format!("{key}.jsonl"))
    }

    pub(crate) fn lock(&self) -> Result<File, WorkStoreError> {
        private_dir(&self.work_dir())?;
        reject_symlink(&self.work_dir().join("work.lock"))?;
        let file = private_options()
            .read(true)
            .write(true)
            .create(true)
            .open(self.work_dir().join("work.lock"))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    }

    /// Declare the current scope and preserve its immutable revision.
    ///
    /// # Errors
    /// Returns a domain, serialization or filesystem error.
    pub fn declare(&self, scope: &WorkScope) -> Result<ScopeRevision, WorkStoreError> {
        scope.validate()?;
        let revision = scope.revision()?;
        let lock = self.lock()?;
        let result: Result<ScopeRevision, WorkStoreError> = (|| {
            let bytes = json_line(scope)?;
            let path = self
                .work_dir()
                .join("scopes")
                .join(format!("{revision}.json"));
            write_create_only(&path, &bytes)?;
            write_atomic(&self.work_dir().join("scope.json"), &bytes)?;
            Ok(revision)
        })();
        let unlock = fs2::FileExt::unlock(&lock);
        let revision = result?;
        unlock?;
        Ok(revision)
    }

    /// Admit a message while holding the work lock across re-fold and commit.
    ///
    /// Payload bytes are synced before an envelope can become visible. The
    /// envelope's create-only link is the idempotency point. A retry after a
    /// crash may finish the missing admission receipt.
    ///
    /// # Errors
    /// Returns a domain refusal, integrity defect or filesystem error.
    pub fn submit(
        &self,
        submission: Submission,
        bytes: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Admission, WorkStoreError> {
        self.submit_inner(submission, bytes, now, |_| Ok(()))
    }

    #[cfg(test)]
    fn submit_with_hook<F: FnMut(SubmissionWriteStage) -> Result<(), WorkStoreError>>(
        &self,
        submission: Submission,
        bytes: &[u8],
        now: DateTime<Utc>,
        hook: F,
    ) -> Result<Admission, WorkStoreError> {
        self.submit_inner(submission, bytes, now, hook)
    }

    fn submit_inner<F: FnMut(SubmissionWriteStage) -> Result<(), WorkStoreError>>(
        &self,
        submission: Submission,
        bytes: &[u8],
        now: DateTime<Utc>,
        mut hook: F,
    ) -> Result<Admission, WorkStoreError> {
        if Hash::of_bytes(bytes) != submission.payload_digest
            || u64::try_from(bytes.len()).ok() != Some(submission.payload_bytes)
        {
            return Err(WorkStoreError::PayloadMismatch);
        }
        let lock = self.lock()?;
        let result = (|| {
            let records = self.load_all()?;
            let scope = records.scope.ok_or(WorkStoreError::NoScope)?;
            let projection = fold(&scope, &records.envelopes, &records.receipts, now)?;
            let decision = admit(&scope, &projection, submission, now)?;
            match &decision {
                Admission::Admit(envelope) => {
                    self.put_payload(&envelope.payload_digest, bytes)?;
                    hook(SubmissionWriteStage::AfterPayload)?;
                    match self.put_envelope(envelope)? {
                        PutOutcome::Created => {}
                        PutOutcome::AlreadyPresent => {
                            return Err(WorkStoreError::EnvelopeConflict(envelope.key.clone()))
                        }
                    }
                    hook(SubmissionWriteStage::AfterEnvelope)?;
                    hook(SubmissionWriteStage::BeforeReceipt)?;
                    self.append_receipt(&Receipt::for_envelope(
                        envelope,
                        ObserverId::Boundary,
                        now,
                        Stage::Admitted,
                    ))?;
                }
                Admission::Duplicate(envelope) => {
                    self.verify_payload(envelope)?;
                    if !records
                        .receipts
                        .iter()
                        .any(|r| r.key == envelope.key && r.stage == Stage::Admitted)
                    {
                        self.append_receipt(&Receipt::for_envelope(
                            envelope,
                            ObserverId::Boundary,
                            envelope.admitted_at,
                            Stage::Admitted,
                        ))?;
                    }
                }
            }
            Ok(decision)
        })();
        let unlock = fs2::FileExt::unlock(&lock);
        let decision = result?;
        unlock?;
        Ok(decision)
    }

    /// Read and verify one envelope's payload by its digest-derived path.
    ///
    /// # Errors
    /// Returns an integrity or I/O error for missing, changed or unsafe files.
    pub fn read_payload(&self, envelope: &Envelope) -> Result<Vec<u8>, WorkStoreError> {
        reject_symlink(&self.work_dir())?;
        reject_symlink(&self.work_dir().join("payloads"))?;
        let path = self.payload_path(&envelope.payload_digest);
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() {
            return Err(WorkStoreError::ExistingPayloadMismatch);
        }
        let bytes = fs::read(path)?;
        if Hash::of_bytes(&bytes) != envelope.payload_digest
            || bytes.len() as u64 != envelope.payload_bytes
        {
            return Err(WorkStoreError::ExistingPayloadMismatch);
        }
        Ok(bytes)
    }

    fn verify_payload(&self, envelope: &Envelope) -> Result<(), WorkStoreError> {
        self.read_payload(envelope).map(|_| ())
    }

    pub(crate) fn put_note(&self, digest: &Hash, bytes: &[u8]) -> Result<(), WorkStoreError> {
        if Hash::of_bytes(bytes) != *digest {
            return Err(WorkStoreError::PayloadMismatch);
        }
        let path = self.work_dir().join("notes").join(digest.to_hex());
        if !write_create_only(&path, bytes)? {
            reject_symlink(&path)?;
            if Hash::of_bytes(&fs::read(path)?) != *digest {
                return Err(WorkStoreError::ExistingPayloadMismatch);
            }
        }
        Ok(())
    }

    /// Rebuild stages from disk and report missing, changed and orphan bytes.
    ///
    /// The method uses only the owner's `work/` subtree. Payload locations are
    /// derived from recorded digests, never followed from envelope path text.
    ///
    /// # Errors
    /// Returns an I/O, JSON or domain error if canonical records cannot be read.
    pub fn reconstruct(&self, now: DateTime<Utc>) -> Result<WorkReconstruction, WorkStoreError> {
        let records = self.load_all()?;
        let projection = records
            .scope
            .as_ref()
            .map(|scope| fold(scope, &records.envelopes, &records.receipts, now))
            .transpose()?;
        let mut findings = Vec::new();
        let mut referenced = BTreeSet::new();
        for envelope in &records.envelopes {
            referenced.insert(envelope.payload_digest.to_hex());
            let path = self.payload_path(&envelope.payload_digest);
            reject_symlink(&self.work_dir().join("payloads"))?;
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    findings.push(PayloadIntegrityFinding::Missing {
                        key: envelope.key.clone(),
                        expected: envelope.payload_digest,
                    });
                }
                Err(error) => return Err(error.into()),
                Ok(meta) if !meta.file_type().is_file() => {
                    findings.push(PayloadIntegrityFinding::UnsafePayload {
                        key: envelope.key.clone(),
                    });
                }
                Ok(_) => {
                    let bytes = fs::read(path)?;
                    let observed = Hash::of_bytes(&bytes);
                    if observed != envelope.payload_digest
                        || bytes.len() as u64 != envelope.payload_bytes
                    {
                        findings.push(PayloadIntegrityFinding::DigestMismatch {
                            key: envelope.key.clone(),
                            expected: envelope.payload_digest,
                            observed,
                        });
                    }
                }
            }
        }
        for path in json_paths(&self.work_dir().join("payloads"), None)? {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !referenced.contains(name) {
                findings.push(PayloadIntegrityFinding::OrphanPayload {
                    digest: name.to_owned(),
                });
            }
        }
        Ok(WorkReconstruction {
            projection,
            findings,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmissionWriteStage {
    AfterPayload,
    AfterEnvelope,
    BeforeReceipt,
}

impl WorkMessageStore for FileWorkMessageStore {
    type Error = WorkStoreError;

    fn load_scope(&self) -> Result<Option<WorkScope>, Self::Error> {
        reject_symlink(&self.work_dir())?;
        read_optional(&self.work_dir().join("scope.json"))
    }

    fn put_payload(&self, digest: &Hash, bytes: &[u8]) -> Result<(), Self::Error> {
        if Hash::of_bytes(bytes) != *digest {
            return Err(WorkStoreError::PayloadMismatch);
        }
        let path = self.payload_path(digest);
        if !write_create_only(&path, bytes)? {
            reject_symlink(&path)?;
            let existing = fs::read(&path)?;
            if Hash::of_bytes(&existing) != *digest {
                return Err(WorkStoreError::ExistingPayloadMismatch);
            }
        }
        Ok(())
    }

    fn put_envelope(&self, envelope: &Envelope) -> Result<PutOutcome, Self::Error> {
        self.verify_payload(envelope)?;
        let bytes = json_line(envelope)?;
        if write_create_only(&self.envelope_path(&envelope.key), &bytes)? {
            Ok(PutOutcome::Created)
        } else {
            let existing: Envelope = read_optional(&self.envelope_path(&envelope.key))?
                .ok_or_else(|| io::Error::other("envelope disappeared"))?;
            if existing == *envelope {
                Ok(PutOutcome::AlreadyPresent)
            } else {
                Err(WorkStoreError::EnvelopeConflict(envelope.key.clone()))
            }
        }
    }

    fn append_receipt(&self, receipt: &Receipt) -> Result<(), Self::Error> {
        append_json_line(&self.receipt_path(&receipt.key), receipt)
    }

    fn record_capability(&self, capability: &AdapterCapability) -> Result<(), Self::Error> {
        let path = self
            .work_dir()
            .join("capabilities")
            .join(format!("{}.jsonl", capability.seat.as_str()));
        append_json_line(&path, capability)
    }

    fn load_all(&self) -> Result<WorkRecords, Self::Error> {
        let scope = self.load_scope()?;
        let envelopes = json_paths(&self.work_dir().join("envelopes"), Some("json"))?
            .into_iter()
            .map(|path| read_json(&path))
            .collect::<Result<Vec<_>, _>>()?;
        let mut receipts = Vec::new();
        for path in json_paths(&self.work_dir().join("receipts"), Some("jsonl"))? {
            receipts.extend(read_json_lines(&path)?);
        }
        let mut capabilities = Vec::new();
        for path in json_paths(&self.work_dir().join("capabilities"), Some("jsonl"))? {
            capabilities.extend(read_json_lines(&path)?);
        }
        Ok(WorkRecords {
            scope,
            envelopes,
            receipts,
            capabilities,
        })
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    reject_symlink(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn json_line<T: Serialize>(value: &T) -> Result<Vec<u8>, WorkStoreError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_create_only(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    private_dir(parent)?;
    let pending = parent.join(format!(
        ".pending-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| {
        let mut file = private_options()
            .write(true)
            .create_new(true)
            .open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let created = match fs::hard_link(&pending, path) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                reject_symlink(path)?;
                false
            }
            Err(error) => return Err(error),
        };
        File::open(parent)?.sync_all()?;
        Ok(created)
    })();
    let _ = fs::remove_file(pending);
    result
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    private_dir(parent)?;
    let pending = parent.join(format!(
        ".pending-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| {
        let mut file = private_options()
            .write(true)
            .create_new(true)
            .open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&pending, path)?;
        File::open(parent)?.sync_all()
    })();
    let _ = fs::remove_file(pending);
    result
}

fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<(), WorkStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    private_dir(parent)?;
    reject_symlink(path)?;
    let mut file = private_options().append(true).create(true).open(path)?;
    file.write_all(&json_line(value)?)?;
    file.sync_all()?;
    Ok(())
}

fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, WorkStoreError> {
    reject_symlink(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, WorkStoreError> {
    reject_symlink(path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_json_lines<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, WorkStoreError> {
    reject_symlink(path)?;
    fs::read(path)?
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).map_err(WorkStoreError::from))
        .collect()
}

fn json_paths(dir: &Path, extension: Option<&str>) -> io::Result<Vec<PathBuf>> {
    reject_symlink(dir)?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if extension.is_some() {
            reject_symlink(&path)?;
        }
        if extension.is_none_or(|suffix| path.extension().and_then(|s| s.to_str()) == Some(suffix))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(io::Error::other("work record is a symlink"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use cosmon_core::advisory_attempt::AdvisorySeatId;
    use cosmon_core::id::MoleculeId;
    use cosmon_core::work_message::{
        Confidentiality, MessageBudget, MessageKey, SeatDecl, SenderEvidence, Submission,
        WorkScope, WORK_MESSAGE_SCHEMA_VERSION,
    };
    use cosmon_hash::Hash;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn time() -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(1_000, 0).single().unwrap()
    }
    fn seat(name: &str) -> AdvisorySeatId {
        AdvisorySeatId::new(name).unwrap()
    }
    fn scope(limit: u32) -> WorkScope {
        WorkScope {
            schema_version: WORK_MESSAGE_SCHEMA_VERSION,
            owner: MoleculeId::new("task-20260928-0000").unwrap(),
            seats: BTreeMap::from([
                (
                    seat("a"),
                    SeatDecl {
                        molecule: MoleculeId::new("task-20260928-aaaa").unwrap(),
                        required: true,
                        provider_requirement: None,
                    },
                ),
                (
                    seat("b"),
                    SeatDecl {
                        molecule: MoleculeId::new("task-20260928-bbbb").unwrap(),
                        required: true,
                        provider_requirement: None,
                    },
                ),
            ]),
            budget: MessageBudget {
                max_payload_bytes: 100,
                max_messages_per_seat: limit,
                max_bytes_per_seat: 500,
                default_ttl_secs: 3600,
                redeliver_after_secs: 60,
                max_delivery_attempts: 2,
            },
            declared_at: time(),
        }
    }
    fn submission(scope: &WorkScope, key: &str, bytes: &[u8]) -> Submission {
        Submission {
            scope_owner: scope.owner.clone(),
            scope_revision: scope.revision().unwrap(),
            sender: seat("a"),
            recipient: seat("b"),
            key: Some(MessageKey::new(key).unwrap()),
            payload_digest: Hash::of_bytes(bytes),
            payload_bytes: bytes.len() as u64,
            sender_time: time(),
            reply_to: None,
            phase: None,
            confidentiality: Confidentiality::Internal,
            ttl_secs: None,
            sender_evidence: SenderEvidence::CallerEnvSameUid,
        }
    }
    fn setup(limit: u32) -> (tempfile::TempDir, FileWorkMessageStore, WorkScope) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileWorkMessageStore::new(dir.path().to_path_buf());
        let scope = scope(limit);
        store.declare(&scope).unwrap();
        (dir, store, scope)
    }
    #[test]
    fn envelope_never_exists_without_payload() {
        let (_dir, store, scope) = setup(2);
        let result = store.submit_with_hook(
            submission(&scope, "m1", b"finding"),
            b"finding",
            time(),
            |_| Err(WorkStoreError::Injected),
        );
        assert!(matches!(result, Err(WorkStoreError::Injected)));
        assert!(store.load_all().unwrap().envelopes.is_empty());
        let recovered = store.reconstruct(time()).unwrap();
        assert!(recovered
            .findings
            .iter()
            .any(|f| matches!(f, PayloadIntegrityFinding::OrphanPayload { .. })));
    }
    #[test]
    fn retry_repairs_each_submission_crash_window_once() {
        for fault in [
            SubmissionWriteStage::AfterPayload,
            SubmissionWriteStage::AfterEnvelope,
            SubmissionWriteStage::BeforeReceipt,
        ] {
            let (_dir, store, scope) = setup(2);
            let failed = store.submit_with_hook(
                submission(&scope, "m1", b"finding"),
                b"finding",
                time(),
                |stage| {
                    if stage == fault {
                        Err(WorkStoreError::Injected)
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(matches!(failed, Err(WorkStoreError::Injected)), "{fault:?}");
            store
                .submit(submission(&scope, "m1", b"finding"), b"finding", time())
                .unwrap();
            let records = store.load_all().unwrap();
            assert_eq!(records.envelopes.len(), 1, "{fault:?}");
            assert_eq!(
                records
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.stage == Stage::Admitted)
                    .count(),
                1,
                "{fault:?}"
            );
        }
    }
    #[test]
    fn concurrent_submits_of_one_key_create_one_envelope() {
        let (_dir, store, scope) = setup(4);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let scope = scope.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    store.submit(submission(&scope, "m1", b"finding"), b"finding", time())
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, cosmon_core::work_message::Admission::Admit(_)))
                .count(),
            1
        );
        assert_eq!(store.load_all().unwrap().envelopes.len(), 1);
    }
    #[test]
    fn concurrent_submits_respect_the_seat_budget() {
        let (_dir, store, scope) = setup(2);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                let scope = scope.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let key = format!("m{i}");
                    barrier.wait();
                    store.submit(
                        submission(&scope, &key, key.as_bytes()),
                        key.as_bytes(),
                        time(),
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 2);
        assert_eq!(store.load_all().unwrap().envelopes.len(), 2);
    }
    #[test]
    fn fresh_store_reconstructs_the_same_projection() {
        let (dir, store, scope) = setup(2);
        store
            .submit(submission(&scope, "m1", b"finding"), b"finding", time())
            .unwrap();
        let before = store.reconstruct(time()).unwrap();
        let fresh = FileWorkMessageStore::new(dir.path().to_path_buf());
        assert_eq!(before, fresh.reconstruct(time()).unwrap());
        assert!(before.findings.is_empty());
    }
    #[test]
    fn deleted_payload_is_an_integrity_finding() {
        let (_dir, store, scope) = setup(2);
        store
            .submit(submission(&scope, "m1", b"finding"), b"finding", time())
            .unwrap();
        std::fs::remove_file(store.payload_path(&Hash::of_bytes(b"finding"))).unwrap();
        assert!(store
            .reconstruct(time())
            .unwrap()
            .findings
            .iter()
            .any(|f| matches!(f, PayloadIntegrityFinding::Missing { .. })));
    }
    #[test]
    fn tampered_payload_is_an_integrity_finding() {
        let (_dir, store, scope) = setup(2);
        store
            .submit(submission(&scope, "m1", b"finding"), b"finding", time())
            .unwrap();
        std::fs::write(store.payload_path(&Hash::of_bytes(b"finding")), b"changed").unwrap();
        assert!(store
            .reconstruct(time())
            .unwrap()
            .findings
            .iter()
            .any(|f| matches!(f, PayloadIntegrityFinding::DigestMismatch { .. })));
    }
    #[test]
    fn reconstruction_reads_nothing_outside_work_dir() {
        let (dir, store, scope) = setup(2);
        store
            .submit(submission(&scope, "m1", b"finding"), b"finding", time())
            .unwrap();
        let outside = tempfile::tempdir().unwrap();
        let expected = format!("PROJECTION:{:?}", store.reconstruct(time()).unwrap());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "work_message::tests::reconstruction_child",
                "--nocapture",
            ])
            .env("COSMON_WORK_TEST_OWNER", dir.path())
            .env("HOME", outside.path())
            .env("CODEX_HOME", outside.path())
            .env("CLAUDE_CONFIG_DIR", outside.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(&expected));
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }
    #[test]
    fn reconstruction_child() {
        let Some(path) = std::env::var_os("COSMON_WORK_TEST_OWNER") else {
            return;
        };
        let store = FileWorkMessageStore::new(path.into());
        println!("PROJECTION:{:?}", store.reconstruct(time()).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn files_are_private_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store, scope) = setup(2);
        store
            .submit(submission(&scope, "m1", b"finding"), b"finding", time())
            .unwrap();
        let mut pending = vec![store.work_dir()];
        let mut files = 0;
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
                    pending.push(entry.path());
                } else {
                    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
                    files += 1;
                }
            }
        }
        assert_eq!(files, 6);
    }
}
