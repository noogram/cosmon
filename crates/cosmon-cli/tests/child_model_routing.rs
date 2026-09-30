// SPDX-License-Identifier: AGPL-3.0-only

//! A planned child's model recommendation must survive nucleation and dispatch.

use std::fs;
use std::process::Command;

#[test]
fn planned_child_model_wins_over_its_formula_and_explicit_tackle_still_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let formulas = root.join(".cosmon/formulas");
    fs::create_dir_all(&formulas).unwrap();
    fs::write(
        root.join(".cosmon/config.toml"),
        "[project]\nproject_id = 'routing-test'\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(root.join(".gitignore"), ".cosmon/state/\n.worktrees/\n").unwrap();
    fs::write(
        formulas.join("routing-test.formula.toml"),
        "formula = 'routing-test'\nversion = 1\ndescription = 'Routing test'\nid_prefix = 'route'\n[[steps]]\nid = 'work'\ntitle = 'Work'\ndescription = 'Work'\nacceptance = 'Done'\nmodel = 'formula-model'\n",
    )
    .unwrap();
    git(&[
        "add",
        ".cosmon/config.toml",
        ".cosmon/formulas",
        ".gitignore",
    ]);
    git(&[
        "-c",
        "user.name=Noogram",
        "-c",
        "user.email=noogram@invalid",
        "commit",
        "-q",
        "-m",
        "fixture",
    ]);
    let bin = env!("CARGO_BIN_EXE_cs");
    let output = Command::new(bin)
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "nucleate",
            "routing-test",
            "--var",
            "cosmon_model=child-model",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleated: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = nucleated["id"].as_str().unwrap();

    for (explicit, expected) in [
        (None, "child-model"),
        (Some("operator-model"), "operator-model"),
    ] {
        let mut cmd = Command::new(bin);
        cmd.current_dir(root)
            .env_remove("COSMON_DEFAULT_MODEL")
            .args(["tackle", id, "--dry-run", "--no-worktree"]);
        if let Some(model) = explicit {
            cmd.args(["--model", model]);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = fs::read_to_string(root.join(".cosmon/state/events.jsonl")).unwrap();
        let selected: Vec<serde_json::Value> = events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["type"] == "model_selected")
            .collect();
        let last = selected.last().unwrap();
        assert_eq!(last["model"], expected);
        assert_eq!(
            last["selection_source"]["source"],
            if explicit.is_some() {
                "flag"
            } else {
                "molecule_pin"
            }
        );
    }

    let before = fs::read_to_string(root.join(".cosmon/state/events.jsonl")).unwrap();
    let output = Command::new(bin)
        .current_dir(root)
        .env_remove("COSMON_DEFAULT_MODEL")
        .args([
            "run",
            "--resident",
            "--timeout",
            "3",
            "--poll-interval",
            "1",
        ])
        .output()
        .unwrap();
    let after = fs::read_to_string(root.join(".cosmon/state/events.jsonl")).unwrap();
    assert!(
        after.len() > before.len(),
        "run did not dispatch: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let new_events = &after[before.len()..];
    let selected: Vec<serde_json::Value> = new_events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["type"] == "model_selected")
        .collect();
    assert!(
        selected.iter().any(|event| event["mol_id"] == id
            && event["model"] == "child-model"
            && event["selection_source"]["source"] == "molecule_pin"),
        "cs run did not use the child's model recommendation: {new_events}"
    );
}
