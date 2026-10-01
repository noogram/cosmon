// SPDX-License-Identifier: AGPL-3.0-only

//! The I/O half of ADR-172: where a harvest grant is read from, which key is
//! trusted to have sealed it, what the galaxy's current epoch is, and what has
//! already been spent.
//!
//! [`cosmon_core::harvest_authorization`] states the decision and holds no
//! answer, because every answer here means touching a file. This module is the
//! set of adapters that touch them.
//!
//! # Where the trust root is pinned, and why there
//!
//! Resolution order, first hit wins:
//!
//! 1. `$COSMON_HARVEST_PUBKEY` — an explicit path, for a key on removable
//!    media or a per-fleet override.
//! 2. `<galaxy>/.cosmon/harvest.pub` — a harvest-specific key.
//! 3. `<galaxy>/.cosmon/takeover.pub` — the galaxy's operator trust root.
//!
//! The third step is the "domain-separated subkey" ADR-172 §D2 permits, not a
//! reuse: the `cosmon-harvest-grant-v1` line means a harvest grant and a
//! takeover grant share no preimage, so one signature can never be replayed as
//! the other even under one key. What is *not* permitted, and is not done, is
//! reaching for the ADR-056 notary key: that one is a plaintext hex file the
//! agent can read, so signing with it would be theatre.
//!
//! All three live in the galaxy and outside `.cosmon/state/`, for the reason
//! [`crate::operator_trust`] gives: `state/` is runtime scratch nobody
//! reviews, and a file beside the galaxy's configuration is one an operator
//! can commit — which turns a key swap from an invisible act into a diff.
//!
//! # Absence is a refusal, not a permission
//!
//! With no key pinned, [`MinisignHarvestVerifier::resolve`] yields `None` and
//! the caller must refuse every grant. The alternative — "unverified when
//! unconfigured" — hands the beneficiary a one-command bypass: delete the key,
//! forge the grant.
//!
//! # The honest ceiling
//!
//! A process running as the operator can overwrite any file the operator can
//! write, this key included. That is not made impossible here. It is made
//! **recorded**: every consumption receipt carries the key id that authorised
//! it, so a substituted trust root appears as a key change in an append-only
//! ledger even if the `.pub` file is put back afterwards. And nothing in this
//! module claims a worker cannot mutate the trunk with git plumbing — only
//! that `cs done` will not perform an unauthorised harvest.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cosmon_core::config::{ProjectConfig, RemoteHarvestPolicy};
use cosmon_core::error::CosmonError;
use cosmon_core::harvest_authorization::{
    harvest_root_rotation_statement, ConsumptionRecord, DoneAuthorization, GrantEpoch,
    HarvestConsumptionLedger, HarvestGrant, HarvestJournalRecord, HarvestJournalStage,
    HarvestSealVerifier, PermitId,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::{AttestationError, OperatorAttestation, OperatorKeyId};
use cosmon_notary::minisign::{self, MinisignPublicKey, MinisignSignature};
use cosmon_state::StateStore;

/// Environment variable naming an explicit harvest trust-root path.
pub const HARVEST_PUBKEY_ENV: &str = "COSMON_HARVEST_PUBKEY";

/// Path of the harvest-specific trust root, relative to a galaxy root.
pub const HARVEST_PUBKEY_REL: &str = ".cosmon/harvest.pub";

/// Path of the galaxy's monotone grant epoch, relative to a galaxy root.
///
/// Beside the trust root and not under `.cosmon/state/`, because bumping it is
/// the operator's entire revocation gesture and must be a committed diff
/// rather than a scratch write.
pub const HARVEST_EPOCH_REL: &str = ".cosmon/harvest.epoch";

/// Durable interruption marker for a multi-file administrative update.
/// Authority reads refuse while it exists; a crash must never expose a
/// partially changed key, epoch and policy as one coherent version.
pub const HARVEST_AUTHORITY_PENDING_REL: &str = ".cosmon/harvest-authority.pending";

/// Append-only local recovery record, outside worker runtime state.
pub const HARVEST_ROOT_RESETS_REL: &str = ".cosmon/harvest-root-resets.log";

/// Directory holding sealed grants, relative to a cosmon state root.
///
/// Unlike the trust root, this one *may* live in worker-writable space: a
/// grant is worthless without the seal, so forging the file forges nothing.
pub const HARVEST_GRANTS_REL: &str = "harvest/grants";

/// The append-only consumption ledger, relative to a cosmon state root.
pub const HARVEST_CONSUMED_REL: &str = "harvest/consumed.jsonl";

/// Versioned progress records for prepared, integrated and finalized attempts.
pub const HARVEST_JOURNAL_REL: &str = "harvest/operations.jsonl";

/// Environment variable naming one explicit grant file to use.
pub const HARVEST_GRANT_ENV: &str = "COSMON_HARVEST_GRANT";

// ---------------------------------------------------------------------------
// Trust root
// ---------------------------------------------------------------------------

/// A verifier holding one pinned operator public key for harvest grants.
///
/// Deliberately a sibling of [`crate::MinisignOperatorVerifier`] rather than a
/// generalisation of it: the two answer different questions over different
/// preimages, and a single verifier parameterised by domain would be one edit
/// away from checking a takeover signature against a harvest grant.
#[derive(Debug, Clone)]
pub struct MinisignHarvestVerifier {
    key: MinisignPublicKey,
    source: PathBuf,
    content_digest: String,
}

impl MinisignHarvestVerifier {
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
            reason: format!("{} is not a minisign public key: {e}", source.display()),
        })?;
        Ok(Self {
            key,
            source,
            content_digest: cosmon_core::harvest_authorization::policy_digest(text.as_bytes()),
        })
    }

    /// Read a pinned key from `path`.
    ///
    /// # Errors
    ///
    /// [`CosmonError::StateStore`] when the file cannot be read or does not
    /// hold a minisign public key.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, CosmonError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to read harvest public key {}: {e}", path.display()),
        })?;
        Self::from_public_key_text(&text, path)
    }

    /// Find the pinned key for a galaxy, or `Ok(None)` when none is pinned.
    ///
    /// An explicit `$COSMON_HARVEST_PUBKEY` that does not exist is an
    /// **error**, not an absence: an operator who names a path meant it, and
    /// silently falling back would be the permissive reading of a typo.
    ///
    /// # Errors
    ///
    /// [`CosmonError::StateStore`] on an unreadable or malformed key file.
    pub fn resolve(galaxy_root: impl AsRef<Path>) -> Result<Option<Self>, CosmonError> {
        if let Some(explicit) = std::env::var_os(HARVEST_PUBKEY_ENV) {
            let path = PathBuf::from(explicit);
            if path.as_os_str().is_empty() {
                return Ok(None);
            }
            return Self::from_path(path).map(Some);
        }
        let galaxy_root = galaxy_root.as_ref();
        for rel in [HARVEST_PUBKEY_REL, crate::TAKEOVER_PUBKEY_REL] {
            let candidate = galaxy_root.join(rel);
            match std::fs::read_to_string(&candidate) {
                Ok(text) => return Self::from_public_key_text(&text, candidate).map(Some),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(CosmonError::StateStore {
                        reason: format!(
                            "failed to read harvest public key {}: {e}",
                            candidate.display()
                        ),
                    });
                }
            }
        }
        Ok(None)
    }

    /// Where this key was read from, for a diagnostic an operator can follow.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Digest of the exact trust-root bytes read for this verifier.
    ///
    /// The effect's preliminary and locked snapshots compare this alongside
    /// the source path so a key replacement cannot inherit an earlier gate.
    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }
}

impl HarvestSealVerifier for MinisignHarvestVerifier {
    fn verify(
        &self,
        grant: &HarvestGrant,
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
        minisign::verify(&self.key, &grant.canonical_bytes(), &parsed).map_err(|e| match e {
            minisign::MinisignError::BadSignature => AttestationError::DoesNotCoverTransfer,
            minisign::MinisignError::BadGlobalSignature => AttestationError::TrustedCommentUnsigned,
            other => AttestationError::Malformed(other.to_string()),
        })
    }

    fn trusted_key_id(&self) -> OperatorKeyId {
        OperatorKeyId::from_bytes(self.key.key_id())
    }
}

/// A verifier for a galaxy that has pinned nothing.
///
/// It exists so the fail-closed branch is a *type* the caller must handle
/// rather than an `Option` somebody can `unwrap_or(permit)`: every method
/// refuses, so wiring it in cannot accidentally widen authority.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHarvestTrustRoot;

impl HarvestSealVerifier for NoHarvestTrustRoot {
    fn verify(
        &self,
        _grant: &HarvestGrant,
        _attestation: &OperatorAttestation,
    ) -> Result<(), AttestationError> {
        Err(AttestationError::NoTrustRoot)
    }

    fn trusted_key_id(&self) -> OperatorKeyId {
        OperatorKeyId::from_bytes([0; 8])
    }
}

// ---------------------------------------------------------------------------
// Epoch
// ---------------------------------------------------------------------------

/// Read the galaxy's current grant epoch.
///
/// A missing file is [`GrantEpoch::first`], not an error: a galaxy that has
/// never revoked anything is at epoch one, and demanding the file exist would
/// make "no revocation yet" indistinguishable from "misconfigured".
///
/// A file that exists and does not hold a number **is** an error. Reading it
/// as one would let a corrupted or half-written epoch silently re-validate
/// every grant the operator meant to revoke.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when the file exists but cannot be read or does
/// not hold a decimal counter.
pub fn read_epoch(galaxy_root: impl AsRef<Path>) -> Result<GrantEpoch, CosmonError> {
    let pending = galaxy_root.as_ref().join(HARVEST_AUTHORITY_PENDING_REL);
    match std::fs::symlink_metadata(&pending) {
        Ok(_) => {
            return Err(CosmonError::StateStore {
                reason: "harvest_facts_unavailable: authority update needs reconciliation"
                    .to_owned(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(CosmonError::StateStore {
                reason: format!("harvest_facts_unavailable: authority marker: {e}"),
            });
        }
    }
    let path = galaxy_root.as_ref().join(HARVEST_EPOCH_REL);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GrantEpoch::first()),
        Err(e) => {
            return Err(CosmonError::StateStore {
                reason: format!("failed to read harvest epoch {}: {e}", path.display()),
            })
        }
    };
    raw.trim()
        .parse::<u64>()
        .map(GrantEpoch::from_u64)
        .map_err(|e| CosmonError::StateStore {
            reason: format!(
                "{} does not hold a grant epoch ({e}) — refusing rather than \
                 guessing, because a misread epoch un-revokes grants",
                path.display()
            ),
        })
}

/// Path of the tracked autonomous-harvest policy, relative to a galaxy root.
pub const HARVEST_POLICY_REL: &str = ".cosmon/harvest-policy.toml";

/// The digest of the autonomous policy a delegation is bound to.
///
/// A [`cosmon_core::harvest_authorization::HarvestScope::Mission`] grant seals
/// this digest, so editing the policy changes it and the delegation lapses —
/// with nobody notified and nothing to revoke. That is the arithmetic form of
/// revocation ADR-172 asks for, applied to policy rather than to the epoch.
///
/// A missing policy file digests the empty string rather than erroring: a
/// galaxy that has approved no autonomous policy has one, and it is empty. A
/// delegation signed against the empty digest is still a delegation the
/// operator sealed.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when the file exists but cannot be read.
pub fn read_policy_digest(galaxy_root: impl AsRef<Path>) -> Result<String, CosmonError> {
    let bytes = read_policy_bytes(galaxy_root)?;
    Ok(cosmon_core::harvest_authorization::policy_digest(
        bytes.as_deref().unwrap_or_default(),
    ))
}

/// Read the policy as an optional byte sequence, preserving the distinction
/// between no policy and an unreadable policy for authority diagnostics.
///
/// # Errors
///
/// Returns [`CosmonError::StateStore`] when an existing policy cannot be read.
pub fn read_policy_bytes(galaxy_root: impl AsRef<Path>) -> Result<Option<Vec<u8>>, CosmonError> {
    let path = galaxy_root.as_ref().join(HARVEST_POLICY_REL);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(CosmonError::StateStore {
                reason: format!("failed to read harvest policy {}: {e}", path.display()),
            })
        }
    };
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Grants on disk
// ---------------------------------------------------------------------------

/// Load the sealed authorisations available for `molecule`.
///
/// `$COSMON_HARVEST_GRANT`, when set, names exactly one file and nothing else
/// is read — an operator handing a specific grant to a specific invocation
/// meant that one. Otherwise every `*.json` under
/// `<state>/harvest/grants` is read. A storage read failure is a fault, and a
/// malformed candidate is reported by the diagnostic reader. The legacy
/// reader still skips torn JSON in a scanned directory for compatibility.
///
/// The molecule is *not* filtered on here. Scope is decided by
/// [`cosmon_core::harvest_authorization::authorize`] against facts re-derived
/// under the trunk lock; filtering here as well would put a second, weaker
/// copy of that rule in the loading path.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when grant storage is unreadable, an explicitly
/// selected candidate is invalid, or decoded semantics fail. A missing grants
/// directory yields an empty list; torn JSON in a scanned directory is skipped.
pub fn load_authorizations(
    state_root: impl AsRef<Path>,
) -> Result<Vec<DoneAuthorization>, CosmonError> {
    let candidates = load_authorizations_with_diagnostics(state_root)?;
    if candidates.legacy_invalid {
        return Err(CosmonError::StateStore {
            reason: "invalid harvest grant semantics or encoding".to_owned(),
        });
    }
    Ok(candidates.authorizations)
}

/// Candidate grants and whether an on-disk candidate was malformed.
/// The malformed flag is safe to classify; it never carries path or payload.
pub struct AuthorizationCandidates {
    /// Decoded and semantically valid candidates in deterministic order.
    pub authorizations: Vec<DoneAuthorization>,
    /// At least one candidate could not be decoded or validated.
    pub malformed: bool,
    // Preserve the historical loader's fault for explicit or semantically
    // invalid grants while allowing it to skip a torn JSON file.
    legacy_invalid: bool,
}

/// Load grants with a safe malformed-candidate classification for the effect.
/// A directory or file read failure remains a state fault, never absence.
///
/// # Errors
///
/// Returns [`CosmonError::StateStore`] for I/O failures reading grant storage.
pub fn load_authorizations_with_diagnostics(
    state_root: impl AsRef<Path>,
) -> Result<AuthorizationCandidates, CosmonError> {
    if let Some(explicit) = std::env::var_os(HARVEST_GRANT_ENV) {
        let path = PathBuf::from(explicit);
        if path.as_os_str().is_empty() {
            return Ok(AuthorizationCandidates {
                authorizations: Vec::new(),
                malformed: false,
                legacy_invalid: false,
            });
        }
        let text = std::fs::read_to_string(&path).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to read harvest grant {}: {e}", path.display()),
        })?;
        let one = serde_json::from_str::<DoneAuthorization>(&text)
            .ok()
            .filter(|one| one.validate().is_ok());
        return Ok(AuthorizationCandidates {
            malformed: one.is_none(),
            legacy_invalid: one.is_none(),
            authorizations: one.into_iter().collect(),
        });
    }

    let dir = state_root.as_ref().join(HARVEST_GRANTS_REL);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthorizationCandidates {
                authorizations: Vec::new(),
                malformed: false,
                legacy_invalid: false,
            });
        }
        Err(e) => {
            return Err(CosmonError::StateStore {
                reason: format!("failed to read harvest grants {}: {e}", dir.display()),
            })
        }
    };
    let mut out = Vec::new();
    let mut malformed = false;
    let mut legacy_invalid = false;
    for entry in entries {
        let entry = entry.map_err(|e| CosmonError::StateStore {
            reason: format!("failed to list harvest grants {}: {e}", dir.display()),
        })?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to read harvest grant {}: {e}", path.display()),
        })?;
        match serde_json::from_str::<DoneAuthorization>(&text) {
            Ok(parsed) if parsed.validate().is_ok() => out.push(parsed),
            Ok(_) => {
                malformed = true;
                legacy_invalid = true;
            }
            Err(_) => malformed = true,
        }
    }
    // Deterministic order, so which grant is tried first does not depend on
    // the directory's iteration order.
    out.sort_by_key(|a| a.grant().fingerprint().as_str().to_owned());
    Ok(AuthorizationCandidates {
        authorizations: out,
        malformed,
        legacy_invalid,
    })
}

/// Write a sealed authorisation to the grants directory.
///
/// This is a *transport* helper, not a signing path: it stores bytes an
/// operator already sealed elsewhere. Nothing here can produce a seal.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when the directory or file cannot be written.
pub fn store_authorization(
    state_root: impl AsRef<Path>,
    name: &MoleculeId,
    authorization: &DoneAuthorization,
) -> Result<PathBuf, CosmonError> {
    let dir = state_root.as_ref().join(HARVEST_GRANTS_REL);
    std::fs::create_dir_all(&dir).map_err(|e| CosmonError::StateStore {
        reason: format!("failed to create {}: {e}", dir.display()),
    })?;
    let path = dir.join(format!("{}.json", name.as_str()));
    let text =
        serde_json::to_string_pretty(authorization).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to encode harvest authorisation: {e}"),
        })?;
    std::fs::write(&path, format!("{text}\n")).map_err(|e| CosmonError::StateStore {
        reason: format!("failed to write {}: {e}", path.display()),
    })?;
    Ok(path)
}

/// Expected current state and requested administrative change. All expected
/// fields are mandatory as a group so a stale operator cannot replace a root
/// or policy after another writer moved any of the three authority axes.
#[derive(Debug, Clone)]
pub struct HarvestAuthorityUpdate {
    /// Previously observed explicit remote policy; `None` means legacy.
    pub expected_policy: Option<RemoteHarvestPolicy>,
    /// Digest of the effective public root, or `None` when none exists.
    pub expected_key_digest: Option<String>,
    /// Previously observed grant epoch.
    pub expected_epoch: GrantEpoch,
    /// Explicit remote policy selected by the operator.
    pub policy: RemoteHarvestPolicy,
    /// Optional public root, never private signing material.
    pub public_key: Option<String>,
    /// Optional replacement epoch. It may only increase.
    pub epoch: Option<GrantEpoch>,
}

/// Current public authority state, safe to return to an operator or tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestAuthorityState {
    /// Explicit policy, absent for a legacy galaxy.
    pub policy: Option<RemoteHarvestPolicy>,
    /// Digest of the effective public key bytes.
    pub key_digest: Option<String>,
    /// Current grant revocation epoch.
    pub epoch: GrantEpoch,
}

fn authority_fault(reason: impl Into<String>) -> CosmonError {
    CosmonError::StateStore {
        reason: reason.into(),
    }
}

/// Read the public, versioned administrative state without changing it.
///
/// # Errors
/// Refuses malformed configuration, epoch or public root.
pub fn authority_state(galaxy_root: &Path) -> Result<HarvestAuthorityState, CosmonError> {
    let config_path = galaxy_root.join(".cosmon/config.toml");
    let raw = std::fs::read_to_string(&config_path)
        .map_err(|_| authority_fault("harvest_facts_unavailable"))?;
    let config =
        ProjectConfig::parse(&raw).map_err(|_| authority_fault("harvest_facts_unavailable"))?;
    let verifier = MinisignHarvestVerifier::resolve(galaxy_root)?;
    Ok(HarvestAuthorityState {
        policy: config.harvest_authority.remote,
        key_digest: verifier.map(|v| v.content_digest().to_owned()),
        epoch: read_epoch(galaxy_root)?,
    })
}

fn refuse_symlink(path: &Path) -> Result<(), CosmonError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(authority_fault("harvest_destination_symlink"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(authority_fault("harvest_facts_unavailable")),
    }
}

static NEXT_AUTHORITY_WRITE: AtomicU64 = AtomicU64::new(0);

fn atomic_authority_write(path: &Path, bytes: &[u8]) -> Result<(), CosmonError> {
    refuse_symlink(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| authority_fault("harvest_write_failed"))?;
    let serial = NEXT_AUTHORITY_WRITE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".harvest-write-{}-{serial}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| authority_fault("harvest_write_failed"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| authority_fault("harvest_write_failed"))?;
        // A cooperating writer holds trunk.lock. Recheck the destination
        // immediately before rename to refuse an intervening symlink edit.
        refuse_symlink(path)?;
        std::fs::rename(&temp, path).map_err(|_| authority_fault("harvest_write_failed"))?;
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| authority_fault("harvest_write_failed"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Compare and set policy, public root and epoch under the shared trunk lock.
/// Configuration is written last so a newly enabled sealed profile never
/// points at a root that has yet to be installed. No signing operation exists
/// on this path.
///
/// # Errors
/// Refuses stale expectations, a local-policy conflict, key rotation without
/// revocation, unsafe destinations and any I/O failure.
pub fn configure_harvest_authority(
    galaxy_root: &Path,
    update: &HarvestAuthorityUpdate,
) -> Result<HarvestAuthorityState, CosmonError> {
    configure_authority(galaxy_root, update, None)
}

/// Apply an API update, requiring a signature from the installed root on rotation.
/// The tenant name is the admitted route tenant, not a request body field.
///
/// # Errors
/// Refuses missing, malformed or non-current signatures and all ordinary CAS errors.
pub fn configure_remote_harvest_authority(
    galaxy_root: &Path,
    tenant: &str,
    update: &HarvestAuthorityUpdate,
    rotation_signature: Option<&str>,
) -> Result<HarvestAuthorityState, CosmonError> {
    configure_authority(galaxy_root, update, Some((tenant, rotation_signature)))
}

fn verify_root_rotation(
    galaxy_root: &Path,
    tenant: &str,
    prior: &str,
    key: &str,
    epoch: GrantEpoch,
    signature: Option<&str>,
) -> Result<(), CosmonError> {
    let signature =
        signature.ok_or_else(|| authority_fault("harvest_rotation_signature_required"))?;
    let statement = harvest_root_rotation_statement(tenant, prior, key, epoch)
        .map_err(|_| authority_fault("harvest_rotation_signature_invalid"))?;
    let verifier = MinisignHarvestVerifier::resolve(galaxy_root)?
        .ok_or_else(|| authority_fault("harvest_facts_unavailable"))?;
    if verifier.content_digest() != prior {
        return Err(authority_fault("harvest_authority_changed"));
    }
    let parsed = MinisignSignature::parse(signature)
        .map_err(|_| authority_fault("harvest_rotation_signature_invalid"))?;
    minisign::verify(&verifier.key, &statement, &parsed)
        .map_err(|_| authority_fault("harvest_rotation_signature_invalid"))
}

fn record_local_root_reset(
    galaxy_root: &Path,
    prior: &str,
    new_digest: &str,
    epoch: GrantEpoch,
) -> Result<(), CosmonError> {
    let path = galaxy_root.join(HARVEST_ROOT_RESETS_REL);
    refuse_symlink(&path)?;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| authority_fault("harvest_write_failed"))?;
    log.write_all(
        format!(
            "harvest-root-reset-v1 prior={prior} new={new_digest} epoch={} intent=local\n",
            epoch.as_u64()
        )
        .as_bytes(),
    )
    .and_then(|()| log.sync_all())
    .map_err(|_| authority_fault("harvest_write_failed"))
}

fn configured_document(raw: &str, policy: RemoteHarvestPolicy) -> Result<String, CosmonError> {
    let mut document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| authority_fault("harvest_facts_unavailable"))?;
    if !document.contains_key("harvest_authority") {
        document["harvest_authority"] = toml_edit::table();
    }
    let policy = match policy {
        RemoteHarvestPolicy::Disabled => "disabled",
        RemoteHarvestPolicy::Scoped => "scoped",
        RemoteHarvestPolicy::Sealed => "sealed",
    };
    document["harvest_authority"]["remote"] = toml_edit::value(policy);
    let result = document.to_string();
    ProjectConfig::parse(&result).map_err(|_| authority_fault("harvest_policy_conflict"))?;
    Ok(result)
}

fn configure_authority(
    galaxy_root: &Path,
    update: &HarvestAuthorityUpdate,
    remote: Option<(&str, Option<&str>)>,
) -> Result<HarvestAuthorityState, CosmonError> {
    let state_root = galaxy_root.join(".cosmon/state");
    let store = crate::FileStore::new(state_root);
    let _guard = store.lock_trunk("harvest authority configure")?;
    let config_path = galaxy_root.join(".cosmon/config.toml");
    let key_path = galaxy_root.join(HARVEST_PUBKEY_REL);
    let epoch_path = galaxy_root.join(HARVEST_EPOCH_REL);
    let pending_path = galaxy_root.join(HARVEST_AUTHORITY_PENDING_REL);
    refuse_symlink(galaxy_root)?;
    refuse_symlink(&galaxy_root.join(".cosmon"))?;
    for path in [&config_path, &key_path, &epoch_path] {
        refuse_symlink(path)?;
    }
    let current = authority_state(galaxy_root)?;
    if current.policy != update.expected_policy
        || current.key_digest != update.expected_key_digest
        || current.epoch != update.expected_epoch
    {
        return Err(authority_fault("harvest_authority_changed"));
    }
    let raw = std::fs::read_to_string(&config_path)
        .map_err(|_| authority_fault("harvest_facts_unavailable"))?;
    let parsed =
        ProjectConfig::parse(&raw).map_err(|_| authority_fault("harvest_facts_unavailable"))?;
    if update.policy == RemoteHarvestPolicy::Scoped && parsed.harvest_authority.required {
        return Err(authority_fault("harvest_policy_conflict"));
    }
    if std::env::var_os(HARVEST_PUBKEY_ENV).is_some() && update.public_key.is_some() {
        return Err(authority_fault("harvest_key_source_conflict"));
    }
    let next_epoch = update.epoch.unwrap_or(current.epoch);
    if next_epoch.as_u64() < current.epoch.as_u64() {
        return Err(authority_fault("harvest_epoch_rollback"));
    }
    if let Some(key) = &update.public_key {
        if key.len() > 16 * 1024 || key.contains("PRIVATE KEY") {
            return Err(authority_fault("harvest_public_key_invalid"));
        }
        MinisignHarvestVerifier::from_public_key_text(key, &key_path)
            .map_err(|_| authority_fault("harvest_public_key_invalid"))?;
        let digest = cosmon_core::harvest_authorization::policy_digest(key.as_bytes());
        if current.key_digest.is_some()
            && current.key_digest.as_deref() != Some(digest.as_str())
            && next_epoch.as_u64() <= current.epoch.as_u64()
        {
            return Err(authority_fault("harvest_rotation_requires_epoch_bump"));
        }
        if let Some(prior) = current
            .key_digest
            .as_deref()
            .filter(|prior| *prior != digest)
        {
            if let Some((tenant, signature)) = remote {
                verify_root_rotation(galaxy_root, tenant, prior, key, next_epoch, signature)?;
            }
        }
    }
    let document = configured_document(&raw, update.policy)?;
    if remote.is_none() {
        if let (Some(prior), Some(key)) =
            (current.key_digest.as_deref(), update.public_key.as_deref())
        {
            let new_digest = cosmon_core::harvest_authorization::policy_digest(key.as_bytes());
            if prior != new_digest {
                record_local_root_reset(galaxy_root, prior, &new_digest, next_epoch)?;
            }
        }
    }
    // The marker is itself durable before the first replacement. A crash or
    // write failure leaves it in place, so every subsequent effect refuses
    // instead of combining old and new authority facts.
    let mut pending_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending_path)
        .map_err(|_| authority_fault("harvest_recovery_required"))?;
    pending_file
        .write_all(b"harvest-authority-update-v1\n")
        .and_then(|()| pending_file.sync_all())
        .map_err(|_| authority_fault("harvest_write_failed"))?;
    let authority_dir = galaxy_root.join(".cosmon");
    std::fs::File::open(&authority_dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| authority_fault("harvest_write_failed"))?;
    if next_epoch != current.epoch {
        atomic_authority_write(&epoch_path, format!("{}\n", next_epoch.as_u64()).as_bytes())?;
    }
    if let Some(key) = &update.public_key {
        if current.key_digest.as_deref()
            != Some(cosmon_core::harvest_authorization::policy_digest(key.as_bytes()).as_str())
        {
            atomic_authority_write(&key_path, key.as_bytes())?;
        }
    }
    atomic_authority_write(&config_path, document.as_bytes())?;
    std::fs::remove_file(&pending_path).map_err(|_| authority_fault("harvest_write_failed"))?;
    std::fs::File::open(&authority_dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| authority_fault("harvest_write_failed"))?;
    authority_state(galaxy_root)
}

/// Install a pre-signed grant without allowing a symlink or torn write to
/// replace a prior candidate. The caller must verify signature and current
/// facts while holding the trunk lock before invoking this helper.
///
/// # Errors
/// Refuses a malformed authorization or a conflicting destination.
pub fn store_verified_authorization(
    state_root: &Path,
    authorization: &DoneAuthorization,
) -> Result<String, CosmonError> {
    authorization
        .validate()
        .map_err(|_| authority_fault("harvest_grant_invalid"))?;
    let harvest_dir = state_root.join("harvest");
    let dir = state_root.join(HARVEST_GRANTS_REL);
    for path in [state_root, harvest_dir.as_path(), dir.as_path()] {
        refuse_symlink(path)?;
    }
    std::fs::create_dir_all(&dir).map_err(|_| authority_fault("harvest_write_failed"))?;
    for path in [harvest_dir.as_path(), dir.as_path()] {
        refuse_symlink(path)?;
    }
    let fingerprint = authorization.grant().fingerprint().to_string();
    let path = dir.join(format!("{fingerprint}.json"));
    refuse_symlink(&path)?;
    let encoded =
        serde_json::to_vec(authorization).map_err(|_| authority_fault("harvest_grant_invalid"))?;
    match std::fs::read(&path) {
        Ok(existing) if existing == encoded => return Ok(fingerprint),
        Ok(_) => return Err(authority_fault("harvest_grant_conflict")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(authority_fault("harvest_write_failed")),
    }
    atomic_authority_write(&path, &encoded)?;
    Ok(fingerprint)
}

// ---------------------------------------------------------------------------
// Consumption ledger
// ---------------------------------------------------------------------------

/// The append-only file recording which permits have been spent.
///
/// Append-only and never rewritten, so a receipt cannot be quietly withdrawn
/// to re-enable a spend. The lookup is by
/// [`PermitId`], which is what makes a retry after a crash idempotent instead
/// of merely refused.
#[derive(Debug, Clone)]
pub struct FileConsumptionLedger {
    path: PathBuf,
}

impl FileConsumptionLedger {
    /// The ledger under a cosmon state root.
    #[must_use]
    pub fn at_state_root(state_root: impl AsRef<Path>) -> Self {
        Self {
            path: state_root.as_ref().join(HARVEST_CONSUMED_REL),
        }
    }

    /// The ledger file itself, for a diagnostic an operator can follow.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether any legacy or current receipt names this molecule. A retry
    /// must discover this before invoking a non-idempotent pre-hook.
    ///
    /// # Errors
    /// I/O or malformed receipt errors.
    pub fn has_receipt_for(&self, molecule: &MoleculeId) -> Result<bool, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("failed to read harvest receipts: {e}")),
        };
        let mut found = false;
        for line in text.lines() {
            let record: ConsumptionRecord = serde_json::from_str(line)
                .map_err(|e| format!("malformed harvest receipt: {e}"))?;
            found |= &record.effect.molecule == molecule;
        }
        Ok(found)
    }
}

impl HarvestConsumptionLedger for FileConsumptionLedger {
    fn recorded(&self, permit: &PermitId) -> Result<Option<ConsumptionRecord>, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            // A ledger that has never been written holds no receipts. Any
            // other error is reported: reporting it as "unspent" would turn a
            // read failure into a second harvest.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("failed to read {}: {e}", self.path.display())),
        };
        let mut found = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // A torn or malformed line may be the only trace of a spend. Its
            // permit cannot be determined safely, so every lookup fails closed.
            let record: ConsumptionRecord = serde_json::from_str(line)
                .map_err(|e| format!("malformed harvest receipt: {e}"))?;
            if &record.permit == permit {
                if let Some(previous) = &found {
                    if previous != &record {
                        return Err(format!("conflicting harvest receipts for permit {permit}"));
                    }
                }
                found = Some(record);
            }
        }
        Ok(found)
    }

    fn consume(&self, record: &ConsumptionRecord) -> Result<(), String> {
        use std::io::Write as _;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        let line = serde_json::to_string(record)
            .map_err(|e| format!("failed to encode consumption receipt: {e}"))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("failed to open {}: {e}", self.path.display()))?;
        writeln!(file, "{line}")
            .map_err(|e| format!("failed to append to {}: {e}", self.path.display()))?;
        // The receipt must be durable before the merge starts, or a crash
        // between the two leaves a spent permit that reads as unspent.
        file.sync_all()
            .map_err(|e| format!("failed to flush {}: {e}", self.path.display()))?;
        Ok(())
    }
}

/// The append-only progress journal, separate from historical v1 receipts.
#[derive(Debug, Clone)]
pub struct FileHarvestJournal {
    path: PathBuf,
}

impl FileHarvestJournal {
    /// Resolve the journal beneath a state root without changing that state.
    #[must_use]
    pub fn at_state_root(state_root: impl AsRef<Path>) -> Self {
        Self {
            path: state_root.as_ref().join(HARVEST_JOURNAL_REL),
        }
    }

    /// Return the latest transition for one permit; malformed or conflicting
    /// lines are errors because an unknown operation must never be replayed.
    ///
    /// # Errors
    /// I/O, decoding, version, or transition errors.
    pub fn latest(&self, permit: &PermitId) -> Result<Option<HarvestJournalRecord>, String> {
        Ok(self.read_progress()?.remove(permit))
    }

    fn read_progress(
        &self,
    ) -> Result<std::collections::BTreeMap<PermitId, HarvestJournalRecord>, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(std::collections::BTreeMap::default())
            }
            Err(e) => return Err(format!("failed to read harvest journal: {e}")),
        };
        let mut progress = std::collections::BTreeMap::<PermitId, HarvestJournalRecord>::new();
        let mut operations = std::collections::HashMap::<String, PermitId>::new();
        for (line_number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                return Err(format!("empty harvest journal line {}", line_number + 1));
            }
            let entry: HarvestJournalRecord = serde_json::from_str(line)
                .map_err(|e| format!("malformed harvest journal line {}: {e}", line_number + 1))?;
            if entry.version != 1 {
                return Err(format!(
                    "unsupported harvest journal version {}",
                    entry.version
                ));
            }
            let permit = entry.receipt.permit.clone();
            if let Some(other) =
                operations.insert(entry.receipt.invocation_id.clone(), permit.clone())
            {
                if other != permit {
                    return Err("harvest operation id belongs to multiple permits".to_owned());
                }
            }
            validate_transition(progress.get(&permit), &entry)?;
            progress.insert(permit, entry);
        }
        Ok(progress)
    }

    /// Whether this molecule has any recorded attempt. Used only to avoid
    /// replaying a pre-integration hook before the locked recovery decision.
    ///
    /// # Errors
    /// I/O or malformed journal errors.
    pub fn has_attempt_for(&self, molecule: &MoleculeId) -> Result<bool, String> {
        Ok(self
            .read_progress()?
            .values()
            .any(|entry| &entry.receipt.effect.molecule == molecule))
    }

    /// Append and sync one legal progress transition before its successor may
    /// act. The caller holds the trunk lock across this read and append.
    ///
    /// # Errors
    /// I/O or invalid transition errors.
    pub fn append(&self, entry: &HarvestJournalRecord) -> Result<(), String> {
        use std::io::Write as _;
        let prior = self.latest(&entry.receipt.permit)?;
        validate_transition(prior.as_ref(), entry)?;
        let parent = self.path.parent().ok_or("harvest journal has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("create harvest journal dir: {e}"))?;
        let line =
            serde_json::to_string(entry).map_err(|e| format!("encode harvest journal: {e}"))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open harvest journal: {e}"))?;
        writeln!(file, "{line}").map_err(|e| format!("append harvest journal: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("sync harvest journal: {e}"))?;
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("sync harvest journal directory: {e}"))?;
        Ok(())
    }
}

fn validate_transition(
    prior: Option<&HarvestJournalRecord>,
    entry: &HarvestJournalRecord,
) -> Result<(), String> {
    if entry.version != 1
        || entry.receipt.invocation_id.is_empty()
        || entry.pre_merge_base.is_empty()
        || entry.options_digest.is_empty()
        || entry.hook_digest.is_empty()
    {
        return Err("invalid harvest journal identity".to_owned());
    }
    match entry.stage {
        HarvestJournalStage::Prepared if entry.merge_oid.is_some() => {
            return Err("prepared harvest names a merge".to_owned())
        }
        HarvestJournalStage::Integrated if entry.merge_oid.is_none() => {
            return Err("integrated harvest lacks a merge oid".to_owned())
        }
        HarvestJournalStage::Finalized
            if entry.merge_oid.is_none() && entry.branch_head.is_some() =>
        {
            return Err("finalized branch harvest lacks a merge oid".to_owned())
        }
        _ => {}
    }
    match prior {
        None if entry.stage == HarvestJournalStage::Prepared => Ok(()),
        Some(old)
            if old.receipt == entry.receipt
                && old.branch_head == entry.branch_head
                && old.pre_merge_base == entry.pre_merge_base
                && old.options_digest == entry.options_digest
                && old.hook_digest == entry.hook_digest
                && (matches!(
                    (old.stage, entry.stage),
                    (
                        HarvestJournalStage::Prepared,
                        HarvestJournalStage::Integrated
                    ) | (
                        HarvestJournalStage::Integrated,
                        HarvestJournalStage::Finalized
                    )
                ) || (old.stage == HarvestJournalStage::Prepared
                    && entry.stage == HarvestJournalStage::Finalized
                    && entry.branch_head.is_none()))
                && (old.merge_oid.is_none() || old.merge_oid == entry.merge_oid) =>
        {
            Ok(())
        }
        _ => Err("conflicting or out-of-order harvest journal transition".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::harvest_authorization::{
        HarvestAction, HarvestEffect, HarvestScope, OperatorHarvestSeal,
    };
    use tempfile::tempdir;

    const PUBLIC_KEY: &str = "untrusted comment: minisign public key D9E8177654011BB4\n\
        RWS0GwFUdhfo2cXncCJhDMZm6ICY0A8vKStQI2LO4//C4saj3AAlazcj\n";

    fn mol(raw: &str) -> MoleculeId {
        MoleculeId::new(raw).expect("fixture molecule id")
    }

    fn authorization(molecule: &MoleculeId) -> DoneAuthorization {
        let grant = HarvestGrant::new(
            "cosmon",
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            "main",
            HarvestAction::Done,
            Vec::<String>::new(),
            GrantEpoch::first(),
            None,
        )
        .expect("fixture grant");
        DoneAuthorization::Ratified(
            OperatorHarvestSeal::new(
                grant,
                OperatorAttestation {
                    key_id: OperatorKeyId::from_bytes([9; 8]),
                    signature: "AAAA".to_owned(),
                    global_signature: "BBBB".to_owned(),
                    trusted_comment: "fixture".to_owned(),
                    untrusted_comment: "fixture".to_owned(),
                },
            )
            .expect("fixture seal"),
        )
    }

    #[test]
    fn a_galaxy_with_no_pinned_key_resolves_to_none() {
        let dir = tempdir().expect("tempdir");
        std::env::remove_var(HARVEST_PUBKEY_ENV);
        assert!(MinisignHarvestVerifier::resolve(dir.path())
            .expect("resolve")
            .is_none());
    }

    #[test]
    fn a_harvest_key_is_preferred_over_the_takeover_key() {
        let dir = tempdir().expect("tempdir");
        std::env::remove_var(HARVEST_PUBKEY_ENV);
        std::fs::create_dir_all(dir.path().join(".cosmon")).expect("mkdir");
        std::fs::write(dir.path().join(crate::TAKEOVER_PUBKEY_REL), PUBLIC_KEY).expect("write");
        let takeover_only = MinisignHarvestVerifier::resolve(dir.path())
            .expect("resolve")
            .expect("the operator trust root is a valid fallback");
        assert!(takeover_only.source().ends_with("takeover.pub"));

        std::fs::write(dir.path().join(HARVEST_PUBKEY_REL), PUBLIC_KEY).expect("write");
        let harvest = MinisignHarvestVerifier::resolve(dir.path())
            .expect("resolve")
            .expect("a harvest key is pinned");
        assert!(harvest.source().ends_with("harvest.pub"));
    }

    #[test]
    fn no_trust_root_refuses_every_grant() {
        let molecule = mol("task-20260901-6da6");
        let auth = authorization(&molecule);
        assert_eq!(
            NoHarvestTrustRoot.verify(auth.grant(), auth.attestation()),
            Err(AttestationError::NoTrustRoot)
        );
    }

    #[test]
    fn a_galaxy_that_never_revoked_is_at_the_first_epoch() {
        let dir = tempdir().expect("tempdir");
        assert_eq!(read_epoch(dir.path()).expect("epoch"), GrantEpoch::first());
    }

    #[test]
    fn an_unreadable_epoch_is_an_error_and_not_a_default() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".cosmon")).expect("mkdir");
        std::fs::write(dir.path().join(HARVEST_EPOCH_REL), "not a number\n").expect("write");
        assert!(
            read_epoch(dir.path()).is_err(),
            "a corrupt epoch must refuse, never silently un-revoke"
        );
    }

    #[test]
    fn a_stored_grant_round_trips() {
        let dir = tempdir().expect("tempdir");
        std::env::remove_var(HARVEST_GRANT_ENV);
        let molecule = mol("task-20260901-6da6");
        let auth = authorization(&molecule);
        store_authorization(dir.path(), &molecule, &auth).expect("store");
        let loaded = load_authorizations(dir.path()).expect("load");
        assert_eq!(loaded, vec![auth]);
    }

    #[test]
    fn a_torn_grant_file_is_skipped_rather_than_fatal() {
        let dir = tempdir().expect("tempdir");
        std::env::remove_var(HARVEST_GRANT_ENV);
        let grants = dir.path().join(HARVEST_GRANTS_REL);
        std::fs::create_dir_all(&grants).expect("mkdir");
        std::fs::write(grants.join("torn.json"), "{ not json").expect("write");
        assert!(load_authorizations(dir.path()).expect("load").is_empty());
    }

    #[test]
    fn a_receipt_is_found_by_permit_so_a_retry_is_idempotent() {
        let dir = tempdir().expect("tempdir");
        let molecule = mol("task-20260901-6da6");
        let auth = authorization(&molecule);
        let ledger = FileConsumptionLedger::at_state_root(dir.path());
        let permit = auth.permit_id(&molecule);

        assert_eq!(ledger.recorded(&permit).expect("lookup"), None);

        let record = ConsumptionRecord {
            permit: permit.clone(),
            grant: auth.grant().fingerprint(),
            effect: HarvestEffect {
                galaxy: "cosmon".to_owned(),
                molecule: molecule.clone(),
                base: "main".to_owned(),
            },
            key_id: OperatorKeyId::from_bytes([9; 8]),
            invocation_id: "inv-1".to_owned(),
        };
        ledger.consume(&record).expect("consume");
        assert_eq!(ledger.recorded(&permit).expect("lookup"), Some(record));
    }

    #[test]
    fn a_torn_receipt_cannot_be_read_as_an_unspent_permit() {
        let dir = tempdir().expect("tempdir");
        let molecule = mol("task-20260901-6da6");
        let permit = authorization(&molecule).permit_id(&molecule);
        let ledger = FileConsumptionLedger::at_state_root(dir.path());
        std::fs::create_dir_all(ledger.path().parent().expect("parent")).expect("mkdir");
        std::fs::write(ledger.path(), "{\"permit\":\n").expect("torn receipt");
        assert!(
            ledger.recorded(&permit).is_err(),
            "a torn spend is unknown, never free"
        );
    }

    #[test]
    fn the_ledger_is_append_only_so_a_second_receipt_does_not_erase_the_first() {
        let dir = tempdir().expect("tempdir");
        let ledger = FileConsumptionLedger::at_state_root(dir.path());
        for (raw, inv) in [
            ("task-20260901-6da6", "inv-1"),
            ("task-20260901-aaaa", "inv-2"),
        ] {
            let molecule = mol(raw);
            let auth = authorization(&molecule);
            ledger
                .consume(&ConsumptionRecord {
                    permit: auth.permit_id(&molecule),
                    grant: auth.grant().fingerprint(),
                    effect: HarvestEffect {
                        galaxy: "cosmon".to_owned(),
                        molecule,
                        base: "main".to_owned(),
                    },
                    key_id: OperatorKeyId::from_bytes([9; 8]),
                    invocation_id: inv.to_owned(),
                })
                .expect("consume");
        }
        let text = std::fs::read_to_string(ledger.path()).expect("read ledger");
        assert_eq!(text.lines().count(), 2);
    }
}
