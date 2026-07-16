#!/usr/bin/env bash
# assert-hits.test.sh — regression tests for scripts/assert-hits.sh (G_inject,
# ADR-134). Exercises both the sourced functions and the CLI dispatch.
#
# Cases (per the ADR's self-test contract — zero→exit-2 / N→pass / --min
# boundary / garbage-args / stream-mode, no external dependency):
#   1. zero hits          → FAIL, exit 2, loud stderr
#   2. N hits             → pass, echoes N
#   3. --min boundary     → count == min passes; count == min-1 fails
#   4. missing/garbage    → usage error, exit 64
#   5. stream mode        → N lines pass; 0 lines fail with exit 2
#
# Usage:   scripts/assert-hits.test.sh        # run all cases
#          scripts/assert-hits.test.sh -v     # verbose
# Exit:    0 all pass, 1 at least one failure.

set -uo pipefail

VERBOSE=0
[[ "${1:-}" == "-v" ]] && VERBOSE=1

HERE="$(cd "$(dirname "$0")" && pwd)"
HELPER="$HERE/assert-hits.sh"

if [[ ! -x "$HELPER" ]]; then
  echo "helper not executable: $HELPER" >&2
  exit 1
fi

# Pull the functions into this shell so we can test them directly, not only
# through the CLI dispatch.
# shellcheck source=/dev/null
source "$HELPER"

PASS_COUNT=0
FAIL_COUNT=0

ok()  { PASS_COUNT=$((PASS_COUNT + 1)); [[ $VERBOSE -eq 1 ]] && echo "  ok   $*"; return 0; }
bad() { FAIL_COUNT=$((FAIL_COUNT + 1)); echo "  FAIL $*" >&2; return 0; }

# assert_eq <case> <expected> <actual>
assert_eq() {
  local case="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    ok "$case: '$actual'"
  else
    bad "$case: expected '$expected', got '$actual'"
  fi
}

# assert_rc <case> <expected_rc> <actual_rc>
assert_rc() {
  local case="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    ok "$case: rc=$actual"
  else
    bad "$case: expected rc=$expected, got rc=$actual"
  fi
}

echo "▸ case 1 — zero hits fails closed (exit 2)"
out=$(assert_hits "favicons" 0 2>/tmp/ah_err.$$); rc=$?
assert_rc "zero/rc" 2 "$rc"
assert_eq "zero/stdout-empty" "" "$out"
if grep -q 'FAIL: favicons: expected >=1 hits, got 0' /tmp/ah_err.$$; then
  ok "zero/stderr-loud"
else
  bad "zero/stderr-loud: missing FAIL line (got: $(cat /tmp/ah_err.$$))"
fi
rm -f /tmp/ah_err.$$

echo "▸ case 2 — N hits passes and echoes the count"
out=$(assert_hits "favicons" 12); rc=$?
assert_rc "N/rc" 0 "$rc"
assert_eq "N/stdout" "12" "$out"

echo "▸ case 3 — --min boundary"
out=$(assert_hits "navbars" 3 --min 3); rc=$?      # count == min → pass
assert_rc "min-eq/rc" 0 "$rc"
assert_eq "min-eq/stdout" "3" "$out"
out=$(assert_hits "navbars" 2 --min 3 2>/dev/null); rc=$?  # count == min-1 → fail
assert_rc "min-below/rc" 2 "$rc"
out=$(assert_hits "navbars" 5 --min=5); rc=$?      # --min=N form
assert_rc "min-eqsign/rc" 0 "$rc"
assert_eq "min-eqsign/stdout" "5" "$out"

echo "▸ case 4 — missing / garbage args are usage errors (exit 64)"
assert_hits "favicons" >/dev/null 2>&1; rc=$?      # missing count
assert_rc "missing-count/rc" 64 "$rc"
assert_hits >/dev/null 2>&1; rc=$?                 # missing everything
assert_rc "missing-all/rc" 64 "$rc"
assert_hits "favicons" "twelve" >/dev/null 2>&1; rc=$?   # non-integer count
assert_rc "garbage-count/rc" 64 "$rc"
assert_hits "favicons" 3 --min "lots" >/dev/null 2>&1; rc=$?  # non-integer min
assert_rc "garbage-min/rc" 64 "$rc"
assert_hits "favicons" 3 --bogus >/dev/null 2>&1; rc=$?  # unknown flag
assert_rc "unknown-flag/rc" 64 "$rc"

echo "▸ case 5 — stream mode counts lines"
out=$(printf 'a\nb\nc\n' | assert_hits_stream "nav bars" --min 1); rc=$?
assert_rc "stream-3/rc" 0 "$rc"
assert_eq "stream-3/stdout" "3" "$out"
out=$(printf 'only-line-no-newline' | assert_hits_stream "footer"); rc=$?  # no trailing \n
assert_rc "stream-1-no-nl/rc" 0 "$rc"
assert_eq "stream-1-no-nl/stdout" "1" "$out"
out=$(printf '' | assert_hits_stream "nav bars" --min 1 2>/dev/null); rc=$?  # empty → fail
assert_rc "stream-0/rc" 2 "$rc"

echo "▸ case 6 — CLI dispatch (executable, not sourced)"
out=$("$HELPER" favicons 7); rc=$?
assert_rc "cli-pass/rc" 0 "$rc"
assert_eq "cli-pass/stdout" "7" "$out"
"$HELPER" favicons 0 >/dev/null 2>&1; rc=$?
assert_rc "cli-fail/rc" 2 "$rc"
out=$(printf 'x\ny\n' | "$HELPER" --stream "nav bars" --min 1); rc=$?
assert_rc "cli-stream/rc" 0 "$rc"
assert_eq "cli-stream/stdout" "2" "$out"

echo
echo "passed: $PASS_COUNT  failed: $FAIL_COUNT"
[[ "$FAIL_COUNT" -gt 0 ]] && exit 1
exit 0
