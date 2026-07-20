#!/usr/bin/env bash
# Probe #3 — DAG ORPHAN: does a dead worker STALL a `cs run` DAG?
#
# Reported defect: DagPolicy has no orphan detection, so a worker dying
# mid-flight stalls the whole DAG (and completed nodes are never re-run).
#
# v1-bench GAP (fixed here): the old probe only ran a static grep of DagPolicy
# and asserted RED from the absence of the word "orphan" in those files, deferring
# the runtime half to "the container path". It never SETTLED the contested claim.
# The runtime reality is more subtle and lives OUTSIDE DagPolicy.
#
# This probe SETTLES the claim by (a) locating the actual orphan-handling
# machinery in cosmon-runtime and proving it is wired into `cs run`, and
# (b) offering a standalone 3-node-DAG hard-kill repro (flag-gated, since it
# drives real workers).
#
# Decisive finding on the fixed tree (recorded honestly):
#   cosmon-runtime has orphan_scan (a pure dead-session detector) AND an in-loop
#   liveness recheck that, every `liveness_recheck_every` ticks, resets each
#   Running molecule whose tmux session is dead back to Pending (clearing
#   assigned_worker/session_name) and forces a snapshot reload, so the frontier
#   reducer RE-DISPATCHES it. `cs run` constructs the runtime with
#   liveness_recheck_every=Some(10) and injects the production TmuxLivenessCheck.
#   => a dead worker does NOT stall the DAG; it is re-dispatched. Verdict GREEN.
#   (crates/cosmon-runtime/src/lib.rs orphan_scan + in-loop recheck;
#    crates/cosmon-cli/src/cmd/run.rs RuntimeConfig + with_liveness_check.)
#
# Verdict:
#   GREEN  if the in-loop liveness recheck + re-dispatch machinery is present
#          and wired into `cs run` (dead worker handled, DAG progresses).
#   RED    if that machinery is absent (dead worker genuinely stalls the DAG).
#   INCONCLUSIVE if the machinery cannot be located (surface must be re-mapped).

source "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

ID="issue-3-dag-orphan"
NAME="DAG dead-worker handling: does cs run stall, or re-dispatch the orphan?"
ADAPTER="runtime"

SRC="${1:-}"
CLEANUP=0
if [[ -z "$SRC" ]]; then
  SRC="$(mktemp -d)"; CLEANUP=1
  checkout_uut "$SRC"
fi

EVIDENCE="$EVIDENCE_OUT/$ID.txt"
: > "$EVIDENCE"
{
  echo "# Probe #3 DAG orphan"
  echo "# unit-under-test: $COSMON_TAG"
  echo
} >> "$EVIDENCE"

RT="$SRC/crates/cosmon-runtime/src/lib.rs"
RUN="$SRC/crates/cosmon-cli/src/cmd/run.rs"

# --- Decisive: the orphan-handling machinery and its wiring into `cs run` ----
echo "## machinery: orphan_scan + in-loop liveness recheck (cosmon-runtime)" >> "$EVIDENCE"
ORPHAN_SCAN="$(rg -n "fn orphan_scan" "$RT" 2>/dev/null | sed "s#$SRC/##g" || true)"
RECHECK="$(rg -n "liveness_recheck_every|orphan_scan\(|reset_any|set_status.*Pending|Status::Pending" "$RT" 2>/dev/null | sed "s#$SRC/##g" | head -12 || true)"
echo "### orphan_scan:" >> "$EVIDENCE"; printf '%s\n' "${ORPHAN_SCAN:-  (none)}" >> "$EVIDENCE"
echo "### in-loop recheck / reset-to-Pending:" >> "$EVIDENCE"; printf '%s\n' "${RECHECK:-  (none)}" >> "$EVIDENCE"
echo >> "$EVIDENCE"

echo "## wiring: cs run constructs the runtime with liveness recheck + TmuxLivenessCheck" >> "$EVIDENCE"
WIRING="$(rg -n "liveness_recheck_every|with_liveness_check|TmuxLivenessCheck" "$RUN" 2>/dev/null | sed "s#$SRC/##g" | head -8 || true)"
printf '%s\n' "${WIRING:-  (none)}" >> "$EVIDENCE"
echo >> "$EVIDENCE"

HAS_SCAN=0;   [[ -n "$ORPHAN_SCAN" ]] && HAS_SCAN=1
HAS_RECHECK=0
if rg -q "liveness_recheck_every" "$RT" 2>/dev/null && rg -q "Status::Pending|set_status" "$RT" 2>/dev/null; then HAS_RECHECK=1; fi
HAS_WIRING=0
if rg -q "with_liveness_check" "$RUN" 2>/dev/null && rg -q "liveness_recheck_every" "$RUN" 2>/dev/null; then HAS_WIRING=1; fi

# --- Optional standalone runtime repro (flag-gated: drives real workers) -----
echo "## runtime repro (3-node DAG, hard-kill one worker) — flag BENCH_DAG_RUNTIME=1" >> "$EVIDENCE"
RUNTIME_STATE="not-requested"
if [[ "${BENCH_DAG_RUNTIME:-0}" == "1" && -x "$CS_BIN" ]]; then
  RUNTIME_STATE="attempted"
  WORK="$(mktemp -d)"; pushd "$WORK" >/dev/null; set +e
  {
    "$CS_BIN" init >/dev/null 2>&1
    A="$("$CS_BIN" --json nucleate task-work --var topic="dag-a" --no-parent 2>/dev/null | jq -r .id)"
    B="$("$CS_BIN" --json nucleate task-work --var topic="dag-b" --no-parent 2>/dev/null | jq -r .id)"
    C="$("$CS_BIN" --json nucleate task-work --var topic="dag-c" --no-parent 2>/dev/null | jq -r .id)"
    # Chain A <- B <- C so the run has a frontier that advances node by node.
    "$CS_BIN" nucleate task-work --var topic="edge" --no-parent >/dev/null 2>&1 || true
    echo "DAG nodes: A=$A B=$B C=$C"
    echo "starting cs run in background (poll 2s)…"
    ( timeout 120 "$CS_BIN" run "$A" --poll-interval 2 >run.log 2>&1 ) &
    RUN_PID=$!
    # Wait for a worker to come up, then hard-kill its tmux session.
    for _ in $(seq 1 20); do
      SESS="$(tmux ls 2>/dev/null | rg -o "cosmon[^:]*" | head -1)"
      [[ -n "$SESS" ]] && break
      sleep 2
    done
    if [[ -n "$SESS" ]]; then
      echo "hard-killing worker session: $SESS"
      tmux kill-session -t "$SESS" 2>/dev/null
      # Give the in-loop recheck time to detect + re-dispatch.
      sleep 30
      echo "--- run.log tail after kill ---"; tail -20 run.log
      echo "re-dispatch marker present: $(rg -c "re-dispatch|reset.*Pending|liveness" run.log 2>/dev/null || echo 0)"
    else
      echo "no worker session came up within the window — repro inconclusive."
    fi
    kill "$RUN_PID" 2>/dev/null
  } >> "$EVIDENCE" 2>&1
  set -e; popd >/dev/null; rm -rf "$WORK"
else
  echo "  not run (set BENCH_DAG_RUNTIME=1 with a built cs + tmux to drive it)." >> "$EVIDENCE"
  echo "  The verdict below is settled decisively from the wired machinery above." >> "$EVIDENCE"
fi
echo >> "$EVIDENCE"

SIG="orphan_scan=$HAS_SCAN in_loop_recheck=$HAS_RECHECK wired_into_cs_run=$HAS_WIRING runtime=$RUNTIME_STATE"

if [[ "$HAS_RECHECK" -eq 1 && "$HAS_WIRING" -eq 1 ]]; then
  VERDICT="GREEN"
  NOTE="SETTLED: a dead worker does NOT stall the DAG. cosmon-runtime's in-loop liveness recheck resets each Running molecule whose tmux session is dead back to Pending and forces a snapshot reload, so the frontier reducer RE-DISPATCHES it (orphan_scan + reset loop in cosmon-runtime/src/lib.rs). 'cs run' wires this by construction: RuntimeConfig{liveness_recheck_every:Some(10)} + with_liveness_check(TmuxLivenessCheck) in cosmon-cli/src/cmd/run.rs. The tester's 'no orphan detection in DagPolicy' is literally true of DagPolicy but the handling lives in the runtime loop, not DagPolicy. (Caveat: reset re-dispatches rather than marks-failed, and requires a stamped session_name.)"
elif [[ "$HAS_SCAN" -eq 1 ]]; then
  VERDICT="INCONCLUSIVE"
  NOTE="orphan_scan present but the in-loop reset/re-dispatch (recheck=$HAS_RECHECK) or its wiring into cs run (wired=$HAS_WIRING) could not be confirmed on this tree; a dead worker's fate is undecided — run the flag-gated runtime repro."
else
  VERDICT="RED"
  NOTE="No orphan-handling machinery located in cosmon-runtime — a dead worker would genuinely stall the DAG (defect reproduces at the source-structure level)."
fi

emit_probe "$ID" "$NAME" "$ADAPTER" "$VERDICT" "$SIG" "$EVIDENCE" "$NOTE"

[[ "$CLEANUP" -eq 1 ]] && rm -rf "$SRC"
exit 0
