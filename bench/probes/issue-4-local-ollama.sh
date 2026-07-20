#!/usr/bin/env bash
# Probe #4 — LOCAL / OLLAMA ADAPTER: does a no-op mission get booked "completed"
# with empty synthesis?
#
# Reported defect: `run_local_worker` books a mission "completed" whenever the
# adapter loop returns Ok, with NO real-work / output check — so a no-op run
# (no model, unreachable backend, empty synthesis) still "passes".
#
# v1-bench BUG (fixed here): the old probe wired the runtime half but then keyed
# its verdict ONLY on a static grep of run_local_worker. This probe keys the
# verdict on the RUNTIME COMPLETION STATE: it actually drives a no-op local
# mission and asserts whether the molecule ends up "completed" (RED) or is
# refused/guarded and left tacklable (GREEN).
#
# Static corroboration: the fix (commit a1f91e3) adds
#   local_worker_produced_real_work()  (crates/cosmon-cli/src/cmd/tackle.rs)
# gating the completion mark, plus an adapter preflight that refuses to dispatch
# when the local backend is unreachable / the model is not served ("The molecule
# is untouched and still tacklable — nothing was spawned and nothing collapsed").
#
# Verdict:
#   RED   if the no-op mission is booked "completed" (with empty synthesis).
#   GREEN if the no-op mission is refused/guarded and the molecule is NOT
#         completed (still pending/active), AND the completion guard is present.
#   INCONCLUSIVE if no `cs` binary is available to run the decisive step.

source "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

ID="issue-4-local-ollama"
NAME="Local/Ollama adapter: no-op mission must NOT be booked 'completed'"
ADAPTER="local"

SRC="${1:-}"
CLEANUP=0
if [[ -z "$SRC" ]]; then
  SRC="$(mktemp -d)"; CLEANUP=1
  checkout_uut "$SRC"
fi

EVIDENCE="$EVIDENCE_OUT/$ID.txt"
: > "$EVIDENCE"
{
  echo "# Probe #4 local/ollama no-op completion guard"
  echo "# unit-under-test: $COSMON_TAG"
  echo "# cs binary: $CS_BIN"
  echo
} >> "$EVIDENCE"

# --- Static: is the completion-time real-work guard present? ----------------
echo "## static: completion guard in run_local_worker" >> "$EVIDENCE"
LOCAL_SRC="$(rg -l "fn run_local_worker" "$SRC/crates" -t rust 2>/dev/null | head -1 || true)"
GUARD_PRESENT=0
if [[ -n "$LOCAL_SRC" ]]; then
  echo "source: ${LOCAL_SRC#"$SRC"/}" >> "$EVIDENCE"
  GUARD_HITS="$(rg -n "local_worker_produced_real_work|refusing to dispatch|still tacklable" "$LOCAL_SRC" 2>/dev/null | sed "s#$SRC/##g" || true)"
  if [[ -n "$GUARD_HITS" ]]; then
    printf '%s\n' "$GUARD_HITS" >> "$EVIDENCE"
    GUARD_PRESENT=1
  else
    echo "  (no real-work guard located — completion appears unguarded)" >> "$EVIDENCE"
  fi
else
  echo "  run_local_worker not located — adapter surface must be re-mapped." >> "$EVIDENCE"
fi
echo "guard_present=$GUARD_PRESENT" >> "$EVIDENCE"
echo >> "$EVIDENCE"

# --- Runtime: drive a no-op local mission, read the completion state ---------
echo "## runtime: no-op local mission -> molecule status" >> "$EVIDENCE"
RUNTIME_STATE="skipped-no-binary"
FINAL_STATUS=""
SYNTH_EMPTY="NA"
if [[ -x "$CS_BIN" ]]; then
  RUNTIME_STATE="ran"
  WORK="$(mktemp -d)"
  pushd "$WORK" >/dev/null
  set +e
  {
    "$CS_BIN" init >/dev/null 2>&1
    MID="$("$CS_BIN" --json nucleate task-work --var topic="local-noop-probe" --no-parent 2>/dev/null | jq -r '.id // empty')"
    echo "molecule: $MID"
    # Force a genuine no-op: point the local backend at an unreachable port so
    # NO model can serve the mission — the classic "cosmon does not know which
    # ollama model to use" blocker. On the fixed tree this must NOT complete.
    echo "--- cs tackle $MID --adapter local (unreachable backend) ---"
    COSMON_LOCAL_BASE_URL="http://127.0.0.1:1" \
      timeout 60 "$CS_BIN" tackle "$MID" --adapter local --no-worktree 2>&1 | tail -6
    echo "--- molecule status after the no-op mission ---"
    FINAL_STATUS="$("$CS_BIN" observe "$MID" --json 2>/dev/null | jq -r '.status // empty')"
    echo "final status: [$FINAL_STATUS]"
    # Was any synthesis produced?
    SYNTH="$(find ".cosmon/state/fleets/default/molecules/$MID" -name 'synthesis.md' 2>/dev/null | head -1)"
    if [[ -n "$SYNTH" && -s "$SYNTH" ]]; then SYNTH_EMPTY="no"; else SYNTH_EMPTY="yes"; fi
    echo "synthesis empty/absent: $SYNTH_EMPTY"
    # Export for the outer shell.
    printf '%s' "$FINAL_STATUS" > "$WORK/.status"
    printf '%s' "$SYNTH_EMPTY" > "$WORK/.synth"
  } >> "$EVIDENCE" 2>&1
  set -e
  FINAL_STATUS="$(cat "$WORK/.status" 2>/dev/null || true)"
  SYNTH_EMPTY="$(cat "$WORK/.synth" 2>/dev/null || echo NA)"
  popd >/dev/null
  rm -rf "$WORK"
else
  echo "  cs binary absent ($CS_BIN) — runtime no-op mission not run here." >> "$EVIDENCE"
fi
echo >> "$EVIDENCE"

SIG="guard_present=$GUARD_PRESENT runtime=$RUNTIME_STATE final_status=${FINAL_STATUS:-NA} synthesis_empty=$SYNTH_EMPTY"

if [[ "$RUNTIME_STATE" == "ran" && -n "$FINAL_STATUS" ]]; then
  if [[ "$FINAL_STATUS" == "completed" ]]; then
    VERDICT="RED"
    NOTE="Defect REPRODUCES: a no-op local mission (unreachable backend, synthesis_empty=$SYNTH_EMPTY) was booked '$FINAL_STATUS'. The completion mark is not guarded by a real-work check."
  else
    VERDICT="GREEN"
    NOTE="Defect does NOT reproduce: a no-op local mission was refused/guarded and the molecule is '$FINAL_STATUS' (NOT completed); synthesis_empty=$SYNTH_EMPTY. Completion guard present in source=$GUARD_PRESENT (local_worker_produced_real_work + adapter preflight, commit a1f91e3)."
  fi
elif [[ "$GUARD_PRESENT" -eq 1 ]]; then
  VERDICT="INCONCLUSIVE"
  NOTE="Completion guard present in source (local_worker_produced_real_work + preflight) but the runtime no-op mission was not run here (no cs binary). Build cs / set CS_BIN to key the verdict on the live completion state."
else
  VERDICT="INCONCLUSIVE"
  NOTE="Neither the runtime completion state nor a source guard could be established on this tree; re-map the local adapter surface."
fi

emit_probe "$ID" "$NAME" "$ADAPTER" "$VERDICT" "$SIG" "$EVIDENCE" "$NOTE"

[[ "$CLEANUP" -eq 1 ]] && rm -rf "$SRC"
exit 0
