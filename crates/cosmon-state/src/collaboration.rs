// SPDX-License-Identifier: AGPL-3.0-only

//! Host-local custody of collaboration bindings (`binding-store` writer of
//! the cross-machine collaboration contract, §2).
//!
//! Records live under `<state>/collaboration/bindings/<binding-id>.json`, one
//! private file per binding, written by temporary file and rename and synced
//! before the call returns. The proof handed to the operator at provisioning
//! is never written; only its verifier is.
//!
//! The crate exposes two types on purpose:
//!
//! - [`CollaborationBindingStore`] provisions and revokes. Only the local
//!   `cs collaboration` command constructs it.
//! - [`CollaborationBindingReader`] loads, admits and rechecks. It has no
//!   method that writes a binding, so a network boundary holding a reader
//!   cannot edit its own grant.
//!
//! Lock order: a writer holds the exclusive binding lock while it changes a
//! record; [`CollaborationBindingReader::commit`] holds the shared binding
//! lock while it rechecks the admission and runs the effect. A revocation
//! therefore lands either before the recheck (the effect is refused) or after
//! the effect (the effect happened under a valid binding). Any work lock is
//! taken inside the effect, after the binding lock.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::collaboration::{
    admit, recheck, AdmissionRequest, AdmittedCollaborator, AttachmentId, AttachmentProof,
    BindingId, BindingSet, BindingSetError, BindingSpec, BindingSpecError, CollaborationBinding,
    CollaborationRefusal, BINDING_SCHEMA_VERSION, PROOF_SECRET_BYTES,
};
use rand::RngCore;

use crate::work_message::{private_dir, private_options, reject_symlink, write_atomic};

/// Upper bound on stored bindings; a larger directory is treated as corrupt
/// rather than read without limit.
pub const MAX_BINDINGS: usize = 256;

/// Upper bound on one binding file, checked before it is read.
pub const MAX_BINDING_BYTES: u64 = 16 * 1024;

/// Failure of the binding store or of a binding decision.
#[derive(Debug, thiserror::Error)]
pub enum CollaborationStoreError {
    /// Filesystem I/O failed.
    #[error("collaboration binding I/O failed: {0}")]
    Io(#[from] io::Error),
    /// A record did not serialize or parse.
    #[error("collaboration binding record is malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// The provisioning request is invalid.
    #[error(transparent)]
    Spec(#[from] BindingSpecError),
    /// Stored records cannot be trusted as a set.
    #[error(transparent)]
    Set(#[from] BindingSetError),
    /// Stored records violate a custody bound.
    #[error("collaboration binding store is corrupt: {0}")]
    Corrupt(String),
    /// The store already holds [`MAX_BINDINGS`] records.
    #[error("collaboration binding store is full ({MAX_BINDINGS} bindings)")]
    Full,
    /// No record has this identifier.
    #[error("no collaboration binding {0}")]
    UnknownBinding(BindingId),
    /// The admission decision refused the caller.
    #[error(transparent)]
    Refused(#[from] CollaborationRefusal),
}

/// Outcome of [`CollaborationBindingReader::commit`].
#[derive(Debug, thiserror::Error)]
pub enum CommitError<E> {
    /// The binding recheck refused or the store failed; the effect did not run.
    #[error(transparent)]
    Binding(CollaborationStoreError),
    /// The effect ran and returned this error.
    #[error("collaboration effect failed")]
    Effect(E),
}

fn collaboration_dir(state_root: &Path) -> PathBuf {
    state_root.join("collaboration")
}

fn bindings_dir(state_root: &Path) -> PathBuf {
    collaboration_dir(state_root).join("bindings")
}

fn lock_path(state_root: &Path) -> PathBuf {
    collaboration_dir(state_root).join("bindings.lock")
}

/// Host-local writer of collaboration bindings.
#[derive(Debug, Clone)]
pub struct CollaborationBindingStore {
    state_root: PathBuf,
}

impl CollaborationBindingStore {
    /// Bind the writer to a galaxy state root (`<galaxy>/.cosmon/state`).
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    /// Read-only view of the same records.
    #[must_use]
    pub fn reader(&self) -> CollaborationBindingReader {
        CollaborationBindingReader::new(self.state_root.clone())
    }

    /// Provision one binding and return it with the attachment proof. The
    /// proof is returned once and never stored.
    ///
    /// # Errors
    /// Refuses an invalid spec, a full store, an untrusted existing set, and
    /// filesystem failures.
    pub fn provision(
        &self,
        spec: BindingSpec,
        now: DateTime<Utc>,
    ) -> Result<(CollaborationBinding, AttachmentProof), CollaborationStoreError> {
        let _lock = self.lock_exclusive()?;
        let current = self.reader().load()?;
        if current.len() >= MAX_BINDINGS {
            return Err(CollaborationStoreError::Full);
        }
        let mut rng = rand::rngs::OsRng;
        let (id, attachment) = loop {
            let mut id = [0_u8; 8];
            let mut attachment = [0_u8; 8];
            rng.try_fill_bytes(&mut id).map_err(io::Error::other)?;
            rng.try_fill_bytes(&mut attachment)
                .map_err(io::Error::other)?;
            let id = BindingId::from_random(id);
            let attachment = AttachmentId::from_random(attachment);
            if !current
                .iter()
                .any(|b| b.id == id || b.attachment == attachment)
            {
                break (id, attachment);
            }
        };
        let mut secret = [0_u8; PROOF_SECRET_BYTES];
        rng.try_fill_bytes(&mut secret).map_err(io::Error::other)?;
        let proof = AttachmentProof::from_secret_bytes(secret);
        secret.fill(0);
        let binding = CollaborationBinding::provision(spec, id, attachment, &proof, now)?;
        self.write(&binding)?;
        Ok((binding, proof))
    }

    /// Revoke one binding. Its revision increases, so every earlier admission
    /// fails its commit recheck. Revoking twice changes nothing.
    ///
    /// # Errors
    /// Returns [`CollaborationStoreError::UnknownBinding`] for an unknown id,
    /// and store failures.
    pub fn revoke(
        &self,
        id: &BindingId,
        now: DateTime<Utc>,
    ) -> Result<CollaborationBinding, CollaborationStoreError> {
        let _lock = self.lock_exclusive()?;
        let current = self
            .reader()
            .load()?
            .iter()
            .find(|binding| &binding.id == id)
            .cloned()
            .ok_or_else(|| CollaborationStoreError::UnknownBinding(id.clone()))?;
        let revoked = current.revoked(now);
        if revoked != current {
            self.write(&revoked)?;
        }
        Ok(revoked)
    }

    fn write(&self, binding: &CollaborationBinding) -> Result<(), CollaborationStoreError> {
        let mut bytes = serde_json::to_vec_pretty(binding)?;
        bytes.push(b'\n');
        let path = bindings_dir(&self.state_root).join(format!("{}.json", binding.id));
        reject_symlink(&path)?;
        write_atomic(&path, &bytes)?;
        Ok(())
    }

    fn lock_exclusive(&self) -> Result<File, CollaborationStoreError> {
        let dir = collaboration_dir(&self.state_root);
        private_dir(&dir)?;
        let path = lock_path(&self.state_root);
        reject_symlink(&path)?;
        let file = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    }
}

/// Read-only access to collaboration bindings for an admission boundary.
#[derive(Debug, Clone)]
pub struct CollaborationBindingReader {
    state_root: PathBuf,
}

impl CollaborationBindingReader {
    /// Bind the reader to a galaxy state root (`<galaxy>/.cosmon/state`).
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    /// Load every binding, enforcing the count and size bounds before any
    /// unbounded read. An absent store is an empty set.
    ///
    /// # Errors
    /// Returns [`CollaborationStoreError::Corrupt`] for a bound violation, a
    /// symlink, a newer schema or a file name that disagrees with its record.
    pub fn load(&self) -> Result<BindingSet, CollaborationStoreError> {
        let dir = bindings_dir(&self.state_root);
        reject_symlink(&dir)?;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(BindingSet::default())
            }
            Err(error) => return Err(error.into()),
        };
        let mut bindings = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(CollaborationStoreError::Corrupt(
                    "non UTF-8 file name".to_owned(),
                ));
            };
            if name.starts_with('.') {
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                return Err(CollaborationStoreError::Corrupt(format!(
                    "unexpected file {name}"
                )));
            };
            if bindings.len() >= MAX_BINDINGS {
                return Err(CollaborationStoreError::Corrupt(format!(
                    "more than {MAX_BINDINGS} bindings"
                )));
            }
            let binding = read_binding(&entry.path())?;
            if binding.id.as_str() != stem {
                return Err(CollaborationStoreError::Corrupt(format!(
                    "{name} holds binding {}",
                    binding.id
                )));
            }
            bindings.push(binding);
        }
        Ok(BindingSet::new(bindings)?)
    }

    /// Admit a request against the current bindings.
    ///
    /// # Errors
    /// Returns [`CollaborationStoreError::Refused`] for a refused caller and a
    /// store error when the bindings cannot be read; neither writes anything.
    pub fn admit(
        &self,
        request: &AdmissionRequest<'_>,
    ) -> Result<AdmittedCollaborator, CollaborationStoreError> {
        Ok(admit(&self.load()?, request)?)
    }

    /// Recheck `admitted` under the shared binding lock and run `effect`
    /// while that lock is held, so no revocation can interleave between the
    /// recheck and the effect.
    ///
    /// # Errors
    /// Returns [`CommitError::Binding`] without running `effect` when the
    /// binding is gone, revoked, changed or unreadable, and
    /// [`CommitError::Effect`] with the effect's own error.
    pub fn commit<T, E>(
        &self,
        admitted: &AdmittedCollaborator,
        effect: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CommitError<E>> {
        let _lock = self.lock_shared().map_err(CommitError::Binding)?;
        let current = self.load().map_err(CommitError::Binding)?;
        recheck(&current, admitted).map_err(|refusal| CommitError::Binding(refusal.into()))?;
        effect().map_err(CommitError::Effect)
    }

    fn lock_shared(&self) -> Result<File, CollaborationStoreError> {
        let path = lock_path(&self.state_root);
        reject_symlink(&path)?;
        // Never create the lock here: a reader writes nothing. A store that
        // was never provisioned holds no binding to recheck against.
        let file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(CollaborationRefusal::BindingMissing.into())
            }
            Err(error) => return Err(error.into()),
        };
        fs2::FileExt::lock_shared(&file)?;
        Ok(file)
    }
}

fn read_binding(path: &Path) -> Result<CollaborationBinding, CollaborationStoreError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(CollaborationStoreError::Corrupt(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > MAX_BINDING_BYTES {
        return Err(CollaborationStoreError::Corrupt(format!(
            "{} exceeds {MAX_BINDING_BYTES} bytes",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BINDING_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BINDING_BYTES {
        return Err(CollaborationStoreError::Corrupt(format!(
            "{} grew past {MAX_BINDING_BYTES} bytes",
            path.display()
        )));
    }
    let binding: CollaborationBinding = serde_json::from_slice(&bytes)?;
    if binding.schema_version > BINDING_SCHEMA_VERSION {
        return Err(CollaborationStoreError::Corrupt(format!(
            "binding {} has unsupported schema {}",
            binding.id, binding.schema_version
        )));
    }
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use cosmon_core::advisory_attempt::AdvisorySeatId;
    use cosmon_core::collaboration::{
        AdmittedIdentity, CollaborationCapability, CollaborationScope, CollaborationTarget,
    };
    use cosmon_core::id::MoleculeId;
    use std::collections::BTreeMap;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).single().unwrap()
    }

    fn identity(subject: &str) -> AdmittedIdentity {
        AdmittedIdentity::new("https://idp", subject, "cosmon-rpp-demo", "demo").unwrap()
    }

    fn owner() -> MoleculeId {
        MoleculeId::new("task-20261002-aaaa").unwrap()
    }

    fn spec(subject: &str, seat: &str) -> BindingSpec {
        BindingSpec {
            identity: identity(subject),
            capability: CollaborationCapability::WorkSeat {
                owner: owner(),
                seat: AdvisorySeatId::new(seat).unwrap(),
            },
            scopes: [CollaborationScope::WorkWrite].into_iter().collect(),
            label: Some("second machine".to_owned()),
        }
    }

    /// Every file under the collaboration directory with its bytes.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&collaboration_dir(root), &mut out);
        out
    }

    fn request<'a>(
        identity: &'a AdmittedIdentity,
        attachment: &'a AttachmentId,
        proof: &'a AttachmentProof,
        target: &'a CollaborationTarget,
    ) -> AdmissionRequest<'a> {
        AdmissionRequest {
            identity,
            attachment,
            proof,
            target,
            required: CollaborationScope::WorkWrite,
        }
    }

    #[test]
    fn provisioned_binding_survives_a_restart_without_its_proof() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (binding, proof) = store
            .provision(spec("pilot-a", "implementer"), t(0))
            .unwrap();

        // A fresh reader stands for a restarted service.
        let reloaded = CollaborationBindingReader::new(dir.path()).load().unwrap();
        assert_eq!(reloaded.iter().collect::<Vec<_>>(), vec![&binding]);
        for bytes in snapshot(dir.path()).values() {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(proof.expose_secret()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = bindings_dir(dir.path()).join(format!("{}.json", binding.id));
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn refused_admissions_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (first, first_proof) = store
            .provision(spec("pilot-a", "implementer"), t(0))
            .unwrap();
        let (second, _) = store.provision(spec("pilot-b", "reviewer"), t(0)).unwrap();
        let before = snapshot(dir.path());
        let reader = store.reader();
        let target = CollaborationTarget::Work { owner: owner() };

        // Pilot A claims pilot B's seat through B's attachment.
        let refused = reader.admit(&request(
            &identity("pilot-a"),
            &second.attachment,
            &first_proof,
            &target,
        ));
        assert!(matches!(
            refused,
            Err(CollaborationStoreError::Refused(
                CollaborationRefusal::BindingMissing
            ))
        ));
        let admitted = reader
            .admit(&request(
                &identity("pilot-a"),
                &first.attachment,
                &first_proof,
                &target,
            ))
            .unwrap();
        assert_eq!(
            admitted.work_caller().unwrap().seat().as_str(),
            "implementer"
        );
        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn revocation_refuses_the_next_commit_and_its_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (binding, proof) = store
            .provision(spec("pilot-a", "implementer"), t(0))
            .unwrap();
        let reader = store.reader();
        let target = CollaborationTarget::Work { owner: owner() };
        let id = identity("pilot-a");
        let admitted = reader
            .admit(&request(&id, &binding.attachment, &proof, &target))
            .unwrap();
        assert_eq!(reader.commit(&admitted, || Ok::<_, ()>(7)).unwrap(), 7);

        let revoked = store.revoke(&binding.id, t(1)).unwrap();
        assert_eq!(revoked.revision, binding.revision.next());
        assert_eq!(store.revoke(&binding.id, t(2)).unwrap(), revoked);

        let mut ran = false;
        let refused = reader.commit(&admitted, || {
            ran = true;
            Ok::<_, ()>(())
        });
        assert!(matches!(
            refused,
            Err(CommitError::Binding(CollaborationStoreError::Refused(
                CollaborationRefusal::BindingRevoked
            )))
        ));
        assert!(!ran, "a revoked binding must refuse before the effect");
        assert!(matches!(
            reader.admit(&request(&id, &binding.attachment, &proof, &target)),
            Err(CollaborationStoreError::Refused(
                CollaborationRefusal::BindingRevoked
            ))
        ));
    }

    #[test]
    fn a_reader_on_an_unprovisioned_host_admits_nothing_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let reader = CollaborationBindingReader::new(dir.path());
        assert!(reader.load().unwrap().is_empty());
        let proof = AttachmentProof::from_secret_bytes([1; PROOF_SECRET_BYTES]);
        let target = CollaborationTarget::Work { owner: owner() };
        assert!(matches!(
            reader.admit(&request(
                &identity("pilot-a"),
                &AttachmentId::from_random([1; 8]),
                &proof,
                &target,
            )),
            Err(CollaborationStoreError::Refused(
                CollaborationRefusal::BindingMissing
            ))
        ));
        assert!(!collaboration_dir(dir.path()).exists());
    }

    #[test]
    fn oversized_or_misnamed_records_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (binding, _) = store
            .provision(spec("pilot-a", "implementer"), t(0))
            .unwrap();
        let bindings = bindings_dir(dir.path());

        fs::write(
            bindings.join("cb-ffffffffffffffff.json"),
            fs::read(bindings.join(format!("{}.json", binding.id))).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            store.reader().load(),
            Err(CollaborationStoreError::Corrupt(_))
        ));
        fs::remove_file(bindings.join("cb-ffffffffffffffff.json")).unwrap();

        fs::write(
            bindings.join("cb-eeeeeeeeeeeeeeee.json"),
            vec![b' '; (MAX_BINDING_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(matches!(
            store.reader().load(),
            Err(CollaborationStoreError::Corrupt(_))
        ));
    }
}
