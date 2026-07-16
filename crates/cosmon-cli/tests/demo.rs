// SPDX-License-Identifier: AGPL-3.0-only

//! Integration coverage for `cs demo`.
//!
//! Full nucleate→tackle→wait→done round-trip requires tmux + a dispatched
//! worker, which is exercised by the Gas Town smoke-test formula in CI.
//! Here we keep the fast, deterministic checks: CLI wiring, fail-fast
//! semantics for bad inputs, and stable help output.

use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

#[test]
fn demo_help_renders() {
    let out = cs().args(["demo", "--help"]).output().expect("spawn cs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--prompt"));
    assert!(stdout.contains("--formula"));
    assert!(stdout.contains("--no-teardown"));
    assert!(stdout.contains("--timeout"));
    // C4 a20b — --adapter is part of the demo surface, with an example
    // that names the ADR-106 canonical `llama-cpp` so the help page itself
    // documents the legacy-alias canonicalisation.
    assert!(stdout.contains("--adapter"));
    assert!(stdout.contains("llama-cpp"));
}

#[test]
fn demo_listed_in_top_level_help() {
    let out = cs().arg("--help").output().expect("spawn cs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("demo"),
        "`cs --help` should list the demo subcommand: {stdout}"
    );
}

#[test]
fn demo_empty_prompt_is_rejected() {
    // Explicit empty `--prompt` must fail fast rather than trigger
    // interactive fallback. This guards against accidental hangs in
    // scripted pipelines.
    let out = cs()
        .args(["demo", "--prompt", "   "])
        .output()
        .expect("spawn cs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("empty"), "stderr was: {stderr}");
}

#[test]
fn demo_without_tty_and_without_prompt_fails_fast() {
    // No TTY (piped stdin in the test harness) and no `--prompt`: we
    // must bail instead of blocking on stdin forever.
    let out = cs().arg("demo").output().expect("spawn cs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a TTY") || stderr.contains("--prompt"),
        "stderr was: {stderr}"
    );
}
