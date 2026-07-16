#!/usr/bin/env bash
# run-bench.sh — the live N-run replay orchestrator for the mode-C
# falsification bench (task-20260707-5fe6, delib-20260707-df9b §M-BENCH).
#
# Drives cosmon's REAL mode-C worker path end-to-end, N times: nucleate a
# task-work molecule carrying the pinned anharmonic-oscillator provocation
# (the exact academy role-1/9 mission that fired the ollama HTTP 500 in
# task-20260707-c253), tackle it on the `local` adapter with the PINNED model
# + endpoint, wait for a terminal state, then classify the molecule dir with
# classify.sh. Aggregates the N verdicts with lib.sh's batch predicate.
#
# The 500 is a JOINT property of model-output-shape × server-parser, so the
# model + endpoint are PINNED (BENCH_MODEL / BENCH_OLLAMA in lib.sh). An absent
# pin is reported as INCONCLUSIVE-UNAVAILABLE, never silently substituted — the
# honest result on a box without the 120B zoo, deferring to the deterministic
# Rust discriminator + negative-control.sh for the proof of power.
#
# Env overrides: BENCH_MODEL · BENCH_OLLAMA · BENCH_N (default 5) ·
#                BENCH_TAG (default bench:mode-c).
#
# Exit: 0 PASS/PASS-WITH-AMBIGUITY · 1 FAIL · 2 INCONCLUSIVE · 3 usage/env.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

N="${BENCH_N:-5}"
TAG="${BENCH_TAG:-bench:mode-c}"
MISSION="$HERE/provocation/anharmonic-mission.md"

command -v cs >/dev/null 2>&1 || { echo "error: cs not on PATH" >&2; exit 3; }

# Honest availability gate — an absent pin proves nothing about the fix.
reachable() { curl -s -m 5 "$1/api/version" >/dev/null 2>&1; }
model_present() { curl -s -m 10 "$1/api/tags" 2>/dev/null | jq -e --arg m "$2" '.models[]?.name | select(. == $m)' >/dev/null 2>&1; }

if ! reachable "$BENCH_OLLAMA" || ! model_present "$BENCH_OLLAMA" "$BENCH_MODEL"; then
  echo "INCONCLUSIVE-UNAVAILABLE: pinned endpoint/model not reachable."
  echo "  endpoint : $BENCH_OLLAMA"
  echo "  model    : $BENCH_MODEL"
  echo "  The exact mode-C provocation cannot run here. Proof of discriminating"
  echo "  power is deterministic and offline:"
  echo "    cargo test -p cosmon-provider --test mode_c_falsification_bench"
  echo "    scripts/mode-c-bench/negative-control.sh"
  exit 2
fi

echo "== mode-C falsification replay =="
echo "model=$BENCH_MODEL endpoint=$BENCH_OLLAMA N=$N"
echo "mission: $MISSION (sha256 $(shasum -a 256 "$MISSION" | awk '{print $1}'))"

TOPIC="$(cat "$MISSION")"
verdicts=()
for i in $(seq 1 "$N"); do
  id="$(cs nucleate task-work --var topic="$TOPIC" --json 2>/dev/null \
        | jq -r '.molecule_id // empty' 2>/dev/null | head -1)"
  if [[ -z "$id" ]]; then echo "run $i: nucleate failed" >&2; verdicts+=("INCONCLUSIVE"); continue; fi
  cs tag "$id" --add "$TAG" >/dev/null 2>&1 || true
  cs tackle "$id" --adapter local --model "$BENCH_MODEL" --no-worktree >/dev/null 2>&1 || true
  cs wait "$id" >/dev/null 2>&1 || true

  mol="$(cs observe "$id" --json 2>/dev/null | jq -r '.molecule_dir // empty' 2>/dev/null | head -1)"
  [[ -z "$mol" ]] && mol=".cosmon/state/fleets/default/molecules/$id"
  state="$(cs observe "$id" --json 2>/dev/null | jq -r '.state // "unknown"' 2>/dev/null | head -1)"

  v="$(bash "$HERE/classify.sh" "$mol" --state "$state")"
  echo "run $i: $v"
  verdicts+=("$(awk '{for(i=1;i<=NF;i++) if($i ~ /^verdict=/){sub(/^verdict=/,"",$i); print $i}}' <<<"$v")")
  # throwaway molecule — collapse so the bench does not sediment the ensemble.
  cs collapse "$id" --reason "mode-c bench replay run $i" >/dev/null 2>&1 || true
done

echo "== aggregate =="
result="$(printf '%s\n' "${verdicts[@]}" | batch_predicate)"
echo "VERDICT: $result"
case "$result" in
  PASS|PASS-WITH-AMBIGUITY) exit 0 ;;
  FAIL) exit 1 ;;
  *) exit 2 ;;
esac
