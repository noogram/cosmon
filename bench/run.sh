#!/usr/bin/env bash
# run.sh — the bench's full production dispatch path.
#
# One entrypoint that (1) materialises the pristine v0.2.1 tree once, (2) runs
# every issue-probe against it, and (3) aggregates a machine-readable report.
# The Docker-heavy halves (probe #2's two builds, and the runtime reproductions
# for #1/#3/#4/#6) run when docker / a built cs / ollama are present; otherwise
# each probe records INCONCLUSIVE for its unrunnable half — never a silent pass.
#
# Usage:
#   bench/run.sh            # all six probes + aggregate
#   bench/run.sh --static   # skip docker builds (probe #2 static-only); fast
#
# Exit status: 0 when the report was produced with all six rows populated (the
# bench's own smoke test). A missing row is a hard failure.

source "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

STATIC_ONLY=0
[[ "${1:-}" == "--static" ]] && STATIC_ONLY=1

log "cosmon $COSMON_TAG regression bench starting (static_only=$STATIC_ONLY, cs=$CS_BIN)"

# Materialise the unit-under-test ONCE and share it across all probes.
UUT="$OUT_DIR/uut"
checkout_uut "$UUT"

if [[ "$STATIC_ONLY" -eq 1 ]]; then
  export BENCH_SKIP_DOCKER=1
fi

# Run each probe against the shared checkout. A probe failure is a bench bug,
# not a finding (findings are encoded in the JSON verdict), so we surface it.
for probe in \
  "$BENCH_DIR/probes/issue-1-cs-verify.sh" \
  "$BENCH_DIR/probes/issue-2-build-deps.sh" \
  "$BENCH_DIR/probes/issue-3-dag-orphan.sh" \
  "$BENCH_DIR/probes/issue-4-local-ollama.sh" \
  "$BENCH_DIR/probes/issue-5-paper-cuts.sh" \
  "$BENCH_DIR/probes/issue-6-claude-adapter.sh" ; do
  name="$(basename "$probe")"
  # Probe #2 owns docker; in --static mode we run it (it self-detects and
  # records INCONCLUSIVE for the build halves) — its static lockfile check
  # still runs.
  log "running $name"
  if ! bash "$probe" "$UUT"; then
    die "probe $name crashed (bench bug) — see stderr above"
  fi
done

# Aggregate into report.json + report.md.
bash "$BENCH_DIR/aggregate.sh"

# Smoke assertion: all six rows populated from a real run.
POP="$(jq -r '.populated' "$OUT_DIR/report.json")"
CNT="$(jq -r '.probe_count' "$OUT_DIR/report.json")"
if [[ "$POP" -ne "$CNT" || "$CNT" -ne 6 ]]; then
  die "report incomplete: $POP/$CNT rows populated (expected 6/6)"
fi

log "bench complete: report at $OUT_DIR/report.json ($POP/$CNT rows)"
