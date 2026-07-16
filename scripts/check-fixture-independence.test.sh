#!/usr/bin/env bash
# check-fixture-independence.test.sh — regression tests for the tautological-
# fixture tripwire (scripts/check-fixture-independence.sh).
#
# Runs against a THROWAWAY git repo in a tmpdir so the assertions are hermetic —
# never against the live cosmon tree. Exercises:
#   1. the built-in --self-test (pattern falsifiability);
#   2. a clean tree (literal fixtures only) → exit 0;
#   3. a tautological fixture seeded under tests/ (assert re-derives size_of) → exit 1;
#   4. the discard idiom `let _ = size_of::<T>();` under tests/ → exit 0 (not flagged);
#   5. the opt-out waiver `// fixture-independence: allow` → exit 0.
#
# Usage: ./scripts/check-fixture-independence.test.sh
# Exit: 0 on pass, non-zero on failure.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="$HERE/check-fixture-independence.sh"

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

pass=0 fail=0
ok()  { echo "PASS: $*"; pass=$((pass+1)); }
bad() { echo "FAIL: $*" >&2; fail=$((fail+1)); }

# ── 1. built-in self-test ────────────────────────────────────────────────────
if bash "$GATE" --self-test >/dev/null 2>&1; then ok "--self-test passes"
else bad "--self-test should pass"; fi

# ── build a throwaway repo with the script copied to scripts/ ────────────────
REPO="$WORK/repo"
mkdir -p "$REPO/scripts" "$REPO/crates/foo/tests"
cp "$GATE" "$REPO/scripts/check-fixture-independence.sh"
cd "$REPO" || exit 2
git init -q
git config user.email "test@example.com"
git config user.name "test"

# a CLEAN fixture — a literal, the correct shape.
cat > crates/foo/tests/layout.rs <<'RS'
#[test]
fn header_is_twelve_bytes() {
    assert_eq!(header_len(), 12);
}
RS
git add -A && git commit -qm "clean fixture"

# ── 2. clean tree → exit 0 ───────────────────────────────────────────────────
if bash scripts/check-fixture-independence.sh >/dev/null 2>&1; then
  ok "clean literal fixture → exit 0"
else
  bad "clean tree should pass (exit 0)"
fi

# ── 4. discard idiom is not a tautology → still exit 0 ───────────────────────
cat >> crates/foo/tests/layout.rs <<'RS'

#[test]
fn is_sized() {
    let _ = std::mem::size_of::<u64>();
}
RS
git add -A && git commit -qm "add sized-smoke discard"
if bash scripts/check-fixture-independence.sh >/dev/null 2>&1; then
  ok "discard 'let _ = size_of::<T>()' under tests/ → exit 0 (not flagged)"
else
  bad "the Sized-smoke discard idiom must NOT be flagged"
fi

# ── 3. seed a tautological fixture → exit 1 ──────────────────────────────────
cat > crates/foo/tests/tautology.rs <<'RS'
#[test]
fn header_matches_type() {
    assert_eq!(HEADER_LEN, std::mem::size_of::<Header>());
}
RS
git add -A && git commit -qm "add tautological fixture"
if bash scripts/check-fixture-independence.sh >/dev/null 2>&1; then
  bad "a fixture re-deriving size_of on an asserted value must be flagged (exit 1)"
else
  ok "tautological fixture (assert re-derives size_of) → exit 1"
fi

# ── 5. opt-out waiver on the same line → exit 0 ──────────────────────────────
cat > crates/foo/tests/tautology.rs <<'RS'
#[test]
fn abi_pin() {
    assert_eq!(HEADER_LEN, std::mem::size_of::<Header>()); // fixture-independence: allow — deliberate ABI conformance pin
}
RS
git add -A && git commit -qm "waive the intentional ABI pin"
if bash scripts/check-fixture-independence.sh >/dev/null 2>&1; then
  ok "opt-out waiver '// fixture-independence: allow' → exit 0"
else
  bad "the opt-out waiver must suppress the finding (exit 0)"
fi

echo "────────────────────────────────────────────"
echo "check-fixture-independence.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
