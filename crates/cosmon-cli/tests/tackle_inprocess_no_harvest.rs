// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tackle --adapter <direct-api>` must not invoke `install_harvest_hook`.
//!
//! Tactical regression test for GAP #1 from the academy smoke chronicle
//! `2026-05-18-grok-direct-api-smoke-result.md`. Pre-fix, the post-spawn
//! pipeline called `install_harvest_hook` and the tmux liveness re-check
//! unconditionally — which always failed for Direct-API adapters
//! (openai, anthropic) because they never create a tmux session. The
//! failure tore down the worktree and left the molecule stuck in
//! `pending` even when the in-process agent loop had completed
//! successfully.
//!
//! The fix gates both calls on `adapter_uses_tmux(&adapter)`. This test
//! pins the contract from two angles:
//!
//! 1. A negative integration test that runs `cs tackle --adapter
//!    anthropic` *without* `ANTHROPIC_API_KEY`. The Direct-API branch
//!    short-circuits with a typed "requires `ANTHROPIC_API_KEY`"
//!    diagnostic, and stderr must NOT mention `install_harvest_hook`
//!    or `pane-died hook` — proof that the path no longer routes
//!    through a tmux-postulated step.
//!
//! 2. The positive structural pin (the `tests` module inside
//!    `tackle.rs`) verifies that `adapter_uses_tmux` returns true for
//!    `claude` / `aider` and false for `openai` / `anthropic` — the
//!    typed predicate that gates both call sites.
//!
//! End-to-end verification of the molecule transitioning
//! `pending → running` for a Direct-API adapter requires either a live
//! API key or a mock HTTP server. Both paths exist as `#[ignore]`d
//! smokes in `cosmon-provider/tests/{openai,anthropic}_smoke.rs`; this
//! test deliberately covers the cheap negative invariant that
//! regression-tests the structural gate every CI run.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd);
    cmd
}

/// Set up a tempdir with a one-step formula and a nucleated molecule;
/// return `(tmp, state_dir, molecule_id)`. Mirrors the setup in
/// `tackle_adapter_flag.rs` — same shape, distinct fixture so the two
/// test files do not race on a shared scratch dir.
fn setup_project_with_molecule() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "tackle-inprocess-test"
version = 1
description = "One-step formula for the inprocess no-harvest regression test"
id_prefix = "ip"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step — the worker would do the work."
acceptance = "Done"
"#;
    fs::write(
        formulas_dir.join("tackle-inprocess-test.formula.toml"),
        formula_toml,
    )
    .unwrap();

    let state_str = state_dir.to_str().unwrap();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "tackle-inprocess-test",
            "--store-dir",
            state_str,
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

    // Minimal `.cosmon/config.toml` so `require_project_identity`
    // accepts the `cs tackle` invocation.
    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"tackle-inprocess-no-harvest-gap1\"\n",
    )
    .unwrap();

    // `cs tackle` (non-dry-run) requires a git repo at the discovery
    // root — `find_repo_root` walks up from the cwd. Initialise an
    // empty repo so the dispatch path can reach the Direct-API
    // credential-check site (the actual surface this test pins).
    // Without this, the test depends on stray `.git` directories
    // polluting `$TMPDIR` from other workers — the historical flake
    // mode that hid the GAP #1 regression for two weeks.
    let _ = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(tmp.path())
        .output();

    (tmp, state_dir, molecule_id)
}

/// `cs tackle --adapter anthropic` with `ANTHROPIC_API_KEY` unset must
/// fail with the typed "requires `ANTHROPIC_API_KEY`" diagnostic from the
/// Direct-API branch — **not** with the `install_harvest_hook` / `pane-died
/// hook` error the pre-fix pipeline would have emitted regardless of
/// what the spawn step returned.
///
/// This is the cheap, deterministic regression for GAP #1: even when the
/// Direct-API branch fails at the spawn step, the failure cannot be a
/// tmux-postulated step. Stronger end-to-end verification (the molecule
/// transitions `pending → running` for a Direct-API adapter) is covered
/// by the live smoke tests in `cosmon-provider/tests/` — gated by
/// `ANTHROPIC_LIVE_SMOKE=1` so they do not run in CI.
#[test]
fn tackle_anthropic_no_api_key_fails_without_install_harvest_hook_error() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .env_remove("ANTHROPIC_API_KEY")
        // ADR-110 / I2: `--no-worktree` on a non-worktree checkout is rejected
        // by the worker-isolation guard before dispatch ever reaches the
        // Direct-API credential check. This test deliberately writes on the
        // throwaway temp repo to pin that credential diagnostic, so it opts
        // into the documented test escape hatch the guard's own error names.
        .env("COSMON_ALLOW_NO_WORKTREE", "1")
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "anthropic",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");

    assert!(
        !output.status.success(),
        "tackle without ANTHROPIC_API_KEY must fail (Direct-API spawn rejects \
         missing credential): stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "stderr must name the missing credential (typed Direct-API diagnostic): \
         got: {stderr}"
    );
    assert!(
        !stderr.contains("install_harvest_hook")
            && !stderr.contains("pane-died hook")
            && !stderr.contains("no such window"),
        "stderr must NOT mention tmux-postulated post-spawn steps for a \
         Direct-API adapter — that was GAP #1 from the academy smoke \
         chronicle 2026-05-18-grok-direct-api-smoke-result.md: \
         {stderr}"
    );
}

/// Symmetric assertion for the openai Direct-API branch. Same contract,
/// distinct branch — the GAP #1 regression must close on both Direct-API
/// adapters at once (the chronicle observed openai first, anthropic
/// mirrors the same code path).
#[test]
fn tackle_openai_no_api_key_fails_without_install_harvest_hook_error() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .env_remove("OPENAI_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("MOONSHOT_API_KEY")
        // ADR-110 / I2 escape hatch — see the anthropic test above for why a
        // throwaway-repo credential-pin test opts into `--no-worktree`.
        .env("COSMON_ALLOW_NO_WORKTREE", "1")
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "openai",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");

    assert!(
        !output.status.success(),
        "tackle without any of OPENAI/XAI/MOONSHOT_API_KEY must fail: \
         stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("OPENAI_API_KEY") || stderr.contains("API_KEY"),
        "stderr must name a missing credential from the openai free-rider trio: \
         got: {stderr}"
    );
    assert!(
        !stderr.contains("install_harvest_hook")
            && !stderr.contains("pane-died hook")
            && !stderr.contains("no such window"),
        "stderr must NOT mention tmux-postulated post-spawn steps for a \
         Direct-API adapter: {stderr}"
    );
}
