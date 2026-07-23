// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the `CLAUDE_CONFIG_DIR` propagation through
//! `cs tackle` → tmux `new-session`.
//!
//! Background — `claude-account` (and the pizzaiolo multi-forfait baker)
//! relies on `CLAUDE_CONFIG_DIR` to pin a Claude Code session to a given
//! OAuth identity. The resolution order is:
//!
//! 1. `cb next` — round-robin account balancer. If on PATH and exits 0,
//!    its stdout is an email. Config dir: `~/.claude-accounts/<email>/`.
//! 2. `CLAUDE_CONFIG_DIR` env var — backward-compat fallback.
//! 3. Neither — no prefix emitted; Claude uses default config.
//!
//! The fix lives in [`cosmon_cli::tackle_env::build_claude_command`] —
//! a pure helper that accepts a `cb_runner` closure and an `env_lookup`
//! closure (so tests do not need subprocess or env mutation).

use cosmon_cli::tackle_env::build_claude_command;

/// When both cb and env are absent, the assembled command has no
/// `CLAUDE_CONFIG_DIR` token but does include the Gödel self-reference
/// guard vars (`CB_SESSION_ROLE=worker CB_DEPTH=1`).
#[test]
fn absent_cb_and_env_yields_byte_identical_legacy_command() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-20260522-62c3",
        "/usr/local/bin/claude",
        "bypassPermissions",
        &[],
        || None,
        |_| None,
    );
    // Note: the trailing `2> <mol_dir>/worker.stderr` redirect is the C2
    // crash-trail capture (task-20260614-e483 / delib-20260614-98f2),
    // already merged into `build_claude_command`.
    assert_eq!(
        cmd,
        "CB_SESSION_ROLE=worker CB_DEPTH=1 \
         COSMON_MOL_DIR=/state/mol-A \
         COSMON_PARENT_MOL_ID=task-20260522-62c3 \
         /usr/local/bin/claude --permission-mode bypassPermissions \
         --disallowedTools 'mcp__playwright-extension mcp__claude-in-chrome' \
         2> /state/mol-A/worker.stderr"
    );
    assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
}

/// `cb next` returns an email → config dir is derived from HOME.
#[test]
fn cb_next_success_derives_config_dir_from_email() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-20260522-62c3",
        "claude",
        "bypassPermissions",
        &[],
        || Some("user-b@example.org".to_owned()),
        |k| match k {
            "HOME" => Some("/Users/you".to_owned()),
            _ => None,
        },
    );
    assert!(
        cmd.starts_with("CLAUDE_CONFIG_DIR=/Users/you/.claude-accounts/user-b@example.org/ "),
        "got {cmd:?}"
    );
    assert!(cmd.contains("COSMON_MOL_DIR=/state/mol-A"));
}

/// `cb next` overrides `CLAUDE_CONFIG_DIR` env when both are present.
#[test]
fn cb_next_takes_precedence_over_env_var() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-20260522-62c3",
        "claude",
        "bypassPermissions",
        &[],
        || Some("operator@example.org".to_owned()),
        |k| match k {
            "HOME" => Some("/Users/you".to_owned()),
            "CLAUDE_CONFIG_DIR" => Some("/should/be/ignored".to_owned()),
            _ => None,
        },
    );
    assert!(cmd.contains(".claude-accounts/operator@example.org/"));
    assert!(!cmd.contains("/should/be/ignored"));
}

/// When `cb next` fails, env fallback kicks in.
#[test]
fn env_fallback_when_cb_fails() {
    let value = "/Users/you/.claude-forfait-2";
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-20260522-62c3",
        "claude",
        "bypassPermissions",
        &[],
        || None,
        |k| (k == "CLAUDE_CONFIG_DIR").then(|| value.to_owned()),
    );
    assert!(
        cmd.starts_with(&format!("CLAUDE_CONFIG_DIR={value} ")),
        "expected CLAUDE_CONFIG_DIR prefix, got {cmd:?}"
    );
    assert!(cmd.contains("COSMON_MOL_DIR=/state/mol-A"));
    assert!(cmd.contains("COSMON_PARENT_MOL_ID=task-20260522-62c3"));
    // C2 worker.stderr redirect (already merged) is the real tail; the
    // headless browser-MCP strip (task-20260704-f153) sits just before it.
    assert!(cmd.ends_with(
        "claude --permission-mode bypassPermissions \
         --disallowedTools 'mcp__playwright-extension mcp__claude-in-chrome' \
         2> /state/mol-A/worker.stderr"
    ));
}

/// Empty-string env is treated as absent.
#[test]
fn empty_claude_config_dir_is_treated_as_absent() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-20260522-62c3",
        "claude",
        "bypassPermissions",
        &[],
        || None,
        |k| (k == "CLAUDE_CONFIG_DIR").then(String::new),
    );
    assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
}

/// Shell-safety: paths with embedded spaces are single-quoted.
#[test]
fn path_with_spaces_is_shell_quoted() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-Y",
        "claude",
        "bypassPermissions",
        &[],
        || None,
        |k| (k == "CLAUDE_CONFIG_DIR").then(|| "/Users/Foo Bar/.claude".to_owned()),
    );
    assert!(cmd.starts_with("CLAUDE_CONFIG_DIR='/Users/Foo Bar/.claude' "));
}

/// POSIX single-quote escape for paths with embedded quotes.
#[test]
fn path_with_embedded_quote_is_posix_escaped() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-Y",
        "claude",
        "bypassPermissions",
        &[],
        || None,
        |k| (k == "CLAUDE_CONFIG_DIR").then(|| "/Users/it's/me".to_owned()),
    );
    assert!(cmd.starts_with("CLAUDE_CONFIG_DIR='/Users/it'\\''s/me' "));
}

/// `cb next` with whitespace-only output falls through to env.
#[test]
fn cb_next_whitespace_only_falls_through() {
    let cmd = build_claude_command(
        "/state/mol-A",
        "task-Y",
        "claude",
        "bypassPermissions",
        &[],
        || Some("  \n".to_owned()),
        |k| (k == "CLAUDE_CONFIG_DIR").then(|| "/fallback".to_owned()),
    );
    assert!(cmd.starts_with("CLAUDE_CONFIG_DIR=/fallback "));
}
