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

use cosmon_transport::claude::{
    check_alive, kill_session, session_config, spawn_claude_session, PermissionMode,
};

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

/// Issue #6 (Jesse Thaler) — the headless `spawn_claude_session` briefing
/// path must reach first output: no `--prompt` crash (#6.1) and no stdin
/// hang (#6.3). We stand in a stub `claude` on PATH that (a) hard-fails if
/// it ever sees the removed `--prompt` flag, and (b) copies whatever arrives
/// on stdin to an output file. Success = the briefing bytes land in that
/// file, proving `-p < <briefing>` delivered the mission without an escaping
/// hang.
#[test]
fn headless_briefing_reaches_first_output_via_stdin() {
    cleanup();

    let dir = tempfile::tempdir().expect("tempdir");
    let work_dir = dir.path();
    let stub_out = work_dir.join("stub_output.txt");

    // Stub `claude`: fail loudly on `--prompt`, otherwise drain stdin to a file.
    let stub = work_dir.join("claude");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in\n\
             *--prompt*) echo 'error: unknown option --prompt' >&2; exit 1;;\n\
             esac\n\
             cat > '{}'\n",
            stub_out.display()
        ),
    )
    .expect("write stub");
    let mut perm = std::fs::metadata(&stub).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&stub, perm).expect("chmod stub");

    // Put the stub dir at the front of PATH *before* the tmux server for this
    // socket is created, so the server snapshot resolves `claude` to the stub.
    let orig_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: single-threaded test; PATH is restored below.
    unsafe {
        std::env::set_var("PATH", format!("{}:{orig_path}", work_dir.display()));
    }

    let briefing = "TRIVIAL_MISSION_MARKER\nsecond line with 'quotes' and $VARS\n".to_owned();
    let config = session_config(
        TEST_SOCKET,
        "claude-headless-brief",
        work_dir.to_str().expect("utf8 work_dir"),
        Clearance::Execute,
        Some(briefing.clone()),
    );
    spawn_claude_session(&config).expect("headless spawn succeeds");

    // Poll for the stub to drain stdin (bounded — a hang is the failure).
    let mut got = String::new();
    for _ in 0..100 {
        if let Ok(contents) = std::fs::read_to_string(&stub_out) {
            if contents.contains("TRIVIAL_MISSION_MARKER") {
                got = contents;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // SAFETY: single-threaded test; restore the original PATH.
    unsafe {
        std::env::set_var("PATH", orig_path);
    }
    cleanup();

    assert!(
        got.contains("TRIVIAL_MISSION_MARKER"),
        "briefing must reach the worker on stdin (no --prompt crash, no hang); got: {got:?}"
    );
    assert!(
        got.contains("$VARS"),
        "the full multi-line briefing must arrive verbatim, unescaped; got: {got:?}"
    );
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
