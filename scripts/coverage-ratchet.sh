#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# coverage-ratchet.sh — per-crate line-coverage ratchet (regression-only).
#
# The L1 lane of delib-20260711-9928 (cosmon full-review) rejected the
# aspirational-absolute coverage gate (the old `≥85%` of coverage-report.md,
# task-20260413-0460): a fixed floor is either so low it never fires or so high
# it blocks legitimate work, and neither answers the real question — *did this
# change make coverage WORSE?* This script gates on that question alone.
#
# It measures per-crate line coverage with `cargo llvm-cov --package <c>
# --summary-only`, reads the recorded baseline from `coverage-baselines.toml`,
# and fails ONLY when a gated crate regresses below `baseline - EPSILON`. It
# never asserts an absolute floor; a crate that jumps to 99% simply raises the
# bar the operator can later re-baseline. Report-only crates print their delta
# and never fail the run.
#
# ⚠️ CADENCE — this is a proposal (N=0, L1 lane). It is NOT wired into the
# per-step `cs evolve` gate or `cs done`. Intended homes (operator ratifies via
# C13): (a) a PR job gating core/graph/hash; (b) a nightly report for
# state/surface. It mirrors the mutation-falsifier precedent: a script the CI
# calls, not a hot-path hard-abort.
#
# WHY NOT `--workspace` — the L1 briefing is explicit: cosmon-cli's ~100 K lines
# of I/O glue tank a naive aggregate number (workspace baseline was ~75% and
# meaningless). Coverage is measured and ratcheted PER pure-logic crate.
#
# WHY `--lib` FOR core — cosmon-core carries trybuild compile-fail harnesses
# (crates/cosmon-core/tests/compile_fail/, role_typestate_compile_fail.rs) that
# recompile the crate as a subprocess. Under llvm-cov instrumentation these do
# not terminate within a 20-minute budget and yield ZERO coverage signal (they
# assert the compiler REJECTS code — no runtime executes). `--lib` restricts to
# the in-src unit tests, which is the meaningful pure-logic coverage surface.
# graph/hash/state/surface have no trybuild harness and run whole-package.
#
# Usage:
#   ./scripts/coverage-ratchet.sh                 # gate all, honour per-crate mode
#   ./scripts/coverage-ratchet.sh --update        # re-record baselines (operator gesture)
#   ./scripts/coverage-ratchet.sh --package core  # single crate
#   EPSILON=0.5 ./scripts/coverage-ratchet.sh     # widen the slack (default 0.3 pts)
#
# Exit: 0 = no gated regression · 1 = a gated crate regressed · 2 = tooling absent.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

EPSILON="${EPSILON:-0.3}"          # allowed line-% slack below baseline (noise floor)
BASELINES="${BASELINES:-$(dirname "$0")/coverage-baselines.toml}"
UPDATE=0
ONLY=""

while [ $# -gt 0 ]; do
  case "$1" in
    --update)  UPDATE=1 ;;
    --package) shift; ONLY="$1" ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

# Per-crate policy: crate | cargo-package | test-scope | mode(gate|sweep)
# gate  → a regression fails CI. sweep → report-only, never fails.
CRATES=(
  "core|cosmon-core|--lib|gate"
  "graph|cosmon-graph||gate"
  "hash|cosmon-hash||gate"
  "state|cosmon-state||sweep"
  "surface|cosmon-surface||sweep"
)

command -v cargo-llvm-cov >/dev/null 2>&1 || {
  echo "coverage-ratchet: cargo-llvm-cov not installed (cargo install cargo-llvm-cov)" >&2
  exit 2
}

# llvm-cov needs llvm-tools; the pinned minimal toolchain may not expose them on
# PATH — fall back to the rustc sysroot binaries if the env vars are unset.
if [ -z "${LLVM_COV:-}" ]; then
  _bin="$(rustc --print target-libdir 2>/dev/null)/../bin"
  [ -x "$_bin/llvm-cov" ]     && export LLVM_COV="$_bin/llvm-cov"
  [ -x "$_bin/llvm-profdata" ] && export LLVM_PROFDATA="$_bin/llvm-profdata"
fi

# read a baseline line-% for <key> from the TOML (`key = NN.NN`); empty if absent
baseline_for() { awk -F'=' -v k="$1" '$1 ~ "^[[:space:]]*"k"[[:space:]]*$" {gsub(/[^0-9.]/,"",$2); print $2}' "$BASELINES" 2>/dev/null; }

# measure line-% for a package: `measure <pkg> <scope-flag-or-empty>`
measure() {
  local pkg="$1" scope="$2" line
  local scope_args=()
  [ -n "$scope" ] && scope_args=("$scope")   # empty scope → whole-package
  # TOTAL row, 10th whitespace field is the line-% (e.g. "93.41%")
  line="$(cargo llvm-cov --package "$pkg" "${scope_args[@]}" --summary-only 2>/dev/null \
            | awk '/^TOTAL/ {print $10}' | tr -d '%')"
  echo "$line"
}

rc=0
declare -a NEWBASE
printf '%-9s %-8s %8s %8s %8s   %s\n' "crate" "mode" "base%" "now%" "delta" "verdict"
printf -- '─%.0s' {1..64}; echo

for row in "${CRATES[@]}"; do
  IFS='|' read -r key pkg scope mode <<<"$row"
  [ -n "$ONLY" ] && [ "$ONLY" != "$key" ] && continue

  now="$(measure "$pkg" "$scope")"
  if [ -z "$now" ]; then
    printf '%-9s %-8s %8s %8s %8s   %s\n' "$key" "$mode" "-" "-" "-" "MEASURE-FAILED"
    [ "$mode" = "gate" ] && rc=1
    continue
  fi
  NEWBASE+=("$key = $now")

  base="$(baseline_for "$key")"
  if [ -z "$base" ]; then
    printf '%-9s %-8s %8s %8.2f %8s   %s\n' "$key" "$mode" "n/a" "$now" "-" "NO-BASELINE"
    continue
  fi

  delta="$(awk -v n="$now" -v b="$base" 'BEGIN{printf "%.2f", n-b}')"
  floor="$(awk -v b="$base" -v e="$EPSILON" 'BEGIN{printf "%.4f", b-e}')"
  regressed="$(awk -v n="$now" -v f="$floor" 'BEGIN{print (n<f)?1:0}')"

  if [ "$regressed" = "1" ] && [ "$mode" = "gate" ]; then
    verdict="REGRESSION ✗ (gate)"; rc=1
  elif [ "$regressed" = "1" ]; then
    verdict="regressed (sweep, advisory)"
  else
    verdict="ok"
  fi
  printf '%-9s %-8s %8s %8.2f %8s   %s\n' "$key" "$mode" "$base" "$now" "$delta" "$verdict"
done

if [ "$UPDATE" = "1" ]; then
  {
    echo "# coverage-baselines.toml — per-crate line-coverage baselines."
    echo "# Regenerated by scripts/coverage-ratchet.sh --update (operator gesture)."
    echo "# Ratchet fails a GATE crate only when now% < baseline − EPSILON (default 0.3)."
    echo "[line_pct]"
    printf '%s\n' "${NEWBASE[@]}"
  } > "$BASELINES"
  echo
  echo "coverage-ratchet: baselines written to $BASELINES"
fi

exit $rc
