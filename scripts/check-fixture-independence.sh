#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-fixture-independence.sh — the tautological-fixture tripwire.
#
# A test fixture must PIN the code under test, not RE-DERIVE from it. When a
# fixture computes its "expected" value from the same self-referential
# primitive the production type would use — `size_of::<T>()`, `align_of::<T>()`,
# `offset_of!(T, field)` — the assertion is a tautology: a bug that changes the
# type's real layout flows into BOTH sides of the `assert_eq!` at once, so the
# test stays green and can never redden. The fixture has silently stopped being
# a fixture. This is the pathology delib-20260710-95a7 §Q6 named: *"reverting
# the fix must redden the test"* — a fixture that re-derives can be reverted and
# nothing goes red.
#
# THE RULE (falsifiable, low-false-positive): a line in TEST/FIXTURE code that
# uses a self-referential layout primitive inside an ASSERTION / EXPECTED
# context is a tautology candidate. A wire-layout fixture must hard-code the
# literal (`assert_eq!(header_len, 12)`), not recompute (`assert_eq!(header_len,
# size_of::<Header>())`).
#
# WHAT IT SCANS — `git grep` over the TRACKED test surface only (never the
# working tree): `.rs` files whose path is under a `tests/` directory or whose
# name matches `*fixture*`. Production `src/` is out of scope by design — the
# pathology lives in integration fixtures and golden data, and scoping here
# keeps false positives near zero without parsing `#[cfg(test)]` regions.
#
# THE PRIMITIVES (self-referential layout queries — reusing the tested type):
#   offset_of! · size_of::< · size_of_val · align_of::< · align_of_val ·
#   mem::size_of · mem::align_of · mem::offset_of
# flagged ONLY when the same line is an assertion/expected context:
#   assert_eq! · assert_ne! · assert! · debug_assert · expected · EXPECTED ·
#   golden · GOLDEN
# A discard (`let _ = size_of::<T>();`, the Sized-smoke idiom) has no
# assertion/expected token and is therefore CLEAN.
#
# OPT-OUT — a genuinely intentional layout pin (ABI conformance test that WANTS
# to fail when the type changes) declares it inline:
#   assert_eq!(off, offset_of!(Wire, tag)); // fixture-independence: allow — ABI pin
# The marker is the single, visible escape hatch; a reviewer sees the waiver.
#
# Usage:
#   ./scripts/check-fixture-independence.sh            # hard gate over test surface
#   ./scripts/check-fixture-independence.sh --self-test # prove the pattern catches
# Exit: 0 clean · 1 tautology candidate(s) found · 2 invocation/environment error.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# Off a TTY, detach stdin so no descendant can block the gate on a stray read
# (the gate scans FILES; it consumes nothing from stdin).
[ -t 0 ] || exec </dev/null

# ── the two halves of the rule, as extended regexes ──────────────────────────
# A self-referential layout primitive: querying the tested type for its own
# size/alignment/field-offset.
PRIMITIVE_RE='offset_of!|size_of::<|size_of_val|align_of::<|align_of_val|mem::size_of|mem::align_of|mem::offset_of'
# An assertion / expected context: the value is being CHECKED, so re-derivation
# is a tautology (vs. a discard, which is harmless).
CONTEXT_RE='assert_eq!|assert_ne!|assert!|debug_assert|expected|EXPECTED|golden|GOLDEN'
# The visible waiver a reviewer can see on the offending line.
OPTOUT_RE='fixture-independence:[[:space:]]*allow'

# ── --self-test: falsifiability. A gate that cannot fail is not a gate. Prove
# each half of the rule: primitive+context = hit; primitive-alone (discard) or
# context-alone (plain literal) = clean; opt-out = clean. ─────────────────────
if [ "${1:-}" = "--self-test" ]; then
  fails=0
  check() { # <line> <expect: hit|clean>
    local line="$1" want="$2" got
    if printf '%s' "$line" | grep -qE "$OPTOUT_RE"; then
      got=clean
    elif printf '%s' "$line" | grep -qE "$PRIMITIVE_RE" \
       && printf '%s' "$line" | grep -qE "$CONTEXT_RE"; then
      got=hit
    else
      got=clean
    fi
    if [ "$got" = "$want" ]; then echo "  ok   [$want] $line"
    else echo "  FAIL expected $want, got $got: $line"; fails=$((fails+1)); fi
  }
  echo "check-fixture-independence self-test:"
  # tautology candidates — primitive re-derived on an asserted/expected line:
  check 'assert_eq!(HEADER_LEN, size_of::<Header>());'          hit
  check 'let expected = offset_of!(Wire, tag);'                 hit
  check 'assert_eq!(align_of::<Token>(), 8);'                   hit
  check '    let EXPECTED = std::mem::size_of::<Frame>();'      hit
  # clean — discard (Sized-smoke idiom), no assertion/expected token:
  check 'let _ = std::mem::size_of::<EventV2>();'               clean
  # clean — a plain literal fixture (the CORRECT shape), no primitive:
  check 'assert_eq!(header.len(), 12);'                         clean
  check 'let expected = 42;'                                    clean
  # clean — prose/comment naming the concept but no primitive:
  check '// the expected golden layout is pinned below'         clean
  # clean — opt-out waiver present, intentional ABI pin:
  check 'assert_eq!(off, offset_of!(Wire, tag)); // fixture-independence: allow — ABI pin' clean
  if [ "$fails" -eq 0 ]; then echo "self-test: PASS"; exit 0
  else echo "self-test: $fails FAILED"; exit 1; fi
fi

case "${1:-}" in
  ''|--) : ;;
  *) echo "check-fixture-independence: unknown argument '$1'" >&2; exit 2 ;;
esac

REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null)" || {
  echo "check-fixture-independence: not inside a git repo" >&2; exit 2; }
cd "$REPO_ROOT" || exit 2

# ── the test surface: tracked `.rs` under a tests/ dir OR a *fixture* file ────
# `git grep` sees exactly `git ls-files` — what ships is what the gate scans.
SURFACE_PATHSPEC=(
  '*/tests/*.rs'
  'tests/*.rs'
  '*fixture*.rs'
)

echo "check-fixture-independence: scanning tracked test surface for tautological fixtures"

# A candidate line matches a primitive AND a context, minus opt-out lines.
# `git grep -E` scopes to the surface; we intersect the two halves and drop
# waived lines. `-n` gives file:line for the report.
prim_hits="$(git grep -I -n -E "$PRIMITIVE_RE" -- "${SURFACE_PATHSPEC[@]}" 2>/dev/null || true)"
hits="$(printf '%s\n' "$prim_hits" \
          | grep -E "$CONTEXT_RE" 2>/dev/null \
          | grep -vE "$OPTOUT_RE" 2>/dev/null || true)"

if [ -z "$hits" ]; then
  echo "check-fixture-independence: CLEAN — no fixture re-derives size_of/align_of/offset_of on an asserted value."
  exit 0
fi

n="$(printf '%s\n' "$hits" | grep -c . || true)"
echo "check-fixture-independence: TAUTOLOGY CANDIDATE(S) — $n line(s) re-derive layout on an asserted/expected value:" >&2
printf '%s\n' "$hits" >&2
echo >&2
echo "A fixture must PIN the layout with a literal, not RE-DERIVE it from the type" >&2
echo "under test — otherwise reverting a layout bug cannot redden the test." >&2
echo "Replace the recomputation with the concrete expected literal, or, if this is" >&2
echo "a deliberate ABI-conformance pin, mark the line: // fixture-independence: allow — <reason>" >&2
exit 1
