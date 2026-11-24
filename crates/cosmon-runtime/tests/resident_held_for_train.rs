// SPDX-License-Identifier: AGPL-3.0-only

//! A molecule tagged `no-auto-harvest` is held by the resident runtime until
//! an external landing (`cs land-train`'s `cs done --expect-head`) stamps its
//! `merged_at`; only then is its dependent dispatched (Refs #237, ADR-192 I9).
//!
//! The fleet is `a` (Completed, tagged) and `b` (pending, blocked by `a`). A
//! stand-in `cs` records every `tackle` and `done` it is asked for together
//! with whether `a` already carried `merged_at` at that moment. A driver
//! thread plays the train: after the runtime has had many ticks to misbehave,
//! it stamps `a.merged_at` the way the `--expect-head` closure does.

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

const PY_STUB: &str = r#"#!/usr/bin/env python3
import json
import sys
from pathlib import Path

FLEET = Path("__FLEET_PATH__")
LOG = Path("__LOG_PATH__")
TICK = Path("__TICK_PATH__")


def load():
    return json.loads(FLEET.read_text())


def a_merged(data):
    return any(m["id"] == "a" and m.get("merged_at") for m in data["molecules"])


def main(argv):
    verb = argv[1]
    data = load()
    if verb == "ensemble":
        sys.stdout.write(json.dumps(data))
        return 0
    if verb == "observe":
        for m in data["molecules"]:
            if m["id"] == argv[2]:
                sys.stdout.write(json.dumps({"id": m["id"], "status": m["status"]}))
                return 0
        return 0
    if verb in ("tackle", "done"):
        mol_id = argv[2]
        with LOG.open("a") as log:
            log.write(f"{verb} {mol_id} a_merged={a_merged(data)}\n")
        for m in data["molecules"]:
            if m["id"] == mol_id:
                if verb == "tackle":
                    m["status"] = "completed"
                else:
                    m["merged_at"] = "2026-11-24T00:00:00Z"
        FLEET.write_text(json.dumps(data))
        TICK.touch()
        return 0
    return 2


sys.exit(main(sys.argv))
"#;

#[test]
fn tagged_blocker_is_held_and_its_dependent_waits_for_merged_at() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let fleet_path = state_dir.join("fleet.json");
    std::fs::write(
        &fleet_path,
        r#"{"molecules":[
            {"id":"a","status":"completed","tags":["no-auto-harvest"],"blocked_by":[]},
            {"id":"b","status":"pending","blocked_by":["a"]}
        ]}"#,
    )
    .unwrap();
    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();
    let log_path = root.join("calls.log");

    let stub_path: PathBuf = root.join("cs_stub.py");
    let body = common::with_fast_python_shebang(PY_STUB)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__LOG_PATH__", log_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, body).unwrap();
    let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub_path, perms).unwrap();

    // The train: land `a` externally after the runtime has run many ticks.
    let driver_fleet = fleet_path.clone();
    let driver_tick = tick_path.clone();
    let driver = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let mut data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&driver_fleet).unwrap()).unwrap();
        for m in data["molecules"].as_array_mut().unwrap() {
            if m["id"] == "a" {
                m["merged_at"] = serde_json::json!("2026-11-24T00:00:00Z");
            }
        }
        std::fs::write(&driver_fleet, serde_json::to_string(&data).unwrap()).unwrap();
        std::fs::File::create(&driver_tick).unwrap();
    });

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    config.max_runtime = Some(Duration::from_secs(60));
    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let summary = runtime
        .run(&Arc::new(AtomicBool::new(false)))
        .expect("resident loop runs");
    driver.join().unwrap();
    if summary.exit != ExitReason::Drained {
        common::dump_trace(&trace_path, &summary);
    }

    let calls = std::fs::read_to_string(&log_path).unwrap_or_default();
    let lines: Vec<&str> = calls.lines().collect();
    assert!(
        !lines.iter().any(|l| l.starts_with("done a ")),
        "the runtime landed the held molecule `a`: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l == &"tackle b a_merged=True"),
        "`b` must be dispatched once `a` carries merged_at: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l == &"tackle b a_merged=False"),
        "`b` was dispatched before `a`'s merged_at: {lines:?}"
    );
}
