// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end test for **config-honoring dispatch**.
//!
//! # What this test proves
//!
//! The ADR-095 Resident Runtime, having sealed its config + binary at
//! launch, **halts fail-closed before forming a dispatch** the instant the
//! on-disk config drifts from that launch seal. It does *not* dispatch the
//! ready molecule on a stale snapshot, and it exits with
//! [`ExitReason::ConfigDrift`] rather than [`ExitReason::Drained`].
//!
//! # The retroactive acceptance criterion
//!
//! > A witness-halting runtime launched May 25 would have *halted on the
//! > first dispatch after the May 31 config edit* — `H' ≠ H`,
//! > refuse-and-exit — and the silent OpenAI billing never happens.
//!
//! We make that scenario a passing test by having the stub `cs` binary
//! rewrite `.cosmon/config.toml` during its `ensemble` call — i.e. the
//! config drifts *between* the launch seal and the pre-dispatch re-check,
//! exactly as a concurrent operator edit would. The loop must refuse the
//! `tackle` it would otherwise have formed.
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

/// Stub `cs` that rewrites `.cosmon/config.toml` on every `ensemble` call,
/// simulating a config/binary redeploy landing between launch and dispatch.
/// `tackle` would mark the molecule completed — but the loop must halt
/// *before* reaching it, so a non-zero `tackles` count means the fix failed.
const PY_STUB: &str = r#"#!/usr/bin/env python3
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
        # `just install` / operator-edit landing mid-flight.
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

#[test]
fn config_drift_between_launch_and_dispatch_halts_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let cosmon = root.join(".cosmon");
    let state_dir = cosmon.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // The config the runtime seals at launch. The stub rewrites it on the
    // first `ensemble` call, so the pre-dispatch re-check sees a drift.
    let config_path = cosmon.join("config.toml");
    std::fs::write(&config_path, b"[adapters]\ndefault = \"local\"\n").unwrap();

    // One ready (unblocked, pending) molecule — the runtime *would* tackle
    // it if it trusted the launch snapshot.
    let fleet_path = state_dir.join("fleet.json");
    std::fs::write(
        &fleet_path,
        br#"{"molecules":[{"id":"task-20260531-aaaa","status":"pending","blocked_by":[]}]}"#,
    )
    .unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let stub_path = root.join("cs_stub.py");
    let stub_body = common::with_fast_python_shebang(PY_STUB)
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

    // The whole point: it halted, it did not drain or dispatch.
    assert_eq!(
        summary.exit,
        ExitReason::ConfigDrift,
        "expected ConfigDrift halt, got {summary:?}",
    );
    assert_eq!(
        summary.tackles, 0,
        "the wrong-oracle dispatch must NEVER be formed, got {summary:?}",
    );

    // The molecule was never tackled — it is still pending on disk.
    let fleet_after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fleet_path).unwrap()).unwrap();
    assert_eq!(
        fleet_after["molecules"][0]["status"], "pending",
        "molecule must remain pending — no dispatch happened",
    );

    // The trace carries the forensic receipt: a config-drift-halt line with
    // an embedded typed `ConfigDriftDetected` event.
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
        "task-20260531-aaaa",
    );
    // Launch and current seals differ — that mismatch is why it halted.
    assert_ne!(
        drift_line["state_hash_before"], drift_line["state_hash_after"],
        "launch seal must differ from the drifted pre-dispatch seal",
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
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("loop returns a summary");

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
