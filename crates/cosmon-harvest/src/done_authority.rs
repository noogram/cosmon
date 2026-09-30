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
//! re-derived *there*, with a durable reservation before the first Git
//! mutation. The transaction journals progress separately, so reservation
//! never claims a merge landed. `authorize_harvest_from_facts` takes the
//! trunk guard as a parameter it does not use, so the type signature is the
//! statement: this is not callable outside the lock.
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
use cosmon_core::harness::Clock;
use cosmon_core::harvest_authorization::{
    authorize, reservations_crossed, AuthorizedHarvest, ConsumptionRecord, DoneAuthorization,
    HarvestAction, HarvestAuthorizationCause, HarvestConsumptionLedger, HarvestFacts,
    HarvestRefusal, HarvestScope, HarvestSealVerifier,
};
use cosmon_core::id::MoleculeId;
use cosmon_filestore::harvest_authority::{
    read_epoch, read_policy_digest, FileConsumptionLedger, MinisignHarvestVerifier,
    NoHarvestTrustRoot,
};
use cosmon_state::StateStore;
use cosmon_state::TrunkGuard;

use crate::authorization_facts::{strict_mission_ancestry, AuthorizationFacts, MissionAncestry};

/// What the effect boundary decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarvestDecision {
    /// The galaxy has not turned the mechanism on. `cs done` proceeds exactly
    /// as a cosmon that predates ADR-172 would.
    NotInForce,
    /// A permit was verified and reserved; integration remains unproved.
    Authorized(Box<ConsumptionRecord>),
    /// Historical variant name for a prior reservation. The transaction must
    /// inspect its progress journal before reporting any outcome or touching Git.
    AlreadyLanded(Box<ConsumptionRecord>),
    /// No authorisation covers this harvest. The string is the local
    /// operator-facing account of every candidate; the cause is the only
    /// diagnostic safe to send to a remote client.
    ///
    /// A *decision*, not an error, and the distinction is the point. Every
    /// path out of [`authorize_harvest`] used to be
    /// [`CosmonError::StateStore`], so a refused harvest and a torn ledger
    /// were the same value to the caller: both reached `cs done` as exit 1
    /// and the harvest route as an anonymous `500 harvest_failed`. A galaxy
    /// that armed `[harvest_authority]` and has not yet been given a grant is
    /// the single most likely production shape there is, and it deserves a
    /// name. Separating it here is what lets the transaction answer
    /// `not_authorized` (exit 71) for the refusal while a genuine I/O fault
    /// stays a fault.
    Refused(String, HarvestAuthorizationCause),
}

/// A classified authority fault. The detailed source stays local; only the
/// closed cause can be sent to a remote client.
#[derive(Debug)]
pub(crate) struct AuthorizationFailure {
    /// Local state or I/O error.
    pub source: Box<CosmonError>,
    /// Safe, stable remote classification.
    pub cause: HarvestAuthorizationCause,
}

impl AuthorizationFailure {
    fn new(source: CosmonError, cause: HarvestAuthorizationCause) -> Self {
        Self {
            source: Box::new(source),
            cause,
        }
    }
}

impl HarvestDecision {
    /// Return the safe refusal cause, when the decision denied authority.
    #[must_use]
    pub const fn authorization_cause(&self) -> Option<HarvestAuthorizationCause> {
        match self {
            Self::Refused(_, cause) => Some(*cause),
            _ => None,
        }
    }
}

/// Legacy positional facts for callers that have not migrated to the strict
/// [`AuthorizationFacts`] loader. The production transaction uses
/// `authorize_harvest_from_facts`.
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

/// Verify and reserve a harvest authorisation from caller-supplied facts.
///
/// This compatibility entry preserves existing signed-grant tests and old
/// library callers. It cannot establish that `mission`, tags or base came
/// from current state. The `cs done` effect uses
/// `authorize_harvest_from_facts` with strict locked facts instead.
///
/// `_trunk` is the proof the caller holds the lock. It is deliberately unused:
/// its job is to make "call this before locking" a compile error rather than a
/// review comment.
///
/// The order is fixed and load-bearing. The seal is checked against the given
/// facts, and the reservation is appended before the first Git mutation. The
/// transaction then syncs a prepared progress record; neither line alone
/// asserts that integration succeeded.
///
/// # Errors
///
/// [`CosmonError::StateStore`] when the ledger cannot be read or written, or
/// when the epoch or policy cannot be resolved — genuine faults. A harvest no
/// authorisation covers is **not** an error: it is
/// [`HarvestDecision::Refused`], so the caller can give it a name rather than
/// a stack trace. There is no permissive branch either way: absence of a trust
/// root refuses.
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

    let candidates =
        cosmon_filestore::harvest_authority::load_authorizations_with_diagnostics(state_root)?;
    authorize_candidates(
        CandidateContext {
            state_root,
            molecule,
            base,
            invocation_id,
        },
        &candidates,
        verifier,
        pinned.is_some(),
        |_| Ok(facts.clone()),
    )
    .map_err(|failure| *failure.source)
}

/// Verify and reserve authority using the strict snapshot read under the
/// trunk lock. Grant tooling uses the same [`AuthorizationFacts`] loader.
/// Mission ancestry is read only for a mission-scoped candidate; an ordinary
/// molecule seal does not depend on unrelated parents being readable.
///
/// # Errors
///
/// Returns a state fault if a needed mission ancestor is missing or ambiguous,
/// or if grant or receipt storage is unreadable.
pub(crate) fn authorize_harvest_from_facts(
    _trunk: &dyn TrunkGuard,
    facts: &AuthorizationFacts,
    request: &LockedHarvestRequest<'_>,
    require_seal: bool,
) -> Result<HarvestDecision, AuthorizationFailure> {
    if !require_seal {
        return Ok(HarvestDecision::NotInForce);
    }
    let no_root = NoHarvestTrustRoot;
    let verifier: &dyn HarvestSealVerifier = facts
        .verifier
        .as_ref()
        .map_or(&no_root, |v| v as &dyn HarvestSealVerifier);
    let candidates = cosmon_filestore::harvest_authority::load_authorizations_with_diagnostics(
        request.state_root,
    )
    .map_err(|source| {
        AuthorizationFailure::new(source, HarvestAuthorizationCause::FactsUnavailable)
    })?;
    authorize_candidates(
        CandidateContext {
            state_root: request.state_root,
            molecule: request.molecule,
            base: &facts.base.branch,
            invocation_id: request.invocation_id,
        },
        &candidates,
        verifier,
        facts.verifier.is_some(),
        |authorization| {
            let mission =
                match &authorization.grant().scope {
                    HarvestScope::Molecule { .. } => None,
                    HarvestScope::Mission { .. } => {
                        let before = match request.preliminary_mission {
                            Some(Ok(before)) => before,
                            Some(Err(_)) => {
                                return Err(AuthorizationFailure::new(CosmonError::StateStore {
                                reason: "harvest_facts_unavailable: preliminary mission ancestry"
                                    .to_owned(),
                            }, HarvestAuthorizationCause::FactsUnavailable));
                            }
                            None => {
                                return Err(AuthorizationFailure::new(CosmonError::StateStore {
                                reason: "harvest_facts_changed: mission grant appeared after gates"
                                    .to_owned(),
                            }, HarvestAuthorizationCause::FactsChanged));
                            }
                        };
                        let current = strict_mission_ancestry(request.store, request.molecule)
                            .map_err(|source| {
                                AuthorizationFailure::new(
                                    source,
                                    HarvestAuthorizationCause::FactsUnavailable,
                                )
                            })?;
                        if before != &current {
                            return Err(AuthorizationFailure::new(CosmonError::StateStore {
                            reason: "harvest_facts_changed: mission ancestry changed after gates"
                                .to_owned(),
                        }, HarvestAuthorizationCause::FactsChanged));
                        }
                        Some(current.root)
                    }
                };
            Ok(facts.for_effect(request.molecule, mission, request.clock.now()))
        },
    )
}

/// Dependencies already under the trunk guard for one effect decision.
pub(crate) struct LockedHarvestRequest<'a> {
    /// State port for strict mission ancestry.
    pub store: &'a dyn StateStore,
    /// Durable grant and receipt residence.
    pub state_root: &'a Path,
    /// Target molecule.
    pub molecule: &'a MoleculeId,
    /// Correlation ID shared by the transaction and receipt.
    pub invocation_id: &'a str,
    /// Pre-gate mission graph, when a mission grant was present.
    pub preliminary_mission: Option<&'a Result<MissionAncestry, CosmonError>>,
    /// Current time port for expiry decisions.
    pub clock: &'a dyn Clock,
}

struct CandidateContext<'a> {
    state_root: &'a Path,
    molecule: &'a MoleculeId,
    base: &'a str,
    invocation_id: &'a str,
}

fn authorize_candidates(
    context: CandidateContext<'_>,
    candidates: &cosmon_filestore::harvest_authority::AuthorizationCandidates,
    verifier: &dyn HarvestSealVerifier,
    root_present: bool,
    mut fact_for: impl FnMut(&DoneAuthorization) -> Result<HarvestFacts, AuthorizationFailure>,
) -> Result<HarvestDecision, AuthorizationFailure> {
    let ledger = FileConsumptionLedger::at_state_root(context.state_root);

    let mut refusals: Vec<(String, Option<HarvestAuthorizationCause>)> = Vec::new();
    if candidates.malformed {
        refusals.push((
            "  - an installed harvest grant is malformed".to_owned(),
            Some(HarvestAuthorizationCause::GrantInvalid),
        ));
    }
    let mut fact_fault: Option<AuthorizationFailure> = None;
    for authorization in &candidates.authorizations {
        let facts = match fact_for(authorization) {
            Ok(facts) => facts,
            Err(error) => {
                fact_fault = Some(error);
                continue;
            }
        };
        let mut prior: Option<ConsumptionRecord> = None;
        for permit in authorization.receipt_ids(&facts.molecule) {
            if let Some(record) = ledger.recorded(&permit).map_err(|reason| {
                AuthorizationFailure::new(
                    CosmonError::StateStore { reason },
                    HarvestAuthorizationCause::RecoveryRequired,
                )
            })? {
                if record.grant != authorization.grant().fingerprint() {
                    return Err(AuthorizationFailure::new(
                        CosmonError::StateStore {
                            reason: format!("harvest receipt for {permit} names a different grant"),
                        },
                        HarvestAuthorizationCause::RecoveryRequired,
                    ));
                }
                if let Some(previous) = &prior {
                    if previous.grant != record.grant
                        || previous.effect != record.effect
                        || previous.key_id != record.key_id
                        || previous.invocation_id != record.invocation_id
                    {
                        return Err(AuthorizationFailure::new(
                            CosmonError::StateStore {
                                reason: format!(
                                    "conflicting harvest receipt aliases for {}",
                                    authorization.grant().fingerprint()
                                ),
                            },
                            HarvestAuthorizationCause::RecoveryRequired,
                        ));
                    }
                } else {
                    prior = Some(record);
                }
            }
        }
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
                    invocation_id: context.invocation_id.to_owned(),
                };
                ledger.consume(&record).map_err(|reason| {
                    AuthorizationFailure::new(
                        CosmonError::StateStore { reason },
                        HarvestAuthorizationCause::RecoveryRequired,
                    )
                })?;
                return Ok(HarvestDecision::Authorized(Box::new(record)));
            }
            Err(refusal) => refusals.push((
                refusal_line(&refusal),
                targets_molecule(authorization, &facts).then(|| refusal.authorization_cause()),
            )),
        }
    }

    if let Some(error) = fact_fault {
        return Err(error);
    }

    let cause = if !root_present {
        HarvestAuthorizationCause::KeyMissing
    } else if candidates.authorizations.is_empty() && candidates.malformed {
        HarvestAuthorizationCause::GrantInvalid
    } else if candidates.authorizations.is_empty() {
        HarvestAuthorizationCause::GrantMissing
    } else {
        // A valid covering candidate returned above. For diagnostics of the
        // rejected set, prefer a malformed/signature failure to a stale or
        // unrelated grant. The ordering is independent of directory order.
        [
            HarvestAuthorizationCause::GrantInvalid,
            HarvestAuthorizationCause::SignatureInvalid,
            HarvestAuthorizationCause::GrantExpired,
            HarvestAuthorizationCause::GrantRevoked,
            HarvestAuthorizationCause::GrantMismatch,
            HarvestAuthorizationCause::KeyMissing,
        ]
        .into_iter()
        .find(|candidate| refusals.iter().any(|(_, seen)| *seen == Some(*candidate)))
        .unwrap_or(HarvestAuthorizationCause::GrantMissing)
    };
    let lines: Vec<String> = refusals.into_iter().map(|(line, _)| line).collect();
    Ok(HarvestDecision::Refused(
        refusal_message(context.molecule, context.base, &lines),
        cause,
    ))
}

/// One refused candidate, rendered for the operator.
fn refusal_line(refusal: &HarvestRefusal) -> String {
    format!("  - {refusal}")
}

fn targets_molecule(authorization: &DoneAuthorization, facts: &HarvestFacts) -> bool {
    match &authorization.grant().scope {
        HarvestScope::Molecule { molecule } => molecule == &facts.molecule,
        HarvestScope::Mission { mission, .. } => facts.mission.as_ref() == Some(mission),
    }
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

    /// The refusal message from a run that must refuse.
    ///
    /// A refusal is `Ok(HarvestDecision::Refused(..))` and a fault is `Err`, so
    /// these tests say which of the two they are asserting rather than
    /// accepting either.
    fn refusal_text(result: Result<HarvestDecision, CosmonError>, why: &str) -> String {
        match result {
            Ok(HarvestDecision::Refused(message, _)) => message,
            other => panic!("{why}: {other:?}"),
        }
    }

    #[test]
    fn armed_without_a_key_reports_key_missing_before_grant_missing() {
        let w = world();
        std::fs::remove_file(w.galaxy.join(cosmon_filestore::HARVEST_PUBKEY_REL))
            .expect("remove fixture key");
        let molecule = mol("task-20260930-a001");
        let decision = run(&w, &required(), &molecule, &[], "main").expect("decision");
        assert_eq!(
            decision.authorization_cause(),
            Some(cosmon_core::harvest_authorization::HarvestAuthorizationCause::KeyMissing)
        );
    }

    #[test]
    fn candidate_priority_is_stable_and_a_valid_grant_wins() {
        let w = world();
        let molecule = mol("task-20260930-a002");
        assert_eq!(
            run(&w, &required(), &molecule, &[], "main")
                .expect("missing grant decision")
                .authorization_cause(),
            Some(HarvestAuthorizationCause::GrantMissing)
        );

        let expired = HarvestGrant::new(
            "cosmon",
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            "main",
            HarvestAction::Done,
            Vec::<String>::new(),
            GrantEpoch::first(),
            Some(Utc::now() - chrono::Duration::hours(1)),
        )
        .expect("expired grant fixture");
        let mismatch = grant_for(&molecule, "release", &[]);
        for (name, grant) in [
            (mol("task-20260930-a003"), expired),
            (mol("task-20260930-a004"), mismatch),
        ] {
            let attestation = seal(&w, &grant);
            store_authorization(
                &w.state,
                &name,
                &DoneAuthorization::Ratified(
                    OperatorHarvestSeal::new(grant, attestation).expect("seal"),
                ),
            )
            .expect("store");
        }
        assert_eq!(
            run(&w, &required(), &molecule, &[], "main")
                .expect("rejected candidates")
                .authorization_cause(),
            Some(HarvestAuthorizationCause::GrantExpired)
        );

        let current = grant_for(&molecule, "main", &[]);
        let attestation = seal(&w, &current);
        store_authorization(
            &w.state,
            &molecule,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(current, attestation).expect("seal"),
            ),
        )
        .expect("store covering grant");
        assert!(matches!(
            run(&w, &required(), &molecule, &[], "main"),
            Ok(HarvestDecision::Authorized(_))
        ));
    }

    #[test]
    fn an_unrelated_grant_does_not_hide_a_missing_target_grant() {
        let w = world();
        let target = mol("task-20260930-a006");
        let other = mol("task-20260930-a007");
        let grant = grant_for(&other, "main", &[]);
        let attestation = seal(&w, &grant);
        store_authorization(
            &w.state,
            &other,
            &DoneAuthorization::Ratified(
                OperatorHarvestSeal::new(grant, attestation).expect("seal"),
            ),
        )
        .expect("store unrelated grant");
        assert_eq!(
            run(&w, &required(), &target, &[], "main")
                .expect("target decision")
                .authorization_cause(),
            Some(HarvestAuthorizationCause::GrantMissing)
        );
    }

    #[test]
    fn malformed_grant_is_distinct_from_an_unreadable_grant_directory() {
        let w = world();
        let molecule = mol("task-20260930-a005");
        let dir = w
            .state
            .join(cosmon_filestore::harvest_authority::HARVEST_GRANTS_REL);
        std::fs::create_dir_all(&dir).expect("grants directory");
        std::fs::write(dir.join("broken.json"), "{broken").expect("malformed candidate");
        assert_eq!(
            run(&w, &required(), &molecule, &[], "main")
                .expect("malformed candidate decision")
                .authorization_cause(),
            Some(HarvestAuthorizationCause::GrantInvalid)
        );
        std::fs::remove_file(dir.join("broken.json")).expect("remove candidate");
        std::fs::remove_dir(&dir).expect("remove directory");
        std::fs::write(&dir, "not a directory").expect("I/O fault fixture");
        assert!(run(&w, &required(), &molecule, &[], "main").is_err());
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
        let text = refusal_text(
            run(&w, &required(), &molecule, &[], "main"),
            "no grant must refuse",
        );
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

        let text = refusal_text(
            run(&w, &required(), &molecule, &[], "release"),
            "an edited grant must not verify",
        );
        assert!(text.contains("not an authorised harvest gesture"), "{text}");
        assert_eq!(
            run(&w, &required(), &molecule, &[], "release")
                .expect("signature refusal")
                .authorization_cause(),
            Some(HarvestAuthorizationCause::SignatureInvalid)
        );
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

        assert!(matches!(
            run(&w, &required(), &molecule, &[], "main"),
            Ok(HarvestDecision::Refused(_, _))
        ));
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
            matches!(
                run(&w, &required(), &fresh, &[], "main"),
                Ok(HarvestDecision::Refused(_, _))
            ),
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

        let text = refusal_text(
            run(&w, &required(), &molecule, &[], "main"),
            "a superseded epoch must refuse",
        );
        assert!(text.contains("revoked"), "{text}");
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
        let text = refusal_text(
            run(&w, &required(), &molecule, &tags, "main"),
            "an unnamed reservation must refuse",
        );
        assert!(text.contains("needs-review"), "{text}");
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
        );
        let text = refusal_text(err, "a policy edit must lapse the delegation");
        assert!(text.contains("outside the grant's scope"), "{text}");
    }
}
