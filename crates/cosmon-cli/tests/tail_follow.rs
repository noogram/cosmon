// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tail -f` must remain attached after it has printed its history.

use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn write_event(root: &std::path::Path, galaxy: &str) {
    let state = root.join(galaxy).join(".cosmon/state");
    fs::create_dir_all(&state).expect("create fixture state");
    fs::write(
        state.join("events.jsonl"),
        "{\"timestamp\":\"2026-10-06T12:00:00Z\",\"type\":\"molecule_nucleated\",\"molecule_id\":\"task-1\"}\n",
    )
    .expect("write fixture event");
}

#[test]
fn follow_stays_alive_after_the_initial_history_until_it_is_signalled() {
    let tmp = tempfile::tempdir().expect("create fixture root");
    write_event(tmp.path(), "alpha");

    let mut child = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args([
            "--json",
            "tail",
            "--follow",
            "--all-galaxies",
            "--cluster-root",
            tmp.path().to_str().expect("fixture root is UTF-8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cs tail");

    // The initial row is already available. A short delay crosses that
    // history boundary without relying on a notification from the fixture.
    thread::sleep(Duration::from_millis(250));
    assert!(
        child.try_wait().expect("inspect cs tail").is_none(),
        "cs tail -f exited after its initial history"
    );

    child.kill().expect("signal cs tail");
    let status = child.wait().expect("wait for cs tail");
    assert!(
        status.code().is_none(),
        "the test must stop follow mode with a signal, not a normal exit: {status}"
    );
}
