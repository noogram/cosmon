// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end test for the phantom-running reap: a worker whose tmux pane
//! died but whose molecule still reads `running` must be reaped so the
//! resident runtime can drain past it.
//!
//! # What this proves
//!
//! A molecule stuck `running` because its worker died — a *phantom* — used to
//! deadlock the resident runtime forever: the scheduler never re-tackles a
//! `running` molecule and never `done`s a non-`completed` one, so the loop
//! neither advances past the corpse nor drains, and every downstream waiter
//! hangs (the *flotte aveugle* class, ADR-116).
//!
//! With the reap wired in, the loop:
//!
//! 1. Sees `a` stuck `running` and `b` pending-blocked-by-`a` — no decisions,
//!    not drained → it accumulates stall ticks.
//! 2. After `STALL_TICKS_BEFORE_REAP`, fires `cs patrol --auto-collapse`, which
//!    collapses the phantom `a` to the terminal `collapsed`.
//! 3. Next tick: `a` is cleared (terminal), so `b` unblocks, gets tackled,
//!    completes, and is done'd.
//! 4. The DAG drains — exit [`ExitReason::Drained`], `summary.reaps >= 1`.
//!
//! The stub's `patrol` verb is liveness-blind on purpose (it collapses every
//! `running` molecule it sees) — the *real* liveness judgement lives in
//! `cs patrol` and is tested there; this test asserts the *loop wiring*: that
//! a sustained stall fires the sweep and that the sweep's terminal transition
//! unblocks the frontier.

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
"""Stub `cs` speaking the protocol the Resident Runtime uses, plus the
phantom-running reap sweep.

Verbs:
  ensemble --json          → print the fleet JSON
  observe <id> --json      → print {id, status}
  tackle <id>              → mark molecule <id> `completed`
  done <id>                → remove molecule <id> from the fleet
  patrol --auto-collapse --json
                           → collapse every `running` molecule to `collapsed`
                             (the loop's phantom-running reap), printing the
                             collapsed set under `auto_transitioned.molecules`
"""
import json
import sys
from pathlib import Path

FLEET = Path("__FLEET_PATH__")
TICK = Path("__TICK_PATH__")


def load():
    return json.loads(FLEET.read_text())


def save(data):
    FLEET.write_text(json.dumps(data))
    TICK.touch()


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        sys.stdout.write(json.dumps(load()))
        return 0
    if verb == "observe":
        if len(argv) < 3:
            return 2
        mol_id = argv[2]
        data = load()
        for m in data["molecules"]:
            if m["id"] == mol_id:
                sys.stdout.write(json.dumps({"id": m["id"], "status": m["status"]}))
                return 0
        sys.stdout.write(json.dumps({"id": mol_id, "status": "unknown"}))
        return 0
    if verb == "patrol":
        # `--auto-collapse`: reap every running molecule (the phantom).
        data = load()
        collapsed = []
        for m in data["molecules"]:
            if m["status"] == "running":
                m["status"] = "collapsed"
                collapsed.append(m["id"])
        if collapsed:
            save(data)
        out = {}
        if collapsed:
            out["auto_transitioned"] = {
                "target_status": "collapsed",
                "molecules": collapsed,
            }
        sys.stdout.write(json.dumps(out))
        return 0
    if verb in ("tackle", "done"):
        if len(argv) < 3:
            return 2
        mol_id = argv[2]
        data = load()
        if verb == "tackle":
            for m in data["molecules"]:
                if m["id"] == mol_id:
                    m["status"] = "completed"
        else:  # done
            data["molecules"] = [m for m in data["molecules"] if m["id"] != mol_id]
        save(data)
        return 0
    sys.stderr.write(f"stub: unknown verb {verb!r}\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
"#;

fn make_executable(path: &PathBuf) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn phantom_running_molecule_is_reaped_and_dag_drains() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // `a` is a phantom — already `running`, its worker dead, it will never
    // complete on its own. `b` is pending, blocked by `a`. Without the reap,
    // the loop hangs forever on `a`.
    let fleet_path = state_dir.join("fleet.json");
    let seed = r#"{"molecules":[
        {"id":"a","status":"running","blocked_by":[]},
        {"id":"b","status":"pending","blocked_by":["a"]}
    ]}"#;
    std::fs::write(&fleet_path, seed).unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(20);
    // Generous safety net — the loop must reap within a handful of stall
    // ticks (≈STALL_TICKS_BEFORE_REAP × 20 ms) and then drain.
    config.max_runtime = Some(Duration::from_secs(60));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime
        .run(&shutdown)
        .expect("resident runtime loop reaps the phantom and drains");

    assert_eq!(
        summary.exit,
        ExitReason::Drained,
        "expected Drained after reaping the phantom, got {:?}",
        summary.exit,
    );
    assert!(
        summary.reaps >= 1,
        "expected at least one reap sweep, got {summary:?}",
    );
    // `b` unblocked once `a` was reaped → it was tackled and done'd.
    assert!(
        summary.tackles >= 1,
        "expected `b` to be tackled after the reap, got {summary:?}",
    );
    assert!(
        summary.dones >= 1,
        "expected `b` to be done'd after the reap, got {summary:?}",
    );

    // Final fleet: `a` collapsed (still present, terminal), `b` done'd (gone).
    let final_text = std::fs::read_to_string(&fleet_path).unwrap();
    let final_json: serde_json::Value = serde_json::from_str(&final_text).unwrap();
    let final_molecules = final_json["molecules"].as_array().unwrap();
    let a = final_molecules.iter().find(|m| m["id"] == "a");
    assert_eq!(
        a.map(|m| m["status"].as_str().unwrap_or_default()),
        Some("collapsed"),
        "phantom `a` must be terminally collapsed, got {final_molecules:?}",
    );
    assert!(
        !final_molecules.iter().any(|m| m["id"] == "b"),
        "dependent `b` must have drained, got {final_molecules:?}",
    );

    // The trace carries a `reap` line for the sweep.
    let trace = std::fs::read_to_string(&trace_path).expect("trace file exists");
    let reap_lines: Vec<&str> = trace
        .lines()
        .filter(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v["action"].as_str().map(|s| s == "reap"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        !reap_lines.is_empty(),
        "expected at least one `reap` trace line, got trace:\n{trace}",
    );
}
