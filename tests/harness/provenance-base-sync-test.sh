#!/usr/bin/env bash
# provenance-base-sync-test.sh — ADR-052 §I9, base-sync sub-case.
#
# The practice this covers
# ------------------------
# When main advances fast, a worker syncs its base *inside* the molecule's
# worktree before `cs done`:
#
#     git merge main          # subject: Merge branch 'main' into feat/task-…
#
# The resulting merge commit is interior to a tracked molecule: it is an
# ancestor of that molecule's own fold merge (`Merge branch 'feat/…'`),
# which is the commit the ledger knows about. The gate must recognise it
# without opening a hole for foreign material.
#
# What makes it safe to accept — the structural claim
# ---------------------------------------------------
# A base-sync merge contributes NOTHING new to main. Its incoming side
# (second parent) is a commit that already sits on main's own first-parent
# trunk, so every line it carries was already gated when it landed there.
# We verify that structurally, per commit — we do not merely trust the
# subject string. A merge that *claims* to be a base-sync but whose second
# parent is off-trunk is still rejected.
#
# Scenarios:
#   1. Legit base-sync (P2 on trunk, target branch names a molecule)
#      → accepted.
#   2. Forged base-sync — same subject, but P2 is a foreign off-trunk
#      branch → still rejected. This is the non-weakening assertion.
#   3. Base-sync into a branch that names no molecule → rejected.
#   4. Replay against this repo's real history: the eight base-sync merges
#      of 2026-07-19 that turned the gate red must all report `ok`.
#      Skipped (not failed) if those SHAs are unreachable.
#   5. The local commit-msg hook agrees with the CI mirror, at the moment
#      the base-sync actually happens: molecule still running, ledger
#      still empty of its completion.
#
# Exit codes: 0 all passed | 1 a scenario failed | 2 harness setup error

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
GATE="$REPO/scripts/check-provenance.sh"

if [ ! -f "$GATE" ]; then
    echo "harness error: $GATE not found" >&2
    exit 2
fi

passed=0
failed=0
skipped=0

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

# ---------------------------------------------------------------------------
# Synthetic repo. No hooks installed: we exercise the CI mirror directly,
# so the harness can build shapes the local hook would refuse.
# ---------------------------------------------------------------------------
TMP="$(mktemp -d -t cosmon-basesync-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

WORK="$TMP/repo"
mkdir -p "$WORK"
cd "$WORK" || exit 2

git init -q -b main
git config user.email "harness@cosmon.test"
git config user.name "cosmon harness"
git config commit.gpgsign false

echo seed > seed.txt
git add . >/dev/null
git commit -q -m "init"

mol_a="task-20260719-aaaa"
mol_b="task-20260719-bbbb"

# --- Scenario 1: legit base-sync, then the fold that lands it on main.
git checkout -q -b "feat/$mol_a"
echo a > a.txt && git add a.txt && git commit -q -m "evolve($mol_a): work"

git checkout -q main
echo trunk > trunk.txt && git add trunk.txt && git commit -q -m "chore: trunk moves"

git checkout -q "feat/$mol_a"
git merge -q --no-ff --no-edit -m "Merge branch 'main' into feat/$mol_a" main
basesync_ok=$(git rev-parse HEAD)

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_a'" "feat/$mol_a"

# --- Scenario 2: forged base-sync — subject lies, P2 is off-trunk.
git checkout -q -b foreign main~2
echo evil > evil.txt && git add evil.txt && git commit -q -m "unreviewed material"

git checkout -q -b "feat/$mol_b" main
echo b > b.txt && git add b.txt && git commit -q -m "evolve($mol_b): work"
git merge -q --no-ff --no-edit -m "Merge branch 'main' into feat/$mol_b" foreign
basesync_forged=$(git rev-parse HEAD)

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_b'" "feat/$mol_b"

# --- Scenario 3: base-sync into a branch naming no molecule.
git checkout -q -b feat/no-molecule main
echo c > c.txt && git add c.txt && git commit -q -m "some work"
git checkout -q main
echo trunk2 > trunk2.txt && git add trunk2.txt && git commit -q -m "chore: trunk moves again"
git checkout -q feat/no-molecule
git merge -q --no-ff --no-edit -m "Merge branch 'main' into feat/no-molecule" main
basesync_nameless=$(git rev-parse HEAD)

git checkout -q main
COSMON_SKIP_PROVENANCE=1 git merge -q --no-ff --no-edit \
    -m "Merge branch 'feat/no-molecule'" feat/no-molecule

# Run the mirror over the whole synthetic history.
out=$(env -u GITHUB_SHA -u GITHUB_BASE_REF -u GITHUB_EVENT_BEFORE \
    COSMON_PROVENANCE_SINCE="2020-01-01 00:00:00" bash "$GATE" 2>&1)

grep -q "^ok    $basesync_ok" <<<"$out" && r=0 || r=1
verdict "1. legit base-sync accepted" 0 "$r"

grep -q "^FAIL  $basesync_forged" <<<"$out" && r=0 || r=1
verdict "2. forged base-sync (off-trunk P2) still rejected" 0 "$r"

grep -q "^FAIL  $basesync_nameless" <<<"$out" && r=0 || r=1
verdict "3. base-sync into non-molecule branch rejected" 0 "$r"

if [ "$failed" -ne 0 ]; then
    printf '%s\n' "$out" | sed 's/^/      /'
fi

# ---------------------------------------------------------------------------
# Scenario 4 — replay the exact commits that turned the gate red.
# Recorded 2026-07-19; these are the eight base-sync merges reported as
# FAIL by the CI run following 82b274d.
# ---------------------------------------------------------------------------
RED_SHAS=(
    56f8253cc031d40268033def26c3054e01a384f1
    fbf9cb72696a1033de3011c43fe0bb0bfb559ec1
    c72a230db544f363d3ec92ee9cf2a9866f5e8435
    69a069e93d42f79221239785c8029fa68da70ebe
    403533b293e9c1f56c88b3d6d7e078ae33bec875
    8e3e847e8ef123778cef9c991f265a86ff5c6c46
    20fd837ac1067fcff4b5edb6462c8488fcae1583
    3a0d7d6dbcf5e70ef173293d394f53fe6fc363dc
)

cd "$REPO" || exit 2
missing=0
for sha in "${RED_SHAS[@]}"; do
    git cat-file -e "$sha^{commit}" 2>/dev/null || missing=1
done

if [ "$missing" -eq 1 ]; then
    echo "SKIP  4. replay of the 2026-07-19 red SHAs (not reachable here)"
    skipped=$((skipped + 1))
else
    # Scope: walk HEAD's own history. GITHUB_* must be stripped — under
    # CI they would narrow the scope to the pushed range and silently
    # exclude the very commits this scenario exists to replay, turning a
    # vacuous pass (or a false red) into the reported result.
    real_all=$(env -u GITHUB_SHA -u GITHUB_BASE_REF -u GITHUB_EVENT_BEFORE \
        COSMON_PROVENANCE_SINCE="2020-01-01 00:00:00" \
        bash "$GATE" 2>&1 || true)
    for sha in "${RED_SHAS[@]}"; do
        if grep -q "^ok    $sha" <<<"$real_all"; then
            verdict "4. replay ${sha:0:7} accepted" 0 0
        else
            verdict "4. replay ${sha:0:7} accepted" 0 1
        fi
    done
fi

# ---------------------------------------------------------------------------
# Scenario 5 — the local hook must agree with the CI mirror. A base-sync
# is performed for real, at the moment it actually happens: molecule still
# running, no completion in the ledger. The hook has to accept it on
# structural evidence alone, and still refuse the forged shape.
# ---------------------------------------------------------------------------
HOOK="$REPO/.cosmon/hooks/commit-msg"
if [ ! -f "$HOOK" ]; then
    echo "SKIP  5. local hook agreement (hook not found)"
    skipped=$((skipped + 1))
else
    HWORK="$TMP/hookrepo"
    mkdir -p "$HWORK"
    cd "$HWORK" || exit 2

    git init -q -b main
    git config user.email "harness@cosmon.test"
    git config user.name "cosmon harness"
    git config commit.gpgsign false

    mkdir -p .cosmon/hooks .cosmon/state .git/hooks
    cp "$HOOK" .cosmon/hooks/commit-msg
    chmod +x .cosmon/hooks/commit-msg
    ln -sf "$HWORK/.cosmon/hooks/commit-msg" .git/hooks/commit-msg
    : > .cosmon/state/events.jsonl

    echo seed > seed.txt
    git add . >/dev/null
    git commit -q -m "init"

    mol_c="task-20260719-cccc"
    git checkout -q -b "feat/$mol_c"
    echo x > x.txt && git add x.txt && git commit -q -m "evolve($mol_c): work"

    git checkout -q main
    echo t > t.txt && git add t.txt && git commit -q -m "chore: trunk moves"

    # 5a — legit base-sync while the molecule is still running.
    git checkout -q "feat/$mol_c"
    git merge --no-ff --no-edit -m "Merge branch 'main' into feat/$mol_c" main \
        >/dev/null 2>&1 && rc=0 || rc=$?
    verdict "5a. hook accepts base-sync of a running molecule" 0 "$rc"

    # 5b — forged base-sync: same subject, off-trunk incoming side.
    git checkout -q -b rogue main~1
    echo evil > evil.txt && git add evil.txt && git commit -q -m "ungated material"
    git checkout -q "feat/$mol_c"
    git merge --no-ff --no-edit -m "Merge branch 'main' into feat/$mol_c" rogue \
        >/dev/null 2>&1 && rc=0 || rc=$?
    git merge --abort 2>/dev/null || true
    verdict "5b. hook rejects forged base-sync (off-trunk incoming)" 1 "$rc"

    cd "$REPO" || exit 2
fi

echo
echo "provenance-base-sync-test: passed=$passed failed=$failed skipped=$skipped"
[ "$failed" -eq 0 ]
