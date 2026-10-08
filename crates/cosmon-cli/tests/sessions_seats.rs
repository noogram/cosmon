// SPDX-License-Identifier: AGPL-3.0-only

//! Wire contracts for the versioned seats document and legacy list stream.

use std::process::Command;

use chrono::Utc;
use cosmon_core::id::SessionId;
use cosmon_core::presence::Presence;
use cosmon_filestore::PresenceStore;

#[test]
fn seats_is_one_document_and_list_remains_ndjson() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let presence = Presence::new(
        SessionId::new("pilot-one").unwrap(),
        "fixture",
        "/tmp",
        0,
        Utc::now(),
    );
    PresenceStore::new(&state).upsert(&presence).unwrap();

    let seats = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args([
            "--config",
            state.to_str().unwrap(),
            "--json",
            "sessions",
            "seats",
        ])
        .output()
        .unwrap();
    assert!(seats.status.success());
    let document: serde_json::Value = serde_json::from_slice(&seats.stdout).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["galaxies"].as_array().unwrap().len(), 1);

    let list = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args([
            "--config",
            state.to_str().unwrap(),
            "--json",
            "sessions",
            "list",
        ])
        .output()
        .unwrap();
    assert!(list.status.success());
    let lines = String::from_utf8(list.stdout).unwrap();
    assert_eq!(lines.lines().count(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines.trim()).unwrap()["session_id"],
        "pilot-one"
    );
}

#[test]
fn seats_discovers_tagged_and_ledger_only_missions() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let molecule = state.join("fleets/default/molecules/task-20261008-tagged");
    std::fs::create_dir_all(&molecule).unwrap();
    std::fs::write(
        molecule.join("state.json"),
        r#"{"id":"task-20261008-tagged","fleet_id":"default","formula_id":"task-work","status":"running","variables":{},"assigned_worker":null,"created_at":"2026-10-08T00:00:00Z","updated_at":"2026-10-08T00:00:00Z","total_steps":1,"current_step":0,"completed_steps":[],"collapse_reason":null,"collapsed_step":null,"links":[],"kind":null,"tags":["pilot-mission"]}"#,
    ).unwrap();
    std::fs::create_dir_all(state.join("pilot-lease")).unwrap();
    std::fs::write(
        state.join("pilot-lease/task-20261008-ledger.requests.jsonl"),
        "",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args([
            "--config",
            state.to_str().unwrap(),
            "--json",
            "sessions",
            "seats",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let missions = doc["galaxies"][0]["missions"].as_array().unwrap();
    assert_eq!(missions.len(), 2);
    assert!(missions
        .iter()
        .any(|mission| mission["mission"] == "task-20261008-tagged"));
    assert!(missions
        .iter()
        .any(|mission| mission["mission"] == "task-20261008-ledger"));
}
