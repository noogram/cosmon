// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test for tmux `send_input` cross-wiring under parallelism.
//!
//! When `send_input` used a
//! server-global paste-buffer name, concurrent `cs tackle` calls raced on the
//! same buffer and could cross-wire prompts between workers. The fix makes
//! the buffer name unique per call (PID + atomic counter).
//!
//! This test spawns `N = 4` tmux sessions each running a line-flushed `awk`
//! that appends every stdin line to a per-session capture file on an
//! isolated socket. It fires `send_input` concurrently with distinct
//! markers, loops many iterations, and asserts — from the files, not from
//! `capture_output` — that every session received exactly its own prompts
//! and no other session's. Using the filesystem as the oracle avoids the
//! flakiness of pane capture (wrap, scrollback, terminal echo).
//!
//! # TTY canonical-mode line limit
//!
//! Every marker stays well below `MAX_CANON` (the TTY canonical-mode line
//! limit — ~1024 bytes on Darwin, ~4096 on Linux). Oversized lines are
//! silently dropped by the line discipline before any program can read
//! them; the test would then be a no-op and silently pass. Keep payloads
//! small.
//!
//! # Running
//!
//! `#[ignore]`'d because it requires `tmux` and spawns real processes:
//!
//! ```bash
//! cargo test -p cosmon-transport -- --ignored
//! ```
//!
//! # Verification of the fix
//!
//! - Pre-fix (buffer name = literal `"cosmon-input"`): concurrent `load-buffer`
//!   calls race on the same name, so a later write overwrites an
//!   earlier-not-yet-pasted buffer and `paste-buffer -d` deletes state still
//!   owned by a peer. At least one session's file is missing its own marker
//!   or contains another session's marker.
//! - Post-fix (buffer name `cosmon-input-<pid>-<seq>` per call): every
//!   session's file contains exactly its own prompts in order.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use cosmon_core::agent::AgentRole;
use cosmon_core::id::AgentId;
use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};
use cosmon_transport::tmux::TmuxBackend;

const N: usize = 4;
const ITERATIONS: usize = 20;
// Markers must fit in a single canonical-mode line on every supported Unix;
// 256 bytes leaves comfortable headroom under the ~1024-byte Darwin limit.
const MARKER_PAD: usize = 256;

fn unique_socket() -> String {
    format!("cosmon-test-parallel-{}", std::process::id())
}

fn test_config() -> RuntimeConfig {
    RuntimeConfig {
        socket_name: String::new(), // unused by TmuxBackend
        session_prefix: String::new(),
    }
}

fn capture_path(index: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cosmon-test-parallel-{}-{index}.log",
        std::process::id()
    ))
}

fn capture_agent(index: usize) -> AgentDefinition {
    let path = capture_path(index);
    // `awk '{ print; fflush() }'` writes each Enter-terminated stdin line to
    // the side-channel file and forces a flush. `cat >> FILE` would be
    // simpler but stdout to a regular file is block-buffered, and
    // `stdbuf` / `cat -u` are not portable enough across the Unices we
    // support.
    AgentDefinition {
        id: AgentId::new(format!("race-{index}")).expect("valid agent id"),
        role: AgentRole::Implementation,
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            format!("awk '{{ print; fflush() }}' > {}", path.display()),
        ],
    }
}

fn kill_socket(socket: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .output();
}

/// Wait (bounded) until `path` contains a line starting with `needle`.
fn wait_for_line(path: &PathBuf, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(path) {
            if contents.lines().any(|l| l.starts_with(needle)) {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn send_input_does_not_cross_wire_under_parallelism() {
    let socket = unique_socket();
    kill_socket(&socket); // clean slate from any leaked prior run

    // Fresh capture files for a deterministic starting state.
    for i in 0..N {
        let _ = fs::remove_file(capture_path(i));
    }

    let backend = TmuxBackend::new(&socket);
    let config = test_config();

    let worker_ids: Vec<_> = (0..N)
        .map(|i| {
            backend
                .spawn(&capture_agent(i), &config)
                .unwrap_or_else(|e| panic!("spawn {i} failed: {e}"))
                .id
        })
        .collect();

    // Let each session's awk actually start reading stdin.
    thread::sleep(Duration::from_millis(500));

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for iter in 0..ITERATIONS {
            let prompts: Vec<String> = (0..N)
                .map(|i| format!("MARKER-{iter}-{i}-{}", "x".repeat(MARKER_PAD)))
                .collect();

            thread::scope(|s| {
                for (id, prompt) in worker_ids.iter().zip(&prompts) {
                    let backend = &backend;
                    s.spawn(move || {
                        backend
                            .send_input(id, prompt)
                            .unwrap_or_else(|e| panic!("send_input({id:?}) failed: {e}"));
                    });
                }
            });

            // Bound the wait per session so a flake fails fast rather than
            // hanging the suite.
            for i in 0..N {
                let prefix = format!("MARKER-{iter}-{i}-");
                let path = capture_path(i);
                assert!(
                    wait_for_line(&path, &prefix, Duration::from_secs(5)),
                    "iter {iter}: session {i} did not receive its own prompt \
                     (prefix {prefix:?}) within 5s; file = {:?}",
                    fs::read_to_string(&path).unwrap_or_default()
                );
            }

            // Primary assertion: every session's file contains its own
            // markers for every iteration so far, and NO other session's.
            // The second clause is the cross-wiring detector.
            for i in 0..N {
                let contents = fs::read_to_string(capture_path(i)).unwrap_or_default();
                for it in 0..=iter {
                    let own = format!("MARKER-{it}-{i}-");
                    assert!(
                        contents.contains(&own),
                        "iter {iter}: session {i} missing its own marker from iter {it} ({own:?})"
                    );
                    for j in 0..N {
                        if j == i {
                            continue;
                        }
                        let other = format!("MARKER-{it}-{j}-");
                        assert!(
                            !contents.contains(&other),
                            "iter {iter}: session {i} received session {j}'s marker \
                             from iter {it} ({other:?}) — CROSS-WIRING"
                        );
                    }
                }
            }
        }
    }));

    // Cleanup runs even on panic so we never leak test sessions.
    for id in &worker_ids {
        let _ = backend.terminate(id);
    }
    kill_socket(&socket);
    for i in 0..N {
        let _ = fs::remove_file(capture_path(i));
    }

    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}
