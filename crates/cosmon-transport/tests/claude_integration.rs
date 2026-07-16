// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for Claude session management via tmux.
//!
//! These tests require tmux to be installed and are gated behind
//! the `integration` feature flag:
//!
//! ```bash
//! cargo test --features integration -p cosmon-transport --test claude_integration
//! ```

#![cfg(feature = "integration")]

use cosmon_transport::claude::{check_alive, kill_session, session_config, PermissionMode};

use cosmon_core::clearance::Clearance;

const TEST_SOCKET: &str = "cosmon-claude-integration-test";

/// Clean up any leftover test sessions.
fn cleanup() {
    let _ = std::process::Command::new("tmux")
        .args(["-L", TEST_SOCKET, "kill-server"])
        .output();
}

#[test]
fn test_spawn_check_kill_lifecycle() {
    cleanup();

    // We can't spawn real claude in CI, so test the tmux plumbing
    // by spawning a sleep command manually, then using check_alive/kill_session.
    let session = "claude-test-lifecycle";

    // Spawn a sleep session directly via tmux
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

    // check_alive should return true
    let alive = check_alive(TEST_SOCKET, session, None).expect("check_alive should succeed");
    assert!(alive, "session should be alive after spawn");

    // kill_session should succeed
    kill_session(TEST_SOCKET, session, None).expect("kill_session should succeed");

    // check_alive should return false after kill
    let alive = check_alive(TEST_SOCKET, session, None).expect("check_alive should succeed");
    assert!(!alive, "session should be dead after kill");

    cleanup();
}

#[test]
fn test_check_alive_nonexistent() {
    cleanup();

    let alive = check_alive(TEST_SOCKET, "nonexistent-session", None)
        .expect("check_alive should not error");
    assert!(!alive, "nonexistent session should not be alive");

    cleanup();
}

#[test]
fn test_kill_nonexistent_returns_error() {
    cleanup();

    let result = kill_session(TEST_SOCKET, "ghost-session", None);
    assert!(result.is_err(), "killing nonexistent session should fail");

    cleanup();
}

#[test]
fn test_session_config_builder() {
    let config = session_config(
        "my-socket",
        "my-session",
        "/home/user/project",
        Clearance::Write,
        Some("do the thing".to_owned()),
    );

    assert_eq!(config.socket, "my-socket");
    assert_eq!(config.session_name, "my-session");
    assert_eq!(config.work_dir, "/home/user/project");
    assert_eq!(config.permission_mode, PermissionMode::AcceptEdits);
    assert_eq!(config.prompt.as_deref(), Some("do the thing"));
}

#[test]
fn test_clearance_permission_mapping_exhaustive() {
    // Verify all clearance levels map to expected permission modes
    let cases = [
        (Clearance::Read, PermissionMode::Plan),
        (Clearance::Write, PermissionMode::AcceptEdits),
        (Clearance::Execute, PermissionMode::BypassPermissions),
    ];

    for (clearance, expected) in cases {
        let mode = PermissionMode::from(clearance);
        assert_eq!(
            mode, expected,
            "Clearance::{clearance} should map to PermissionMode::{expected}"
        );
    }
}
