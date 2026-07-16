#!/usr/bin/env bash
# check-docs-one-gate.test.sh — regression tests for the "one gate" docs-lint
# (scripts/check-docs-one-gate.sh).
#
# Runs against a THROWAWAY git repo in a tmpdir so the assertions are hermetic —
# never against the live cosmon tree. Exercises:
#   1. the built-in --self-test (pattern falsifiability);
#   2. a CLEAN doc surface (cosmon-only, neurion in prose) → exit 0;
#   3. a confidential tool as a SUMMARY.md nav title → exit 1 (GATE A1);
#   4. a confidential tool as a section HEADING → exit 1 (GATE A2);
#   5. `neurion-core` organ crate in prose → exit 0 (permitted spelling);
#   6. a premature-tool install endpoint (/topon/install.sh) → exit 1 (GATE B);
#   7. the installable-tool endpoint (/cosmon/install.sh) → exit 0.
#
# Usage: ./scripts/check-docs-one-gate.test.sh
# Exit: 0 on pass, non-zero on failure.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="$HERE/check-docs-one-gate.sh"

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
mkdir -p "$REPO/scripts" "$REPO/docs/book/src/explanation"
cp "$GATE" "$REPO/scripts/check-docs-one-gate.sh"
cd "$REPO" || exit 2
git init -q
git config user.email "test@example.com"
git config user.name "test"

# a CLEAN doc surface: cosmon is the kernel; neurion only in prose + as the
# name-scrubbed organ crate; the installable endpoint names cosmon.
cat > docs/book/src/SUMMARY.md <<'MD'
# Summary

[Introduction](./introduction.md)

# Reference

- [CLI overview](./reference/overview.md)
- [Observability commands](./reference/observability.md)
MD
cat > docs/book/src/introduction.md <<'MD'
# Noogram

cosmon is the kernel. Install it with
`curl -fsSL https://noogram.org/cosmon/install.sh | sh`.

Cosmon integrates with the neurion registry as an external integration point;
the neurion-core organ crate is vendored inside the workspace.
MD
git add -A && git commit -qm "clean doc surface"

# ── 2. clean surface → exit 0 ────────────────────────────────────────────────
if bash scripts/check-docs-one-gate.sh >/dev/null 2>&1; then
  ok "clean doc surface (cosmon endpoint, neurion in prose/organ) → exit 0"
else
  bad "clean surface should pass (exit 0)"
  bash scripts/check-docs-one-gate.sh >&2 || true
fi

# ── 5. neurion-core organ crate in prose is already committed above → still 0.
ok "neurion-core organ-crate prose does not trip the gate (covered by test 2)"

# ── 3. confidential tool as a SUMMARY.md nav title → exit 1 (GATE A1) ─────────
cp docs/book/src/SUMMARY.md "$WORK/SUMMARY.bak"
cat >> docs/book/src/SUMMARY.md <<'MD'
- [The neurion product](./neurion.md)
MD
git add -A && git commit -qm "leak: neurion nav title"
if bash scripts/check-docs-one-gate.sh >/dev/null 2>&1; then
  bad "neurion as a nav title must FAIL (GATE A1)"
else
  ok "neurion as a SUMMARY.md nav title → exit 1 (GATE A1)"
fi
cp "$WORK/SUMMARY.bak" docs/book/src/SUMMARY.md
git add -A && git commit -qm "revert nav leak"

# ── 4. confidential tool as a section HEADING → exit 1 (GATE A2) ──────────────
cat > docs/book/src/explanation/neurion.md <<'MD'
# Neurion

The neurion product maps operator infrastructure.
MD
git add -A && git commit -qm "leak: neurion heading"
if bash scripts/check-docs-one-gate.sh >/dev/null 2>&1; then
  bad "neurion as a section heading must FAIL (GATE A2)"
else
  ok "neurion as a section heading → exit 1 (GATE A2)"
fi
git rm -q docs/book/src/explanation/neurion.md
git commit -qm "revert heading leak"

# ── 6. premature-tool install endpoint → exit 1 (GATE B) ─────────────────────
mkdir -p docs/book/src/explanation  # git rm of test 4 may have pruned the dir
cat > docs/book/src/explanation/topon.md <<'MD'
# Structural maps

Install with `curl -fsSL https://noogram.org/topon/install.sh | sh`.
MD
git add -A && git commit -qm "leak: topon install endpoint"
if bash scripts/check-docs-one-gate.sh >/dev/null 2>&1; then
  bad "/topon/install.sh (premature tool) must FAIL (GATE B)"
else
  ok "/topon/install.sh (premature tool) → exit 1 (GATE B)"
fi
git rm -q docs/book/src/explanation/topon.md
git commit -qm "revert topon leak"

# ── 7. installable-tool endpoint stays clean → exit 0 ────────────────────────
if bash scripts/check-docs-one-gate.sh >/dev/null 2>&1; then
  ok "/cosmon/install.sh (installable tool) → exit 0 (GATE B allows)"
else
  bad "the installable-tool endpoint must pass"
fi

echo
echo "check-docs-one-gate.test.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
