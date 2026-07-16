# shellcheck shell=bash
# lib.sh — shared substrate for the mode-C falsification bench.
#
# Single source of truth for:
#   * the pinned provocation (model + endpoint + mission),
#   * the on-disk MARKERS that the pass/fail predicate greps,
#   * the THREE-verdict classifier (RECOVERED / DIED / INCONCLUSIVE),
#   * the batch predicate over N runs.
#
# Sourced by classify.sh, negative-control.sh, da-stream-ab.sh, run-bench.sh.
# No side effects at source time — only definitions + `readonly` constants.
#
# Provenance: delib-20260707-df9b outcomes.md §M-BENCH (turing full spec).
# The markers below MUST stay byte-aligned with the strings the cosmon
# OpenAI adapter writes to events.jsonl:
#   * recovery fired  → AdapterProbeResult::Retried{reason:"tool_parse_reinject"}
#     (crates/cosmon-provider/src/openai/mod.rs, emit_retry_probe)
#   * fatal stuck     → emit_silent_failure reason "SF-1 http: …" /
#     "SF-1 server_error …" / "tool_call_parse (unrecoverable after retries)"
# If those strings ever change, THIS file is where the bench is re-pinned.

set -uo pipefail

# ── The pinned provocation (a joint property model × server, not prompt) ──
# Overridable by env so the bench can run on whatever box hosts the 120B zoo,
# but the DEFAULTS are the exact pins that fired the 500 at role 1/9
# (academy task-20260707-c253, physics-intern.fleet.toml routing v3-local).
BENCH_MODEL="${BENCH_MODEL:-gpt-oss:120b}"
# Mode-C's ollama endpoint is a tunnel to the g5 box, NOT local ollama.
# run-comparison.sh pins WRAPPER_LOCAL=http://127.0.0.1:11436 (tunnel → g5:11436).
BENCH_OLLAMA="${BENCH_OLLAMA:-http://127.0.0.1:11436}"
ACADEMY_ROOT="${ACADEMY_ROOT:-/srv/cosmon/academy}"

# ── On-disk MARKERS (the load-bearing greps of the predicate) ─────────────
# RECOVERY-FIRED: the typed re-inject event M1 added. Its presence is the
# proof the 500 actually hit AND the adapter tried to recover in place —
# this is the `fired >= 1` clause without which a self-chunking run scores a
# false PASS.
readonly MARKER_FIRED='tool_parse_reinject'
# DEATH: a fatal AdapterLivenessProbed{Stuck} whose reason names the mode-C
# fault. `SF-1 http` / `SF-1 server_error` cover the raw-500 stuck path;
# `tool_call_parse` covers the retry-budget-exhausted ToolCallParse surface.
readonly MARKER_DEATH='tool_call_parse|SF-1 http|SF-1 server_error'
# Companion (non-load-bearing, reported for colour): transient 5xx/transport
# retries the same ride-along emits.
readonly MARKER_RETRY_5XX='server_error_5xx|server_error_transport|rate_limited'

# grep -c that never trips `set -e` on zero matches (grep exits 1 on no match).
count_marker() { # $1=pattern  $2=file
  [[ -f "$2" ]] || { echo 0; return; }
  grep -Ec "$1" "$2" 2>/dev/null || true
}

# ── Artefact test — a molecule "produced work" iff a non-empty synthesis or
# a non-empty responses/ file exists. Mirrors grade-run.sh's fleet_artifact
# notion (an ANSWERS-bearing synthesis) but is deliberately laxer: ANY
# durable cognitive artefact counts as survival, format-scoring is the
# oracle's job, not the bench's.
has_artefacts() { # $1=molecule_dir
  local d="$1"
  [[ -s "$d/synthesis.md" ]] && return 0
  local f
  for f in "$d"/responses/*; do [[ -s "$f" ]] && return 0; done
  [[ -s "$d/frame.md" ]] && return 0
  return 1
}

# ── THE three-verdict classifier ─────────────────────────────────────────
# Emits exactly one of: RECOVERED | DIED | INCONCLUSIVE  (plus AMBIGUOUS,
# folded into INCONCLUSIVE for the batch predicate but flagged for the human).
#
# Args: $1=events_file  $2=completed(0|1)  $3=has_artefacts(0|1)
# The caller resolves completed/artefacts (from `cs observe` or a fixture
# override) so this function is pure and unit-testable offline.
classify_verdict() {
  local events="$1" completed="$2" artefacts="$3"
  local fired died
  fired=$(count_marker "$MARKER_FIRED" "$events")
  died=$(count_marker "$MARKER_DEATH" "$events")

  # `fired >= 1` is the gate: no fire → the provocation did not hit → the run
  # proves nothing, whatever else happened.
  if (( fired < 1 )); then
    echo "INCONCLUSIVE"
    return
  fi
  # Fired AND survived with artefacts → the fix carried the worker through.
  if (( completed == 1 && artefacts == 1 && died < 1 )); then
    echo "RECOVERED"
    return
  fi
  # Fired AND stuck with no artefact → the fatal path this bench exists to
  # catch.
  if (( died >= 1 && artefacts == 0 )); then
    echo "DIED"
    return
  fi
  # Fired, but the signals disagree (e.g. a death marker WITH artefacts, or
  # completed WITH a death marker). Not a clean RECOVERED nor a clean DIED —
  # never silently upgrade to PASS.
  echo "AMBIGUOUS"
}

# ── Batch predicate over N verdicts (turing: PASS iff ≥1 RECOVERED and 0 DIED)
# Reads verdicts on stdin, one per line. Prints PASS | FAIL | INCONCLUSIVE
# and a tally to stderr.
batch_predicate() {
  local rec=0 died=0 inc=0 amb=0 v
  while read -r v; do
    case "$v" in
      RECOVERED) ((rec++)) ;;
      DIED) ((died++)) ;;
      INCONCLUSIVE) ((inc++)) ;;
      AMBIGUOUS) ((amb++)) ;;
    esac
  done
  echo "tally: RECOVERED=$rec DIED=$died INCONCLUSIVE=$inc AMBIGUOUS=$amb" >&2
  if (( died >= 1 )); then
    echo "FAIL"                       # any death fails the batch
  elif (( rec >= 1 && amb == 0 )); then
    echo "PASS"                       # ≥1 clean recovery, 0 death, 0 ambiguity
  elif (( rec >= 1 )); then
    echo "PASS-WITH-AMBIGUITY"        # recovered but some runs were ambiguous
  else
    echo "INCONCLUSIVE"               # no death, no recovery → provocation too weak
  fi
}
