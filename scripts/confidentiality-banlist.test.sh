#!/usr/bin/env bash
# confidentiality-banlist.test.sh — regression tests for the operator/fund
# identity tripwire (scripts/confidentiality-banlist.sh).
#
# Runs against a THROWAWAY git repo in a tmpdir so the assertions are
# hermetic — never against the live cosmon tree. Exercises three things:
#   1. the built-in --self-test (pattern falsifiability);
#   2. the default publishable-surface gate: clean tree → exit 0, a re-leak
#      seeded into docs/book/src/ → exit 1;
#   3. the --whole-repo advisory mode: an INTERNAL doc naming the operator
#      → exit 1, while the operator author homeserver email alone → exit 0
#      (oxymake golden rule keep).
#
# Usage: ./scripts/confidentiality-banlist.test.sh
# Exit: 0 on pass, non-zero on failure.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="$HERE/confidentiality-banlist.sh"

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

pass=0 fail=0
ok()   { echo "PASS: $*"; pass=$((pass+1)); }
bad()  { echo "FAIL: $*" >&2; fail=$((fail+1)); }

# ── 1. built-in self-test ────────────────────────────────────────────────────
if bash "$GATE" --self-test >/dev/null 2>&1; then ok "--self-test passes"
else bad "--self-test should pass"; fi

# ── build a throwaway repo mirroring the real script's copy at scripts/ ───────
REPO="$WORK/repo"
mkdir -p "$REPO/scripts" "$REPO/docs/book/src" "$REPO/docs/adr"
cp "$GATE" "$REPO/scripts/confidentiality-banlist.sh"
cp "$HERE/confidentiality-banlist.test.sh" "$REPO/scripts/confidentiality-banlist.test.sh"
cd "$REPO"
git init -q
git config user.email "test@example.com"
git config user.name "Test"

seed()   { mkdir -p "$(dirname "$1")"; printf '%s\n' "$2" > "$1"; git add -A >/dev/null 2>&1; }
unseed() { rm -f "$1"; git add -A >/dev/null 2>&1; }   # stage the deletion

run() { bash scripts/confidentiality-banlist.sh "$@" >/dev/null 2>&1; echo $?; }

# ── 2. default gate: clean publishable surface → 0 ───────────────────────────
seed docs/book/src/intro.md '# Cosmon by The Noogram authors, noogram.dev'
seed README.md '# Cosmon — a stateless CLI'
[ "$(run)" = "0" ] && ok "clean publishable surface → exit 0" \
  || bad "clean publishable surface should exit 0"

# ── 2b. re-leak into the book surface → 1 ────────────────────────────────────
seed docs/book/src/leak.md 'Written by Noogram.'
[ "$(run)" = "1" ] && ok "operator name in docs/book/src → exit 1" \
  || bad "operator name on public surface should exit 1"
unseed docs/book/src/leak.md

# ── 2c. fund name on public surface → 1 ──────────────────────────────────────
# Accented spelling: exercises the `[ÉE]pinoia` branch without inlining the
# ascii confidential literal into this committed test (ADR-127 §6).
seed README.md '# Cosmon — a project of Épinoia Research'
[ "$(run)" = "1" ] && ok "fund name in README → exit 1" \
  || bad "fund name on public surface should exit 1"
seed README.md '# Cosmon — a stateless CLI'   # restore clean

# ── 3. whole-repo advisory: internal doc naming operator → 1 ─────────────────
seed docs/adr/001-example.md 'Decision recorded by Noogram.'
[ "$(run --whole-repo)" = "1" ] && ok "internal doc with operator name → --whole-repo exit 1" \
  || bad "--whole-repo should flag the internal doc"
[ "$(run)" = "0" ] && ok "same internal doc does NOT trip the publishable gate" \
  || bad "internal-only leak must not fail the default gate"
unseed docs/adr/001-example.md

# ── 3b. author homeserver email alone is an intentional keep → whole-repo 0 ───
# The homeserver domain is assembled at runtime so this committed test does not
# inline the confidential literal (ADR-127 §6 — the test must not re-leak).
sd="serie"".dev"                       # homeserver domain, assembled at runtime
seed docs/adr/002-authors.md "Co-authored by someone <someone@${sd}>."
[ "$(run --whole-repo)" = "0" ] && ok "author homeserver email is kept (whole-repo exit 0)" \
  || bad "author homeserver email should be an intentional keep"

echo
echo "confidentiality-banlist.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
