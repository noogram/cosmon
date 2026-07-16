#!/usr/bin/env bash
# provenance-gate-test.sh — reproduces the c1cb pathology against the
# commit-msg hook and verifies it is rejected, then verifies a
# legitimate merge with a recorded completion is accepted.
#
# Layout per scenario:
#   $TMP/repo/                  bare git repo + .cosmon/state/events.jsonl
#       feat/task-…             worker branch with commits
#       main                    target of `git merge`
#   .git/hooks/commit-msg symlink to .cosmon/hooks/commit-msg
#
# Scenarios (each must verify the expected outcome):
#   1. c1cb pathology — events.jsonl has NO MoleculeCompleted for mol_id
#      → `git merge --no-ff` exits non-zero, hook stderr cites ADR-052 §I9.
#   2. Legitimate flow — append MoleculeCompleted line, retry → exits 0.
#   3. Bad subject — `git merge --no-ff -m 'fix: yolo'` → rejected.
#   4. Bypass — same merge with COSMON_SKIP_PROVENANCE=1 → accepted.
#   5. CI mirror — scripts/check-provenance.sh on the merged commit ladder
#      reports the failing case AND the passing case correctly.
#
# Exit codes:
#   0  every scenario passed
#   1  at least one scenario failed
#   2  harness setup error

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"

if [ ! -f "$REPO/.cosmon/hooks/commit-msg" ]; then
    echo "harness error: $REPO/.cosmon/hooks/commit-msg not found" >&2
    exit 2
fi

TMP="$(mktemp -d -t cosmon-provenance-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

WORK="$TMP/repo"
mkdir -p "$WORK"
cd "$WORK"

git init -q -b main
git config user.email "harness@cosmon.test"
git config user.name "cosmon harness"

mkdir -p .cosmon/hooks .cosmon/state
cp "$REPO/.cosmon/hooks/commit-msg" .cosmon/hooks/commit-msg
chmod +x .cosmon/hooks/commit-msg
mkdir -p .git/hooks
ln -sf "$WORK/.cosmon/hooks/commit-msg" .git/hooks/commit-msg

: > .cosmon/state/events.jsonl
echo "seed" > seed.txt
git add . >/dev/null
git commit -q -m "init"

# ---------------------------------------------------------------------------
mol_id="task-20260419-f357"
branch="feat/$mol_id"

git checkout -q -b "$branch"
echo "worker output" > work.txt
git add work.txt >/dev/null
git commit -q -m "evolve($mol_id): step 1/1 — worker work"
git checkout -q main

passed=0
failed=0

verdict() {
    local name="$1" expected="$2" got="$3"
    if [ "$expected" = "$got" ]; then
        echo "PASS  $name"
        passed=$((passed + 1))
    else
        echo "FAIL  $name (expected $expected, got $got)"
        failed=$((failed + 1))
    fi
}

# Scenario 1 — c1cb pathology: ledger has no completion for this mol_id.
out=$(git merge --no-ff --no-edit "$branch" 2>&1) && rc=0 || rc=$?
git merge --abort 2>/dev/null || true
verdict "1. c1cb pathology rejected" 1 "$rc"
if ! printf '%s' "$out" | grep -q "ADR-052"; then
    echo "      WARNING: rejection message did not cite ADR-052"
fi

# Scenario 2 — legitimate flow: record MoleculeCompleted, retry.
printf '{"timestamp":"2026-04-19T00:00:00Z","kind":"molecule_completed","molecule_id":"%s","reason":"harness"}\n' \
    "$mol_id" >> .cosmon/state/events.jsonl
git add .cosmon/state/events.jsonl >/dev/null
git commit -q -m "chore: harness records completion for $mol_id"
git merge --no-ff --no-edit "$branch" >/dev/null 2>&1 && rc=0 || rc=$?
verdict "2. legitimate merge accepted" 0 "$rc"
legitimate_merge_sha=$(git rev-parse HEAD)

# Scenario 3 — bad subject (no mol_id at all). Use a separate branch.
git checkout -q -b feat/freeform
echo "freeform" > free.txt
git add free.txt >/dev/null
git commit -q -m "freeform commit"
git checkout -q main
out=$(git merge --no-ff --no-edit -m "fix: yolo no-mol" feat/freeform 2>&1) && rc=0 || rc=$?
git merge --abort 2>/dev/null || true
verdict "3. bad-subject merge rejected" 1 "$rc"

# Scenario 4 — bypass via COSMON_SKIP_PROVENANCE=1.
COSMON_SKIP_PROVENANCE=1 \
    git merge --no-ff --no-edit -m "fix: yolo bypass" feat/freeform >/dev/null 2>&1 && rc=0 || rc=$?
verdict "4. bypass accepted with COSMON_SKIP_PROVENANCE=1" 0 "$rc"

# Scenario 5 — CI mirror: run scripts/check-provenance.sh against the
# whole history. The bypass commit (#4) has no mol_id and no bypass at
# replay time, so the CI mirror should flag it.
out=$(bash "$REPO/scripts/check-provenance.sh" 2>&1) && rc=0 || rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q "$legitimate_merge_sha"; then
    if printf '%s' "$out" | grep -q "ok    $legitimate_merge_sha"; then
        verdict "5a. CI mirror accepts legitimate merge" 0 0
    else
        verdict "5a. CI mirror accepts legitimate merge" 0 1
        printf '%s\n' "$out" | sed 's/^/      /'
    fi
    if printf '%s' "$out" | grep -q "FAIL  "; then
        verdict "5b. CI mirror flags bad-subject merge" 0 0
    else
        verdict "5b. CI mirror flags bad-subject merge" 0 1
    fi
else
    verdict "5. CI mirror exit code (expected non-zero with mixed history)" 1 "$rc"
fi

echo
echo "provenance-gate-test: passed=$passed failed=$failed"
[ "$failed" -eq 0 ]
