// SPDX-License-Identifier: AGPL-3.0-only

//! Real signatures and durable receipts at the harvest effect boundary.

use cosmon_core::config::HarvestAuthorityConfig;
use cosmon_core::harvest_authorization::{
    authorize, ConsumptionRecord, DelegatedHarvestCapability, DoneAuthorization, GrantEpoch,
    HarvestAction, HarvestConsumptionLedger, HarvestFacts, HarvestGrant, HarvestRefusal,
    HarvestScope, OperatorHarvestSeal,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};
use cosmon_filestore::harvest_authority::{store_authorization, FileConsumptionLedger};
use cosmon_harvest::done_authority::{authorize_harvest, HarvestDecision, HarvestRequest};
use cosmon_minisign_testkit::Operator;
use cosmon_state::TrunkGuard;
use tempfile::TempDir;

// Literal identities calculated from the frozen v1 baseline, before the
// production permit helper changes. These are historical ledger keys.
const MOLECULE_PERMIT: &str = "e72edb4db6186d869a6842f62ddf86dd0b69ab0a0bbf43a5f5305332a67030e8";
const DELEGATED_MOLECULE_ALIAS: &str =
    "a5a87c19d0af1823ac8d0386541033fdab8095025af46871827bb874007ffbcd";

struct HeldLock;
impl TrunkGuard for HeldLock {}

fn mol(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("fixture molecule")
}

fn signed(grant: HarvestGrant, operator: &Operator, ratified: bool) -> DoneAuthorization {
    let signature = operator.sign(&grant.canonical_bytes());
    let mut lines = signature.lines();
    let attestation = OperatorAttestation {
        key_id: OperatorKeyId::parse(&operator.key_id_display()).expect("key id"),
        untrusted_comment: lines
            .next()
            .expect("untrusted")
            .replace("untrusted comment: ", ""),
        signature: lines.next().expect("signature").to_owned(),
        trusted_comment: lines
            .next()
            .expect("trusted")
            .replace("trusted comment: ", ""),
        global_signature: lines.next().expect("global signature").to_owned(),
    };
    if ratified {
        DoneAuthorization::Ratified(OperatorHarvestSeal::new(grant, attestation).expect("seal"))
    } else {
        DoneAuthorization::Delegated(DelegatedHarvestCapability::new(grant, attestation))
    }
}

fn run(root: &TempDir, molecule: &MoleculeId, tags: &[String]) -> HarvestDecision {
    let galaxy = root.path();
    let state = galaxy.join(".cosmon/state");
    authorize_harvest(
        &HeldLock,
        &HarvestAuthorityConfig {
            required: true,
            ..Default::default()
        },
        &HarvestRequest {
            galaxy_root: galaxy,
            state_root: &state,
            galaxy: "cosmon",
            molecule,
            mission: None,
            tags,
            base: "main",
            invocation_id: "inv-2",
        },
    )
    .expect("authorization decision")
}

fn fixture() -> (TempDir, Operator, MoleculeId, HarvestGrant) {
    let root = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(root.path().join(".cosmon/state")).expect("state dir");
    let operator = Operator::from_seed(7);
    std::fs::write(
        root.path().join(".cosmon/harvest.pub"),
        operator.public_key_file(),
    )
    .expect("public key");
    let molecule = mol("task-20260901-6da6");
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
    .expect("grant");
    (root, operator, molecule, grant)
}

#[test]
fn relabeling_a_signed_molecule_grant_cannot_spend_it_twice() {
    let (root, operator, molecule, grant) = fixture();
    let state = root.path().join(".cosmon/state");
    assert_eq!(grant.fingerprint().as_str(), MOLECULE_PERMIT);
    let ratified = signed(grant.clone(), &operator, true);
    store_authorization(&state, &molecule, &ratified).expect("store");
    assert!(matches!(
        run(&root, &molecule, &[]),
        HarvestDecision::Authorized(_)
    ));

    // Only the unsigned discriminator changes; the signed bytes and signature
    // remain identical. Decode through the same JSON path used by filestore.
    let mut wire = serde_json::to_value(&ratified).expect("encode");
    wire["authority"] = serde_json::json!("delegated");
    let relabeled: DoneAuthorization = serde_json::from_value(wire).expect("decode");
    store_authorization(&state, &molecule, &relabeled).expect("replace grant");
    match run(&root, &molecule, &[]) {
        HarvestDecision::AlreadyLanded(record) => {
            assert_eq!(record.permit.as_str(), MOLECULE_PERMIT)
        }
        other => panic!("same signed grant acquired another spend: {other:?}"),
    }
}

#[test]
fn historical_delegated_alias_blocks_a_new_spend() {
    let (root, operator, molecule, grant) = fixture();
    let state = root.path().join(".cosmon/state");
    let delegated = signed(grant, &operator, false);
    store_authorization(&state, &molecule, &delegated).expect("store");
    let ledger = FileConsumptionLedger::at_state_root(&state);
    let record = ConsumptionRecord {
        permit: serde_json::from_value(serde_json::json!(DELEGATED_MOLECULE_ALIAS))
            .expect("historical permit id"),
        grant: delegated.grant().fingerprint(),
        effect: cosmon_core::harvest_authorization::HarvestEffect {
            galaxy: "cosmon".into(),
            molecule: molecule.clone(),
            base: "main".into(),
        },
        key_id: OperatorKeyId::parse(&operator.key_id_display()).expect("key id"),
        invocation_id: "historical".into(),
    };
    assert_eq!(record.permit.as_str(), DELEGATED_MOLECULE_ALIAS);
    ledger.consume(&record).expect("old receipt");
    assert!(matches!(
        run(&root, &molecule, &[]),
        HarvestDecision::AlreadyLanded(_)
    ));
}

#[test]
fn a_removed_reservation_refuses_the_old_ratification() {
    let (root, operator, molecule, _) = fixture();
    let grant = HarvestGrant::new(
        "cosmon",
        HarvestScope::Molecule {
            molecule: molecule.clone(),
        },
        "main",
        HarvestAction::Done,
        ["needs-review"],
        GrantEpoch::first(),
        None,
    )
    .expect("grant");
    store_authorization(
        root.path().join(".cosmon/state"),
        &molecule,
        &signed(grant, &operator, true),
    )
    .expect("store");
    assert!(matches!(
        run(&root, &molecule, &[]),
        HarvestDecision::Refused(_, _)
    ));
}

#[test]
fn a_decoded_mission_cannot_be_relabelled_as_ratification() {
    let (root, operator, _molecule, _) = fixture();
    let mission = mol("delib-20260819-cda2");
    let grant = HarvestGrant::new(
        "cosmon",
        HarvestScope::Mission {
            mission: mission.clone(),
            policy_digest: cosmon_core::harvest_authorization::policy_digest(b""),
        },
        "main",
        HarvestAction::Done,
        Vec::<String>::new(),
        GrantEpoch::first(),
        None,
    )
    .expect("mission grant");
    let delegated = signed(grant, &operator, false);
    let mut wire = serde_json::to_value(delegated).expect("encode");
    wire["authority"] = serde_json::json!("ratified");
    let path = root.path().join(".cosmon/state/harvest/grants");
    std::fs::create_dir_all(&path).expect("grant dir");
    std::fs::write(
        path.join("mission.json"),
        serde_json::to_vec(&wire).expect("json"),
    )
    .expect("grant file");
    assert!(
        cosmon_filestore::harvest_authority::load_authorizations(root.path().join(".cosmon/state"))
            .is_err(),
        "decoded mission ratification must be rejected"
    );
    let decoded: DoneAuthorization = serde_json::from_value(wire).expect("decode variant");
    let verifier =
        cosmon_filestore::harvest_authority::MinisignHarvestVerifier::resolve(root.path())
            .expect("read trust root")
            .expect("pinned root");
    let facts = HarvestFacts {
        galaxy: "cosmon".into(),
        molecule: mission.clone(),
        mission: Some(mission),
        policy_digest: Some(cosmon_core::harvest_authorization::policy_digest(b"")),
        base: "main".into(),
        action: HarvestAction::Done,
        reservations_crossed: Vec::new(),
        epoch: GrantEpoch::first(),
        now: chrono::Utc::now(),
    };
    assert!(matches!(
        authorize(&decoded, &facts, None, &verifier),
        Err(HarvestRefusal::InvalidGrant(_))
    ));
}

#[test]
fn conflicting_receipt_aliases_refuse_instead_of_selecting_one() {
    let (root, operator, molecule, grant) = fixture();
    let state = root.path().join(".cosmon/state");
    let delegated = signed(grant, &operator, false);
    store_authorization(&state, &molecule, &delegated).expect("store");
    let ledger = FileConsumptionLedger::at_state_root(&state);
    let canonical = serde_json::json!({
        "permit": MOLECULE_PERMIT, "grant": MOLECULE_PERMIT,
        "effect": {"galaxy":"cosmon","molecule":molecule,"base":"main"},
        "key_id": OperatorKeyId::parse(&operator.key_id_display()).expect("key id"),
        "invocation_id":"original"
    });
    let record: ConsumptionRecord = serde_json::from_value(canonical.clone()).expect("receipt");
    ledger.consume(&record).expect("canonical receipt");
    let mut conflicting = canonical;
    conflicting["permit"] = serde_json::json!(DELEGATED_MOLECULE_ALIAS);
    conflicting["invocation_id"] = serde_json::json!("different");
    let record: ConsumptionRecord = serde_json::from_value(conflicting).expect("alias receipt");
    ledger.consume(&record).expect("alias receipt");
    let result = authorize_harvest(
        &HeldLock,
        &HarvestAuthorityConfig {
            required: true,
            ..Default::default()
        },
        &HarvestRequest {
            galaxy_root: root.path(),
            state_root: &state,
            galaxy: "cosmon",
            molecule: &molecule,
            mission: None,
            tags: &[],
            base: "main",
            invocation_id: "inv-2",
        },
    );
    assert!(
        result.is_err(),
        "conflicting aliases cannot report a landed outcome"
    );
}

#[test]
fn decoded_fields_and_reserved_delegation_are_refused_before_effect() {
    let (root, operator, molecule, grant) = fixture();
    let state = root.path().join(".cosmon/state");
    for malformed in [
        {
            let mut wire =
                serde_json::to_value(signed(grant.clone(), &operator, true)).expect("json");
            wire["grant"]["base"] = serde_json::json!("main\nforged=field");
            wire
        },
        {
            let mut wire =
                serde_json::to_value(signed(grant.clone(), &operator, true)).expect("json");
            wire["grant"]["reservations"] = serde_json::json!(["security", "needs-review"]);
            wire
        },
        {
            let reserved = HarvestGrant::new(
                "cosmon",
                HarvestScope::Molecule {
                    molecule: molecule.clone(),
                },
                "main",
                HarvestAction::Done,
                ["needs-review"],
                GrantEpoch::first(),
                None,
            )
            .expect("reserved grant");
            serde_json::to_value(signed(reserved, &operator, false)).expect("json")
        },
    ] {
        let path = state.join("harvest/grants");
        std::fs::create_dir_all(&path).expect("grant dir");
        std::fs::write(
            path.join("invalid.json"),
            serde_json::to_vec(&malformed).expect("json"),
        )
        .expect("grant file");
        assert!(cosmon_filestore::harvest_authority::load_authorizations(&state).is_err());
        std::fs::remove_file(path.join("invalid.json")).expect("remove fixture");
    }
}
