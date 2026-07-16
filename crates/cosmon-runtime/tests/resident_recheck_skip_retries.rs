// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test for the orphan-on-skip deadlock.
//!
//! # What this proves
//!
//! The resident loop sits an anti-preemption *recheck* gate
//! (`recheck_tackle_candidate`) between a scheduler's
//! `Tackle` decision and the `cs tackle` shell-out. Under CPU contention the
//! recheck's `cs observe` spawn can fail transiently, which makes the loop
//! **skip** that dispatch. Before the fix, the scheduler had already recorded
//! the molecule as `tackled` when it emitted the decision, so the skipped
//! molecule was *orphaned*: still `pending` on disk, but never re-emitted. Its
//! dependents never unblocked, the DAG never drained, and the loop ran to its
//! `max_runtime` ([`ExitReason::Deadline`]) — the deterministic 60 s hang seen
//! on a loaded dev machine.
//!
//! This test reproduces the transient veto deterministically by having the
//! stub fail the **first** `cs observe a` call (and only that one). With the
//! `forget_dispatch` retraction in place, the loop drops its optimistic mark on
//! the skip and re-emits `Tackle(a)` on the next tick, when `observe` succeeds.
//! The DAG drains. Without the fix, this same scenario hangs to `Deadline`.
//!
//! # Why a POSIX `sh` stub, not python3
//!
//! The original drain test stubbed `cs` with a python3 script. On a loaded dev
//! machine (load average in the hundreds, homebrew Python 3.14) `python3`
//! *startup alone* cost 1–2 s per spawn; a multi-tick drain spawns the stub a
//! dozen-plus times, so the wall-clock blew past any fixed `max_runtime`
//! regardless of the loop's logic. A `/bin/sh` stub starts in single-digit
//! milliseconds, so the test now measures the loop's behaviour, not the
//! interpreter's startup. State lives in a line-oriented file
//! (`id|status|csv-blockers`) so the stub never shells out to a JSON parser; it
//! only ever *emits* JSON (which the loop reads) via `printf`.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cosmon_runtime::{
    ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
};

/// POSIX-`sh` stub speaking the `cs` subset the loop uses, with one twist: the
/// **first** `observe a` returns a non-zero exit (simulating a transient
/// spawn/read failure under load), which drives the recheck gate down the
/// `TackleRecheck::SkipReadFailed` path. A sidecar counter file tracks how many
/// `observe a` failures remain to inject.
const SH_STUB_FLAKY_OBSERVE: &str = r#"#!/bin/sh
STATE="__STATE_PATH__"
TICK="__TICK_PATH__"
FAILS="__FAILS_PATH__"
verb="$1"
mol="$2"

# Distinct variable names (b*) so this never clobbers the caller's `first`
# accumulator — POSIX sh has no function-local scope.
emit_blocked() {
  printf '['
  brest="$1"
  bfirst=1
  while [ -n "$brest" ]; do
    case "$brest" in
      *,*) bitem="${brest%%,*}"; brest="${brest#*,}" ;;
      *)   bitem="$brest"; brest="" ;;
    esac
    [ "$bfirst" -eq 0 ] && printf ','
    bfirst=0
    printf '"%s"' "$bitem"
  done
  printf ']'
}

case "$verb" in
  ensemble)
    printf '{"molecules":['
    first=1
    while IFS='|' read -r id status blocked; do
      [ -z "$id" ] && continue
      [ "$first" -eq 0 ] && printf ','
      first=0
      printf '{"id":"%s","status":"%s","blocked_by":' "$id" "$status"
      emit_blocked "$blocked"
      printf '}'
    done < "$STATE"
    printf ']}'
    ;;
  observe)
    [ -z "$mol" ] && exit 2
    if [ "$mol" = "a" ] && [ -f "$FAILS" ]; then
      remaining=$(cat "$FAILS")
      if [ "${remaining:-0}" -gt 0 ]; then
        echo $((remaining - 1)) > "$FAILS"
        echo "stub: injected transient observe failure" >&2
        exit 1
      fi
    fi
    while IFS='|' read -r id status blocked; do
      if [ "$id" = "$mol" ]; then
        printf '{"id":"%s","status":"%s"}' "$id" "$status"
        exit 0
      fi
    done < "$STATE"
    printf '{"id":"%s","status":"unknown"}' "$mol"
    ;;
  tackle)
    [ -z "$mol" ] && exit 2
    tmp="${STATE}.tmp"
    : > "$tmp"
    while IFS='|' read -r id status blocked; do
      [ -z "$id" ] && continue
      [ "$id" = "$mol" ] && status="completed"
      printf '%s|%s|%s\n' "$id" "$status" "$blocked" >> "$tmp"
    done < "$STATE"
    mv "$tmp" "$STATE"
    : > "$TICK"
    ;;
  done)
    [ -z "$mol" ] && exit 2
    tmp="${STATE}.tmp"
    : > "$tmp"
    while IFS='|' read -r id status blocked; do
      [ -z "$id" ] && continue
      [ "$id" = "$mol" ] && continue
      printf '%s|%s|%s\n' "$id" "$status" "$blocked" >> "$tmp"
    done < "$STATE"
    mv "$tmp" "$STATE"
    : > "$TICK"
    ;;
  *)
    echo "stub: unknown verb $verb" >&2
    exit 2
    ;;
esac
"#;

fn make_executable(path: &PathBuf) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Count molecule lines remaining in the line-format state file.
fn molecule_count(state_path: &std::path::Path) -> usize {
    std::fs::read_to_string(state_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[test]
fn recheck_skip_is_retried_not_orphaned() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Two-molecule chain — `a` gates `b`. If `a` is orphaned by a skipped
    // recheck, `b` never unblocks and the loop hangs to Deadline.
    // Line format: `id|status|csv-blockers`.
    let state_path = state_dir.join("fleet.lines");
    std::fs::write(&state_path, "a|pending|\nb|pending|a\n").unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    // Inject exactly one transient `observe a` failure.
    let fails_path = root.join("observe_fails.txt");
    std::fs::write(&fails_path, b"1\n").unwrap();

    let stub_path = root.join("cs_stub.sh");
    let stub_body = SH_STUB_FLAKY_OBSERVE
        .replace("__STATE_PATH__", state_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref())
        .replace("__FAILS_PATH__", fails_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(20);
    // Tight budget: with the orphan bug, the loop hangs to this deadline; with
    // the fix it drains in a handful of fast `sh` ticks well under 1 s, so even
    // on a heavily loaded machine 30 s is a comfortable safety net.
    config.max_runtime = Some(Duration::from_secs(30));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("resident loop runs");

    if summary.exit != ExitReason::Drained {
        let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
        eprintln!("=== TRACE ===\n{trace}\n=== END TRACE ===\nsummary: {summary:?}");
    }

    assert_eq!(
        summary.exit,
        ExitReason::Drained,
        "loop must retry the recheck-skipped tackle and drain, not orphan it \
         into Deadline; got {:?}",
        summary.exit,
    );
    assert_eq!(
        summary.tackles, 2,
        "expected 2 tackles (a retried, b), got {summary:?}"
    );
    assert_eq!(summary.dones, 2, "expected 2 dones, got {summary:?}");

    assert_eq!(
        molecule_count(&state_path),
        0,
        "expected empty fleet after drain",
    );

    // The injected failure was consumed — proving the recheck really did skip
    // once (otherwise the test would pass vacuously).
    let remaining = std::fs::read_to_string(&fails_path).unwrap();
    assert_eq!(
        remaining.trim(),
        "0",
        "the transient observe failure must have fired once",
    );
}

/// A pilot reservation exists before `cs tackle`, so `tackled_by` is absent.
/// The resident recheck must nevertheless defer: `hold:pilot` is the positive,
/// durable ownership marker that closes the pending-work race.
#[test]
fn pilot_hold_is_never_preempted_by_resident_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();
    let tackled_path = root.join("tackled.txt");
    let stub_path = root.join("cs_stub.sh");
    let stub = r#"#!/bin/sh
case "$1" in
  ensemble) printf '{"molecules":[{"id":"a","status":"pending","blocked_by":[]}]}' ;;
  observe) printf '{"id":"a","status":"pending","tags":["hold:pilot"]}' ;;
  tackle) echo "$2" >> "__TACKLED_PATH__" ;;
  *) exit 2 ;;
esac
"#
    .replace("__TACKLED_PATH__", tackled_path.to_string_lossy().as_ref());
    std::fs::write(&stub_path, stub).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(10);
    config.max_runtime = Some(Duration::from_millis(80));
    let mut runtime = RuntimeLoop::new(config, Box::new(ReadyFrontierScheduler::new()));
    let summary = runtime.run(&Arc::new(AtomicBool::new(false))).unwrap();

    assert_eq!(summary.tackles, 0, "runtime must defer to hold:pilot");
    assert!(
        !tackled_path.exists(),
        "resident must never shell out to tackle a pilot-held molecule"
    );
}
