// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end test for **config-honoring dispatch**.
//!
//! # What this test proves
//!
//! The ADR-095 Resident Runtime seals `.cosmon/config.toml` (+ the global
//! tier) at launch and re-checks it before every dispatch. Issue #91
//! narrowed the original blanket halt (any edit at all) to: **reload** the
//! fresh config and keep going when nothing already dispatched can be
//! affected, **halt fail-closed** only when the `[adapters]` dispatch
//! surface changed *and* a molecule is currently `running` under the old
//! one.
//!
//! # The retroactive acceptance criterion (still binding)
//!
//! > A witness-halting runtime launched May 25 would have *halted on the
//! > first dispatch after the May 31 config edit* — `H' ≠ H`,
//! > refuse-and-exit — and the silent OpenAI billing never happens.
//!
//! `config_drift_while_molecule_running_still_halts_fail_closed` reproduces
//! exactly that shape (a `[adapters]` edit landing while a molecule is
//! `running`) and asserts the halt.
//!
//! # Issue #91 — the narrowed cases
//!
//! `config_drift_with_no_running_molecules_reloads_and_dispatches` and
//! `config_drift_in_unrelated_section_reloads_even_with_running_molecule`
//! reproduce the complaint in the issue: a config edit with nothing already
//! dispatched to protect must not halt an otherwise-idle DAG waiting on an
//! external watchdog to relaunch it.
//!
//! # The self-poisoning regression
//!
//! The seal originally also hashed the `cs` binary image. That made the
//! runtime self-poison: `cs done`'s post-merge `just install` reinstalls
//! the binary on every successful drain, so the next tick saw `H' ≠ H` and
//! halted on a phantom "drift" that was actually the propulsion's own
//! success. `binary_reinstall_does_not_trip_the_seal` proves the inverse:
//! a binary rewrite mid-run, with config untouched, must let the loop
//! drain normally.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cosmon_runtime::{
    ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
};

mod common;

/// Stub `cs` that rewrites `.cosmon/config.toml`'s `[adapters]` table on
/// every `ensemble` call, simulating an operator edit landing mid-flight —
/// with **no molecule running** (the fleet passed in has only the one
/// `pending` candidate). Per issue #91 this must be safely reloaded and the
/// molecule still dispatched: a non-zero `tackles` count is the expected
/// (fixed) outcome, not a failure.
const PY_STUB_ADAPTERS_DRIFT_IDLE: &str = r#"#!/usr/bin/env python3
import json
import sys
from pathlib import Path

FLEET = Path("__FLEET_PATH__")
CONFIG = Path("__CONFIG_PATH__")
TICK = Path("__TICK_PATH__")


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        # Drift the config the runtime sealed at launch — this is the
        # operator-edit landing mid-flight. No molecule is running, so this
        # must be safe to reload rather than a reason to halt.
        CONFIG.write_text('[adapters]\ndefault = "openai"\n')
        TICK.touch()
        sys.stdout.write(FLEET.read_text())
        return 0
    if verb == "observe":
        # Anti-preemption recheck (recheck_tackle_candidate): the loop reads
        # the candidate fresh before dispatch. Echo the molecule's status so
        # a pending molecule is confirmed dispatchable.
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        for m in data["molecules"]:
            if m["id"] == argv[2]:
                sys.stdout.write(json.dumps({"status": m["status"]}))
                return 0
        sys.stdout.write(json.dumps({"status": "absent"}))
        return 0
    if verb in ("tackle", "done"):
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        if verb == "tackle":
            for m in data["molecules"]:
                if m["id"] == argv[2]:
                    m["status"] = "completed"
        else:
            data["molecules"] = [m for m in data["molecules"] if m["id"] != argv[2]]
        FLEET.write_text(json.dumps(data))
        TICK.touch()
        return 0
    sys.stderr.write(f"stub: unknown verb {verb!r}\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
"#;

/// Stub `cs` that rewrites `.cosmon/config.toml`'s `[adapters]` table on
/// every `ensemble` call **while a molecule is already `running`** — the
/// hazard the halt still exists for. `tackle`/`done` would let the second
/// (`pending`) molecule dispatch — but the loop must halt *before* reaching
/// it, so a non-zero `tackles` count means the safety net regressed.
const PY_STUB_ADAPTERS_DRIFT_WHILE_RUNNING: &str = r#"#!/usr/bin/env python3
import json
import sys
from pathlib import Path

FLEET = Path("__FLEET_PATH__")
CONFIG = Path("__CONFIG_PATH__")
TICK = Path("__TICK_PATH__")


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        CONFIG.write_text('[adapters]\ndefault = "openai"\n')
        TICK.touch()
        sys.stdout.write(FLEET.read_text())
        return 0
    if verb in ("tackle", "done"):
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        if verb == "tackle":
            for m in data["molecules"]:
                if m["id"] == argv[2]:
                    m["status"] = "completed"
        else:
            data["molecules"] = [m for m in data["molecules"] if m["id"] != argv[2]]
        FLEET.write_text(json.dumps(data))
        TICK.touch()
        return 0
    sys.stderr.write(f"stub: unknown verb {verb!r}\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
"#;

/// Stub `cs` that rewrites `.cosmon/config.toml`'s `[worker]` section (never
/// `[adapters]`) on every `ensemble` call **while a molecule is already
/// `running`** — a drift outside the dispatch surface must reload and keep
/// going even though something is running, unlike
/// `PY_STUB_ADAPTERS_DRIFT_WHILE_RUNNING`.
const PY_STUB_UNRELATED_DRIFT_WHILE_RUNNING: &str = r#"#!/usr/bin/env python3
import json
import sys
from pathlib import Path

FLEET = Path("__FLEET_PATH__")
CONFIG = Path("__CONFIG_PATH__")
TICK = Path("__TICK_PATH__")


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        # [worker] is not [adapters] — the dispatch surface is untouched.
        CONFIG.write_text('[worker]\non_complete = "commit+push"\n')
        TICK.touch()
        sys.stdout.write(FLEET.read_text())
        return 0
    if verb == "observe":
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        for m in data["molecules"]:
            if m["id"] == argv[2]:
                sys.stdout.write(json.dumps({"status": m["status"]}))
                return 0
        sys.stdout.write(json.dumps({"status": "absent"}))
        return 0
    if verb in ("tackle", "done"):
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        if verb == "tackle":
            for m in data["molecules"]:
                if m["id"] == argv[2]:
                    m["status"] = "completed"
        else:
            data["molecules"] = [m for m in data["molecules"] if m["id"] != argv[2]]
        FLEET.write_text(json.dumps(data))
        TICK.touch()
        return 0
    sys.stderr.write(f"stub: unknown verb {verb!r}\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
"#;

/// Stub `cs` that rewrites **itself** (bumps len + mtime of the `cs`
/// binary) on every `ensemble` call, simulating `cs done`'s post-merge
/// `just install` landing mid-run. The config is left untouched. With the
/// binary term dropped from the seal, the loop must drain normally — a
/// `ConfigDrift` exit here means the self-poisoning bug regressed.
const PY_STUB_BINARY_REINSTALL: &str = r#"#!/usr/bin/env python3
import json
import sys
from pathlib import Path

FLEET = Path("__FLEET_PATH__")
SELF = Path("__SELF_PATH__")
TICK = Path("__TICK_PATH__")


def main(argv):
    if len(argv) < 2:
        return 2
    verb = argv[1]
    if verb == "ensemble":
        # Simulate `just install`: rewrite the `cs` binary in place (new
        # bytes + bumped mtime), exactly as the merge install hook does.
        # The config is NOT touched.
        with SELF.open('a') as f:
            f.write('# reinstalled-by-merge-hook\n')
        TICK.touch()
        sys.stdout.write(FLEET.read_text())
        return 0
    if verb == "observe":
        # Anti-preemption recheck (recheck_tackle_candidate): the loop reads
        # the candidate fresh before dispatch. Echo the molecule's status so
        # a pending molecule is confirmed dispatchable.
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        for m in data["molecules"]:
            if m["id"] == argv[2]:
                sys.stdout.write(json.dumps({"status": m["status"]}))
                return 0
        sys.stdout.write(json.dumps({"status": "absent"}))
        return 0
    if verb in ("tackle", "done"):
        if len(argv) < 3:
            return 2
        data = json.loads(FLEET.read_text())
        if verb == "tackle":
            for m in data["molecules"]:
                if m["id"] == argv[2]:
                    m["status"] = "completed"
        else:
            data["molecules"] = [m for m in data["molecules"] if m["id"] != argv[2]]
        FLEET.write_text(json.dumps(data))
        TICK.touch()
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

/// Issue #91 reproduction: nothing is `running`, so an `[adapters]` edit
/// landing mid-flight cannot affect anything already dispatched. The loop
/// must reload and dispatch the lone pending molecule instead of halting an
/// otherwise-idle DAG.
#[test]
fn config_drift_with_no_running_molecules_reloads_and_dispatches() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let cosmon = root.join(".cosmon");
    let state_dir = cosmon.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // The config the runtime seals at launch. The stub rewrites its
    // `[adapters]` table on the first `ensemble` call.
    let config_path = cosmon.join("config.toml");
    std::fs::write(&config_path, b"[adapters]\ndefault = \"local\"\n").unwrap();

    // One ready (unblocked, pending) molecule and nothing running.
    let fleet_path = state_dir.join("fleet.json");
    std::fs::write(
        &fleet_path,
        br#"{"molecules":[{"id":"task-20260531-aaaa","status":"pending","blocked_by":[]}]}"#,
    )
    .unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB_ADAPTERS_DRIFT_IDLE)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__CONFIG_PATH__", config_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    config.max_runtime = Some(Duration::from_secs(60));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("loop returns a summary");

    if summary.exit != ExitReason::Drained {
        common::dump_trace(&trace_path, &summary);
    }

    // The whole point: it reloaded and dispatched instead of halting.
    assert_eq!(
        summary.exit,
        ExitReason::Drained,
        "expected the DAG to drain after a safe reload, got {summary:?}",
    );
    assert_eq!(
        summary.tackles, 1,
        "the pending molecule must be dispatched under the fresh config, got {summary:?}",
    );
    assert_eq!(
        summary.config_reloads, 1,
        "exactly one reload must be recorded, got {summary:?}",
    );

    // The molecule was tackled and then merged away by `cs done`.
    let fleet_after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fleet_path).unwrap()).unwrap();
    assert_eq!(
        fleet_after["molecules"].as_array().unwrap().len(),
        0,
        "the molecule must have drained (tackled then done), got {fleet_after:?}",
    );

    // The trace carries a `config-reloaded` line, not a halt.
    let trace = std::fs::read_to_string(&trace_path).expect("trace exists");
    assert!(
        !trace.contains("\"action\":\"config-drift-halt\""),
        "no halt line must be present, got: {trace}",
    );
    let reload_line = trace
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("trace line is JSON"))
        .find(|v| v["action"] == "config-reloaded")
        .expect("a config-reloaded line is present");
    assert_eq!(
        reload_line["decision_basis"],
        "config-seal-reload-no-running-molecules",
    );
    assert_ne!(
        reload_line["state_hash_before"], reload_line["state_hash_after"],
        "launch seal must differ from the reloaded seal",
    );
}

/// The retroactive acceptance criterion the halt still exists for: a
/// molecule is `running` when the `[adapters]` table drifts. The loop must
/// refuse to form its next dispatch (the sibling `pending` molecule) rather
/// than mix adapter resolutions mid-DAG.
#[test]
fn config_drift_while_molecule_running_still_halts_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let cosmon = root.join(".cosmon");
    let state_dir = cosmon.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = cosmon.join("config.toml");
    std::fs::write(&config_path, b"[adapters]\ndefault = \"local\"\n").unwrap();

    // One `running` molecule (already dispatched under the old surface) and
    // one `pending` sibling the runtime *would* tackle next if it trusted
    // the launch snapshot.
    let fleet_path = state_dir.join("fleet.json");
    std::fs::write(
        &fleet_path,
        br#"{"molecules":[
            {"id":"task-20260531-bbbb","status":"running","blocked_by":[]},
            {"id":"task-20260531-cccc","status":"pending","blocked_by":[]}
        ]}"#,
    )
    .unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB_ADAPTERS_DRIFT_WHILE_RUNNING)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__CONFIG_PATH__", config_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    config.max_runtime = Some(Duration::from_secs(60));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("loop returns a summary");

    if summary.exit != ExitReason::ConfigDrift {
        common::dump_trace(&trace_path, &summary);
    }

    assert_eq!(
        summary.exit,
        ExitReason::ConfigDrift,
        "expected ConfigDrift halt while a molecule is running, got {summary:?}",
    );
    assert_eq!(
        summary.tackles, 0,
        "the wrong-oracle dispatch must NEVER be formed, got {summary:?}",
    );
    assert_eq!(summary.config_reloads, 0, "no reload must be recorded");

    // Neither molecule advanced — the running one stayed running, the
    // pending one was never dispatched.
    let fleet_after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fleet_path).unwrap()).unwrap();
    let statuses: Vec<&str> = fleet_after["molecules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, vec!["running", "pending"]);

    let trace = std::fs::read_to_string(&trace_path).expect("trace exists");
    let drift_line = trace
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("trace line is JSON"))
        .find(|v| v["action"] == "config-drift-halt")
        .expect("a config-drift-halt line is present");
    assert_eq!(drift_line["decision_basis"], "config-seal-mismatch");
    assert_eq!(
        drift_line["event"]["type"], "config_drift_detected",
        "the embedded event is the typed ConfigDriftDetected variant",
    );
    assert_eq!(drift_line["event"]["refused_verb"], "tackle");
    assert_eq!(
        drift_line["event"]["refused_molecule"],
        "task-20260531-cccc",
    );
}

/// A drift confined to a section other than `[adapters]` (here `[worker]`)
/// must reload and keep going even while a molecule is `running` — the
/// dispatch surface never changed, so there is nothing for the running
/// molecule to be affected by.
#[test]
fn config_drift_in_unrelated_section_reloads_even_with_running_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let cosmon = root.join(".cosmon");
    let state_dir = cosmon.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // No `[adapters]` table at all, before or after the drift.
    let config_path = cosmon.join("config.toml");
    std::fs::write(&config_path, b"[worker]\non_complete = \"commit\"\n").unwrap();

    let fleet_path = state_dir.join("fleet.json");
    std::fs::write(
        &fleet_path,
        br#"{"molecules":[
            {"id":"task-20260531-dddd","status":"running","blocked_by":[]},
            {"id":"task-20260531-eeee","status":"pending","blocked_by":[]}
        ]}"#,
    )
    .unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB_UNRELATED_DRIFT_WHILE_RUNNING)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__CONFIG_PATH__", config_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(20);
    // The `running` sibling never completes in this stub (nothing external
    // marks it `completed`), so the loop cannot drain — bound the run to a
    // small deadline instead of the usual 60 s so the test stays fast; a few
    // hundred ms is ample for the pending sibling to dispatch and the trace
    // lines this test asserts on to be written.
    config.max_runtime = Some(Duration::from_millis(400));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("loop returns a summary");

    // The running molecule never drains in this stub (no external event
    // marks it `completed`), so the loop stops on the deadline rather than
    // `Drained` — the assertion that matters is that it did NOT halt on
    // config drift, and that the pending sibling WAS dispatched.
    if summary.exit == ExitReason::ConfigDrift {
        common::dump_trace(&trace_path, &summary);
    }
    assert_ne!(
        summary.exit,
        ExitReason::ConfigDrift,
        "an unrelated-section edit must never halt, got {summary:?}",
    );
    assert_eq!(
        summary.tackles, 1,
        "the pending sibling must still be dispatched, got {summary:?}",
    );
    assert!(
        summary.config_reloads >= 1,
        "at least one reload must be recorded, got {summary:?}",
    );

    let trace = std::fs::read_to_string(&trace_path).expect("trace exists");
    assert!(
        !trace.contains("\"action\":\"config-drift-halt\""),
        "no halt line must be present, got: {trace}",
    );
    let reload_line = trace
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("trace line is JSON"))
        .find(|v| v["action"] == "config-reloaded")
        .expect("a config-reloaded line is present");
    assert_eq!(
        reload_line["decision_basis"],
        "config-seal-reload-unaffected-surface",
    );
}

#[test]
fn binary_reinstall_does_not_trip_the_seal() {
    // task-20260608-1c59 regression: a `just install` reinstalling the `cs`
    // binary mid-run (which `cs done`'s post-merge hook does on EVERY
    // successful drain) must NOT trip the launch seal. Before the fix the
    // seal hashed the binary image, so the propulsion died of its own
    // success. Here the stub rewrites itself on `ensemble` but leaves the
    // config alone; the loop must tackle the molecule and drain.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let cosmon = root.join(".cosmon");
    let state_dir = cosmon.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Config sealed at launch — never modified by the stub.
    let config_path = cosmon.join("config.toml");
    std::fs::write(&config_path, b"[adapters]\ndefault = \"local\"\n").unwrap();

    let fleet_path = state_dir.join("fleet.json");
    std::fs::write(
        &fleet_path,
        br#"{"molecules":[{"id":"task-20260608-bbbb","status":"pending","blocked_by":[]}]}"#,
    )
    .unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB_BINARY_REINSTALL)
        .replace("__FLEET_PATH__", fleet_path.to_string_lossy().as_ref())
        .replace("__SELF_PATH__", stub_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(50);
    config.max_runtime = Some(Duration::from_secs(60));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("loop returns a summary");

    if summary.exit != ExitReason::Drained {
        common::dump_trace(&trace_path, &summary);
    }

    // The molecule drained — the reinstall did NOT masquerade as drift.
    assert_eq!(
        summary.exit,
        ExitReason::Drained,
        "a binary reinstall must NOT trip the seal, got {summary:?}",
    );
    assert_eq!(
        summary.tackles, 1,
        "the ready molecule must be tackled despite the mid-run reinstall, got {summary:?}",
    );
}
