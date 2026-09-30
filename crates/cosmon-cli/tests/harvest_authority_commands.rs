// SPDX-License-Identifier: AGPL-3.0-only

//! The local operator commands use the same authority facts as the route.

use std::process::Command;

use cosmon_core::harvest_authorization::{DoneAuthorization, HarvestGrant, OperatorHarvestSeal};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};
use cosmon_filestore::FileStore;
use cosmon_minisign_testkit::Operator;
use cosmon_state::{MoleculeData, StateStore};
use serde_json::{json, Value};

#[test]
fn local_configure_status_and_challenge_use_one_galaxy() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join(".cosmon/state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        root.path().join(".cosmon/config.toml"),
        "[project]\nproject_id = \"local-harvest\"\n",
    )
    .unwrap();
    let id = MoleculeId::new("task-20260930-a1b2").unwrap();
    let data: MoleculeData = serde_json::from_value(json!({
        "id": id.as_str(),
        "fleet_id": "default",
        "formula_id": "task-work",
        "status": "completed",
        "created_at": "2026-09-30T00:00:00Z",
        "updated_at": "2026-09-30T00:01:00Z",
        "total_steps": 2,
        "current_step": 2,
        "variables": {},
        "completed_steps": [],
        "links": []
    }))
    .unwrap();
    FileStore::new(&state).save_molecule(&id, &data).unwrap();

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cs"))
            .arg("--config")
            .arg(&state)
            .arg("harvest-authority")
            .args(args)
            .output()
            .unwrap()
    };
    let configured = run(&["configure", "--policy", "scoped"]);
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    assert!(
        std::fs::read_to_string(root.path().join(".cosmon/config.toml"))
            .unwrap()
            .contains("remote = \"scoped\"")
    );

    let status = run(&["status"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["policy"], "scoped");
    assert_eq!(status["provenance"], "explicit");

    let challenged = run(&[
        "challenge",
        "--molecule",
        id.as_str(),
        "--expires-at",
        "2100-01-01T00:00:00Z",
    ]);
    assert!(
        challenged.status.success(),
        "{}",
        String::from_utf8_lossy(&challenged.stderr)
    );
    let challenged: Value = serde_json::from_slice(&challenged.stdout).unwrap();
    assert!(challenged["canonical"]
        .as_str()
        .unwrap()
        .contains("galaxy=local-harvest\n"));
    assert!(challenged["canonical"]
        .as_str()
        .unwrap()
        .contains("epoch=1\n"));

    let operator = Operator::from_seed(63);
    let public_file = root.path().join("operator-public.key");
    std::fs::write(&public_file, operator.public_key_file()).unwrap();
    let sealed = run(&[
        "configure",
        "--policy",
        "sealed",
        "--public-key-file",
        public_file.to_str().unwrap(),
    ]);
    assert!(
        sealed.status.success(),
        "{}",
        String::from_utf8_lossy(&sealed.stderr)
    );

    let grant: HarvestGrant = serde_json::from_value(challenged["grant"].clone()).unwrap();
    let signature = operator.sign(&grant.canonical_bytes());
    let mut lines = signature.lines();
    let attestation = OperatorAttestation {
        key_id: OperatorKeyId::parse(&operator.key_id_display()).unwrap(),
        untrusted_comment: lines.next().unwrap().replace("untrusted comment: ", ""),
        signature: lines.next().unwrap().to_owned(),
        trusted_comment: lines.next().unwrap().replace("trusted comment: ", ""),
        global_signature: lines.next().unwrap().to_owned(),
    };
    let authorization =
        DoneAuthorization::Ratified(OperatorHarvestSeal::new(grant, attestation).unwrap());
    let grant_file = root.path().join("signed-grant.json");
    std::fs::write(&grant_file, serde_json::to_vec(&authorization).unwrap()).unwrap();
    let imported = run(&[
        "import",
        "--molecule",
        id.as_str(),
        "--file",
        grant_file.to_str().unwrap(),
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let imported: Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(imported["installed"], true);

    let checked = run(&["status", "--molecule", id.as_str()]);
    assert!(checked.status.success());
    let checked: Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(checked["grant"]["valid"], true);

    let mut tampered = serde_json::to_value(authorization).unwrap();
    tampered["grant"]["galaxy"] = json!("other-galaxy");
    std::fs::write(&grant_file, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let refused = run(&[
        "import",
        "--molecule",
        id.as_str(),
        "--file",
        grant_file.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());

    let disabled = run(&["configure", "--policy", "disabled"]);
    assert!(disabled.status.success());
    let refused_challenge = run(&["challenge", "--molecule", id.as_str()]);
    assert!(!refused_challenge.status.success());
}
