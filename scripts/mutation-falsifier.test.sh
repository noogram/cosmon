#!/usr/bin/env bash
# mutation-falsifier.test.sh — regression tests for the mutation-falsifier gate
# (scripts/mutation-falsifier.sh).
#
# Fast, hermetic checks that do NOT run a full mutation sweep on the cosmon
# workspace (that is minutes-to-hours, reserved for the nightly cron / periodic
# formula). Exercises:
#   1. --check availability probe returns a sane code (0 if installed, 2 if not);
#   2. an unknown argument → exit 2;
#   3. the empty-diff fast path → exit 0 without invoking cargo-mutants;
#   4. the built-in --self-test — a REAL bounded mutation proof (a hole crate
#      reddens, a well-tested crate stays green) when cargo-mutants is present;
#      gracefully SKIPs (exit 0) when it is absent.
#
# Usage: ./scripts/mutation-falsifier.test.sh
# Exit: 0 on pass, non-zero on failure.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="$HERE/mutation-falsifier.sh"

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

pass=0 fail=0
ok()  { echo "PASS: $*"; pass=$((pass+1)); }
bad() { echo "FAIL: $*" >&2; fail=$((fail+1)); }

have_mutants() { command -v cargo-mutants >/dev/null 2>&1 || cargo mutants --version >/dev/null 2>&1; }

# ── 1. --check availability probe ────────────────────────────────────────────
bash "$GATE" --check >/dev/null 2>&1; rc=$?
if have_mutants; then
  [ "$rc" -eq 0 ] && ok "--check → 0 (cargo-mutants installed)" || bad "--check should be 0 when installed, got $rc"
else
  [ "$rc" -eq 2 ] && ok "--check → 2 (cargo-mutants absent)" || bad "--check should be 2 when absent, got $rc"
fi

# ── 2. unknown argument → exit 2 ─────────────────────────────────────────────
bash "$GATE" --nonsense-flag >/dev/null 2>&1; rc=$?
[ "$rc" -eq 2 ] && ok "unknown arg → exit 2" || bad "unknown arg should be exit 2, got $rc"

# ── 3. empty-diff fast path → exit 0, no cargo-mutants run ────────────────────
# Only meaningful when cargo-mutants is present (else the availability guard
# short-circuits to exit 2 before the diff check — which is correct behaviour).
if have_mutants; then
  REPO="$WORK/repo"
  mkdir -p "$REPO"
  cd "$REPO" || exit 2
  git init -q
  git config user.email "test@example.com"
  git config user.name "test"
  echo "seed" > seed.txt
  git add -A && git commit -qm "seed"
  git branch -f main HEAD >/dev/null 2>&1 || true
  # HEAD == main, so the diff vs main is empty → fast path exit 0.
  bash "$GATE" --base main >/dev/null 2>&1; rc=$?
  [ "$rc" -eq 0 ] && ok "empty diff vs base → exit 0 (fast path)" || bad "empty diff should be exit 0, got $rc"
  cd "$HERE" || exit 2
else
  echo "SKIP: empty-diff path needs cargo-mutants (absent)"
fi

# ── 4. built-in --self-test (real bounded mutation proof, or graceful SKIP) ──
if bash "$GATE" --self-test; then ok "--self-test passes (or SKIPs cleanly)"
else bad "--self-test should pass"; fi

echo "────────────────────────────────────────────"
echo "mutation-falsifier.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
