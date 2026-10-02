// SPDX-License-Identifier: AGPL-3.0-only

//! Private persistence of collaboration attachment proofs.
//!
//! The cross-machine collaboration contract (§2) gives every client
//! attachment its own revocable proof, presented next to the OIDC token. The
//! proof belongs in the client credential store and nowhere else: never in
//! argv, a payload, a log or a published artifact. This module stores it in
//! the same backend as the token — the OS keyring or a 0600 file written by
//! temporary file and rename — under a slot of its own, so a proof and a token
//! never share storage and two attachments of one identity never collide.
//!
//! The static-bearer [`BackendKind::Env`](super::BackendKind::Env) backend has
//! nowhere to keep a proof: storing reports
//! [`StoreOutcome::Discarded`](super::StoreOutcome::Discarded) and loading
//! returns nothing.

use std::fs;
use std::io::{self, Read};
use std::sync::atomic::Ordering;

use cosmon_core::collaboration::{AttachmentId, AttachmentProof};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::key::CredentialKey;
use super::store::{
    backend_err, check_permissions, fsync_dir, harden_dir, open_read, write_tmp, Backend,
    CredentialStore, StoreOutcome, KEYRING_SERVICE, TMP_SEQ,
};
use crate::error::{CredentialStoreError, Error, Result};

/// Wire schema of a persisted proof file.
const PROOF_SCHEMA: u32 = 1;

/// Upper bound on a proof file, checked before it is read.
const MAX_PROOF_FILE_BYTES: u64 = 4096;

/// The slot of one attachment proof: the token identity it is presented with
/// and the host-issued attachment ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSlot {
    key: CredentialKey,
    attachment: AttachmentId,
}

impl AttachmentSlot {
    /// Address the proof of `attachment` used with the identity `key`.
    #[must_use]
    pub fn new(key: CredentialKey, attachment: AttachmentId) -> Self {
        Self { key, attachment }
    }

    /// Token identity of the slot.
    #[must_use]
    pub fn key(&self) -> &CredentialKey {
        &self.key
    }

    /// Attachment of the slot.
    #[must_use]
    pub fn attachment(&self) -> &AttachmentId {
        &self.attachment
    }

    /// Stable identifier of the slot. The `attachment-v1` tag keeps it
    /// disjoint from every token slot id.
    #[must_use]
    pub fn storage_id(&self) -> String {
        let canonical = format!(
            "cosmon-remote\u{1f}attachment-v1\u{1f}{}\u{1f}{}",
            self.key.storage_id(),
            self.attachment
        );
        blake3::hash(canonical.as_bytes()).to_hex().to_string()
    }

    fn keyring_account(&self) -> String {
        format!("attachment-{}", self.storage_id())
    }
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ProofWire {
    #[zeroize(skip)]
    schema_version: u32,
    #[zeroize(skip)]
    attachment: String,
    proof: String,
}

impl CredentialStore {
    /// Persist the proof of one attachment, replacing any earlier proof for
    /// the same slot.
    ///
    /// # Errors
    /// Returns a backend or filesystem error.
    pub fn store_attachment_proof(
        &self,
        slot: &AttachmentSlot,
        proof: &AttachmentProof,
    ) -> Result<StoreOutcome> {
        match self.backend {
            Backend::Env => Ok(StoreOutcome::Discarded),
            Backend::Keyring => {
                keyring::Entry::new(KEYRING_SERVICE, &slot.keyring_account())
                    .and_then(|entry| entry.set_password(proof.expose_secret()))
                    .map_err(|e| Error::from(backend_err(e)))?;
                Ok(StoreOutcome::Persisted)
            }
            Backend::File => {
                self.file_store_proof(slot, proof)?;
                Ok(StoreOutcome::Persisted)
            }
        }
    }

    /// Load the proof of one attachment, or `Ok(None)` when none is stored.
    ///
    /// # Errors
    /// Returns [`CredentialStoreError::Malformed`] for a proof of the wrong
    /// shape or slot, and [`CredentialStoreError::InsecurePermissions`] for a
    /// widened file or a symlink.
    pub fn load_attachment_proof(&self, slot: &AttachmentSlot) -> Result<Option<AttachmentProof>> {
        match self.backend {
            Backend::Env => Ok(None),
            Backend::Keyring => {
                let entry = keyring::Entry::new(KEYRING_SERVICE, &slot.keyring_account())
                    .map_err(|e| Error::from(backend_err(e)))?;
                match entry.get_password() {
                    Ok(text) => {
                        let text = Zeroizing::new(text);
                        parse_proof(&text).map(Some)
                    }
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(backend_err(e).into()),
                }
            }
            Backend::File => self.file_load_proof(slot),
        }
    }

    /// Remove the proof of one attachment. Removing an absent proof succeeds.
    ///
    /// # Errors
    /// Returns a backend or filesystem error.
    pub fn delete_attachment_proof(&self, slot: &AttachmentSlot) -> Result<()> {
        match self.backend {
            Backend::Env => Ok(()),
            Backend::Keyring => {
                let entry = keyring::Entry::new(KEYRING_SERVICE, &slot.keyring_account())
                    .map_err(|e| Error::from(backend_err(e)))?;
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(backend_err(e).into()),
                }
            }
            Backend::File => match fs::remove_file(self.proof_path(slot)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::Io(e)),
            },
        }
    }

    fn attachments_dir(&self) -> std::path::PathBuf {
        self.root.join("attachments")
    }

    fn proof_path(&self, slot: &AttachmentSlot) -> std::path::PathBuf {
        self.attachments_dir()
            .join(format!("{}.proof", slot.storage_id()))
    }

    fn file_store_proof(&self, slot: &AttachmentSlot, proof: &AttachmentProof) -> Result<()> {
        let dir = self.attachments_dir();
        fs::create_dir_all(&dir)?;
        harden_dir(&dir)?;
        let wire = ProofWire {
            schema_version: PROOF_SCHEMA,
            attachment: slot.attachment.to_string(),
            proof: proof.expose_secret().to_owned(),
        };
        let blob = Zeroizing::new(serde_json::to_string(&wire).map_err(Error::Json)?);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(
            ".{}.{}.{}.tmp",
            slot.storage_id(),
            std::process::id(),
            seq
        ));
        let final_path = self.proof_path(slot);
        if let Err(e) = write_tmp(&tmp, blob.as_bytes())
            .and_then(|()| fs::rename(&tmp, &final_path).map_err(Error::Io))
        {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        let _ = fsync_dir(&dir);
        Ok(())
    }

    fn file_load_proof(&self, slot: &AttachmentSlot) -> Result<Option<AttachmentProof>> {
        let path = self.proof_path(slot);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Io(e)),
        };
        if !meta.file_type().is_file() {
            return Err(CredentialStoreError::InsecurePermissions {
                path: path.display().to_string(),
            }
            .into());
        }
        let file = open_read(&path)?;
        check_permissions(&file, &path)?;
        let mut blob = Zeroizing::new(String::new());
        file.take(MAX_PROOF_FILE_BYTES + 1)
            .read_to_string(&mut blob)?;
        if blob.len() as u64 > MAX_PROOF_FILE_BYTES {
            return Err(malformed("proof file exceeds its size bound"));
        }
        let wire: ProofWire =
            serde_json::from_str(&blob).map_err(|_| malformed("proof file does not parse"))?;
        if wire.schema_version > PROOF_SCHEMA {
            return Err(malformed("proof file has an unsupported schema"));
        }
        if wire.attachment != slot.attachment.as_str() {
            return Err(malformed("proof file belongs to another attachment"));
        }
        parse_proof(&wire.proof).map(Some)
    }
}

fn parse_proof(text: &str) -> Result<AttachmentProof> {
    AttachmentProof::parse(text).map_err(|_| malformed("attachment proof has the wrong shape"))
}

fn malformed(reason: &str) -> Error {
    CredentialStoreError::Malformed {
        reason: reason.to_owned(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(sub: &str, attachment: u8) -> AttachmentSlot {
        AttachmentSlot::new(
            CredentialKey::new("https://idp", sub, "cosmon-rpp-demo"),
            AttachmentId::from_random([attachment; 8]),
        )
    }

    fn proof(byte: u8) -> AttachmentProof {
        AttachmentProof::from_secret_bytes([byte; 32])
    }

    #[test]
    fn proof_survives_a_new_store_instance_and_stays_private() {
        let dir = tempfile::tempdir().unwrap();
        let first = slot("pilot-a", 1);
        let outcome = CredentialStore::file_at(dir.path())
            .store_attachment_proof(&first, &proof(1))
            .unwrap();
        assert_eq!(outcome, StoreOutcome::Persisted);

        // A new instance stands for the next process after a restart.
        let reopened = CredentialStore::file_at(dir.path());
        let loaded = reopened.load_attachment_proof(&first).unwrap().unwrap();
        assert_eq!(loaded.expose_secret(), proof(1).expose_secret());

        let path = reopened.proof_path(&first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(!path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(first.attachment().as_str()));
        let litter: Vec<_> = fs::read_dir(reopened.attachments_dir())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(litter.is_empty());
    }

    #[test]
    fn two_attachments_of_one_identity_keep_distinct_proofs() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::file_at(dir.path());
        let laptop = slot("pilot-a", 1);
        let desktop = slot("pilot-a", 2);
        store.store_attachment_proof(&laptop, &proof(1)).unwrap();
        store.store_attachment_proof(&desktop, &proof(2)).unwrap();
        assert_ne!(laptop.storage_id(), desktop.storage_id());
        assert_ne!(
            laptop.storage_id(),
            laptop.key().storage_id(),
            "a proof slot must never alias a token slot"
        );
        let a = store.load_attachment_proof(&laptop).unwrap().unwrap();
        let b = store.load_attachment_proof(&desktop).unwrap().unwrap();
        assert_ne!(a.expose_secret(), b.expose_secret());

        store.delete_attachment_proof(&laptop).unwrap();
        store.delete_attachment_proof(&laptop).unwrap();
        assert!(store.load_attachment_proof(&laptop).unwrap().is_none());
        assert!(store.load_attachment_proof(&desktop).unwrap().is_some());
    }

    #[test]
    fn a_proof_copied_to_another_slot_or_widened_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::file_at(dir.path());
        let own = slot("pilot-a", 1);
        let other = slot("pilot-a", 2);
        store.store_attachment_proof(&own, &proof(1)).unwrap();
        fs::copy(store.proof_path(&own), store.proof_path(&other)).unwrap();
        assert!(store.load_attachment_proof(&other).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(store.proof_path(&own), fs::Permissions::from_mode(0o644)).unwrap();
            assert!(store.load_attachment_proof(&own).is_err());
        }
    }
}
