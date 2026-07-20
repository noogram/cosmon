#!/usr/bin/env bash
# smoke-dispatch.sh (worktree ROOT) — prove ONE real dispatch through the
# regression bench's production path and record the produced artifacts beneath
# $MOLECULE_DIR/dispatch-output/.
#
# The producer here is the hardened cosmon regression bench (bench/). Its
# production dispatch path is `bench/run.sh`: it materialises the unit-under-test
# (COSMON_TAG, default HEAD — the FIXED tree), runs all six issue probes against
# it with a real built `cs` binary, and aggregates a machine-readable report.
# Nothing is fabricated or doubled — every row is emitted by a real probe run.
#
# This is NOT a preflight check: it drives the real probes (which build/scan the
# actual source and run `cs` for the runtime-decisive issues), then copies the
# produced report + per-probe records + evidence into the molecule's durable
# dispatch-output directory and asserts a real verdict landed.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

# Resolve the molecule directory: prefer the env the fleet injects; fall back to
# this molecule's canonical resolved path.
MOLECULE_DIR="${MOLECULE_DIR:-/Users/eserie/galaxies/cosmon/.cosmon/state/fleets/default/molecules/task-20260720-47e4}"
DISPATCH_OUT="$MOLECULE_DIR/dispatch-output"
mkdir -p "$DISPATCH_OUT"

# Prefer the fixed-tree release binary for the runtime-decisive probes.
export CS_BIN="${CS_BIN:-$ROOT/target/release/cs}"
export COSMON_TAG="${COSMON_TAG:-HEAD}"

echo "[smoke] driving the real bench production path (COSMON_TAG=$COSMON_TAG, CS_BIN=$CS_BIN)"

# Drive the real production dispatch path. --static skips only the docker halves
# (probe #2's Linux build discrimination); every other probe runs for real,
# including the `cs`-backed runtime-decisive verdicts for #1/#3/#4.
bash "$ROOT/bench/run.sh" --static

REPORT="$ROOT/bench/out/report.json"
[[ -f "$REPORT" ]] || { echo "[smoke][fatal] bench produced no report.json" >&2; exit 1; }

# Copy the produced records into the molecule's durable dispatch-output.
cp "$REPORT"                     "$DISPATCH_OUT/report.json"
cp "$ROOT/bench/out/report.md"   "$DISPATCH_OUT/report.md"
mkdir -p "$DISPATCH_OUT/probes" "$DISPATCH_OUT/evidence"
cp "$ROOT"/bench/out/probes/*.json   "$DISPATCH_OUT/probes/"   2>/dev/null || true
cp "$ROOT"/bench/out/evidence/*.txt  "$DISPATCH_OUT/evidence/" 2>/dev/null || true

# Assert a real record with a real verdict landed (the dispatch's exit proof).
POP="$(jq -r '.populated' "$DISPATCH_OUT/report.json")"
CNT="$(jq -r '.probe_count' "$DISPATCH_OUT/report.json")"
REAL_VERDICTS="$(jq -r '[.rows[] | select(.verdict=="RED" or .verdict=="GREEN" or .verdict=="INCONCLUSIVE")] | length' "$DISPATCH_OUT/report.json")"
if [[ "$POP" -ne "$CNT" || "$CNT" -ne 6 || "$REAL_VERDICTS" -lt 1 ]]; then
  echo "[smoke][fatal] incomplete dispatch: $POP/$CNT rows, $REAL_VERDICTS real verdicts" >&2
  exit 1
fi

echo "[smoke] OK: $POP/$CNT rows, tally $(jq -c '.verdict_tally' "$DISPATCH_OUT/report.json")"
echo "[smoke] records written under: $DISPATCH_OUT"
printf '%s\n' "$DISPATCH_OUT/report.json"
