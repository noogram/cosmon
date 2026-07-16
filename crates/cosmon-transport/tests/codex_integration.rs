// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for Codex (`@openai/codex`) session management
//! via tmux. Sibling of `claude_integration.rs` and
//! `aider_integration.rs`.
//!
//! These tests require tmux to be installed and are gated behind the
//! `integration` feature flag:
//!
//! ```bash
//! cargo test --features integration -p cosmon-transport --test codex_integration
//! ```
//!
//! The lifecycle test does NOT require the `codex` binary on PATH —
//! it exercises the tmux plumbing by spawning a `sleep` command in
//! place of `codex`, then verifies that `check_alive` / `kill_session`
//! observe the session correctly. The same shape as the aider sibling
//! (forgemaster §3.3: load-bearing forcing function is a real tmux
//! lifecycle, not a mock).

#![cfg(feature = "integration")]

use cosmon_transport::codex::{check_alive, kill_session, ADAPTER_NAME, DEFAULT_PIN_PATH};

const TEST_SOCKET: &str = "cosmon-codex-integration-test";

fn cleanup() {
    let _ = std::process::Command::new("tmux")
        .args(["-L", TEST_SOCKET, "kill-server"])
        .output();
}

#[test]
fn test_codex_spawn_check_kill_lifecycle() {
    cleanup();

    let session = "codex-test-lifecycle";

    let status = std::process::Command::new("tmux")
        .args([
            "-L",
            TEST_SOCKET,
            "new-session",
            "-d",
            "-s",
            session,
            "sleep 300",
        ])
        .status()
        .expect("tmux should be available");
    assert!(status.success(), "tmux new-session should succeed");

    let alive = check_alive(TEST_SOCKET, session, None).expect("check_alive should succeed");
    assert!(alive, "session should be alive after spawn");

    kill_session(TEST_SOCKET, session, None).expect("kill_session should succeed");

    let alive = check_alive(TEST_SOCKET, session, None).expect("check_alive should succeed");
    assert!(!alive, "session should be dead after kill");

    cleanup();
}

#[test]
fn test_codex_check_alive_nonexistent() {
    cleanup();

    let alive = check_alive(TEST_SOCKET, "nonexistent-codex-session", None)
        .expect("check_alive should not error");
    assert!(!alive, "nonexistent session should not be alive");

    cleanup();
}

#[test]
fn test_codex_kill_nonexistent_returns_error() {
    cleanup();

    let result = kill_session(TEST_SOCKET, "ghost-codex-session", None);
    assert!(result.is_err(), "killing nonexistent session should fail");

    cleanup();
}

/// Pane-signature regression — the default registry must match both
/// `codex` and `node` as foreground
/// commands. The `@openai/codex` npm package may surface as either:
/// the wrapper shim is named `codex` but the Node.js entry point that
/// keeps the process alive often shows up as `node` in tmux's
/// `pane_current_command`.
#[test]
fn test_codex_pane_signature_registered_in_default_registry() {
    use cosmon_transport::registry::default_registry;

    let r = default_registry();
    assert!(r.matches(ADAPTER_NAME, "codex"));
    assert!(r.matches(ADAPTER_NAME, "node"));
    assert!(!r.matches(ADAPTER_NAME, "claude"));
    assert!(!r.matches(ADAPTER_NAME, "aider"));
}

/// The default pin path is the operator-visible address the codex
/// Adapter consults for the SF-7 three-pillar check.
/// Asserting the constant pins the
/// contract so a future rename surfaces here first.
#[test]
fn test_codex_default_pin_path_is_stable() {
    assert_eq!(DEFAULT_PIN_PATH, ".cosmon/adapters/codex.toml");
}
