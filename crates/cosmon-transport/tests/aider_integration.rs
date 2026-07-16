// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for Aider session management via tmux.
//!
//! These tests require tmux to be installed and are gated behind
//! the `integration` feature flag:
//!
//! ```bash
//! cargo test --features integration -p cosmon-transport --test aider_integration
//! ```
//!
//! The lifecycle test does NOT require the `aider` binary on PATH —
//! it exercises the tmux plumbing by spawning a `sleep` command in
//! place of `aider`, then verifies that `check_alive` / `kill_session`
//! observe the session correctly. The same shape as
//! `claude_integration.rs`.

#![cfg(feature = "integration")]

use cosmon_transport::aider::{check_alive, kill_session, session_config, AiderPermissionFlags};

use cosmon_core::clearance::Clearance;

const TEST_SOCKET: &str = "cosmon-aider-integration-test";

fn cleanup() {
    let _ = std::process::Command::new("tmux")
        .args(["-L", TEST_SOCKET, "kill-server"])
        .output();
}

#[test]
fn test_aider_spawn_check_kill_lifecycle() {
    cleanup();

    let session = "aider-test-lifecycle";

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
fn test_aider_check_alive_nonexistent() {
    cleanup();

    let alive = check_alive(TEST_SOCKET, "nonexistent-aider-session", None)
        .expect("check_alive should not error");
    assert!(!alive, "nonexistent session should not be alive");

    cleanup();
}

#[test]
fn test_aider_kill_nonexistent_returns_error() {
    cleanup();

    let result = kill_session(TEST_SOCKET, "ghost-aider-session", None);
    assert!(result.is_err(), "killing nonexistent session should fail");

    cleanup();
}

#[test]
fn test_aider_session_config_builder() {
    let config = session_config(
        "my-socket",
        "my-session",
        "/home/user/project",
        Clearance::Write,
        "kimi-k2.6",
        Some("do the thing".to_owned()),
    );

    assert_eq!(config.socket, "my-socket");
    assert_eq!(config.session_name, "my-session");
    assert_eq!(config.work_dir, "/home/user/project");
    assert_eq!(config.permission_flags, AiderPermissionFlags::AcceptEdits);
    assert_eq!(config.model, "kimi-k2.6");
    assert_eq!(config.prompt.as_deref(), Some("do the thing"));
}

#[test]
fn test_aider_clearance_mapping_exhaustive() {
    let cases = [
        (Clearance::Read, AiderPermissionFlags::Plan),
        (Clearance::Write, AiderPermissionFlags::AcceptEdits),
        (Clearance::Execute, AiderPermissionFlags::Bypass),
    ];

    for (clearance, expected) in cases {
        let flags = AiderPermissionFlags::from(clearance);
        assert_eq!(
            flags, expected,
            "Clearance::{clearance:?} should map to {expected:?}"
        );
    }
}

/// Pane-signature regression — the default registry (C3) populated
/// with the Aider entry from C4 must match a live tmux session whose
/// `pane_current_command` reads as `"sleep"` only if we lie about
/// it. We can't easily force the pane command, so this test asserts
/// the structural pre-condition: the registered signatures include
/// the known Aider variants. The C5/C6 integration will exercise
/// the full propulsion flow against a real Aider worker.
#[test]
fn test_aider_pane_signature_registered_in_default_registry() {
    use cosmon_transport::registry::default_registry;

    let r = default_registry();
    assert!(r.matches("aider", "aider"));
    assert!(r.matches("aider", "python"));
    assert!(r.matches("aider", "python3"));
    assert!(r.matches("aider", "python3.11"));
}
