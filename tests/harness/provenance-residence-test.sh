#!/usr/bin/env bash
# provenance-residence-test.sh — ADR-052 §I9 × ADR-055 residence.
#
# The thing this pins
# -------------------
# `scripts/check-provenance.sh` reads the ledger with
# `git show "$head:.cosmon/state/events.jsonl"`. Whether that string is
# ever non-empty is not a property of the gate — it is a property of the
# galaxy's *residence*:
#
#   solo         `.cosmon/state/` tracked   → ledger in every tree → both
#                                             halves of the gate run.
#   team/remote  `.cosmon/state/` gitignored → narration lives on the
#                                             orphan branch `cosmon/state`
#                                             (or a server) → NO tree of
#                                             the working branch can ever
#                                             carry the ledger.
#
# cosmon-the-repo is a team residence. So on this repo the ledger half has
# never bound, not once, and a run that prints `checked=94 failed=0` is
# reporting 94 *subject-shape* verdicts. That is exactly the refusal
# ADR-052 §D5 assigns to CI — the ledger refusal is assigned to the
# pre-merge hook, which reads a working tree and therefore can enforce it.
# So the defect was never "the check is off"; it was that the gate said
# `skip` in the middle of 94 lines and then summed to a number a reader
# takes for ledger coverage.
#
# Scenarios:
#   1. Team residence — `.cosmon/state/` untracked. Gate exits 0, prints
#      the residence NOTE once, gives each merge a `shape` verdict, and
#      reports ledger_verified=0. No summary line can be read as ledger
#      coverage.
#   2. Tracked residence, ledger REMOVED — `.cosmon/state/` has tracked
#      paths but no events.jsonl. Gate FAILS. This is the case the old
#      code let through with a silent `skip` and exit 0: absence of a
#      ledger that this residence is supposed to keep under git is a
#      removed ledger, not an expected one.
#   3. Tracked residence, ledger present and complete — gate exits 0 and
#      reports ledger_verified>0. Regression guard: the honesty work must
#      not have cost the check its teeth where it does bind.
#
# Exit codes: 0 all passed | 1 a scenario failed | 2 harness setup error

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
GATE="$REPO/scripts/check-provenance.sh"

# Hermetic invocation: these scenarios assert the gate's WHOLE-HISTORY
# fallback inside a synthetic repo. On a pull_request event the runner's
# GITHUB_* would steer the gate into its PR scope (which resolves to
# nothing here) and every assertion would invert. Measured 2026-08-04 on
# noogram/cosmon#44 — the first external PR whose workflows ever ran.
run_gate() {
    env -u GITHUB_BASE_REF -u GITHUB_SHA -u GITHUB_EVENT_BEFORE \
        -u COSMON_PROVENANCE_HEAD bash "$GATE" "$@"
}

if [ ! -f "$GATE" ]; then
    echo "harness error: $GATE not found" >&2
    exit 2
fi

TMP="$(mktemp -d -t cosmon-prov-residence-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

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

MOL="task-20260729-dc53"

# Build a repo with one legitimately-shaped fold merge on main. `mode`
# decides what the tree says about the ledger:
#   untracked → .cosmon/state/ gitignored, nothing under it tracked
#   removed   → .cosmon/state/ has a tracked path, but no events.jsonl
#   present   → .cosmon/state/events.jsonl tracked, with a completion
build_repo() {
    local mode="$1"
    local work="$TMP/$mode"
    mkdir -p "$work" || return 2
    cd "$work" || return 2
    git init -q -b main
    git config user.email "harness@cosmon.test"
    git config user.name "cosmon harness"
    git config commit.gpgsign false

    mkdir -p .cosmon/state
    case "$mode" in
        untracked)
            printf '.cosmon/state/\n' > .gitignore
            ;;
        removed)
            # A tracked path under .cosmon/state/ — this residence keeps
            # narration under git — but the ledger itself is not there.
            mkdir -p .cosmon/state/fleets
            printf 'default\n' > .cosmon/state/fleets/ROSTER
            ;;
        present)
            printf '{"timestamp":"2026-07-29T00:00:00Z","kind":"molecule_completed","molecule_id":"%s","reason":"harness"}\n' \
                "$MOL" > .cosmon/state/events.jsonl
            ;;
    esac

    echo seed > seed.txt
    git add -A >/dev/null
    git commit -q -m "init"

    git checkout -q -b "feat/$MOL"
    echo work > work.txt
    git add work.txt >/dev/null
    git commit -q -m "evolve($MOL): step 1/1 — worker work"
    git checkout -q main
    git merge -q --no-ff --no-edit "feat/$MOL" >/dev/null 2>&1 || return 2
}

# ---------------------------------------------------------------------------
# Scenario 1 — team residence.
build_repo untracked || { echo "harness error: build untracked" >&2; exit 2; }
out=$(run_gate 2>&1) && rc=0 || rc=$?
verdict "1a. team residence: gate still passes on a well-shaped merge" 0 "$rc"

if printf '%s' "$out" | grep -q "team/remote residence"; then
    verdict "1b. team residence: the vacuity is announced once, up front" 0 0
else
    verdict "1b. team residence: the vacuity is announced once, up front" 0 1
    printf '%s\n' "$out" | sed 's/^/      /'
fi

if printf '%s' "$out" | grep -q "^shape .*($MOL)"; then
    verdict "1c. team residence: verdict reads 'shape', not 'skip'" 0 0
else
    verdict "1c. team residence: verdict reads 'shape', not 'skip'" 0 1
fi

if printf '%s' "$out" | grep -q "ledger_verified=0"; then
    verdict "1d. team residence: summary reports zero ledger coverage" 0 0
else
    verdict "1d. team residence: summary reports zero ledger coverage" 0 1
    printf '%s\n' "$out" | sed 's/^/      /'
fi

# ---------------------------------------------------------------------------
# Scenario 2 — the ledger was removed from a residence that tracks it.
# THIS is the assertion that fails against the pre-dc53 gate, which
# printed `skip … no ledger at scope tip` and exited 0.
build_repo removed || { echo "harness error: build removed" >&2; exit 2; }
out=$(run_gate 2>&1) && rc=0 || rc=$?
verdict "2a. tracked residence with the ledger removed: gate FAILS" 1 "$rc"

if printf '%s' "$out" | grep -q "ledger is not there"; then
    verdict "2b. removed ledger: failure names the removal, not a skip" 0 0
else
    verdict "2b. removed ledger: failure names the removal, not a skip" 0 1
    printf '%s\n' "$out" | sed 's/^/      /'
fi

# ---------------------------------------------------------------------------
# Scenario 3 — where the ledger DOES live under git, the check still bites.
build_repo present || { echo "harness error: build present" >&2; exit 2; }
out=$(run_gate 2>&1) && rc=0 || rc=$?
verdict "3a. tracked residence with a complete ledger: gate passes" 0 "$rc"

if printf '%s' "$out" | grep -q "ledger_verified=1"; then
    verdict "3b. tracked residence: the merge is counted as ledger-verified" 0 0
else
    verdict "3b. tracked residence: the merge is counted as ledger-verified" 0 1
    printf '%s\n' "$out" | sed 's/^/      /'
fi

cd "$REPO" || exit 2

echo
echo "provenance-residence-test: passed=$passed failed=$failed"
[ "$failed" -eq 0 ]
