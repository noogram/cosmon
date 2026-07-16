// SPDX-License-Identifier: AGPL-3.0-only

//! Regression for the SIGINT-race spurious-error suppression.
//!
//! # What this proves
//!
//! When a SIGINT (or SIGTERM) lands on the runtime's process group while
//! an `cs ensemble --json` child is mid-flight, the child is killed by
//! the signal — `output.status.code()` returns `None` and `output.stderr`
//! is empty because the child never got to write anything. The pre-fix
//! loop translated this into a trace line that read literally:
//!
//! ```text
//! {"action":"tick","decision_basis":"ensemble-read-failed",
//!  "error":"cs ensemble failed for : exit -1: ", ...}
//! ```
//!
//! The empty token between *for* and *:* was misdiagnosed as a missing
//! CLI argument; it was the always-empty `mol_id` slot for the global
//! `ensemble` verb. The real bug was the spurious error line — a clean
//! shutdown should end with `shutdown / operator-signal`, not with a
//! signal-kill mistaken for a parse failure.
//!
//! The fix surfaces signal-kill as a distinct error
//! ([`cosmon_runtime::ResidentError::SubprocessInterrupted`]) which the
//! loop body promotes to a graceful shutdown. This test exercises that
//! contract end-to-end on **both** subprocess paths — `read_ensemble`
//! and `shell_out` (for `cs tackle` / `cs done`) — using Python stubs
//! that self-kill with the default SIGTERM handler, reliably producing
//! the `status.code() == None / stderr == ""` shape both callers see
//! when the operator presses Ctrl-C on `just self-runtime`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cosmon_runtime::{
    ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
};

mod common;

/// `cs`-protocol stub whose `ensemble` verb **self-kills with SIGTERM**.
///
/// `os.kill(os.getpid(), signal.SIGTERM)` against the default disposition
/// (Python installs no handler for SIGTERM) causes the kernel to mark the
/// process WIFSIGNALED — exactly the codepath `read_ensemble` exercises
/// when the operator's Ctrl-C propagates to an in-flight child.
const SUICIDE_STUB: &str = r#"#!/usr/bin/env python3
"""Self-signalling stub for the sigint-race regression.

`os.kill(os.getpid(), signal.SIGTERM)` exits via signal — the parent's
`output.status.code()` returns `None`, `output.stderr` is empty.
"""
import os
import signal
import sys


def main(argv):
    if len(argv) >= 2 and argv[1] == "ensemble":
        # Default disposition for SIGTERM is "terminate" — the kernel
        # marks the process WIFSIGNALED so the parent sees
        # `status.code() == None`. This is the same shape the loop
        # observes when SIGINT propagates from `Ctrl-C` on the operator's
        # `just self-runtime`.
        os.kill(os.getpid(), signal.SIGTERM)
        # Unreachable — kept so the script is still syntactically valid.
        return 0
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
"#;

fn make_executable(path: &PathBuf) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn signal_killed_ensemble_yields_clean_shutdown_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let stub_path = root.join("cs_suicide_stub.py");
    std::fs::write(&stub_path, common::with_fast_python_shebang(SUICIDE_STUB)).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    config.max_runtime = Some(Duration::from_secs(5));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    // The signal-killed child is itself the shutdown signal — the loop
    // promotes [`ResidentError::EnsembleInterrupted`] to a graceful
    // shutdown without ever needing the shutdown flag to flip. (In
    // production this models the in-the-wild race where the operator's
    // Ctrl-C reaches the child a fraction of a millisecond before our
    // own SIGINT handler flips the flag.)
    let summary = runtime
        .run(&shutdown)
        .expect("loop returns cleanly on signal-killed ensemble");

    assert_eq!(
        summary.exit,
        ExitReason::Shutdown,
        "expected Shutdown exit reason, got {:?}",
        summary.exit,
    );

    let trace = std::fs::read_to_string(&trace_path).expect("trace file exists");
    let lines: Vec<&str> = trace.lines().collect();

    // The decisive assertion: no `ensemble-read-failed` line was
    // written. Pre-fix, the loop would have produced exactly one with
    // `error: "cs ensemble failed for : exit -1: "`.
    let spurious: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.contains("ensemble-read-failed"))
        .collect();
    assert!(
        spurious.is_empty(),
        "found {} spurious ensemble-read-failed line(s) — the SIGINT race \
         regressed:\n{}",
        spurious.len(),
        spurious.join("\n"),
    );

    // And the symmetric positive assertion: the trace tail is the
    // shutdown handshake.
    let last = lines.last().expect("at least one trace line");
    let parsed: serde_json::Value = serde_json::from_str(last).expect("trace tail is JSON");
    assert_eq!(parsed["action"], "shutdown", "trace tail: {last}");
    assert_eq!(
        parsed["decision_basis"], "operator-signal",
        "trace tail decision_basis: {last}",
    );
}

/// Stub whose `ensemble` returns one ready molecule and whose `done`
/// self-kills with SIGTERM — the loop must suppress the spurious
/// `cs done failed for <id>: exit -1: ` decision line.
const DONE_SUICIDE_STUB: &str = r#"#!/usr/bin/env python3
"""`ensemble` advertises one completed molecule; `done` dies by signal.

This drives the loop through the decision-dispatch path (Decision::Done)
and exercises the SIGINT race in `shell_out`, the sibling of the
`read_ensemble` path covered by SUICIDE_STUB above.
"""
import json
import os
import signal
import sys


FLEET = {
    "molecules": [
        {"id": "ready", "status": "completed", "blocked_by": []},
    ]
}


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        sys.stdout.write(json.dumps(FLEET))
        return 0
    if verb in ("tackle", "done"):
        # Signal-kill so the parent observes status.code() == None.
        os.kill(os.getpid(), signal.SIGTERM)
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
"#;

#[test]
fn signal_killed_done_subprocess_yields_clean_shutdown_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let stub_path = root.join("cs_done_suicide_stub.py");
    std::fs::write(
        &stub_path,
        common::with_fast_python_shebang(DONE_SUICIDE_STUB),
    )
    .unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    config.max_runtime = Some(Duration::from_secs(5));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime
        .run(&shutdown)
        .expect("loop returns cleanly on signal-killed done");

    assert_eq!(
        summary.exit,
        ExitReason::Shutdown,
        "expected Shutdown, got {:?}",
        summary.exit,
    );

    let trace = std::fs::read_to_string(&trace_path).expect("trace file exists");
    let lines: Vec<&str> = trace.lines().collect();

    // Decisive: zero `cs done failed for ... exit -1` lines. Pre-fix
    // (and pre-task-20260518-eb67) the loop would have written exactly
    // one with `error: "cs done failed for ready: exit -1: "`.
    let spurious: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.contains(r#""cs done failed for"#) || l.contains(r"cs done failed for"))
        .collect();
    assert!(
        spurious.is_empty(),
        "found {} spurious `cs done failed` line(s):\n{}",
        spurious.len(),
        spurious.join("\n"),
    );

    let last = lines.last().expect("at least one trace line");
    let parsed: serde_json::Value = serde_json::from_str(last).expect("trace tail is JSON");
    assert_eq!(parsed["action"], "shutdown", "trace tail: {last}");
    assert_eq!(
        parsed["decision_basis"], "operator-signal",
        "trace tail decision_basis: {last}",
    );
}
