// SPDX-License-Identifier: AGPL-3.0-only

//! The trust root for operator gestures: a pinned minisign public key.
//!
//! [`cosmon_core::operator_attestation`] states the question — *is this grant
//! an operator's gesture?* — and holds no answer, because answering it means
//! reading a file. This module is the adapter that reads it.
//!
//! # Where the key is pinned, and why there
//!
//! Resolution order, first hit wins:
//!
//! 1. `$COSMON_TAKEOVER_PUBKEY` — an explicit path, for a key on removable
//!    media or a per-fleet override.
//! 2. `<galaxy>/.cosmon/takeover.pub` — the default.
//!
//! The default deliberately sits in the galaxy and not in `~/.config`, and
//! deliberately outside `.cosmon/state/`. `state/` is runtime scratch nobody
//! reviews; a file beside the galaxy's configuration is one an operator can
//! commit, and a committed trust root turns "the agent swapped the key" from
//! an invisible act into a line in a diff.
//!
//! That is the honest ceiling of this mechanism. A process running as the
//! operator can overwrite any file the operator can write, this one included.
//! What it cannot do is overwrite it *quietly*: the key id is recorded in
//! every grant, so a substituted root shows up as a changed key in an
//! append-only ledger even if the `.pub` file itself is put back afterwards.
//!
//! # Absence is a refusal, not a permission
//!
//! With no key pinned, [`PilotLeaseStore`](crate::PilotLeaseStore) honours no
//! grant at all. The alternative — "unverified when unconfigured" — would hand
//! the beneficiary a one-command bypass: delete the key, forge the grant. Here
//! deleting the key stops transfers instead of unlocking them, which is the
//! same fail-closed shape [`cosmon_core::pilot_lease::authorize`] already has.

use std::path::{Path, PathBuf};

use cosmon_core::error::CosmonError;
use cosmon_core::operator_attestation::{
    AttestationError, GrantChallenge, OperatorAttestation, OperatorGestureVerifier, OperatorKeyId,
};
use cosmon_notary::minisign::{self, MinisignPublicKey, MinisignSignature};

/// Environment variable naming an explicit trust-root path.
pub const TAKEOVER_PUBKEY_ENV: &str = "COSMON_TAKEOVER_PUBKEY";

/// Path of the default trust root, relative to a galaxy root.
pub const TAKEOVER_PUBKEY_REL: &str = ".cosmon/takeover.pub";

/// A verifier holding one pinned operator public key.
#[derive(Debug, Clone)]
pub struct MinisignOperatorVerifier {
    key: MinisignPublicKey,
    source: PathBuf,
}

impl MinisignOperatorVerifier {
    /// Build a verifier from the text of a minisign `.pub` file.
    ///
    /// # Errors
    ///
    /// [`CosmonError::StateStore`] when the text is not a minisign public key.
    pub fn from_public_key_text(
        text: &str,
        source: impl Into<PathBuf>,
    ) -> Result<Self, CosmonError> {
        let source = source.into();
        let key = MinisignPublicKey::parse(text).map_err(|e| CosmonError::StateStore {
            reason: format!(
                "{} is not a minisign public key: {e}",
                source.display()
            ),
        })?;
        Ok(Self { key, source })
    }

    /// Read a pinned key from `path`.
    ///
    /// # Errors
    ///
    /// [`CosmonError::StateStore`] when the file cannot be read or does not
    /// hold a minisign public key. A missing file is an error here and an
    /// absent trust root at the call site — see [`Self::resolve`].
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, CosmonError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to read operator public key {}: {e}", path.display()),
        })?;
        Self::from_public_key_text(&text, path)
    }

    /// Find the pinned key for a galaxy, or `Ok(None)` when none is pinned.
    ///
    /// An explicit `$COSMON_TAKEOVER_PUBKEY` that does not exist is an
    /// **error**, not an absence: an operator who names a path meant it, and
    /// silently falling back would be the permissive reading of a typo.
    ///
    /// # Errors
    ///
    /// [`CosmonError::StateStore`] on an unreadable or malformed key file.
    pub fn resolve(galaxy_root: impl AsRef<Path>) -> Result<Option<Self>, CosmonError> {
        if let Some(explicit) = std::env::var_os(TAKEOVER_PUBKEY_ENV) {
            let path = PathBuf::from(explicit);
            if path.as_os_str().is_empty() {
                return Ok(None);
            }
            return Self::from_path(path).map(Some);
        }
        let default = galaxy_root.as_ref().join(TAKEOVER_PUBKEY_REL);
        if default.exists() {
            return Self::from_path(default).map(Some);
        }
        Ok(None)
    }

    /// Find the pinned key from a cosmon **state root** (`<galaxy>/.cosmon/state`).
    ///
    /// The convenience the CLI actually has: it knows where state lives, and
    /// the trust root is its grandparent's `.cosmon/takeover.pub`.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`].
    pub fn resolve_for_state_root(state_root: impl AsRef<Path>) -> Result<Option<Self>, CosmonError> {
        let galaxy_root = state_root
            .as_ref()
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self::resolve(galaxy_root)
    }

    /// Where this key was read from, for a diagnostic an operator can follow.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }
}

impl OperatorGestureVerifier for MinisignOperatorVerifier {
    fn verify(
        &self,
        challenge: &GrantChallenge,
        attestation: &OperatorAttestation,
    ) -> Result<(), AttestationError> {
        let parsed = MinisignSignature::parse(&attestation.to_minisig_file())
            .map_err(|e| AttestationError::Malformed(e.to_string()))?;
        if parsed.key_id != self.key.key_id() {
            return Err(AttestationError::UnknownKey {
                presented: OperatorKeyId::from_bytes(parsed.key_id),
                trusted: self.trusted_key_id(),
            });
        }
        minisign::verify(&self.key, &challenge.canonical_bytes(), &parsed).map_err(|e| match e {
            minisign::MinisignError::BadSignature => AttestationError::DoesNotCoverTransfer,
            minisign::MinisignError::BadGlobalSignature => {
                AttestationError::TrustedCommentUnsigned
            }
            other => AttestationError::Malformed(other.to_string()),
        })
    }

    fn trusted_key_id(&self) -> OperatorKeyId {
        OperatorKeyId::from_bytes(self.key.key_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const PUBLIC_KEY: &str = "untrusted comment: minisign public key D9E8177654011BB4\n\
        RWS0GwFUdhfo2cXncCJhDMZm6ICY0A8vKStQI2LO4//C4saj3AAlazcj\n";

    #[test]
    fn a_pinned_key_reports_the_id_an_operator_can_compare_by_eye() {
        let v = MinisignOperatorVerifier::from_public_key_text(PUBLIC_KEY, "test.pub")
            .expect("parse pinned key");
        assert_eq!(v.trusted_key_id().to_string(), "D9E8177654011BB4");
    }

    #[test]
    fn a_galaxy_with_no_pinned_key_resolves_to_none() {
        let dir = tempdir().expect("tempdir");
        // Guard against an inherited override leaking into the assertion.
        std::env::remove_var(TAKEOVER_PUBKEY_ENV);
        assert!(MinisignOperatorVerifier::resolve(dir.path())
            .expect("resolve")
            .is_none());
    }

    #[test]
    fn a_key_file_that_is_not_a_key_is_an_error_and_not_an_absence() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("bad.pub");
        std::fs::write(&path, "not a key\n").expect("write");
        assert!(MinisignOperatorVerifier::from_path(&path).is_err());
    }
}
