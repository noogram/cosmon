// SPDX-License-Identifier: AGPL-3.0-only

//! Local-adapter model selection must be *discoverable* (COSMON #23).
//!
//! # The defect these tests freeze
//!
//! External tester Matteo Cacciari (a non-expert user) ran `cs demo` and
//! `cs tackle --adapter local` against Ollama, got `qwen3:8b` every time,
//! and found **no way to ask for a different local model**: no `--model`
//! on `cs demo`, no mention of a config key or an env var anywhere in
//! `cs demo --help`, and nothing in the dispatch output naming the model
//! that was actually chosen or how to change it.
//!
//! The resolution *mechanism* partly existed (`cs tackle --model`,
//! `[adapters.local].default_model`, `COSMON_LOCAL_MODEL`) but was
//! reachable only by reading the source. A capability nobody can find is,
//! from the user's chair, a capability that does not exist — which is why
//! this is filed as a defect and not an enhancement.
//!
//! # What is asserted
//!
//! 1. `cs demo` carries a `--model` flag, and its help page names the
//!    three override mechanisms with their precedence.
//! 2. Every dispatch to the `local` / `ollama` floor **announces the
//!    effective model on stderr**, with its origin, plus the one-line
//!    recipe for overriding it. That line is the discoverability surface:
//!    a user who never reads a guide still learns the model and the knob.
//! 3. The announced model tracks each mechanism: env var, per-galaxy
//!    config, and `--model` all move it off the `qwen3:8b` default.
//!
//! Every test runs `cs tackle --dry-run`, so **no live Ollama is
//! required** — the dry-run path walks the whole model-resolution chain
//! and returns before any backend is dialled or any worktree lands.

use std::fs;
use std::path::Path;
use std::process::Command;

/// The compile-time default. Selecting a model must move *off* this
/// value — a test that passed while the answer stayed `qwen3:8b` would
/// be asserting nothing (this is exactly the bug's shape).
const DEFAULT_LOCAL_MODEL: &str = "qwen3:8b";

/// A deliberately different model id. Never pulled by these tests — the
/// dry-run never dials Ollama, so the id only has to be *distinguishable*.
const OTHER_LOCAL_MODEL: &str = "llama3.2:3b";

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Hermetic model/adapter chain: strip every session-scoped hammer
        // the developer may have exported, so the test exercises the
        // documented resolution order rather than the ambient shell.
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env_remove("COSMON_DEFAULT_MODEL")
        .env_remove("ANTHROPIC_MODEL")
        .env_remove("COSMON_LOCAL_MODEL");
    cmd
}

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd);
    // Hermetic global-config tier: the operator's real
    // `~/.config/cosmon/config.toml` legitimately pins other adapters and
    // models; point the lookup at an empty dir under the per-test tmp.
    cmd.env("COSMON_CONFIG_HOME", cwd.join("isolated-config-home"));
    cmd
}

/// Nucleate a one-step molecule in a tempdir project; return
/// `(tmp, state_dir, molecule_id)`. `adapters_block` is appended verbatim
/// to `.cosmon/config.toml` so a test can declare `[adapters.local]`.
fn setup_project(adapters_block: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "local-model-test"
version = 1
description = "One-step formula for the local model-selection tests"
id_prefix = "lms"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step — the worker would do the work."
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("local-model-test.formula.toml"), formula_toml).unwrap();

    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "local-model-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        format!("[project]\nproject_id = \"local-model-test-2323\"\n\n{adapters_block}"),
    )
    .unwrap();

    (tmp, state_dir, molecule_id)
}

/// Run `cs tackle --dry-run` on the `local` adapter and return stderr.
fn dry_run_local_stderr(
    tmp: &Path,
    state_dir: &Path,
    mol_id: &str,
    extra: &[&str],
    env: &[(&str, &str)],
) -> String {
    let mut cmd = cosmon_bin_in(tmp);
    cmd.args([
        "tackle",
        mol_id,
        "--adapter",
        "local",
        "--dry-run",
        "--no-worktree",
        "--config",
        state_dir.to_str().unwrap(),
    ]);
    cmd.args(extra);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("tackle failed to spawn");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "tackle --dry-run should succeed: stderr={stderr}"
    );
    stderr
}

/// `cs demo` must carry a `--model` flag. Without it, the single command
/// cosmon puts in a newcomer's hands can only ever run one local model.
#[test]
fn demo_help_exposes_a_model_flag() {
    let out = cosmon_bin()
        .args(["demo", "--help"])
        .output()
        .expect("spawn cs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--model"),
        "`cs demo --help` must expose --model (COSMON #23): {stdout}"
    );
}

/// The demo help page must *name* the override mechanisms. A flag with
/// no stated precedence is only half-discoverable: the user still cannot
/// tell whether their config key or their env var will win.
#[test]
fn demo_help_names_the_local_model_override_mechanisms() {
    let out = cosmon_bin()
        .args(["demo", "--help"])
        .output()
        .expect("spawn cs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in ["COSMON_LOCAL_MODEL", "adapters.local", DEFAULT_LOCAL_MODEL] {
        assert!(
            stdout.contains(needle),
            "`cs demo --help` must mention `{needle}` so the local model is \
             selectable without reading the source (COSMON #23): {stdout}"
        );
    }
}

/// A bare local dispatch announces which model it resolved and how to
/// change it. This is the line Matteo never saw.
#[test]
fn local_dispatch_announces_the_default_model_and_the_knob() {
    let (tmp, state_dir, mol_id) = setup_project("");
    let stderr = dry_run_local_stderr(tmp.path(), &state_dir, &mol_id, &[], &[]);
    assert!(
        stderr.contains(DEFAULT_LOCAL_MODEL),
        "a local dispatch must name the model it resolved: {stderr}"
    );
    for needle in ["--model", "COSMON_LOCAL_MODEL", "adapters.local"] {
        assert!(
            stderr.contains(needle),
            "the local-model notice must name `{needle}` as an override \
             (COSMON #23): {stderr}"
        );
    }
}

/// `COSMON_LOCAL_MODEL` moves the resolved model off the default.
#[test]
fn env_var_selects_the_local_model() {
    let (tmp, state_dir, mol_id) = setup_project("");
    let stderr = dry_run_local_stderr(
        tmp.path(),
        &state_dir,
        &mol_id,
        &[],
        &[("COSMON_LOCAL_MODEL", OTHER_LOCAL_MODEL)],
    );
    assert!(
        stderr.contains(OTHER_LOCAL_MODEL),
        "COSMON_LOCAL_MODEL must select the dispatched model: {stderr}"
    );
    assert!(
        !stderr.contains(&format!("model {DEFAULT_LOCAL_MODEL}")),
        "the default must not be announced once a model is selected: {stderr}"
    );
}

/// `[adapters.local].default_model` in the per-galaxy `.cosmon/config.toml`
/// selects the model — the durable, per-project mechanism.
#[test]
fn config_default_model_selects_the_local_model() {
    let adapters = format!("[adapters.local]\ndefault_model = \"{OTHER_LOCAL_MODEL}\"\n");
    let (tmp, state_dir, mol_id) = setup_project(&adapters);
    let stderr = dry_run_local_stderr(tmp.path(), &state_dir, &mol_id, &[], &[]);
    assert!(
        stderr.contains(OTHER_LOCAL_MODEL),
        "[adapters.local].default_model must select the dispatched model: {stderr}"
    );
}

/// `cs tackle --model` outranks the config row — the per-molecule pin.
#[test]
fn model_flag_outranks_config_default_model() {
    let adapters = format!("[adapters.local]\ndefault_model = \"{DEFAULT_LOCAL_MODEL}\"\n");
    let (tmp, state_dir, mol_id) = setup_project(&adapters);
    let stderr = dry_run_local_stderr(
        tmp.path(),
        &state_dir,
        &mol_id,
        &["--model", OTHER_LOCAL_MODEL],
        &[],
    );
    assert!(
        stderr.contains(OTHER_LOCAL_MODEL),
        "--model must outrank [adapters.local].default_model: {stderr}"
    );
}
