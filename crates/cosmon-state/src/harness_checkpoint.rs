// SPDX-License-Identifier: AGPL-3.0-only

//! File-backed [`TurnEvidenceStore`] for the in-process direct arms.
//!
//! Records go to the one fleet ledger as
//! [`EventV2::HarnessTurnRecorded`],
//! so ordering, sequence numbers and the molecule journal projection are the
//! ledger's. Raw content goes to immutable blobs under
//! `<molecule dir>/harness-turns/blobs/<hex digest>`, outside the git worktree
//! that `cs done` destroys.
//!
//! A blob is written to a temporary file, flushed, and renamed into place, so
//! a reader sees the whole blob or none of it. The file name is its digest, so
//! a blob cannot be rewritten with other content: an existing file with the
//! right digest is reused, one with the wrong digest is a corruption the store
//! refuses to paper over. Reading re-hashes the bytes and checks the length
//! recorded in the reference.
//!
//! [`load_attempt`] reads one attempt back from disk alone. It returns the
//! pure reconstruction together with every blob problem it found, so partial
//! or corrupt evidence is reported with what survived instead of being
//! discarded.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cosmon_core::event_v2::EventV2;
use cosmon_core::harness_turn::{
    reconstruct, AttemptReconstruction, BlobDigest, BlobKind, BlobRef, EvidenceError,
    HarnessTurnEvidence, SequenceError, TurnEvidenceStore, TurnRecord, HARNESS_TURN_SCHEMA_VERSION,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::usage::{UsageHistory, UsageObservationId};

/// Largest blob the store accepts. A native log past this is not checkpointed;
/// the loop stops instead of silently dropping evidence it was told to keep.
pub const MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Directory holding a molecule's evidence blobs.
#[must_use]
pub fn blob_dir(molecule_dir: &Path) -> PathBuf {
    molecule_dir.join("harness-turns").join("blobs")
}

fn io_err(context: &str, err: &std::io::Error) -> EvidenceError {
    EvidenceError::Io(format!("{context}: {err}"))
}

/// Evidence store for one worker attempt.
#[derive(Debug)]
pub struct FileTurnEvidenceStore {
    molecule_dir: PathBuf,
    events_path: PathBuf,
    mol_id: MoleculeId,
    worker_id: WorkerId,
    history_id: String,
}

impl FileTurnEvidenceStore {
    /// Create the store for one attempt. `history_id` must be the id the
    /// attempt's usage records carry, so both streams name the same attempt.
    #[must_use]
    pub fn new(
        state_dir: &Path,
        molecule_dir: &Path,
        mol_id: MoleculeId,
        worker_id: WorkerId,
        history_id: impl Into<String>,
    ) -> Self {
        Self {
            molecule_dir: molecule_dir.to_owned(),
            events_path: crate::event_log::resolve_events_log_path(state_dir),
            mol_id,
            worker_id,
            history_id: history_id.into(),
        }
    }
}

impl TurnEvidenceStore for FileTurnEvidenceStore {
    fn put_blob(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobRef, EvidenceError> {
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(EvidenceError::Rejected(format!(
                "blob of {} bytes exceeds the {MAX_BLOB_BYTES}-byte cap",
                bytes.len()
            )));
        }
        let digest = BlobDigest::of(bytes);
        let reference = BlobRef {
            kind,
            digest: digest.clone(),
            len: bytes.len() as u64,
            schema_version: HARNESS_TURN_SCHEMA_VERSION,
        };
        let dir = blob_dir(&self.molecule_dir);
        fs::create_dir_all(&dir).map_err(|e| io_err("create blob dir", &e))?;
        let target = dir.join(digest.hex());
        match fs::read(&target) {
            Ok(existing) if BlobDigest::of(&existing) == digest => return Ok(reference),
            Ok(_) => {
                return Err(EvidenceError::Corrupt(format!(
                    "blob {digest} exists with different content"
                )))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("read existing blob", &e)),
        }
        let tmp = dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let write = (|| {
            let mut file = File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &target)
        })();
        if let Err(e) = write {
            let _ = fs::remove_file(&tmp);
            return Err(io_err("write blob", &e));
        }
        Ok(reference)
    }

    fn append(&self, record: TurnRecord) -> Result<(), EvidenceError> {
        let event = EventV2::HarnessTurnRecorded {
            mol_id: self.mol_id.clone(),
            evidence: Box::new(HarnessTurnEvidence {
                schema_version: HARNESS_TURN_SCHEMA_VERSION,
                history_id: self.history_id.clone(),
                worker_id: self.worker_id.clone(),
                record,
            }),
        };
        crate::event_log::emit_one(&self.events_path, event, None)
            .map(|_| ())
            .map_err(|e| io_err("append ledger record", &e))
    }
}

/// Read a blob back and prove it is the one the reference promises.
///
/// # Errors
/// Returns [`EvidenceError::Corrupt`] if the file is missing, has the wrong
/// length or hashes to another digest, and [`EvidenceError::Io`] for other
/// read failures.
pub fn read_blob(molecule_dir: &Path, reference: &BlobRef) -> Result<Vec<u8>, EvidenceError> {
    if reference.schema_version > HARNESS_TURN_SCHEMA_VERSION {
        return Err(EvidenceError::Corrupt(format!(
            "blob schema {} is newer than this reader",
            reference.schema_version
        )));
    }
    let path = blob_dir(molecule_dir).join(reference.digest.hex());
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(EvidenceError::Corrupt(format!(
                "blob {} is missing",
                reference.digest
            )))
        }
        Err(e) => return Err(io_err("read blob", &e)),
    };
    if bytes.len() as u64 != reference.len {
        return Err(EvidenceError::Corrupt(format!(
            "blob {} has {} bytes, expected {}",
            reference.digest,
            bytes.len(),
            reference.len
        )));
    }
    if BlobDigest::of(&bytes) != reference.digest {
        return Err(EvidenceError::Corrupt(format!(
            "blob {} does not hash to its digest",
            reference.digest
        )));
    }
    Ok(bytes)
}

/// One attempt, read back from disk.
#[derive(Debug)]
pub struct LoadedAttempt {
    /// What the attempt's records establish.
    pub reconstruction: AttemptReconstruction,
    /// Every referenced blob that is missing or does not match its reference.
    pub blob_problems: Vec<(BlobRef, EvidenceError)>,
    /// Observation ids of the attempt's `UsageObserved` records, so the usage
    /// accounted before a restart is traceable from the same attempt.
    pub usage_observation_ids: Vec<String>,
    /// Number of turn records read.
    pub records_read: usize,
}

impl LoadedAttempt {
    /// True when every referenced blob validated.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.blob_problems.is_empty()
    }
}

/// Why an attempt could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The ledger could not be read.
    #[error("ledger unreadable: {0}")]
    Ledger(#[from] std::io::Error),
    /// A record carries a schema this reader does not understand.
    #[error("unsupported turn evidence schema {0}")]
    Schema(u32),
    /// The records break the ordering the writer guarantees.
    #[error("{0}")]
    Sequence(#[from] SequenceError),
    /// The ledger holds no record for this attempt.
    #[error("no turn evidence for attempt {0}")]
    NoEvidence(String),
}

/// Load one attempt's evidence from the ledger and the blob directory, and
/// validate every blob it references.
///
/// # Errors
/// See [`LoadError`]. A damaged blob is not an error: it is listed in
/// [`LoadedAttempt::blob_problems`] next to the reconstruction.
pub fn load_attempt(
    state_dir: &Path,
    molecule_dir: &Path,
    mol_id: &MoleculeId,
    history_id: &str,
) -> Result<LoadedAttempt, LoadError> {
    let envelopes =
        crate::event_log::read_all(crate::event_log::resolve_events_log_path(state_dir))?;
    let mut records = Vec::new();
    let mut usage_observation_ids = Vec::new();
    for envelope in envelopes {
        match envelope.event {
            EventV2::HarnessTurnRecorded {
                mol_id: row_mol,
                evidence,
            } if row_mol == *mol_id && evidence.history_id == history_id => {
                if evidence.schema_version > HARNESS_TURN_SCHEMA_VERSION {
                    return Err(LoadError::Schema(evidence.schema_version));
                }
                records.push(evidence.record);
            }
            EventV2::UsageObserved { usage } => {
                let same_history = matches!(
                    &usage.subject.history,
                    UsageHistory::Known { id } if id == history_id
                );
                if let (true, UsageObservationId::Known { id }) =
                    (same_history, &usage.observation_id)
                {
                    usage_observation_ids.push(id.clone());
                }
            }
            _ => {}
        }
    }
    if records.is_empty() {
        return Err(LoadError::NoEvidence(history_id.to_owned()));
    }
    let reconstruction = reconstruct(&records)?;
    let blob_problems = reconstruction
        .blob_refs()
        .into_iter()
        .filter_map(|reference| {
            read_blob(molecule_dir, reference)
                .err()
                .map(|e| (reference.clone(), e))
        })
        .collect();
    Ok(LoadedAttempt {
        reconstruction,
        blob_problems,
        usage_observation_ids,
        records_read: records.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::harness_turn::{TerminalKind, TurnLimits};
    use std::collections::BTreeMap;

    struct Fixture {
        _tmp: tempfile::TempDir,
        state: PathBuf,
        mol_dir: PathBuf,
        mol: MoleculeId,
        store: FileTurnEvidenceStore,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let mol_dir = tmp.path().join("mol");
        fs::create_dir_all(&state).expect("state dir");
        fs::create_dir_all(&mol_dir).expect("mol dir");
        let mol = MoleculeId::new("task-20261003-aaaa").expect("molecule id");
        let store = FileTurnEvidenceStore::new(
            &state,
            &mol_dir,
            mol.clone(),
            WorkerId::new("worker-1").expect("worker id"),
            "harness/mol/worker-1/a1",
        );
        Fixture {
            _tmp: tmp,
            state,
            mol_dir,
            mol,
            store,
        }
    }

    fn started() -> TurnRecord {
        TurnRecord::AttemptStarted {
            limits: TurnLimits {
                max_turns: 30,
                max_tool_calls: 64,
                max_input_tokens: 32_768,
            },
            pins: BTreeMap::new(),
        }
    }

    #[test]
    fn a_blob_round_trips_and_is_immutable() {
        let f = fixture();
        let one = f
            .store
            .put_blob(BlobKind::ToolResult, b"hello")
            .expect("put");
        let again = f
            .store
            .put_blob(BlobKind::ToolResult, b"hello")
            .expect("reput");
        assert_eq!(one, again);
        assert_eq!(read_blob(&f.mol_dir, &one).expect("read"), b"hello");
        let path = blob_dir(&f.mol_dir).join(one.digest.hex());
        fs::write(&path, b"tampered").expect("tamper");
        assert!(matches!(
            read_blob(&f.mol_dir, &one),
            Err(EvidenceError::Corrupt(_))
        ));
        assert!(matches!(
            f.store.put_blob(BlobKind::ToolResult, b"hello"),
            Err(EvidenceError::Corrupt(_))
        ));
    }

    #[test]
    fn a_blob_over_the_cap_is_rejected_not_truncated() {
        let f = fixture();
        let big = vec![0u8; MAX_BLOB_BYTES + 1];
        assert!(matches!(
            f.store.put_blob(BlobKind::LogCheckpoint, &big),
            Err(EvidenceError::Rejected(_))
        ));
    }

    #[test]
    fn no_temporary_file_survives_a_write() {
        let f = fixture();
        f.store.put_blob(BlobKind::PartialText, b"x").expect("put");
        let leftovers = fs::read_dir(blob_dir(&f.mol_dir))
            .expect("dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn records_reach_the_ledger_and_load_back_with_intact_blobs() {
        let f = fixture();
        let partial = f
            .store
            .put_blob(BlobKind::PartialText, b"cut off")
            .expect("blob");
        f.store.append(started()).expect("started");
        f.store
            .append(TurnRecord::RequestIntent {
                turn: 0,
                estimated_input_tokens: 5,
                tools_spent: 0,
            })
            .expect("intent");
        f.store
            .append(TurnRecord::Terminal {
                turn: 0,
                disposition: TerminalKind::OutputLimit,
                partial_text: Some(partial),
                tools_spent: 0,
            })
            .expect("terminal");
        let loaded =
            load_attempt(&f.state, &f.mol_dir, &f.mol, "harness/mol/worker-1/a1").expect("load");
        assert!(loaded.is_intact());
        assert_eq!(loaded.records_read, 3);
        assert!(loaded.reconstruction.terminal.is_some());
    }

    #[test]
    fn a_corrupt_blob_is_reported_next_to_the_surviving_reconstruction() {
        let f = fixture();
        let partial = f
            .store
            .put_blob(BlobKind::PartialText, b"cut off")
            .expect("blob");
        f.store.append(started()).expect("started");
        f.store
            .append(TurnRecord::RequestIntent {
                turn: 0,
                estimated_input_tokens: 5,
                tools_spent: 0,
            })
            .expect("intent");
        f.store
            .append(TurnRecord::Terminal {
                turn: 0,
                disposition: TerminalKind::Refused,
                partial_text: Some(partial.clone()),
                tools_spent: 0,
            })
            .expect("terminal");
        fs::remove_file(blob_dir(&f.mol_dir).join(partial.digest.hex())).expect("remove");
        let loaded =
            load_attempt(&f.state, &f.mol_dir, &f.mol, "harness/mol/worker-1/a1").expect("load");
        assert!(!loaded.is_intact());
        assert_eq!(loaded.blob_problems.len(), 1);
        assert!(loaded.reconstruction.terminal.is_some());
    }

    #[test]
    fn another_attempt_has_no_evidence_here() {
        let f = fixture();
        f.store.append(started()).expect("started");
        assert!(matches!(
            load_attempt(&f.state, &f.mol_dir, &f.mol, "harness/mol/worker-1/other"),
            Err(LoadError::NoEvidence(_))
        ));
    }
}
