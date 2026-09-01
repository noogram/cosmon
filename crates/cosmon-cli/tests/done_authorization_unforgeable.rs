// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest authority is unforgeable by its beneficiary (ADR-172).
//!
//! # What this file is falsifying
//!
//! ADR-172 §D2 says cosmon *verifies* an operator harvest seal and ships no
//! path that can *produce* one. That sentence is the foundation the whole
//! decision rests on: if a shipped `cs` path could mint an
//! `OperatorHarvestSeal`, then a worker could authorise its own harvest, the
//! capability would be a label, and the panel's unanimous finding would
//! collapse with it. The delegation named this the central falsifier of the
//! dossier.
//!
//! So the claim under test is not "the happy path works". It is:
//!
//! > **An agent holding everything the beneficiary holds — the state
//! > directory, the grants directory, the consumption ledger, the `cs` binary,
//! > the pinned public key, a shell — cannot produce authority for itself.**
//!
//! This is the homologue of
//! `takeover_unforgeable::the_shipped_tree_owns_no_signing_path_for_the_operator_key`,
//! for the other operator gesture. The two exist separately on purpose: one
//! signing path is enough to falsify one claim, and a single shared test would
//! let a signer added for one domain pass for the other.
//!
//! # The claim bound this file also pins
//!
//! ADR-172 §D5 is explicit that this authorises a *cosmon harvest* and does
//! **not** make a same-uid worker unable to mutate the trunk. That bound is a
//! property of the words `cs done` prints, so
//! [`the_refusal_never_claims_the_trunk_is_immutable`] asserts it on the
//! shipped text rather than trusting a reviewer to keep noticing.

use std::path::{Path, PathBuf};

/// Walk every file under `dir`, calling `f` on each.
fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&path, f);
        } else {
            f(&path);
        }
    }
}

/// Every `.rs` file in the workspace's crates.
fn workspace_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir");
    let mut out = Vec::new();
    walk(crates, &mut |path| {
        if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.to_path_buf());
        }
    });
    out
}

/// The central falsifier of ADR-172: no shipped path can mint a harvest seal.
///
/// The two exemptions are structural rather than conventional — the test
/// harness, and the `publish = false` testkit crate that appears only in
/// `[dev-dependencies]`, so `cs` has no dependency edge to a signer.
#[test]
fn the_shipped_tree_owns_no_signing_path_for_the_harvest_seal() {
    let mut offenders = Vec::new();
    for path in workspace_sources() {
        let as_str = path.display().to_string();
        if as_str.contains("/tests/") || as_str.contains("cosmon-minisign-testkit") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        // A signer is a secret key plus the format it signs in. Either alone
        // is innocent (`MinisignPublicKey` parses; `SigningKey` appears in
        // unrelated crypto), which is why the conjunction is the signal.
        if body.contains("SigningKey") && body.contains("minisign") {
            offenders.push(as_str);
        }
    }
    assert!(
        offenders.is_empty(),
        "cosmon must verify harvest seals and never produce one; found: {offenders:?}"
    );
}

/// No shipped path constructs a seal out of thin air either.
///
/// The signing-key check above catches a *cryptographic* forge. This catches
/// the cheaper one: a `cs` verb that builds an `OperatorHarvestSeal` from
/// caller-supplied bytes and hands it to the effect boundary, which would make
/// the seal a struct literal rather than a gesture.
#[test]
fn no_shipped_verb_constructs_a_harvest_seal() {
    let mut offenders = Vec::new();
    for path in workspace_sources() {
        let as_str = path.display().to_string();
        if as_str.contains("/tests/") || as_str.contains("cosmon-minisign-testkit") {
            continue;
        }
        // The domain defines the type and the filestore round-trips sealed
        // bytes; both are allowed to name it. Everything else must not build
        // one.
        if as_str.contains("cosmon-core/src/harvest_authorization.rs")
            || as_str.contains("cosmon-filestore/src/harvest_authority.rs")
        {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Strip the file's own `#[cfg(test)]` module, which is allowed a
        // stand-in operator for exactly the reason the testkit is.
        let shipped = body.split("#[cfg(test)]").next().unwrap_or(&body);
        if shipped.contains("OperatorHarvestSeal::new") {
            offenders.push(as_str);
        }
    }
    assert!(
        offenders.is_empty(),
        "no shipped `cs` path may mint an operator harvest seal; found: {offenders:?}"
    );
}

/// ADR-172 §D5's claim bound, asserted on the shipped words.
///
/// The decision is violated if documentation claims this prevents direct
/// same-uid git mutation before repository custody has been separated. The
/// refusal an operator actually reads is where that overclaim would appear
/// first, so the bound is pinned there.
#[test]
fn the_refusal_never_claims_the_trunk_is_immutable() {
    let boundary = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/done_authority.rs");
    let body = std::fs::read_to_string(&boundary).expect("read the effect boundary");
    let shipped = body.split("#[cfg(test)]").next().unwrap_or(&body);

    for overclaim in [
        "cannot mutate the trunk",
        "a worker cannot mutate",
        "makes the trunk immutable",
        "prevents direct git",
    ] {
        assert!(
            !shipped.contains(overclaim),
            "ADR-172 D5 bounds the claim: {overclaim:?} must not appear"
        );
    }
    assert!(
        shipped.contains("authorised cosmon harvest"),
        "the refusal must name what it actually authorises"
    );
    assert!(
        shipped.contains("does not make the trunk\\n"),
        "the refusal message must carry the residual-risk statement"
    );
}
