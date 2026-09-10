// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-e2e-harvest-seal` — the human at the keyboard, for a throwaway galaxy.
//!
//! # Why this exists
//!
//! ADR-172 §D1 is two halves. `[harvest_authority] required = true` makes the
//! door's decision half admit; the effect half then demands an
//! operator-**sealed** grant, verified inside the trunk lock. A galaxy that
//! armed only the first refuses `not_authorized` at the effect boundary — the
//! shape a stock deployment must never be left in, and precisely the shape the
//! container end-to-end suite was asserting before it could mint a seal.
//!
//! Cosmon verifies operator signatures and never produces one, so the signer
//! cannot live in `cs` or in the adapter. It lives here, in the crate that is
//! already the structural home of the test operator: `publish = false`, named
//! only in `[dev-dependencies]`, and exempt from
//! `takeover_unforgeable::the_shipped_tree_owns_no_signing_path_for_the_operator_key`
//! by path rather than by convention. Nothing that ships links it.
//!
//! # Why a binary and not another test fixture
//!
//! The suite that needs it is `tests/e2e/` — pytest, on the host, driving a
//! container stack through the real `cosmon-remote`. It cannot call a Rust
//! function. It could re-implement minisign's `ED` format and the grant's
//! canonical bytes in Python, and would then be asserting against its own
//! second spelling of the spec: the first time [`HarvestGrant::canonical_bytes`]
//! changed, the suite would keep passing against a grant nothing else accepts.
//! So the provisioning goes through the same library the verifier uses.
//!
//! # Usage
//!
//! ```text
//! cs-e2e-harvest-seal <galaxy-root> <galaxy-id> <molecule-id> <base-branch>
//! ```
//!
//! Writes two files under `<galaxy-root>`: the trust root
//! (`.cosmon/harvest.pub`) and one molecule-scoped ratified grant under
//! `.cosmon/state/harvest/grants/`. Both are what an operator would place
//! there by hand; neither is reachable from a shipped verb.
//!
//! [`HarvestGrant::canonical_bytes`]: cosmon_core::harvest_authorization::HarvestGrant::canonical_bytes

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cosmon_core::harvest_authorization::{
    DoneAuthorization, GrantEpoch, HarvestAction, HarvestGrant, HarvestScope, OperatorHarvestSeal,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};

/// The seed the throwaway operator's keypair is derived from.
///
/// Fixed, so a failing run re-runs with the same key and the same bytes — and
/// so the trust root a second invocation pins is the one that signed the first
/// grant. A random key per call would leave every grant but the last one
/// unverifiable.
const OPERATOR_SEED: u8 = 7;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [galaxy_root, galaxy_id, molecule, base] = args.as_slice() else {
        eprintln!(
            "usage: cs-e2e-harvest-seal <galaxy-root> <galaxy-id> <molecule-id> <base-branch>"
        );
        return ExitCode::from(2);
    };
    match seal(Path::new(galaxy_root), galaxy_id, molecule, base) {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("cs-e2e-harvest-seal: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Pin the trust root and write one molecule-scoped ratified grant.
///
/// The grant is `HarvestScope::Molecule` rather than a mission delegation for
/// the reason [`OperatorHarvestSeal::new`] enforces: a ratification covers one
/// reviewed molecule, and a mission-scoped one would let a single signature
/// ratify a whole DAG — which is not what a harness is entitled to provision.
fn seal(
    galaxy_root: &Path,
    galaxy_id: &str,
    molecule: &str,
    base: &str,
) -> anyhow::Result<PathBuf> {
    let operator = cosmon_minisign_testkit::Operator::from_seed(OPERATOR_SEED);
    let pubkey = galaxy_root.join(cosmon_filestore::HARVEST_PUBKEY_REL);
    if let Some(parent) = pubkey.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pubkey, operator.public_key_file())?;

    let id = MoleculeId::new(molecule)
        .map_err(|e| anyhow::anyhow!("{molecule} is not a molecule id: {e}"))?;
    let grant = HarvestGrant::new(
        galaxy_id,
        HarvestScope::Molecule {
            molecule: id.clone(),
        },
        base,
        HarvestAction::Done,
        std::iter::empty::<&str>(),
        GrantEpoch::first(),
        None,
    )
    .map_err(|e| anyhow::anyhow!("grant: {e}"))?;

    let attestation = attest(&operator, &grant)?;
    let seal = OperatorHarvestSeal::new(grant, attestation)
        .map_err(|e| anyhow::anyhow!("ratification: {e}"))?;
    let state_root = galaxy_root.join(".cosmon").join("state");
    let path = cosmon_filestore::harvest_authority::store_authorization(
        &state_root,
        &id,
        &DoneAuthorization::Ratified(seal),
    )?;
    Ok(path)
}

/// Split a minisign artefact into the four fields the attestation carries.
///
/// The `.minisig` layout is fixed: untrusted comment, signature, trusted
/// comment, global signature — one per line, in that order. Parsed rather than
/// re-assembled, so the bytes stored are the bytes the signer produced.
fn attest(
    operator: &cosmon_minisign_testkit::Operator,
    grant: &HarvestGrant,
) -> anyhow::Result<OperatorAttestation> {
    let minisig = operator.sign(&grant.canonical_bytes());
    let mut lines = minisig.lines();
    let untrusted_comment = lines
        .next()
        .and_then(|l| l.strip_prefix("untrusted comment: "))
        .ok_or_else(|| anyhow::anyhow!("signature has no untrusted comment line"))?
        .to_owned();
    let signature = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("signature has no signature line"))?
        .to_owned();
    let trusted_comment = lines
        .next()
        .and_then(|l| l.strip_prefix("trusted comment: "))
        .ok_or_else(|| anyhow::anyhow!("signature has no trusted comment line"))?
        .to_owned();
    let global_signature = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("signature has no global signature line"))?
        .to_owned();
    Ok(OperatorAttestation {
        key_id: OperatorKeyId::parse(&operator.key_id_display())
            .map_err(|e| anyhow::anyhow!("key id: {e}"))?,
        signature,
        global_signature,
        trusted_comment,
        untrusted_comment,
    })
}
