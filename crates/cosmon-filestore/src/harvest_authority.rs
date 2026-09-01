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

use std::path::{Path, PathBuf};

use cosmon_core::error::CosmonError;
use cosmon_core::harvest_authorization::{
    ConsumptionRecord, DoneAuthorization, GrantEpoch, HarvestConsumptionLedger, HarvestGrant,
    HarvestSealVerifier, PermitId,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::{
    AttestationError, OperatorAttestation, OperatorKeyId,
};
use cosmon_notary::minisign::{self, MinisignPublicKey, MinisignSignature};

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

/// Directory holding sealed grants, relative to a cosmon state root.
///
/// Unlike the trust root, this one *may* live in worker-writable space: a
/// grant is worthless without the seal, so forging the file forges nothing.
pub const HARVEST_GRANTS_REL: &str = "harvest/grants";

/// The append-only consumption ledger, relative to a cosmon state root.
pub const HARVEST_CONSUMED_REL: &str = "harvest/consumed.jsonl";

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
        Ok(Self { key, source })
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
            if candidate.exists() {
                return Self::from_path(candidate).map(Some);
            }
        }
        Ok(None)
    }

    /// Where this key was read from, for a diagnostic an operator can follow.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
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
    let path = galaxy_root.as_ref().join(HARVEST_EPOCH_REL);
    if !path.exists() {
        return Ok(GrantEpoch::first());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| CosmonError::StateStore {
        reason: format!("failed to read harvest epoch {}: {e}", path.display()),
    })?;
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

// ---------------------------------------------------------------------------
// Grants on disk
// ---------------------------------------------------------------------------

/// Load the sealed authorisations available for `molecule`.
///
/// `$COSMON_HARVEST_GRANT`, when set, names exactly one file and nothing else
/// is read — an operator handing a specific grant to a specific invocation
/// meant that one. Otherwise every `*.json` under
/// `<state>/harvest/grants` is read and the unreadable ones are skipped
/// rather than fatal: a torn file is a grant that did not happen, exactly like
/// a signature that does not check.
///
/// The molecule is *not* filtered on here. Scope is decided by
/// [`cosmon_core::harvest_authorization::authorize`] against facts re-derived
/// under the trunk lock; filtering here as well would put a second, weaker
/// copy of that rule in the loading path.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when an explicitly named grant cannot be read
/// or parsed. A missing grants directory yields an empty list.
pub fn load_authorizations(
    state_root: impl AsRef<Path>,
) -> Result<Vec<DoneAuthorization>, CosmonError> {
    if let Some(explicit) = std::env::var_os(HARVEST_GRANT_ENV) {
        let path = PathBuf::from(explicit);
        if path.as_os_str().is_empty() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to read harvest grant {}: {e}", path.display()),
        })?;
        let one: DoneAuthorization =
            serde_json::from_str(&text).map_err(|e| CosmonError::StateStore {
                reason: format!("{} is not a harvest authorisation: {e}", path.display()),
            })?;
        return Ok(vec![one]);
    }

    let dir = state_root.as_ref().join(HARVEST_GRANTS_REL);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_str::<DoneAuthorization>(&text) {
            out.push(parsed);
        }
    }
    // Deterministic order, so which grant is tried first does not depend on
    // the directory's iteration order.
    out.sort_by_key(|a| a.grant().fingerprint().as_str().to_owned());
    Ok(out)
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
            // A torn line is a receipt that did not happen; skipping it is the
            // conservative reading, because the effect check below still
            // refuses a genuine second spend.
            let Ok(record) = serde_json::from_str::<ConsumptionRecord>(line) else {
                continue;
            };
            if &record.permit == permit {
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
    fn the_ledger_is_append_only_so_a_second_receipt_does_not_erase_the_first() {
        let dir = tempdir().expect("tempdir");
        let ledger = FileConsumptionLedger::at_state_root(dir.path());
        for (raw, inv) in [("task-20260901-6da6", "inv-1"), ("task-20260901-aaaa", "inv-2")] {
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
