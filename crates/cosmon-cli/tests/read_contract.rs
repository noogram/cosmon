// SPDX-License-Identifier: AGPL-3.0-only

//! Read contracts for external consumers (issue #164).
//!
//! A consumer that never imports cosmon code reads three artefacts: the
//! `events.jsonl` log, each molecule's `state.json`, and `cs ensemble --json`.
//! These tests drive the real `cs` binary through a molecule's life and assert
//! on the raw JSON such a consumer sees: a `schema_version` on each artefact,
//! step evidence and the completion summary carried as event fields, and the
//! canonical `molecule_id` on events.

use std::fs;
use std::path::Path;
use std::process::Command;

/// A `cs` command with no ambient `COSMON_*` redirection, run from an empty
/// directory outside any repository so the fixture touches only its tempdir.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_CONFIG")
        .env_remove("COSMON_CONFIG_HOME")
        .env_remove("COSMON_FORMULAS_DIR")
        .env_remove("COSMON_ARTIFACT_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .env("COSMON_ASSUME_TRUSTED", "1");
    cmd
}

fn run_ok(cmd: &mut Command, what: &str) -> String {
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read_events(state_dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(state_dir.join("events.jsonl"))
        .expect("events.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).expect("event line is JSON"))
        .collect()
}

/// Nucleate a two-step molecule, mark it running, and evolve it to completion.
/// Returns `(tmp, state_dir, molecule_id)`.
fn finished_molecule() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    let formula_path = formulas_dir.join("contract-test.formula.toml");
    fs::write(
        &formula_path,
        r#"
formula = "contract-test"
version = 1
description = "Read contract fixture"
id_prefix = "rc"

[[steps]]
id = "do"
title = "Do the thing"
description = "Work."
acceptance = "Done"

[[steps]]
id = "check"
title = "Check the thing"
description = "Verify."
needs = ["do"]
"#,
    )
    .unwrap();

    let out = run_ok(
        cs(tmp.path()).args([
            "--json",
            "nucleate",
            "contract-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ]),
        "nucleate",
    );
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).expect("nucleate JSON");
    let id = parsed["id"].as_str().expect("molecule id").to_owned();

    let state_path = state_dir
        .join("fleets/default/molecules")
        .join(&id)
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["status"] = serde_json::json!("running");
    fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    for evidence in ["first step evidence", "all done: summary text"] {
        run_ok(
            cs(tmp.path()).args([
                "evolve",
                &id,
                "--evidence",
                evidence,
                "--ops-dir",
                state_dir.to_str().unwrap(),
                "--formula",
                formula_path.to_str().unwrap(),
            ]),
            "evolve",
        );
    }
    (tmp, state_dir, id)
}

#[test]
fn events_carry_version_canonical_id_evidence_and_summary() {
    let (_tmp, state_dir, id) = finished_molecule();
    let events = read_events(&state_dir);
    let of_type =
        |t: &str| -> Vec<&serde_json::Value> { events.iter().filter(|e| e["type"] == t).collect() };

    for e in events.iter().filter(|e| e.get("type").is_some()) {
        assert_eq!(e["schema_version"], 1, "unversioned record: {e}");
    }

    let steps = of_type("molecule_step_completed");
    assert_eq!(steps.len(), 2, "two steps expected: {events:?}");
    assert_eq!(steps[0]["molecule_id"], id.as_str());
    assert_eq!(steps[0]["evidence"], "first step evidence");
    assert_eq!(steps[1]["evidence"], "all done: summary text");

    let done = of_type("molecule_completed");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["molecule_id"], id.as_str());
    assert_eq!(done[0]["summary"], "all done: summary text");
    // The existing field stays for existing readers.
    assert!(done[0]["reason"].is_string());
}

#[test]
fn state_json_carries_schema_version() {
    let (_tmp, state_dir, id) = finished_molecule();
    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["schema_version"], 1);
    // Existing readers still find their fields.
    assert_eq!(state["id"], id.as_str());
    assert_eq!(state["status"], "completed");
}

#[test]
fn ensemble_json_carries_schema_version() {
    let (tmp, state_dir, _id) = finished_molecule();
    let out = run_ok(
        cs(tmp.path())
            .env("COSMON_STATE_DIR", &state_dir)
            .args(["--json", "ensemble"]),
        "ensemble --json",
    );
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("ensemble JSON");
    assert_eq!(v["schema_version"], 1);
    assert!(v["workers"].is_array());
    let row = &v["molecule_states"][0];
    assert_eq!(row["phase"], "done");
    assert_eq!(row["fleet"], "default");
    assert!(row["updated_at"].is_string());
    assert!(row["typed_links"].is_array());
}
