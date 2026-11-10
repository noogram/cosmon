// SPDX-License-Identifier: AGPL-3.0-only

//! Project gate instructions must survive the shipped formula and CLI paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cs(root: &Path, args: &[&str]) -> String {
    let output = Command::new(repo().join("scripts/no-pilot-env.sh"))
        .arg(env!("CARGO_BIN_EXE_cs"))
        .args(args)
        .env_remove("COSMON_CONFIG")
        .env_remove("COSMON_STATE_DIR")
        .current_dir(root)
        .output()
        .expect("run cs through the gate boundary");
    assert!(
        output.status.success(),
        "cs {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 CLI output")
}

fn galaxy(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("fresh disposable galaxy outside the repository");
    cs(dir.path(), &["init"]);
    let path = dir.path().join(".cosmon/config.toml");
    let mut body = fs::read_to_string(&path).expect("init config");
    body.push_str(config);
    fs::write(path, body).expect("configure project gates");
    dir
}

fn nucleate(root: &Path, formula: &str) -> (String, PathBuf, String) {
    let output = cs(
        root,
        &[
            "--json",
            "nucleate",
            formula,
            "--var",
            "topic=Exercise project verification",
        ],
    );
    let json: serde_json::Value = serde_json::from_str(&output).expect("nucleate JSON");
    let id = json["id"].as_str().expect("molecule id").to_owned();
    let briefing = root
        .join(".cosmon/state/fleets/default/molecules")
        .join(&id)
        .join("briefing.md");
    let text = fs::read_to_string(&briefing).expect("durable rendered briefing");
    (id, briefing, text)
}

#[test]
fn shipped_task_work_names_python_gates_without_cargo() {
    let dir = galaxy("\n[gates]\ntest_command = 'python3 -m unittest discover'\n");
    let (id, briefing, text) = nucleate(dir.path(), "task-work");
    assert!(text.contains("python3 -m unittest discover"), "{text}");
    assert!(!text.contains("cargo"), "{text}");

    // Set up the running phase without spawning a model: the operation under
    // test is the real CLI's next-step rendering, not worker dispatch.
    let state_path = briefing.with_file_name("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).expect("molecule state"))
            .expect("state JSON");
    state["status"] = serde_json::json!("running");
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("encode fixture"),
    )
    .expect("set up running molecule");
    cs(
        dir.path(),
        &[
            "evolve",
            &id,
            "--evidence",
            "Implementation compiled",
            "--formula",
            ".cosmon/formulas/task-work.formula.toml",
        ],
    );
    let evolved = fs::read_to_string(briefing).expect("evolved briefing");
    assert!(!evolved.contains("cargo"), "{evolved}");
    assert!(evolved.contains(".cosmon/config.toml"), "{evolved}");
    let prompt = cs(
        dir.path(),
        &["tackle", &id, "--adapter", "claude", "--dry-run"],
    );
    assert!(prompt.contains("python3 -m unittest discover"), "{prompt}");
    assert!(!prompt.contains("cargo"), "{prompt}");
}

#[test]
fn producer_and_merge_formulas_use_the_projects_gates() {
    for formula in ["producer-work", "merge-conflict"] {
        let dir = galaxy("\n[gates]\ntest_command = 'python3 -m unittest discover'\n");
        let filename = format!("{formula}.formula.toml");
        fs::copy(
            repo().join(".cosmon/formulas").join(&filename),
            dir.path().join(".cosmon/formulas").join(filename),
        )
        .expect("install the repository's optional formula");
        let (_, _, text) = nucleate(dir.path(), formula);
        assert!(text.contains("python3 -m unittest discover"), "{text}");
        assert!(!text.contains("cargo"), "{text}");
    }
}

#[test]
fn undeclared_gates_refer_to_project_documentation() {
    let dir = galaxy("");
    let (id, _, text) = nucleate(dir.path(), "task-work");
    assert!(
        text.contains(
            "## Verification gates\n\nRun the project's own test, lint and build gates \
             (see its CLAUDE.md / CONTRIBUTING).\n\n"
        ),
        "the generated gate section must supply the fallback itself: {text}"
    );
    assert!(!text.contains("cargo"), "{text}");

    let prompt = cs(
        dir.path(),
        &["tackle", &id, "--adapter", "claude", "--dry-run"],
    );
    assert!(prompt.contains("[gates]"), "{prompt}");
    assert!(prompt.contains("verification gates"), "{prompt}");
    assert!(prompt.contains("CLAUDE.md / CONTRIBUTING"), "{prompt}");
    assert!(!prompt.contains("cargo"), "{prompt}");
}

#[test]
fn cosmon_config_keeps_every_declared_cargo_gate() {
    let dir = galaxy("");
    fs::copy(
        repo().join(".cosmon/config.toml"),
        dir.path().join(".cosmon/config.toml"),
    )
    .expect("use cosmon's actual gate configuration in a disposable galaxy");
    let (_, _, text) = nucleate(dir.path(), "task-work");
    for command in [
        "cargo check --workspace",
        "cargo test --workspace --no-fail-fast",
        "cargo clippy --workspace -- -D warnings",
        "cargo fmt --all -- --check",
        "RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps",
    ] {
        assert!(text.contains(command), "missing {command}: {text}");
    }
}
