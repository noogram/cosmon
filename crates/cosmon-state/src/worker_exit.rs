// SPDX-License-Identifier: AGPL-3.0-only

//! Private, durable launch receipts. Reads never spawn, adopt, or signal a process.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use cosmon_core::worker_exit::{
    LaunchIdentity, LaunchRequest, StopAccepted, StopRefused, SupervisionPoint,
    WorkerExitObservation, WorkerExitReceipt, WorkerExitStore,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Ex ante launch identity retained independently of worktree and terminal lifetime.
pub const CURRENT_LAUNCH: &str = "worker-launch.json";

/// Append-only audit of stop requests for one launch (ADR-188). Evidence only:
/// it is never read as authority or as a receipt.
pub const STOP_REQUESTS: &str = "stop-requests.jsonl";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopRequestEvidence {
    identity: LaunchIdentity,
    requested_at: String,
    outcome: Result<StopAccepted, StopRefused>,
}

/// File adapter for one launch/incarnation, reconstructed using an expected identity.
pub struct FileWorkerExitStore {
    directory: PathBuf,
    request: LaunchRequest,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessWitness {
    identity: LaunchIdentity,
    supervisor_pid: u32,
    child_pid: Option<u32>,
}

impl FileWorkerExitStore {
    /// Prepare a new launch. The caller must serialize dispatches with the fleet lock.
    /// An unresolved previous launch refuses redispatch, even if its PID disappeared.
    ///
    /// # Errors
    /// Refuses unresolved/foreign previous evidence, collisions, and I/O failures.
    pub fn prepare(molecule_dir: &Path, request: LaunchRequest) -> Result<Self, String> {
        validate_identity(&request.identity)?;
        if let Some(previous) = Self::current(molecule_dir)? {
            if previous.request.identity.molecule_id != request.identity.molecule_id {
                return Err("previous worker launch belongs to a different molecule".to_owned());
            }
            if !previous.read().is_terminal() {
                return Err("previous supervised launch is unresolved; no automatic retry or PID adoption is allowed".to_owned());
            }
        }
        let root = molecule_dir.join("worker-launches");
        if !root.exists() {
            DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .map_err(io_error)?;
            sync_dir(molecule_dir)?;
        }
        let directory = launch_directory(molecule_dir, &request.identity);
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(io_error)?;
        sync_dir(&root)?;
        create_json(&directory.join("request.json"), &request)?;
        sync_dir(&directory)?;
        atomic_json(molecule_dir, CURRENT_LAUNCH, &request.identity)?;
        Ok(Self { directory, request })
    }

    /// Reconstruct the expected current launch; malformed metadata is an error, never legacy.
    ///
    /// # Errors
    /// Reports missing or malformed supervised metadata and I/O failures.
    pub fn current(molecule_dir: &Path) -> Result<Option<Self>, String> {
        let path = molecule_dir.join(CURRENT_LAUNCH);
        match fs::read(&path) {
            Ok(bytes) => {
                let identity: LaunchIdentity = serde_json::from_slice(&bytes)
                    .map_err(|_| "invalid expected worker launch identity".to_owned())?;
                Self::open(molecule_dir, &identity).map(Some)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if molecule_dir.join("worker-launches").exists() {
                    Err("expected supervised launch metadata is missing; result unknown".to_owned())
                } else {
                    Ok(None)
                }
            }
            Err(error) => Err(io_error(error)),
        }
    }

    /// Open only the specified launch. Foreign handles cannot select a different result.
    ///
    /// # Errors
    /// Refuses missing, malformed, or foreign request evidence.
    pub fn open(molecule_dir: &Path, expected: &LaunchIdentity) -> Result<Self, String> {
        validate_identity(expected)?;
        let directory = launch_directory(molecule_dir, expected);
        let request: LaunchRequest = read_json(&directory.join("request.json"))?;
        if &request.identity != expected {
            return Err("worker request does not match expected launch identity".to_owned());
        }
        Ok(Self { directory, request })
    }

    /// Read the non-secret request binding used to check the actual command before spawn.
    #[must_use]
    pub fn request(&self) -> &LaunchRequest {
        &self.request
    }

    /// Return the canonical evidence directory, including retained crash temporaries.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Append one stop request and its outcome to [`STOP_REQUESTS`].
    ///
    /// # Errors
    /// Reports encoding and I/O failures; callers treat them as non-fatal.
    pub fn record_stop_request(
        &self,
        outcome: Result<StopAccepted, StopRefused>,
    ) -> Result<(), String> {
        let mut line = serde_json::to_vec(&StopRequestEvidence {
            identity: self.request.identity.clone(),
            requested_at: chrono::Utc::now().to_rfc3339(),
            outcome,
        })
        .map_err(|_| "stop request evidence cannot be encoded".to_owned())?;
        line.push(b'\n');
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(self.directory.join(STOP_REQUESTS))
            .map_err(io_error)?;
        file.write_all(&line).map_err(io_error)?;
        file.sync_all().map_err(io_error)
    }

    fn witness(&self, supervisor_pid: u32, child_pid: Option<u32>) -> ProcessWitness {
        ProcessWitness {
            identity: self.request.identity.clone(),
            supervisor_pid,
            child_pid,
        }
    }

    fn validated_receipt(&self) -> Result<WorkerExitReceipt, String> {
        validate_identity(&self.request.identity)?;
        let claim: ProcessWitness = read_json(&self.directory.join("claim.json"))?;
        let receipt: WorkerExitReceipt = read_json(&self.directory.join("result.json"))?;
        if receipt.identity != self.request.identity
            || claim.identity != receipt.identity
            || claim.supervisor_pid == 0
            || claim.supervisor_pid != receipt.supervisor_pid
            || claim.child_pid.is_some()
            || !receipt.observation.is_terminal()
        {
            return Err("invalid worker receipt or claim".to_owned());
        }
        match receipt.observation {
            WorkerExitObservation::ExecFailed { .. } if receipt.child_pid.is_none() => {
                if self.directory.join("birth.json").exists() {
                    return Err("exec failure has a contradictory child birth".to_owned());
                }
            }
            WorkerExitObservation::Exited { .. } | WorkerExitObservation::Signaled { .. } => {
                let birth: ProcessWitness = read_json(&self.directory.join("birth.json"))?;
                if birth.identity != receipt.identity
                    || birth.supervisor_pid != receipt.supervisor_pid
                    || birth.child_pid != receipt.child_pid
                    || receipt.child_pid.is_none_or(|pid| pid == 0)
                {
                    return Err("worker receipt does not match child birth".to_owned());
                }
            }
            _ => return Err("worker receipt has no valid terminal provenance".to_owned()),
        }
        Ok(receipt)
    }
}

impl WorkerExitStore for FileWorkerExitStore {
    fn claim(&self, supervisor_pid: u32) -> Result<(), String> {
        create_json(
            &self.directory.join("claim.json"),
            &self.witness(supervisor_pid, None),
        )?;
        sync_dir(&self.directory)
    }

    fn record_birth(&self, supervisor_pid: u32, child_pid: u32) -> Result<(), String> {
        create_json(
            &self.directory.join("birth.json"),
            &self.witness(supervisor_pid, Some(child_pid)),
        )?;
        sync_dir(&self.directory)
    }

    fn publish(&self, receipt: &WorkerExitReceipt) -> Result<(), String> {
        self.publish_observed(receipt, &|_| {})
    }

    fn publish_observed(
        &self,
        receipt: &WorkerExitReceipt,
        checkpoint: &dyn Fn(SupervisionPoint),
    ) -> Result<(), String> {
        if receipt.identity != self.request.identity || self.directory.join("result.json").exists()
        {
            return Err("worker receipt is foreign or already published".to_owned());
        }
        atomic_json_observed(&self.directory, "result.json", receipt, checkpoint)
    }

    fn read(&self) -> WorkerExitObservation {
        self.validated_receipt()
            .map_or(WorkerExitObservation::Unknown, |receipt| {
                receipt.observation
            })
    }
}

fn launch_directory(molecule_dir: &Path, identity: &LaunchIdentity) -> PathBuf {
    molecule_dir.join("worker-launches").join(format!(
        "{}-{}",
        identity.launch_id.as_str(),
        identity.incarnation.as_str()
    ))
}

fn validate_identity(identity: &LaunchIdentity) -> Result<(), String> {
    if identity.schema == 1
        && identity.request_hash.len() == 64
        && identity
            .request_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err("unsupported worker launch identity schema or request binding".to_owned())
    }
}

#[allow(clippy::needless_pass_by_value)] // An owned map_err callback that redacts the error value.
fn io_error(error: std::io::Error) -> String {
    format!("worker receipt I/O failed ({:?})", error.kind())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid worker receipt JSON".to_owned())
}

fn create_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| "worker receipt cannot be encoded".to_owned())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

/// Atomically publish private evidence, syncing both file contents and directory entry.
/// Temporary files left by a crash are intentionally retained and never read as results.
///
/// # Errors
/// Reports encoding, exclusive creation, synchronization, and rename failures.
pub fn atomic_json(directory: &Path, name: &str, value: &impl Serialize) -> Result<(), String> {
    atomic_json_observed(directory, name, value, &|_| {})
}

fn atomic_json_observed(
    directory: &Path,
    name: &str,
    value: &impl Serialize,
    checkpoint: &dyn Fn(SupervisionPoint),
) -> Result<(), String> {
    let temporary = directory.join(format!(".{name}.{:016x}.tmp", rand::random::<u64>()));
    create_json(&temporary, value)?;
    checkpoint(SupervisionPoint::AfterTemporarySync);
    fs::rename(&temporary, directory.join(name)).map_err(io_error)?;
    sync_dir(directory)?;
    checkpoint(SupervisionPoint::AfterDurableResult);
    Ok(())
}

fn sync_dir(directory: &Path) -> Result<(), String> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::{id::MoleculeId, worker_exit::LaunchToken};

    fn prepare(root: &Path) -> Result<FileWorkerExitStore, String> {
        FileWorkerExitStore::prepare(
            root,
            LaunchRequest::bind(
                MoleculeId::new("task-20261019-aaaa").map_err(|e| e.to_string())?,
                LaunchToken::from_bytes([1; 16]),
                LaunchToken::from_bytes([2; 16]),
                "worker",
                &[],
                Path::new("/work"),
                &[],
            )?,
        )
    }

    #[test]
    fn receipt_requires_matching_claim_birth_and_expected_identity() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(io_error)?;
        let store = prepare(root.path())?;
        store.claim(123)?;
        assert!(store.claim(124).is_err());
        assert!(prepare(root.path()).is_err());
        store.record_birth(123, 456)?;
        let receipt = WorkerExitReceipt {
            identity: store.request.identity.clone(),
            supervisor_pid: 123,
            child_pid: Some(456),
            observation: WorkerExitObservation::Exited { code: 143 },
        };
        store.publish(&receipt)?;
        assert_eq!(store.read(), WorkerExitObservation::Exited { code: 143 });
        assert_eq!(
            FileWorkerExitStore::current(root.path())?.map(|s| s.read()),
            Some(WorkerExitObservation::Exited { code: 143 })
        );
        fs::write(store.directory.join("birth.json"), b"{}").map_err(io_error)?;
        assert_eq!(store.read(), WorkerExitObservation::Unknown);
        Ok(())
    }

    #[test]
    fn a_crash_temporary_or_corrupt_final_never_gives_a_terminal_verdict() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(io_error)?;
        let store = prepare(root.path())?;
        store.claim(123)?;
        create_json(
            &store.directory.join(".result.json.tmp"),
            &WorkerExitObservation::Exited { code: 0 },
        )?;
        assert_eq!(store.read(), WorkerExitObservation::Unknown);
        fs::write(store.directory.join("result.json"), b"null").map_err(io_error)?;
        assert_eq!(store.read(), WorkerExitObservation::Unknown);
        assert!(store.claim(123).is_err());
        Ok(())
    }
}
