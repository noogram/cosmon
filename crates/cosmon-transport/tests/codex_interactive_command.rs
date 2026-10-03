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
//! Three invariants this file pins:
//! 1. **Interactive is the default and steerable** — quiet telemetry
//!    (`RUST_LOG=error`), the autonomy + inline-scrollback flags, and *no*
//!    positional prompt (the prompt is injected into the composer after
//!    readiness, mirroring the claude paste-then-Enter dance).
//! 2. **Exec keeps the legacy shape** — `[adapters.codex].mode = "exec"`
//!    reproduces the `codex exec '<prompt>'` fire-and-forget shape,
//!    so the batch path never regresses.
//! 3. **Update policy remains operator-owned** — command assembly does not
//!    inject `check_for_update_on_startup`; an explicit harness setting is
//!    carried unchanged in both launch modes.

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
        git_identity: None,
        writable_roots: vec![],
        harness_args: vec![],
        pass_api_key: false,
        work_hook_home: None,
        mol_dir: None,
        parent_mol_id: None,
    }
}

#[test]
fn hook_trust_bypass_is_member_only() {
    let ordinary = config(CodexMode::Interactive, None, vec![]);
    let baseline = build_codex_command(&ordinary);
    assert!(!baseline.contains("--dangerously-bypass-hook-trust"));
    let mut member = ordinary;
    member.work_hook_home = Some(std::path::PathBuf::from("/tmp/work hooks"));
    let command = build_codex_command(&member);
    assert!(
        command.contains("CODEX_HOME='/tmp/work hooks'"),
        "{command}"
    );
    assert!(command.contains("COSMON_MOL_DIR=/tmp"), "{command}");
    assert!(
        command.contains("--dangerously-bypass-hook-trust"),
        "{command}"
    );
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
        "env -u OPENAI_API_KEY -u CODEX_API_KEY RUST_LOG=error codex \
         -p cosmon-worker-polecat-codex \
         --dangerously-bypass-approvals-and-sandbox --no-alt-screen"
    );
    // The prompt must never leak onto the interactive command line.
    assert!(!cmd.contains("write the failing test first"), "got {cmd:?}");
    // No `exec` subcommand — this is the interactive TUI, not batch.
    assert!(!cmd.contains(" exec"), "got {cmd:?}");
    // The quiet prefix keeps the `cs peek` pane free of OTEL INFO noise, after
    // the default API-key strip (observed 2026-09-26).
    assert!(cmd.contains(&format!("RUST_LOG={INTERACTIVE_LOG_LEVEL} ")));
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
/// shape, with the prompt baked into the command line.
#[test]
fn exec_mode_is_byte_identical_legacy_shape() {
    let cmd = build_codex_command(&config(CodexMode::Exec, Some("run the batch job"), vec![]));
    assert_eq!(
        cmd,
        "env -u OPENAI_API_KEY -u CODEX_API_KEY codex exec 'run the batch job'"
    );
}

/// Exec mode single-quote-escapes an apostrophe in the prompt (POSIX
/// `'\''` dance) — the batch prompt survives the shell round-trip.
#[test]
fn exec_mode_escapes_prompt_apostrophe() {
    let cmd = build_codex_command(&config(CodexMode::Exec, Some("it's a batch"), vec![]));
    assert_eq!(
        cmd,
        "env -u OPENAI_API_KEY -u CODEX_API_KEY codex exec 'it'\\''s a batch'"
    );
}

/// Startup update policy belongs to the operator. An absent setting must not
/// be invented, while either explicit value must survive command assembly in
/// both modes.
#[test]
fn startup_update_policy_is_operator_owned() {
    for mode in [CodexMode::Interactive, CodexMode::Exec] {
        let bare = build_codex_command(&config(mode, Some("work"), vec![]));
        assert!(
            !bare.contains("check_for_update_on_startup"),
            "got {bare:?}"
        );

        for value in ["true", "false"] {
            let mut explicit = config(mode, Some("work"), vec![]);
            explicit.harness_args = vec![
                "-c".to_owned(),
                format!("check_for_update_on_startup={value}"),
            ];
            let cmd = build_codex_command(&explicit);
            assert!(
                cmd.contains(&format!("-c check_for_update_on_startup={value}")),
                "operator update policy missing from {cmd:?}"
            );
        }
    }
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
        "env -u OPENAI_API_KEY -u CODEX_API_KEY RUST_LOG=error codex \
         -p cosmon-worker-polecat-codex \
         --sandbox workspace-write -m gpt-5-codex"
    );
    // Overriding drops the nuclear default flag.
    assert!(!cmd.contains("--dangerously-bypass-approvals-and-sandbox"));
    assert!(!cmd.contains("ignored"));
}

/// Blocage 2 (task-20260723-91db): a codex worker's cwd is its worktree, but
/// the fleet lock it must write on `cs evolve` / `cs complete` lives in the
/// main repo's out-of-worktree `.cosmon/state/`. Declaring that dir via
/// codex's first-class `--add-dir` flag is what lets the worker persist its
/// own completion under a `workspace-write` sandbox. The flag is emitted in
/// both launch modes and survives an `extra_args` override (structural).
#[test]
fn writable_root_is_declared_in_both_modes_and_survives_override() {
    let root = PathBuf::from("/main-repo/.cosmon");

    let mut interactive = config(CodexMode::Interactive, Some("audit"), vec![]);
    interactive.writable_roots = vec![root.clone()];
    assert!(
        build_codex_command(&interactive).contains("--add-dir /main-repo/.cosmon"),
        "interactive spawn must declare the state dir writable"
    );

    let mut exec = config(CodexMode::Exec, Some("audit"), vec![]);
    exec.writable_roots = vec![root.clone()];
    assert!(
        build_codex_command(&exec).contains("--add-dir /main-repo/.cosmon"),
        "exec spawn must declare the state dir writable"
    );

    // A hardened `--sandbox workspace-write` override (which drops the nuclear
    // bypass default) must NOT strip the completion-lock writability.
    let mut overridden = config(
        CodexMode::Interactive,
        None,
        vec!["--sandbox".to_owned(), "workspace-write".to_owned()],
    );
    overridden.writable_roots = vec![root];
    let cmd = build_codex_command(&overridden);
    assert!(cmd.contains("--sandbox workspace-write"), "got {cmd:?}");
    assert!(cmd.contains("--add-dir /main-repo/.cosmon"), "got {cmd:?}");
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

/// Issue #84: a setting an operator changes inside a worker's codex TUI
/// (`/model` → reasoning level, "enter default") is written back by codex to
/// the active *user config layer*. Without a profile that layer is the
/// machine-wide `$CODEX_HOME/config.toml`, so the change leaks to every later
/// codex worker in every project.
///
/// The interactive worker must therefore launch with its own profile-v2
/// overlay (`-p <name>` → `$CODEX_HOME/<name>.config.toml`): codex reads the
/// global config underneath it as the default and writes the worker's changes
/// into the overlay. It must survive an `extra_args` override (structural,
/// like `--add-dir`), and must stay out of `codex exec`, which has no TUI.
#[test]
fn interactive_worker_writes_settings_to_its_own_profile_overlay() {
    let cmd = build_codex_command(&config(CodexMode::Interactive, None, vec![]));
    assert!(
        cmd.contains(" -p cosmon-worker-polecat-codex "),
        "interactive worker must select a per-worker profile overlay, got {cmd:?}"
    );

    let overridden = build_codex_command(&config(
        CodexMode::Interactive,
        None,
        vec!["--no-alt-screen".to_owned()],
    ));
    assert!(
        overridden.contains(" -p cosmon-worker-polecat-codex "),
        "an extra_args override must not drop the overlay, got {overridden:?}"
    );

    let exec = build_codex_command(&config(CodexMode::Exec, Some("go"), vec![]));
    assert!(
        !exec.contains(" -p "),
        "exec has no TUI to persist from, got {exec:?}"
    );
}

/// Issue #163 item 3: every codex launch line names the molecule, member or not,
/// so a wrapper the worker runs can stamp it.
#[test]
fn every_codex_launch_line_carries_the_molecule_env() {
    for mode in [CodexMode::Interactive, CodexMode::Exec] {
        let mut ordinary = config(mode, Some("go"), vec![]);
        assert!(ordinary.work_hook_home.is_none(), "not a work member");
        ordinary.mol_dir = Some(std::path::PathBuf::from("/tmp/state/mol X"));
        ordinary.parent_mol_id = Some("task-20261003-cd5e".to_owned());
        let command = build_codex_command(&ordinary);
        assert!(
            command.contains("COSMON_MOL_DIR='/tmp/state/mol X'"),
            "{command}"
        );
        assert!(
            command.contains("COSMON_PARENT_MOL_ID=task-20261003-cd5e"),
            "{command}"
        );
    }
}
