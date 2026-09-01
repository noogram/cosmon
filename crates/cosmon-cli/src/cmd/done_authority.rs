// SPDX-License-Identifier: AGPL-3.0-only

//! The effect boundary of ADR-172 §D3: where `cs done` spends its authority.
//!
//! # Why this is here and not earlier
//!
//! Checking a grant when it is *written* would be advice. A same-uid process
//! can write the state files directly, so a check anywhere upstream of the
//! mutation is a check the caller can walk around. Checking before the trunk
//! lock is taken is worse than useless: it opens a TOCTOU window exactly wide
//! enough for the facts to change between the decision and the merge.
//!
//! So the check lives in one place — under the trunk lock, with every fact
//! re-derived *there*, and with the permit consumed on the line before the
//! first git mutation. [`authorize_harvest`] takes the trunk guard as a
//! parameter it does not use, so the type signature is the statement: this is
//! not callable outside the lock.
//!
//! # What it claims
//!
//! That `cs done` performs only an **authorised cosmon harvest**. That is the
//! whole claim. A determined same-uid process can still drive git plumbing
//! against the shared repository and never come near this function; until
//! repository custody is separated, such a mutation stays detectable through
//! the provenance ledger and the git/CI gates rather than being ruled out.
//! ADR-172 §D5 fixes this vocabulary and this module keeps it — which is why
//! `done_authorization_unforgeable` asserts the wording rather than trusting a
//! reviewer to keep noticing.

use std::path::Path;

use chrono::Utc;
use cosmon_core::config::HarvestAuthorityConfig;
use cosmon_core::error::CosmonError;
use cosmon_core::harvest_authorization::{
    authorize, reservations_crossed, AuthorizedHarvest, ConsumptionRecord, HarvestAction,
    HarvestConsumptionLedger, HarvestFacts, HarvestRefusal, HarvestSealVerifier,
};
use cosmon_core::id::MoleculeId;
use cosmon_filestore::harvest_authority::{
    read_epoch, read_policy_digest, FileConsumptionLedger, MinisignHarvestVerifier,
    NoHarvestTrustRoot,
};
use cosmon_state::TrunkGuard;

/// What the effect boundary decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarvestDecision {
    /// The galaxy has not turned the mechanism on. `cs done` proceeds exactly
    /// as a cosmon that predates ADR-172 would.
    NotInForce,
    /// A permit was verified against facts re-derived here and consumed. The
    /// string is the receipt line for the transaction's `actions` list.
    Authorized(String),
    /// This exact harvest already landed under this permit. The caller reports
    /// the recorded outcome and mutates nothing further.
    AlreadyLanded(Box<ConsumptionRecord>),
}

/// Everything the effect boundary needs from the `cs done` transaction.
///
/// Grouped into one struct rather than passed as nine parameters because the
/// call site assembles all of them at one point — under the trunk lock — and a
/// long positional list is where a `base` and a `galaxy` get transposed.
#[derive(Debug, Clone)]
pub struct HarvestRequest<'a> {
    /// Galaxy root, where the trust root, the epoch and the policy are pinned.
    pub galaxy_root: &'a Path,
    /// Cosmon state root, where grants and the consumption ledger live.
    pub state_root: &'a Path,
    /// Galaxy identity, as sealed in the grant.
    pub galaxy: &'a str,
    /// Molecule about to be harvested.
    pub molecule: &'a MoleculeId,
    /// DAG root it belongs to, which a delegation is scoped by.
    pub mission: Option<MoleculeId>,
    /// The molecule's tags at this instant, from which the reservations
    /// actually crossed are derived.
    pub tags: &'a [String],
    /// The base branch as *resolved* here, not as configured.
    pub base: &'a str,
    /// The id the `cs done` transaction and the receipt share in the ledger.
    pub invocation_id: &'a str,
}

/// Verify and consume a harvest authorisation, under the trunk lock.
///
/// `_trunk` is the proof the caller holds the lock. It is deliberately unused:
/// its job is to make "call this before locking" a compile error rather than a
/// review comment.
///
/// The order is fixed and load-bearing. Facts are re-derived here, the seal is
/// checked against them, and the receipt is appended **before** the first git
/// mutation — because a crash between a landed merge and an unwritten receipt
/// leaves a spent permit that reads as unspent, which is the double-spend the
/// ledger exists to prevent.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when the mechanism is in force and no
/// authorisation covers this harvest, when the ledger cannot be read or
/// written, or when the epoch or policy cannot be resolved. There is no
/// permissive branch: absence of a trust root refuses.
pub fn authorize_harvest(
    _trunk: &dyn TrunkGuard,
    cfg: &HarvestAuthorityConfig,
    request: &HarvestRequest<'_>,
) -> Result<HarvestDecision, CosmonError> {
    let HarvestRequest {
        galaxy_root,
        state_root,
        galaxy,
        molecule,
        mission,
        tags,
        base,
        invocation_id,
    } = request;
    if !cfg.is_required() {
        return Ok(HarvestDecision::NotInForce);
    }

    let facts = HarvestFacts {
        galaxy: (*galaxy).to_owned(),
        molecule: (*molecule).clone(),
        mission: mission.clone(),
        policy_digest: Some(read_policy_digest(galaxy_root)?),
        base: (*base).to_owned(),
        action: HarvestAction::Done,
        reservations_crossed: reservations_crossed(*tags),
        epoch: read_epoch(galaxy_root)?,
        now: Utc::now(),
    };

    // Fail-closed: a galaxy with the mechanism in force and nothing pinned
    // refuses every harvest. Deleting the key stops harvests; it never
    // unlocks them.
    let pinned = MinisignHarvestVerifier::resolve(galaxy_root)?;
    let verifier: &dyn HarvestSealVerifier = match pinned.as_ref() {
        Some(v) => v,
        None => &NoHarvestTrustRoot,
    };

    let ledger = FileConsumptionLedger::at_state_root(state_root);
    let candidates = cosmon_filestore::harvest_authority::load_authorizations(state_root)?;

    let mut refusals: Vec<String> = Vec::new();
    for authorization in &candidates {
        let permit = authorization.permit_id(&facts.molecule);
        let prior = ledger
            .recorded(&permit)
            .map_err(|reason| CosmonError::StateStore { reason })?;
        match authorize(authorization, &facts, prior.as_ref(), verifier) {
            Ok(AuthorizedHarvest::AlreadyLanded(record)) => {
                return Ok(HarvestDecision::AlreadyLanded(record));
            }
            Ok(AuthorizedHarvest::Fresh(granted)) => {
                let record = ConsumptionRecord {
                    permit: granted.permit.clone(),
                    grant: granted.grant.clone(),
                    effect: granted.effect.clone(),
                    key_id: granted.key_id,
                    invocation_id: (*invocation_id).to_owned(),
                };
                ledger
                    .consume(&record)
                    .map_err(|reason| CosmonError::StateStore { reason })?;
                return Ok(HarvestDecision::Authorized(format!(
                    "harvest_authorized: permit={} key={} effect={}",
                    granted.permit, granted.key_id, granted.effect
                )));
            }
            Err(refusal) => refusals.push(refusal_line(&refusal)),
        }
    }

    Err(CosmonError::StateStore {
        reason: refusal_message(molecule, base, &refusals),
    })
}

/// One refused candidate, rendered for the operator.
fn refusal_line(refusal: &HarvestRefusal) -> String {
    format!("  - {refusal}")
}

/// The message a refused harvest prints.
///
/// It lists every candidate's refusal rather than one summary, for the reason
/// ADR-171 §D6 gives for showing refused grant lines: a refusal nobody can
/// read is a refusal nobody investigates.
fn refusal_message(molecule: &MoleculeId, base: &str, refusals: &[String]) -> String {
    let mut out = format!(
        "no operator-sealed authorisation covers harvesting {} into {base}.\n",
        molecule.as_str()
    );
    if refusals.is_empty() {
        out.push_str(
            "No grant was found at all. An operator seals one out of band with stock\n\
             `minisign` and drops it under `.cosmon/state/harvest/grants/`.\n",
        );
    } else {
        out.push_str("Grants considered, and why each was refused:\n");
        for line in refusals {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(
        "\nThis refuses an *unauthorised cosmon harvest*. It does not make the trunk\n\
         immutable: a same-uid process can still drive git directly, which stays\n\
         detectable through the provenance ledger and the CI gates rather than\n\
         impossible (ADR-172 D5).\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::harvest_authorization::{
        DelegatedHarvestCapability, DoneAuthorization, GrantEpoch, HarvestGrant, HarvestScope,
        OperatorHarvestSeal,
    };
    use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};
    use cosmon_filestore::harvest_authority::store_authorization;
    use cosmon_minisign_testkit::Operator;
    use tempfile::TempDir;

    /// A trunk guard stand-in. The real one is an RAII file lock; the boundary
    /// only ever uses it as a witness.
    struct HeldLock;
    impl TrunkGuard for HeldLock {}

    struct World {
        _tmp: TempDir,
        galaxy: std::path::PathBuf,
        state: std::path::PathBuf,
        operator: Operator,
    }

    fn world() -> World {
        let tmp = TempDir::new().expect("tempdir");
        let galaxy = tmp.path().to_path_buf();
        let state = galaxy.join(".cosmon/state");
        std::fs::create_dir_all(galaxy.join(".cosmon")).expect("mkdir");
        std::fs::create_dir_all(&state).expect("mkdir");
        let operator = Operator::from_seed(7);
        std::fs::write(
            galaxy.join(cosmon_filestore::HARVEST_PUBKEY_REL),
            operator.public_key_file(),
        )
        .expect("pin key");
        std::env::remove_var(cosmon_filestore::HARVEST_PUBKEY_ENV);
        std::env::remove_var(cosmon_filestore::HARVEST_GRANT_ENV);
        World {
            _tmp: tmp,
            galaxy,
            state,
            operator,
        }
    }

    fn required() -> HarvestAuthorityConfig {
        HarvestAuthorityConfig {
            required: true,
            ..HarvestAuthorityConfig::default()
        }
    }

    fn mol(raw: &str) -> MoleculeId {
        MoleculeId::new(raw).expect("fixture molecule id")
    }

    /// Seal `grant` with the world's operator, producing the attestation the
    /// verifier checks. This is the *test* operator: the shipped tree has no
    /// equivalent, which `done_authorization_unforgeable` asserts.
    fn seal(w: &World, grant: &HarvestGrant) -> OperatorAttestation {
        let minisig = w.operator.sign(&grant.canonical_bytes());
        let mut lines = minisig.lines();
        let untrusted = lines
            .next()
            .and_then(|l| l.strip_prefix("untrusted comment: "))
            .unwrap_or_default()
            .to_owned();
        let signature = lines.next().unwrap_or_default().to_owned();
        let trusted = lines
            .next()
            .and_then(|l| l.strip_prefix("trusted comment: "))
            .unwrap_or_default()
            .to_owned();
        let global = lines.next().unwrap_or_default().to_owned();
        OperatorAttestation {
            key_id: OperatorKeyId::parse(&w.operator.key_id_display()).expect("key id"),
            signature,
            global_signature: global,
            trusted_comment: trusted,
            untrusted_comment: untrusted,
        }
    }

    fn grant_for(molecule: &MoleculeId, base: &str, reservations: &[&str]) -> HarvestGrant {
        HarvestGrant::new(
            "cosmon",
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            base,
            HarvestAction::Done,
            reservations.iter().copied(),
            GrantEpoch::first(),
            None,
        )
        .expect("fixture grant")
    }

    fn run(
        w: &World,
        cfg: &HarvestAuthorityConfig,
        molecule: &MoleculeId,
        tags: &[String],
        base: &str,
    ) -> Result<HarvestDecision, CosmonError> {
        authorize_harvest(
            &HeldLock,
            cfg,
            &HarvestRequest {
                galaxy_root: &w.galaxy,
                state_root: &w.state,
                galaxy: "cosmon",
                molecule,
                mission: None,
                tags,
                base,
                invocation_id: "inv-1",
            },
        )
    }

    #[test]
    fn a_galaxy_that_has_not_turned_it_on_is_untouched() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        assert_eq!(
            run(
                &w,
                &HarvestAuthorityConfig::default(),
                &molecule,
                &[],
                "main"
            )
            .expect("inert"),
            HarvestDecision::NotInForce
        );
    }

    #[test]
    fn with_it_on_and_no_grant_the_harvest_is_refused() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let err = run(&w, &required(), &molecule, &[], "main").expect_err("no grant must refuse");
        let text = err.to_string();
        assert!(text.contains("no operator-sealed authorisation"), "{text}");
        // The claim bound of ADR-172 D5 is in the message an operator reads.
        assert!(text.contains("does not make the trunk"), "{text}");
    }

    #[test]
    fn an_operator_sealed_grant_authorises_and_is_consumed_once() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let grant = grant_for(&molecule, "main", &[]);
        let attestation = seal(&w, &grant);
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(grant, attestation).expect("seal"),
            ),
        )
        .expect("store");

        let first = run(&w, &required(), &molecule, &[], "main").expect("authorised");
        assert!(matches!(first, HarvestDecision::Authorized(_)));

        // The retry a crashed `cs done` performs: idempotent, not refused.
        let second = run(&w, &required(), &molecule, &[], "main").expect("replay");
        match second {
            HarvestDecision::AlreadyLanded(record) => {
                assert_eq!(record.invocation_id, "inv-1");
            }
            other => panic!("a consumed permit must report what landed, got {other:?}"),
        }
    }

    #[test]
    fn a_grant_the_beneficiary_edited_after_signing_stops_verifying() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let grant = grant_for(&molecule, "main", &[]);
        let attestation = seal(&w, &grant);

        // The move an agent actually makes: widen the grant it was given.
        let mut widened = grant;
        widened.base = "release".to_owned();
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(widened, attestation).expect("seal"),
            ),
        )
        .expect("store");

        let err = run(&w, &required(), &molecule, &[], "release")
            .expect_err("an edited grant must not verify");
        assert!(err
            .to_string()
            .contains("not an authorised harvest gesture"));
    }

    #[test]
    fn a_grant_signed_by_another_key_is_refused() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let grant = grant_for(&molecule, "main", &[]);
        // An agent that generates its own keypair and signs its own grant.
        let impostor = Operator::from_seed(200);
        let minisig = impostor.sign(&grant.canonical_bytes());
        let mut lines = minisig.lines();
        let untrusted = lines.next().unwrap_or_default().to_owned();
        let signature = lines.next().unwrap_or_default().to_owned();
        let trusted = lines.next().unwrap_or_default().to_owned();
        let global = lines.next().unwrap_or_default().to_owned();
        let attestation = OperatorAttestation {
            key_id: OperatorKeyId::parse(&impostor.key_id_display()).expect("key id"),
            signature,
            global_signature: global,
            trusted_comment: trusted,
            untrusted_comment: untrusted,
        };
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(grant, attestation).expect("seal"),
            ),
        )
        .expect("store");

        assert!(run(&w, &required(), &molecule, &[], "main").is_err());
    }

    #[test]
    fn deleting_the_trust_root_refuses_instead_of_permitting() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let grant = grant_for(&molecule, "main", &[]);
        let attestation = seal(&w, &grant);
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(grant, attestation).expect("seal"),
            ),
        )
        .expect("store");
        assert!(run(&w, &required(), &molecule, &[], "main").is_ok());

        // ADR-172 falsifier 4, as the move an agent would make.
        std::fs::remove_file(w.galaxy.join(cosmon_filestore::HARVEST_PUBKEY_REL))
            .expect("remove trust root");
        let fresh = mol("task-20260901-aaaa");
        let g = grant_for(&fresh, "main", &[]);
        let a = seal(&w, &g);
        store_authorization(
            &w.state,
            &fresh,
            &DoneAuthorization::Ratified(OperatorHarvestSeal::new(g, a).expect("seal")),
        )
        .expect("store");
        assert!(
            run(&w, &required(), &fresh, &[], "main").is_err(),
            "deleting the trust root must stop harvests, not unlock them"
        );
    }

    #[test]
    fn bumping_the_epoch_revokes_a_grant_already_on_disk() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let grant = grant_for(&molecule, "main", &[]);
        let attestation = seal(&w, &grant);
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(grant, attestation).expect("seal"),
            ),
        )
        .expect("store");

        // The operator's entire revocation gesture. Nobody is notified.
        std::fs::write(
            w.galaxy
                .join(cosmon_filestore::harvest_authority::HARVEST_EPOCH_REL),
            "2\n",
        )
        .expect("bump epoch");

        let err = run(&w, &required(), &molecule, &[], "main")
            .expect_err("a superseded epoch must refuse");
        assert!(err.to_string().contains("revoked"), "{err}");
    }

    #[test]
    fn a_reservation_added_after_signing_refuses_the_grant() {
        let w = world();
        let molecule = mol("task-20260901-6da6");
        let grant = grant_for(&molecule, "main", &[]);
        let attestation = seal(&w, &grant);
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(grant, attestation).expect("seal"),
            ),
        )
        .expect("store");

        let tags = vec!["needs-review".to_owned()];
        let err = run(&w, &required(), &molecule, &tags, "main")
            .expect_err("an unnamed reservation must refuse");
        assert!(err.to_string().contains("needs-review"), "{err}");
    }

    #[test]
    fn a_mission_delegation_drains_a_dag_without_a_gesture_per_edge() {
        let w = world();
        let mission = mol("delib-20260819-cda2");
        let digest = read_policy_digest(&w.galaxy).expect("policy digest");
        let grant = HarvestGrant::new(
            "cosmon",
            HarvestScope::Mission {
                mission: mission.clone(),
                policy_digest: digest,
            },
            "main",
            HarvestAction::Done,
            Vec::<String>::new(),
            GrantEpoch::first(),
            None,
        )
        .expect("fixture grant");
        let attestation = seal(&w, &grant);
        let capability =
            DoneAuthorization::Delegated(DelegatedHarvestCapability::new(grant, attestation));
        store_authorization(&w.state, &mission, &capability).expect("store");

        for raw in ["task-20260901-6da6", "task-20260901-aaaa"] {
            let molecule = mol(raw);
            let decision = authorize_harvest(
                &HeldLock,
                &required(),
                &HarvestRequest {
                    galaxy_root: &w.galaxy,
                    state_root: &w.state,
                    galaxy: "cosmon",
                    molecule: &molecule,
                    mission: Some(mission.clone()),
                    tags: &[],
                    base: "main",
                    invocation_id: "inv-1",
                },
            )
            .unwrap_or_else(|e| panic!("{raw} is inside the delegation: {e}"));
            assert!(matches!(decision, HarvestDecision::Authorized(_)));
        }
    }

    #[test]
    fn editing_the_policy_lapses_the_delegation() {
        let w = world();
        let mission = mol("delib-20260819-cda2");
        let digest = read_policy_digest(&w.galaxy).expect("policy digest");
        let grant = HarvestGrant::new(
            "cosmon",
            HarvestScope::Mission {
                mission: mission.clone(),
                policy_digest: digest,
            },
            "main",
            HarvestAction::Done,
            Vec::<String>::new(),
            GrantEpoch::first(),
            None,
        )
        .expect("fixture grant");
        let attestation = seal(&w, &grant);
        store_authorization(
            &w.state,
            &mission,
            &DoneAuthorization::Delegated(DelegatedHarvestCapability::new(grant, attestation)),
        )
        .expect("store");

        std::fs::write(
            w.galaxy
                .join(cosmon_filestore::harvest_authority::HARVEST_POLICY_REL),
            "max_parallel = 12\n",
        )
        .expect("edit policy");

        let molecule = mol("task-20260901-6da6");
        let err = authorize_harvest(
            &HeldLock,
            &required(),
            &HarvestRequest {
                galaxy_root: &w.galaxy,
                state_root: &w.state,
                galaxy: "cosmon",
                molecule: &molecule,
                mission: Some(mission),
                tags: &[],
                base: "main",
                invocation_id: "inv-1",
            },
        )
        .expect_err("a policy edit must lapse the delegation");
        assert!(
            err.to_string().contains("outside the grant's scope"),
            "{err}"
        );
    }
}
