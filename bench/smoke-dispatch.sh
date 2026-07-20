#!/usr/bin/env bash
# smoke-dispatch.sh — prove ONE real dispatch through the bench's production
# path and record the produced artifact beneath $MOLECULE_DIR/dispatch-output/.
#
# The "minimal real unit" is probe #5 (paper cuts): it is the only probe whose
# decisive half runs fully headless — no docker, no built binary, no network —
# by scanning a pristine v0.2.1 tree materialised via `git archive`. It is a
# genuine production probe, not a test double: it greps real shipped source and
# emits a real report record with file:line evidence.
#
# This script drives the REAL production path (lib/common.sh -> probe #5 ->
# aggregate.sh), then copies the produced report + probe record + evidence into
# the molecule's durable dispatch-output directory. Nothing is fabricated.

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$BENCH_DIR/lib/common.sh"

# Resolve the molecule directory. Prefer the env the fleet injects; fall back
# to the canonical resolved path for this molecule.
MOLECULE_DIR="${MOLECULE_DIR:-/Users/eserie/galaxies/cosmon/.cosmon/state/fleets/default/molecules/task-20260720-35e1}"
DISPATCH_OUT="$MOLECULE_DIR/dispatch-output"
mkdir -p "$DISPATCH_OUT"

log "smoke-dispatch: materialising $COSMON_TAG and running the real probe-5 production path"

# 1) Materialise the unit-under-test (pristine v0.2.1, never mutated).
UUT="$OUT_DIR/uut"
checkout_v021 "$UUT"

# 2) Drive the real production probe for one unit (probe #5, headless-decisive).
bash "$BENCH_DIR/probes/issue-5-paper-cuts.sh" "$UUT"

# 3) Aggregate the produced probe record into the machine-readable report.
bash "$BENCH_DIR/aggregate.sh"

# 4) Copy the produced records into the molecule's durable dispatch-output.
cp "$OUT_DIR/report.json"                 "$DISPATCH_OUT/report.json"
cp "$OUT_DIR/report.md"                   "$DISPATCH_OUT/report.md"
cp "$PROBES_OUT/issue-5-paper-cuts.json"  "$DISPATCH_OUT/issue-5-paper-cuts.json"
cp "$EVIDENCE_OUT/issue-5-paper-cuts.txt" "$DISPATCH_OUT/issue-5-paper-cuts.evidence.txt"

# 5) Assert a real record landed (the dispatch's exit-criteria proof).
VERDICT="$(jq -r '.rows[] | select(.id=="issue-5-paper-cuts") | .verdict' "$DISPATCH_OUT/report.json")"
SIG="$(jq -r '.rows[] | select(.id=="issue-5-paper-cuts") | .captured_signature' "$DISPATCH_OUT/report.json")"
if [[ -z "$VERDICT" || "$VERDICT" == "null" ]]; then
  die "smoke-dispatch produced no probe-5 verdict — production path did not run"
fi

log "smoke-dispatch OK: probe-5 verdict=$VERDICT signature=[$SIG]"
log "record written under: $DISPATCH_OUT"
printf '%s\n' "$DISPATCH_OUT/issue-5-paper-cuts.json"
