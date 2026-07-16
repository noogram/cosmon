// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the codex worker command assembly — the codex
//! sibling of `cosmon-cli/tests/tackle_claude_config_dir.rs`.
//!
//! Background — task-20260711-246d made codex's interactive TUI the default
//! launch mode so a codex worker is pilotable exactly like a claude worker
//! (whisper-steerable, clean pane, autonomous to completion). The pure,
//! injectable seam is [`cosmon_transport::codex::build_codex_command`]
//! (mirror of `cosmon_cli::tackle_env::build_claude_command`): it decides
//! the exact bytes handed to the tmux backend, so it is unit-testable
//! without spawning tmux or the `codex` binary.
//!
//! Two invariants this file pins:
//! 1. **Interactive is the default and steerable** — quiet telemetry
//!    (`RUST_LOG=error`), the autonomy + inline-scrollback flags, and *no*
//!    positional prompt (the prompt is injected into the composer after
//!    readiness, mirroring the claude paste-then-Enter dance).
//! 2. **Exec stays byte-identical** — `[adapters.codex].mode = "exec"`
//!    reproduces the legacy `codex exec '<prompt>'` fire-and-forget shape,
//!    so the batch path never regresses.

use std::path::PathBuf;

use cosmon_transport::codex::{
    build_codex_command, CodexMode, CodexSessionConfig, DEFAULT_INTERACTIVE_ARGS,
    INTERACTIVE_LOG_LEVEL,
};

/// Build a config with the shared fixture fields, varying only the axes
/// each test cares about.
fn config(mode: CodexMode, prompt: Option<&str>, extra_args: Vec<String>) -> CodexSessionConfig {
    CodexSessionConfig {
        socket: "cosmon".to_owned(),
        session_name: "polecat-codex".to_owned(),
        work_dir: "/state/wt".to_owned(),
        binary: PathBuf::from("codex"),
        prompt: prompt.map(str::to_owned),
        mode,
        model: None,
        extra_args,
        telemetry: None,
        pre_existing_worker: None,
    }
}

/// The default (`CodexMode::Interactive`) command is the steerable TUI:
/// quiet telemetry prefix + bare binary + the autonomy / inline-scrollback
/// defaults, and crucially NO positional prompt (it is injected after
/// readiness so it also gets submitted — the claude-mirror).
#[test]
fn interactive_default_is_quiet_steerable_and_promptless() {
    let cmd = build_codex_command(&config(
        CodexMode::Interactive,
        Some("write the failing test first"),
        vec![],
    ));
    assert_eq!(
        cmd,
        "RUST_LOG=error codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen"
    );
    // The prompt must never leak onto the interactive command line.
    assert!(!cmd.contains("write the failing test first"), "got {cmd:?}");
    // No `exec` subcommand — this is the interactive TUI, not batch.
    assert!(!cmd.contains(" exec"), "got {cmd:?}");
    // The quiet prefix keeps the `cs peek` pane free of OTEL INFO noise.
    assert!(cmd.starts_with(&format!("RUST_LOG={INTERACTIVE_LOG_LEVEL} ")));
    // Each documented default flag is present.
    for flag in DEFAULT_INTERACTIVE_ARGS {
        assert!(
            cmd.contains(flag),
            "default flag {flag} missing from {cmd:?}"
        );
    }
}

/// `CodexMode::default()` is Interactive — selecting `--adapter codex` with
/// no config gives the steerable pane (parity with claude).
#[test]
fn mode_default_is_interactive() {
    assert_eq!(CodexMode::default(), CodexMode::Interactive);
}

/// `[adapters.codex].mode = "exec"` reproduces the legacy fire-and-forget
/// shape byte-for-byte, with the prompt baked into the command line.
#[test]
fn exec_mode_is_byte_identical_legacy_shape() {
    let cmd = build_codex_command(&config(CodexMode::Exec, Some("run the batch job"), vec![]));
    assert_eq!(cmd, "codex exec 'run the batch job'");
}

/// Exec mode single-quote-escapes an apostrophe in the prompt (POSIX
/// `'\''` dance) — the batch prompt survives the shell round-trip.
#[test]
fn exec_mode_escapes_prompt_apostrophe() {
    let cmd = build_codex_command(&config(CodexMode::Exec, Some("it's a batch"), vec![]));
    assert_eq!(cmd, "codex exec 'it'\\''s a batch'");
}

/// A non-empty `extra_args` replaces the interactive defaults verbatim —
/// the per-installation escape hatch (e.g. a model pin or a softer sandbox
/// posture). The quiet prefix is retained; the prompt stays off the line.
#[test]
fn interactive_extra_args_override_replaces_defaults() {
    let cmd = build_codex_command(&config(
        CodexMode::Interactive,
        Some("ignored"),
        vec![
            "--sandbox".to_owned(),
            "workspace-write".to_owned(),
            "-m".to_owned(),
            "gpt-5-codex".to_owned(),
        ],
    ));
    assert_eq!(
        cmd,
        "RUST_LOG=error codex --sandbox workspace-write -m gpt-5-codex"
    );
    // Overriding drops the nuclear default flag.
    assert!(!cmd.contains("--dangerously-bypass-approvals-and-sandbox"));
    assert!(!cmd.contains("ignored"));
}

/// Config-string parsing fails *open* to the steerable interactive mode —
/// a typo in `mode = "…"` must never silently drop a worker into the
/// non-steerable batch path.
#[test]
fn mode_parse_fails_open_to_interactive() {
    assert_eq!(CodexMode::from_config_str("exec"), CodexMode::Exec);
    assert_eq!(CodexMode::from_config_str("EXEC"), CodexMode::Exec);
    assert_eq!(
        CodexMode::from_config_str("interactive"),
        CodexMode::Interactive
    );
    assert_eq!(CodexMode::from_config_str(""), CodexMode::Interactive);
    assert_eq!(CodexMode::from_config_str("typo"), CodexMode::Interactive);
}
