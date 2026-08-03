// SPDX-License-Identifier: AGPL-3.0-only

//! A formula whose steps are shell work must not be dispatched to a
//! chat-only adapter — noogram/cosmon **#4, clause 2**.
//!
//! # What the reporter observed
//!
//! A `task-work`-shaped mission on the `local` adapter: 65 s of wall clock,
//! `log.md` reading *"in-process agent loop returned Ok"*, an empty branch,
//! an untouched energy budget, and no work product. The briefing had told
//! the worker to read `CLAUDE.md`, run the cargo gates, `git commit`, and
//! walk `cs evolve` / `cs complete` — a contract the ADR-100 Direct-API chat
//! loop can satisfy none of. As the reporter put it: *"the machinery runs
//! end-to-end but missions can't actually satisfy their own briefing."*
//!
//! The first fix made the *briefing* adapter-aware (commit `d81b58a`): a
//! local worker is no longer told to do what it cannot. That left the half
//! the reporter's own suggestion named — **gate formulas on adapter
//! capabilities** — because a formula's *step text* still describes the
//! work ("Run all gates: build, test, lint, format, doc"), and no amount of
//! prompt wording gives a chat loop a shell.
//!
//! # What this file pins
//!
//! End to end, through the real `cs` binary: a formula declaring
//! `requires_capabilities = ["shell", "vcs"]` is **refused before dispatch**
//! on a local adapter, with the typed exit code, while the same formula
//! dispatches normally to a coding-agent adapter and every formula that
//! declares nothing is untouched. Remove the guard and the first test goes
//! red: the tackle proceeds and exits 0 on the dry run.
//!
//! # Why `--dry-run`
//!
//! The property is *whether the dispatch is refused*, which is decided
//! before any worktree, pane or model call. Driving a real local worker
//! would need an Ollama on the machine and would measure the model, not the
//! gate. `--dry-run` renders the prompt a real dispatch would use and stops;
//! the guard sits ahead of that, so the dry run is a faithful probe of the
//! decision and a hermetic one.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Exit code for `GuardError::AdapterLacksCapability`, restated here as the
/// contract a script or an external scheduler branches on. A change to the
/// constant that is not a deliberate contract change fails this file.
const ADAPTER_LACKS_CAPABILITY: i32 = 17;

/// A `cs` invocation with the ambient cosmon session stripped, so the run is
/// hermetic: no inherited worker depth, no adapter hammer, no model pin left
/// over from the session that is running the test suite.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_DEFAULT_MODEL",
        "COSMON_SKIP_CAPABILITY_GATE",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
        "ANTHROPIC_MODEL",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

/// A galaxy carrying two formulas: one that declares shell + VCS
/// requirements (the `producer-work` / `merge-conflict` shape) and one that
/// declares nothing (every formula written before the field existed).
fn galaxy(tmp: &Path) -> std::path::PathBuf {
    let root = tmp.join("galaxy");
    fs::create_dir_all(&root).unwrap();
    assert!(cs(&root).arg("init").status().unwrap().success());

    let formulas = root.join(".cosmon").join("formulas");
    fs::create_dir_all(&formulas).unwrap();
    fs::write(
        formulas.join("shell-work.formula.toml"),
        r#"formula = "shell-work"
version = 1
description = "A mission whose contract is shell work."
id_prefix = "task"
requires_capabilities = ["shell", "vcs"]

[vars.topic]
description = "The task."
required = true

[[steps]]
id = "implement"
title = "Implement"
description = "Run the gates and commit."
"#,
    )
    .unwrap();
    fs::write(
        formulas.join("chat-work.formula.toml"),
        r#"formula = "chat-work"
version = 1
description = "A mission any worker can satisfy."
id_prefix = "task"

[vars.topic]
description = "The task."
required = true

[[steps]]
id = "answer"
title = "Answer"
description = "Write the answer to result.md."
"#,
    )
    .unwrap();
    root
}

/// Nucleate a molecule of `formula` and return its id.
fn nucleate(root: &Path, formula: &str) -> String {
    let out = cs(root)
        .args([
            "--json",
            "nucleate",
            formula,
            "--kind",
            "task",
            "--var",
            "topic=a mission that needs a shell",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "nucleate {formula} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("`cs nucleate --json` must emit json")
        .get("id")
        .and_then(|v| v.as_str())
        .expect("`cs nucleate --json` must carry the molecule id")
        .to_owned()
}

#[test]
fn a_shell_formula_is_refused_on_every_chat_only_adapter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mol = nucleate(&root, "shell-work");

    // Every adapter `egress::adapter_is_local` classifies as local — the
    // whole family, not just the `local` floor the reporter happened to use.
    for adapter in ["local", "ollama", "llama-cpp", "llama"] {
        let out = cs(&root)
            .args(["tackle", &mol, "--adapter", adapter, "--dry-run"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        assert_eq!(
            out.status.code(),
            Some(ADAPTER_LACKS_CAPABILITY),
            "--adapter {adapter} must be refused with the typed exit code.\n\
             stderr: {stderr}",
        );
        // The refusal must be THIS one, not another gate that also exits
        // non-zero and would make the assertion above vacuous.
        assert!(
            stderr.contains("shell, vcs"),
            "the refusal must name the missing capabilities.\nstderr: {stderr}",
        );
    }
}

#[test]
fn the_refusal_names_a_way_forward() {
    // A refusal an operator cannot act on is a stall with better manners.
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mol = nucleate(&root, "shell-work");

    let out = cs(&root)
        .args(["tackle", &mol, "--adapter", "local", "--dry-run"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        stderr.contains("--adapter claude"),
        "must name the adapter that works.\nstderr: {stderr}",
    );
    assert!(
        stderr.contains("COSMON_SKIP_CAPABILITY_GATE=1"),
        "must name the override for an operator who means it.\nstderr: {stderr}",
    );
}

#[test]
fn the_override_dispatches_anyway() {
    // The escape hatch is load-bearing: the local-floor acceptance tests
    // drive shell-shaped formulas onto a mocked chat model on purpose, and
    // an operator running an experiment must not be locked out by a gate
    // whose whole justification is "this will not work well".
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mol = nucleate(&root, "shell-work");

    let out = cs(&root)
        .args(["tackle", &mol, "--adapter", "local", "--dry-run"])
        .env("COSMON_SKIP_CAPABILITY_GATE", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the override must let the dispatch through.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn a_formula_that_declares_nothing_still_reaches_the_local_floor() {
    // The gate is opt-in per formula. Every formula written before the
    // field existed — including the ones `cs demo` routes to on the local
    // floor, which is the newcomer's first contact with cosmon — must be
    // completely unaffected.
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mol = nucleate(&root, "chat-work");

    let out = cs(&root)
        .args(["tackle", &mol, "--adapter", "local", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a formula declaring no capabilities must dispatch to the local \
         floor unchanged.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn a_shell_formula_reaches_a_coding_agent_adapter() {
    // The other half of the claim: the gate refuses a pairing, not a
    // formula. On an adapter that has a shell and VCS it must not fire.
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mol = nucleate(&root, "shell-work");

    let out = cs(&root)
        .args(["tackle", &mol, "--adapter", "claude", "--dry-run"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_ne!(
        out.status.code(),
        Some(ADAPTER_LACKS_CAPABILITY),
        "a coding-agent adapter must never trip the capability gate.\n\
         stderr: {stderr}",
    );
}
