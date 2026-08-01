// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test for the teardown retry storm (task-20260725-64d8).
//!
//! # The defect
//!
//! A worker writes its deliverable into the shared main checkout instead of
//! its own worktree. The file is untracked there, so the harvest aborts:
//!
//! ```text
//! cs: teardown aborted: merge error: The following untracked working tree
//! files would be overwritten by merge: <path>
//! ```
//!
//! The resident runtime's failure handler treated that as *transient* — it
//! retracted the optimistic merge mark, the molecule re-entered the frontier
//! on the very next tick, and the loop re-issued the identical `cs done`. The
//! loop wakes on every FS event under `.cosmon/state/`, so "next tick" meant
//! milliseconds: five identical failures inside one second were recorded on a
//! real run, and it kept going indefinitely.
//!
//! An untracked-file merge conflict is *deterministic*. Nothing the loop does
//! between two attempts can clear it, so the retries only burned cycles and
//! flooded the trace while the molecule stayed `completed`-but-unharvested and
//! every descendant stayed `pending`. The operator saw a fleet with N nodes
//! green and nothing running, and no error anywhere they would look.
//!
//! # What this test pins
//!
//! Molecule `a` is `completed`, and this stub's `cs done a` always fails with
//! the real untracked-file message. `b` is `pending` and blocked on `a`.
//!
//! - `cs done a` is attempted **exactly** [`CEILING`] times — not once per FS
//!   event. Before the fix this count grew without bound until `max_runtime`.
//! - The loop reports one `teardown_blocked`, once, however long it then runs.
//! - The molecule is **surfaced**, not just abandoned: `cs tag --add
//!   blocked-on-teardown` and `cs note` are each invoked once. That tag is
//!   what `cs peek` counts in its vital bar; before the fix the only record of
//!   the failure was a line in `runtime-trace.jsonl`.
//! - `b` stays pending — the DAG is still blocked, honestly. Parking the
//!   harvest does not invent progress; it makes the absence of progress
//!   legible.
//!
//! # Why a POSIX `sh` stub
//!
//! Same rationale as `resident_briefless_parks_not_busyloops.rs`: a `/bin/sh`
//! stub starts in single-digit milliseconds, so the test measures the loop's
//! behaviour rather than an interpreter's startup. State lives in a
//! line-oriented file (`id|status|csv-blockers`); sidecar counter files record
//! how many times each verb was invoked.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cosmon_runtime::{
    ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
};

/// The loop's ceiling on consecutive failed harvests, mirrored here because
/// the constant is private to the runtime. A change on either side must be a
/// deliberate edit on both — which is the point.
const CEILING: u32 = 5;

/// POSIX-`sh` stub speaking the `cs` subset the loop uses. `done a` always
/// fails with the untracked-file teardown abort after bumping a counter,
/// leaving `a` `completed` in the state file. `tag` and `note` bump their own
/// counters and record their arguments so the test can prove the park was
/// *written down*, not merely stopped.
const SH_STUB_DONE_ALWAYS_FAILS: &str = r#"#!/bin/sh
STATE="__STATE_PATH__"
TICK="__TICK_PATH__"
DONE_A="__DONE_A_PATH__"
TAG_LOG="__TAG_LOG_PATH__"
NOTE_LOG="__NOTE_LOG_PATH__"
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
  done)
    [ -z "$mol" ] && exit 2
    count=$(cat "$DONE_A" 2>/dev/null || echo 0)
    echo $((count + 1)) > "$DONE_A"
    # The verbatim shape of the real abort: an untracked file in the main
    # checkout that the merge would overwrite. Deterministic — no number of
    # retries clears it.
    echo "cs: teardown aborted: merge error: The following untracked working tree files would be overwritten by merge: docs/result.md" >&2
    exit 1
    ;;
  tag)
    shift
    echo "$*" >> "$TAG_LOG"
    ;;
  note)
    echo "$mol" >> "$NOTE_LOG"
    ;;
  patrol)
    printf '{"auto_transitioned":{"molecules":[]}}'
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

fn read_count(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0)
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn a_deterministic_teardown_failure_backs_off_then_parks_and_is_surfaced() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let state_dir = root.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // `a` is completed and unmergeable; `b` waits on it. Line: `id|status|blockers`.
    let state_path = state_dir.join("fleet.lines");
    std::fs::write(&state_path, "a|completed|\nb|pending|a\n").unwrap();

    let tick_path = state_dir.join("wake.touch");
    std::fs::write(&tick_path, b"").unwrap();

    let done_a_path = root.join("done_a_count.txt");
    let tag_log_path = root.join("tag_log.txt");
    let note_log_path = root.join("note_log.txt");

    let stub_path = root.join("cs_stub.sh");
    let stub_body = SH_STUB_DONE_ALWAYS_FAILS
        .replace("__STATE_PATH__", state_path.to_string_lossy().as_ref())
        .replace("__TICK_PATH__", tick_path.to_string_lossy().as_ref())
        .replace("__DONE_A_PATH__", done_a_path.to_string_lossy().as_ref())
        .replace("__TAG_LOG_PATH__", tag_log_path.to_string_lossy().as_ref())
        .replace(
            "__NOTE_LOG_PATH__",
            note_log_path.to_string_lossy().as_ref(),
        );
    std::fs::write(&stub_path, stub_body).unwrap();
    make_executable(&stub_path);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = stub_path;
    config.poll_interval = Duration::from_millis(20);
    // Compress the production schedule (2s doubling) so the whole run to the
    // ceiling fits inside the test's window: 10+20+40+80 ms of waiting for the
    // five attempts. The *shape* under test is the schedule and the ceiling,
    // not the wall-clock constants.
    config.teardown_backoff_base = Duration::from_millis(10);
    // Generous relative to the ~150 ms the schedule needs. The surplus is the
    // measurement: WITHOUT the fix the loop spends the whole window re-issuing
    // `cs done a` (dozens to hundreds of attempts at a 20 ms poll plus every
    // FS event); WITH it, the count stops dead at the ceiling.
    config.max_runtime = Some(Duration::from_secs(3));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime.run(&shutdown).expect("resident loop runs");

    let attempts = read_count(&done_a_path);
    if attempts != CEILING || summary.teardown_blocked != 1 {
        let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
        eprintln!("=== TRACE ===\n{trace}\n=== END TRACE ===\nsummary: {summary:?}");
    }

    // The core regression signal.
    assert_eq!(
        attempts, CEILING,
        "a deterministic teardown failure must be retried on a backoff and \
         stop at the ceiling ({CEILING}); got {attempts} attempts in 3 s \
         (summary: {summary:?})",
    );
    assert_eq!(
        summary.teardown_blocked, 1,
        "the loop must report exactly one blocked harvest — once per molecule, \
         not once per attempt; got {summary:?}",
    );

    // The park is written where an operator meets it, not only in the trace.
    let tags = read_lines(&tag_log_path);
    assert_eq!(
        tags.len(),
        1,
        "expected exactly one `cs tag` invocation, got {tags:?}",
    );
    assert!(
        tags[0].contains(cosmon_core::tag::BLOCKED_ON_TEARDOWN),
        "the tag must be `{}` — it is what `cs peek` counts; got {:?}",
        cosmon_core::tag::BLOCKED_ON_TEARDOWN,
        tags[0],
    );
    let notes = read_lines(&note_log_path);
    assert_eq!(
        notes,
        vec!["a".to_string()],
        "expected exactly one `cs note a` carrying the teardown error",
    );

    // The trace still holds the forensic record — the surfacing is additive.
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
    assert!(
        trace.contains("teardown-blocked"),
        "the trace must carry the `teardown-blocked` verdict line; got:\n{trace}",
    );
    assert!(
        trace.contains("teardown-retry"),
        "the trace must carry the intermediate backoff lines; got:\n{trace}",
    );

    // Parking a harvest does not invent progress: `b` is still blocked on a
    // molecule that cannot merge, and the loop honestly runs to its deadline.
    assert_eq!(
        summary.exit,
        ExitReason::Deadline,
        "with `a` unmergeable the loop cannot drain; got {:?}",
        summary.exit,
    );
    assert_eq!(
        summary.dones, 0,
        "no harvest succeeded, so the done counter must stay 0; got {summary:?}",
    );
    let state = std::fs::read_to_string(&state_path).unwrap_or_default();
    assert!(
        state.contains("b|pending|a"),
        "`b` must stay pending behind the unmergeable blocker; got:\n{state}",
    );
}
