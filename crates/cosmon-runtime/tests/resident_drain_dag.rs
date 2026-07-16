// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end test for the ADR-095 Resident Runtime loop.
//!
//! # What this test proves
//!
//! A three-molecule DAG `a → b → c` (each blocked by its predecessor)
//! drains under `cosmon_runtime::RuntimeLoop` with **no manual
//! `cs tackle` / `cs done`** intervention. The loop:
//!
//! 1. Reads the ensemble via the stubbed `cs` binary.
//! 2. Dispatches `Tackle` for `a` (only ready molecule).
//! 3. Sees `a` flip to `completed`, dispatches `Done(a)`, then `Tackle(b)`.
//! 4. Repeats for `c`.
//! 5. Exits with [`ExitReason::Drained`] when nothing is `pending` /
//!    `running`.
//!
//! # Why a stub instead of the real `cs`
//!
//! The IFBDD discipline (ADR-095 §2 RR-1) says the loop must shell out
//! to the transactional core *the way a human would*. The cheapest way
//! to assert that contract from a unit test is to swap the binary on
//! the loop's `cs_binary` config for a tiny script that speaks the same
//! protocol — `ensemble --json` for reads, `tackle <id>` / `done <id>`
//! for writes. The stub is structurally identical to what
//! [`std::process::Command::new("cs")`] would invoke; the test asserts
//! the loop's *behaviour* against the protocol, not the binary's name.
//!
//! # Trace assertions
//!
//! The NDJSON trace at `.cosmon/state/runtime-trace.jsonl` is the
//! IFBDD instrument the build-camp's RR-5 invariant requires. The test
//! reads it back and asserts:
//!
//! - Every line is valid JSON.
//! - Every line carries `ts`, `action`, `decision_basis`,
//!   `state_hash_before`, `state_hash_after`, `error`.
//! - At least one `tackle` decision and one `done` decision is present.

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
"""Test stub speaking the subset of the `cs` protocol the Resident Runtime uses.

Supported verbs:
  ensemble --json   → print the current fleet JSON to stdout
  observe <id> --json → print `{id, status}` for one molecule (anti-preemption
                        lease re-read, task-20260531-a12f)
  tackle <id>       → mark molecule <id> as `completed`
  done <id>         → remove molecule <id> from the fleet
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
    # Touch a file under .cosmon/state/ so the loop's notify watcher
    # wakes up promptly (the poll interval is the fallback).
    TICK.touch()


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        sys.stdout.write(json.dumps(load()))
        return 0
    if verb == "observe":
        # The resident loop re-reads each Tackle candidate fresh from disk
        # right before dispatch (anti-preemption lease). It needs `status`
        # (and optionally `tackled_by` — omitted here, so the lease treats
        # the molecule as runtime-claimable and dispatches).
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
fn three_molecule_dag_drains_under_resident_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Seed the fleet — three molecules in a chain.
    let fleet_path = state_dir.join("fleet.json");
    let seed = r#"{"molecules":[
        {"id":"a","status":"pending","blocked_by":[]},
        {"id":"b","status":"pending","blocked_by":["a"]},
        {"id":"c","status":"pending","blocked_by":["b"]}
    ]}"#;
    std::fs::write(&fleet_path, seed).unwrap();

    // A "wake" file under .cosmon/state/ that the stub touches so the
    // notify watcher fires promptly.
    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    // Write the Python stub with the absolute fleet path baked in.
    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    // Safety net only — the loop should drain in ~7 ticks at 50ms each
    // (≈400 ms). The generous budget keeps the test stable under heavy
    // parallel CPU contention (python3 spawns dominate the wall time).
    config.max_runtime = Some(Duration::from_secs(60));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime
        .run(&shutdown)
        .expect("resident runtime loop drains cleanly");

    assert_eq!(
        summary.exit,
        ExitReason::Drained,
        "expected Drained, got {:?}",
        summary.exit,
    );
    assert_eq!(summary.tackles, 3, "expected 3 tackles, got {summary:?}");
    assert_eq!(summary.dones, 3, "expected 3 dones, got {summary:?}");

    // Fleet file: all three molecules have been done'd → empty list.
    let final_text = std::fs::read_to_string(&fleet_path).unwrap();
    let final_json: serde_json::Value = serde_json::from_str(&final_text).unwrap();
    let final_molecules = final_json["molecules"].as_array().unwrap();
    assert!(
        final_molecules.is_empty(),
        "expected empty fleet after drain, got {final_molecules:?}",
    );

    // NDJSON trace assertions — the IFBDD instrument is populated.
    let trace = std::fs::read_to_string(&trace_path).expect("trace file exists");
    let lines: Vec<&str> = trace.lines().collect();
    assert!(
        lines.len() >= 7,
        "expected at least 7 NDJSON lines (3×tackle + 3×done + drained), got {}: {trace}",
        lines.len(),
    );
    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("trace line is not JSON: {l:?} ({e})"))
        })
        .collect();
    for v in &parsed {
        for field in [
            "ts",
            "action",
            "decision_basis",
            "state_hash_before",
            "state_hash_after",
            "error",
        ] {
            assert!(
                v.get(field).is_some(),
                "trace line missing field {field}: {v}",
            );
        }
    }

    let tackles: Vec<_> = parsed.iter().filter(|v| v["action"] == "tackle").collect();
    let dones: Vec<_> = parsed.iter().filter(|v| v["action"] == "done").collect();
    assert_eq!(tackles.len(), 3, "expected 3 tackle trace lines");
    assert_eq!(dones.len(), 3, "expected 3 done trace lines");

    // Every decision line carries a unique invocation uuid.
    let mut invocations: Vec<&str> = parsed
        .iter()
        .filter_map(|v| v["invocation_uuid"].as_str())
        .collect();
    invocations.sort_unstable();
    let before = invocations.len();
    invocations.dedup();
    assert_eq!(
        invocations.len(),
        before,
        "expected all invocation_uuid values to be distinct",
    );
}
