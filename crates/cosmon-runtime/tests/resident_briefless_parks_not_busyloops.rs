// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test for the briefless-dispatch busy-loop (task-20260711-4310).
//!
//! # What this proves
//!
//! The sibling fix task-20260711-919a taught `cs tackle` to *refuse* a
//! **briefless** molecule — one whose formula declares required, default-free
//! variables that are now missing or blank — by exiting with the guard code
//! [`cosmon_core::dispatch_refusal::BRIEFLESS_DISPATCH`]. That stops a garbage
//! worker from spawning, but it left a second defect in the resident runtime:
//! `cs run` dispatches by shelling out `cs tackle`, and its failure handler
//! treated *every* non-zero exit as **transient** — retracting the optimistic
//! dispatch mark (`forget_dispatch`) so the molecule re-entered the frontier
//! and was re-emitted **every tick**. A briefless molecule can never satisfy
//! the guard on retry, so this was an infinite busy-loop: `cs tackle` spawned
//! each poll interval, the trace flooded, and — because every tick then
//! "produced decisions" — the phantom-running stall gate perpetually reset,
//! starving the reap sweep.
//!
//! The fix classifies the briefless exit code as a **permanent** refusal
//! ([`ResidentError::TackleRefusedBriefless`]) and *parks* the molecule: it
//! keeps the optimistic mark, so the molecule is attempted **exactly once**
//! and never re-emitted. This test pins that: molecule `a` is briefless (its
//! `cs tackle` always exits `16`), molecule `b` is well-formed (drains
//! normally). We assert:
//!
//! - `cs tackle a` was invoked **exactly once** (the park, not a busy-loop).
//!   Without the fix this count grows without bound until `max_runtime`.
//! - `b` still drained — the park is specific to the briefless refusal and
//!   does not freeze the rest of the DAG.
//! - The loop reports exactly one `briefless_parked`.
//!
//! # Why a POSIX `sh` stub
//!
//! Same rationale as `resident_recheck_skip_retries.rs`: a `/bin/sh` stub
//! starts in single-digit milliseconds, so the test measures the loop's
//! behaviour, not an interpreter's startup. State lives in a line-oriented
//! file (`id|status|csv-blockers`); a sidecar counter file records how many
//! times `tackle a` was attempted.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cosmon_runtime::{
    ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
};

/// POSIX-`sh` stub speaking the `cs` subset the loop uses. `tackle a` always
/// exits `16` (the briefless-dispatch guard code) after bumping a counter file
/// and leaving `a`'s state untouched (it stays `pending`). Every other
/// `tackle`/`done` mutates the line-format state so a well-formed molecule
/// drains. `patrol` (the reap sweep) is a clean no-op so a stall never errors.
const SH_STUB_BRIEFLESS_A: &str = r#"#!/bin/sh
STATE="__STATE_PATH__"
TICK="__TICK_PATH__"
TACKLE_A="__TACKLE_A_PATH__"
verb="$1"
mol="$2"

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
    if [ "$mol" = "a" ]; then
      # Briefless refusal: bump the attempt counter, leave `a` pending, and
      # exit with the guard code so the loop must park (not retry) it.
      count=$(cat "$TACKLE_A" 2>/dev/null || echo 0)
      echo $((count + 1)) > "$TACKLE_A"
      echo "cs tackle: refusing dispatch — molecule a is briefless" >&2
      exit 16
    fi
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
  patrol)
    # Reap sweep no-op: emit an empty auto_transitioned set, exit clean.
    printf '{"auto_transitioned":{"molecules":[]}}'
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

fn read_count(path: &std::path::Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Whether a molecule id is still present in the line-format state file.
fn state_has(state_path: &std::path::Path, id: &str) -> bool {
    std::fs::read_to_string(state_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split('|').next())
        .any(|first| first == id)
}

#[test]
fn briefless_tackle_is_parked_not_busylooped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // `a` is briefless (tackle always exits 16); `b` is well-formed and has no
    // blocker, so it drains independently. Line format: `id|status|blockers`.
    let state_path = state_dir.join("fleet.lines");
    std::fs::write(&state_path, "a|pending|\nb|pending|\n").unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let tackle_a_path = root.join("tackle_a_count.txt");

    let stub_path = root.join("cs_stub.sh");
    let stub_body = SH_STUB_BRIEFLESS_A
        .replace("__STATE_PATH__", state_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref())
        .replace(
            "__TACKLE_A_PATH__",
            tackle_a_path.to_string_lossy().as_ref(),
        );
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(20);
    // `a` never drains, so the loop runs to this deadline. WITHOUT the fix it
    // spends that whole window busy-looping `cs tackle a` (dozens of attempts
    // at a 20 ms poll); WITH the fix it attempts `a` exactly once and idles.
    // A short-but-generous 3 s window makes the busy-loop signal (attempt
    // count) unmistakable while keeping the test fast even under load.
    config.max_runtime = Some(Duration::from_secs(3));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("resident loop runs");

    let attempts = read_count(&tackle_a_path);
    if attempts != 1 || summary.briefless_parked != 1 {
        let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
        eprintln!("=== TRACE ===\n{trace}\n=== END TRACE ===\nsummary: {summary:?}");
    }

    // The core regression signal: a briefless molecule is attempted ONCE and
    // then parked — not re-dispatched every tick. Before the fix this count
    // grew without bound (one attempt per poll interval until Deadline).
    assert_eq!(
        attempts, 1,
        "briefless `cs tackle a` must be attempted exactly once then parked, \
         not busy-looped; got {attempts} attempts (summary: {summary:?})",
    );
    assert_eq!(
        summary.briefless_parked, 1,
        "loop must record exactly one parked briefless molecule; got {summary:?}",
    );

    // The park is specific to the briefless refusal: the well-formed `b` still
    // drained (tackled + done + removed), proving the fix does not freeze the
    // rest of the DAG.
    assert!(
        !state_has(&state_path, "b"),
        "well-formed molecule `b` must still drain despite `a` being parked",
    );
    assert_eq!(
        summary.dones, 1,
        "expected exactly one `done` (for the well-formed `b`); got {summary:?}",
    );

    // `a` never completes, so the loop cannot drain — it runs to the deadline.
    // This is the honest outcome: a briefless molecule genuinely blocks
    // progress until an operator restores its brief or collapses it.
    assert_eq!(
        summary.exit,
        ExitReason::Deadline,
        "with `a` parked-but-pending the loop should reach its deadline, not \
         drain; got {:?}",
        summary.exit,
    );
    assert!(
        state_has(&state_path, "a"),
        "the briefless molecule `a` stays pending (parked), awaiting operator",
    );
}
