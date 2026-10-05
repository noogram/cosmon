// SPDX-License-Identifier: AGPL-3.0-only

//! Read-only CLI commands must not create operator-presence telemetry.

use std::fs;
use std::path::Path;
use std::process::Command;

fn bytes_or_absent(path: &Path) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

fn read_only_command(root: &Path, state: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cs"));
    command
        .current_dir(root)
        .env("COSMON_STATE_DIR", state)
        .env_remove("COSMON_NO_OPERATOR_EVENTS")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args(args);
    command
}

#[test]
fn read_only_commands_leave_the_event_log_and_index_byte_identical() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let state = root.join(".cosmon/state");
    fs::create_dir_all(&state).expect("state directory");
    fs::write(
        root.join(".cosmon/config.toml"),
        "[project]\nproject_id = \"read-only-presence\"\n",
    )
    .expect("project config");
    let events = state.join("events.jsonl");
    fs::write(&events, "").expect("empty event log");
    let index = state.join("events.jsonl.seqidx");
    let events_before = bytes_or_absent(&events);
    let index_before = bytes_or_absent(&index);

    for args in [
        ["status"].as_slice(),
        ["observe", "task-20261005-8d2d"].as_slice(),
        ["peek"].as_slice(),
        ["project", "--check"].as_slice(),
    ] {
        let _ = read_only_command(root, &state, args)
            .output()
            .unwrap_or_else(|error| panic!("run cs {}: {error}", args.join(" ")));
        assert_eq!(
            bytes_or_absent(&events),
            events_before,
            "cs {} changed events.jsonl",
            args.join(" ")
        );
        assert_eq!(
            bytes_or_absent(&index),
            index_before,
            "cs {} changed events.jsonl.seqidx",
            args.join(" ")
        );
    }
}
