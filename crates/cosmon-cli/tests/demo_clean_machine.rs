// SPDX-License-Identifier: AGPL-3.0-only

//! Clean-machine end-to-end gate for `cs demo`.
//!
//! # What this proves
//!
//! `cs demo` runs the full **nucleate → tackle → wait → render** cycle on a
//! *fresh clone* with **zero private siblings** (no `/srv/cosmon/`), **zero
//! pre-seeded state**, in a throwaway temp dir, and **no operator config
//! leaking in** — the stranger's-machine contract. Today the first-run gesture
//! IS `cs demo`; if it dies opaquely on a clean box the product is dead on
//! contact (jobs, convergence #3). This test pins that it does not.
//!
//! # Why this is deterministic in any CI (no tmux, no adapter, no network)
//!
//! The cycle is driven through a **native-only formula** (`cosmon::noop`
//! steps). Native steps execute in-process and *cascade* to completion inside
//! a single `cs tackle` — they bypass `TransportBackend` entirely: no tmux
//! session, no git worktree, no LLM adapter. So the round-trip completes the
//! same way on a laptop with the full stack and on a bare CI runner with
//! neither tmux nor `claude` installed. The actionable-fast-fail preflight
//! (the W3 sibling deliverable) sits *after* the native-routing early return,
//! so a native molecule never needs tmux to reach Completed.
//!
//! The teardown half (`cs done` → git merge) is the one step that genuinely
//! needs a worktree/branch to exist; a native molecule has neither, so we run
//! `--no-teardown` and assert the molecule is left Completed. The full
//! teardown round-trip is exercised by the Gas Town smoke-test formula in CI
//! (see `docs/cs-demo-design.md` §7).

use std::fs;
use std::path::Path;
use std::process::Command;

/// A native-only formula whose steps cascade to completion inside one
/// `cs tackle`, with no tmux / worktree / adapter. This is what lets the
/// demo round-trip run deterministically on a bare machine.
const NOOP_DEMO_FORMULA: &str = r#"formula = "noop-demo"
version = 1
description = "Two no-op native steps — deterministic demo round-trip with no tmux/adapter"
id_prefix = "noop"

[tier]
level = 0

[[steps]]
id = "first"
title = "first no-op"
description = "Native: cosmon::noop"
native = "cosmon::noop"
timeout = 30

[[steps]]
id = "second"
title = "second no-op"
description = "Native: cosmon::noop"
needs = ["first"]
native = "cosmon::noop"
timeout = 30
"#;

/// Spawn the `cs` binary with a hermetically isolated environment — no
/// operator config, no parent-molecule wiring, no `/srv/cosmon/` sibling can
/// influence resolution. The redirected `COSMON_CONFIG_HOME` points at a
/// stable nonexistent path so the global-config tier reads as absent and the
/// resolver falls through to the in-repo `.cosmon/config.toml`.
fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env(
            "COSMON_CONFIG_HOME",
            std::env::temp_dir().join("cosmon-test-xdg-isolated-demo-clean-machine"),
        );
    cmd
}

/// Scaffold a minimal, self-contained cosmon project at `dir` — exactly what
/// a stranger gets from `git clone` + `cs init`, nothing more. No private
/// formulas beyond the one native formula the demo will run.
fn setup_clean_project(dir: &Path) {
    let cosmon_dir = dir.join(".cosmon");
    fs::create_dir_all(cosmon_dir.join("state")).unwrap();
    fs::create_dir_all(cosmon_dir.join("formulas")).unwrap();

    let cfg = "[project]\nproject_id = \"demo-clean-machine-20d4\"\n";
    fs::write(cosmon_dir.join("config.toml"), cfg).unwrap();
    fs::write(
        cosmon_dir.join("formulas").join("noop-demo.formula.toml"),
        NOOP_DEMO_FORMULA,
    )
    .unwrap();

    // A git repo is the substrate (find_repo_root walks up for `.git`), but
    // the demo molecule is native so no worktree/branch is ever created.
    run_git(dir, &["init", "-q"]);
    fs::write(dir.join(".gitignore"), ".cosmon/state/\n").unwrap();
    fs::write(dir.join("README.md"), "# demo-clean-machine\n").unwrap();
    run_git(dir, &["add", "."]);
    run_git(
        dir,
        &[
            "-c",
            "user.email=test@cosmon.test",
            "-c",
            "user.name=cosmon-test",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
}

fn run_git(dir: &Path, args: &[&str]) {
    let _ = Command::new("git").args(args).current_dir(dir).output();
}

/// Locate the single molecule directory under the fleet-scoped layout.
fn locate_molecule_dir(state_dir: &Path) -> Option<std::path::PathBuf> {
    let fleets_dir = state_dir.join("fleets");
    let entries = fs::read_dir(&fleets_dir).ok()?;
    for fleet in entries.flatten() {
        let molecules = fleet.path().join("molecules");
        if let Ok(mol_entries) = fs::read_dir(&molecules) {
            let dirs: Vec<_> = mol_entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect();
            if dirs.len() == 1 {
                return Some(dirs.into_iter().next().unwrap());
            }
        }
    }
    None
}

/// **The clean-machine gate.** `cs demo` walks nucleate → tackle → wait →
/// render to a Completed molecule on a fresh temp dir, no tmux, no adapter,
/// no private siblings, no operator config.
#[test]
fn demo_runs_end_to_end_on_clean_temp_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    setup_clean_project(tmp.path());
    let state_dir = tmp.path().join(".cosmon").join("state");

    let out = cs()
        .current_dir(tmp.path())
        .args([
            "demo",
            "--formula",
            "noop-demo",
            "--prompt",
            "demonstrate the first-run cycle",
            "--no-teardown",
            "--timeout",
            "60",
        ])
        .output()
        .expect("spawn cs demo");

    assert!(
        out.status.success(),
        "cs demo must run end-to-end on a clean temp dir.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // The molecule landed on disk and ran to Completed.
    let mol_dir = locate_molecule_dir(&state_dir)
        .expect("cs demo must create exactly one molecule under <state>/fleets/<fleet>/molecules/");

    // prompt.md is always written by `cs nucleate` — the proof-of-work seed
    // the demo's render step falls back to when no synthesis exists.
    assert!(
        mol_dir.join("prompt.md").is_file(),
        "prompt.md must exist in {}",
        mol_dir.display(),
    );

    // The state file records a terminal-completed molecule (native cascade
    // drove it there without any tmux/worktree).
    let state_json = fs::read_to_string(mol_dir.join("state.json"))
        .unwrap_or_else(|e| panic!("read state.json in {}: {e}", mol_dir.display()));
    let state: serde_json::Value =
        serde_json::from_str(&state_json).expect("state.json must be valid JSON");
    let status = state
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert_eq!(
        status, "completed",
        "the demo molecule must reach Completed via the native cascade; state.json was:\n{state_json}",
    );
}

/// `cs demo` in `--json` mode emits the canonical NDJSON event stream
/// (`demo_start` … `demo_done`) on a clean machine — the scripting surface
/// the design doc promises, proven to work without tmux/adapter.
#[test]
fn demo_json_event_stream_on_clean_temp_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    setup_clean_project(tmp.path());

    let out = cs()
        .current_dir(tmp.path())
        .args([
            "--json",
            "demo",
            "--formula",
            "noop-demo",
            "--prompt",
            "json mode on a clean box",
            "--no-teardown",
            "--timeout",
            "60",
        ])
        .output()
        .expect("spawn cs demo --json");

    assert!(
        out.status.success(),
        "cs demo --json must succeed on a clean temp dir.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let events: Vec<String> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| {
            v.get("event")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert!(
        events.contains(&"demo_start".to_owned()),
        "missing demo_start event in:\n{stdout}",
    );
    assert!(
        events.contains(&"demo_done".to_owned()),
        "missing demo_done event in:\n{stdout}",
    );
}
