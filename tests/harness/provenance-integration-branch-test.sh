#!/usr/bin/env bash
# provenance-integration-branch-test.sh — ADR-052 §D7 (2026-09-05).
#
# The practice this covers
# ------------------------
# Work answering an external GitHub issue lands on a local integration
# branch `feat/issue-<N>` — molecules tackle with `--base feat/issue-<N>`,
# `cs done` merges into it — and reaches main through a pull request.
# GitHub writes that merge commit's subject itself:
#
#     Merge pull request #<N> from <owner>/feat/issue-<N>
#
# or, when the same landing happens without GitHub's wrapper, a plain
#
#     Merge branch 'feat/issue-<N>'
#
# Either is accepted by `scripts/check-provenance.sh` iff EVERY merge
# reachable on the landing commit's second parent since the merge-base
# with its first parent is itself provenance-clean — an existing
# molecule-merge pattern, a base-sync, or (recursively) another
# integration-branch landing. This is the same §I9 property the gate
# has always enforced, one level of PR-wrapping removed: no code
# reaches main except through a molecule's `cs done`.
#
# Scenarios:
#   1. Clean integration branch (one molecule merge + a base-sync from
#      main) landed via the PR-shaped subject → accepted.
#   2. Same shape, but the integration branch also carries one plain,
#      non-provenance merge → rejected, naming that inner commit.
#   3. The same landing without GitHub's wrapper (a local
#      `Merge branch 'feat/issue-<N>'`) → accepted.
#   4. Stacked integration branches: issue-11's branch merges into
#      issue-10's branch before issue-11's own PR merges; issue-11's
#      branch is itself clean → the two-level recursion accepts the
#      final PR-shaped landing of issue-10.
#   5. PR-shaped subject where the PR number and the issue number in
#      the branch name disagree → still recognised (2026-09-10): the
#      issue number is read from the branch name, and a clean second
#      parent is accepted.
#   6. A bare base-sync onto `feat/issue-<N>` (`Merge branch 'main'
#      into feat/issue-<N>`), scanned directly (not nested inside a PR
#      landing) → accepted, mirroring the existing `feat/<mol_id>`
#      base-sync shape.
#   7. Old shapes unaffected: `feat/<mol_id>` merges, evolve/done/
#      auto-merge subjects, and mol_id-targeted base-syncs still
#      PASS/FAIL exactly as before (delegated to the other three
#      provenance harnesses, which this script re-runs and folds into
#      its own verdict so one green run covers the whole gate).
#   8. Suffixed integration branches (2026-09-09): one external issue
#      answered by several chained PRs needs several branch names, so a
#      branch may read `feat/issue-<N>-<slug>`. A stacked merge between
#      two suffixed branches is accepted exactly like a bare one — the
#      slug names the PR, not the provenance.
#   9. The slug buys no leniency: a suffixed integration branch whose
#      second parent carries a plain, non-provenance merge is still
#      rejected, naming that inner commit.
#  10. A suffixed branch whose PR number disagrees with its issue number
#      is likewise recognised and accepted when clean (2026-09-10).
#  11. The real 2026-09-09/10 shape: `Merge pull request #64 from
#      owner/feat/issue-51-status`, one issue answered by three chained
#      PRs, over a clean second parent → accepted.
#  12. The complementary case: the same subject over a second parent
#      carrying a non-provenance merge → still rejected, naming it.
#  13. Nothing else widens: a PR-shaped subject whose branch is not
#      `feat/issue-<N>(-<slug>)?` still falls through to the ordinary
#      "subject does not match" rejection.
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

run_gate() {
    env -u GITHUB_SHA -u GITHUB_BASE_REF -u GITHUB_EVENT_BEFORE -u COSMON_PROVENANCE_HEAD \
        COSMON_PROVENANCE_SINCE="2020-01-01 00:00:00" bash "$GATE" 2>&1
}

# ---------------------------------------------------------------------------
# Synthetic repo. No hooks installed: we exercise the CI mirror directly,
# so the harness can build shapes the local commit-msg hook never sees
# (it only runs at commit time, on one branch at a time).
# ---------------------------------------------------------------------------
TMP="$(mktemp -d -t cosmon-provenance-issue-XXXXXX)"
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

# --- Scenario 1: clean integration branch, PR-shaped landing.
mol_1="task-20260905-a001"
git checkout -q -b feat/issue-7
git checkout -q -b "feat/$mol_1"
echo w1 > w1.txt && git add w1.txt && git commit -q -m "evolve($mol_1): work"
git checkout -q feat/issue-7
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_1'" "feat/$mol_1"

git checkout -q main
echo trunk1 > trunk1.txt && git add trunk1.txt && git commit -q -m "chore: trunk moves"
git checkout -q feat/issue-7
git merge -q --no-ff --no-edit -m "Merge branch 'main' into feat/issue-7" main

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge pull request #7 from noogram/feat/issue-7" feat/issue-7
pr_clean=$(git rev-parse HEAD)

# --- Scenario 2: same shape, but with one non-provenance inner merge.
mol_2="task-20260905-a002"
git checkout -q -b feat/issue-8 main
git checkout -q -b "feat/$mol_2"
echo w2 > w2.txt && git add w2.txt && git commit -q -m "evolve($mol_2): work"
git checkout -q feat/issue-8
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_2'" "feat/$mol_2"

git checkout -q -b rogue-8 main
echo r8 > r8.txt && git add r8.txt && git commit -q -m "unreviewed material"
git checkout -q feat/issue-8
git merge -q --no-ff --no-edit -m "fix: sneak it in" rogue-8
bad_inner=$(git rev-parse HEAD)

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge pull request #8 from noogram/feat/issue-8" feat/issue-8
pr_dirty=$(git rev-parse HEAD)

# --- Scenario 3: same landing, no GitHub wrapper.
mol_3="task-20260905-a003"
git checkout -q -b feat/issue-9 main
git checkout -q -b "feat/$mol_3"
echo w3 > w3.txt && git add w3.txt && git commit -q -m "evolve($mol_3): work"
git checkout -q feat/issue-9
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_3'" "feat/$mol_3"

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge branch 'feat/issue-9'" feat/issue-9
local_clean=$(git rev-parse HEAD)

# --- Scenario 4: stacked integration branches, both clean.
mol_4a="task-20260905-a004"
mol_4b="task-20260905-a005"
git checkout -q -b feat/issue-11 main
git checkout -q -b "feat/$mol_4b"
echo w4b > w4b.txt && git add w4b.txt && git commit -q -m "evolve($mol_4b): work"
git checkout -q feat/issue-11
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_4b'" "feat/$mol_4b"

git checkout -q -b feat/issue-10 main
git checkout -q -b "feat/$mol_4a"
echo w4a > w4a.txt && git add w4a.txt && git commit -q -m "evolve($mol_4a): work"
git checkout -q feat/issue-10
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_4a'" "feat/$mol_4a"
git merge -q --no-ff --no-edit -m "Merge branch 'feat/issue-11' into feat/issue-10" feat/issue-11

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge pull request #10 from noogram/feat/issue-10" feat/issue-10
pr_stacked=$(git rev-parse HEAD)

# --- Scenario 5: PR number and issue number disagree, branch is clean.
mol_5="task-20260905-a006"
git checkout -q -b feat/issue-12 main
echo w5 > w5.txt && git add w5.txt && git commit -q -m "evolve($mol_5): work"
git checkout -q main
git merge -q --no-ff --no-edit -m "Merge pull request #99 from noogram/feat/issue-12" feat/issue-12
pr_mismatch=$(git rev-parse HEAD)

# --- Scenario 6: a bare base-sync onto feat/issue-<N>, scanned directly
#     (not nested inside a PR landing — it stays on its own branch).
git checkout -q -b feat/issue-13 main
echo w6 > w6.txt && git add w6.txt && git commit -q -m "evolve(task-20260905-a007): work"
git checkout -q main
echo trunk6 > trunk6.txt && git add trunk6.txt && git commit -q -m "chore: trunk moves again"
git checkout -q feat/issue-13
git merge -q --no-ff --no-edit -m "Merge branch 'main' into feat/issue-13" main
basesync_issue=$(git rev-parse HEAD)
# The fallback scope is `git log --merges --since ... HEAD` — only commits
# reachable from main's tip are walked, so land the branch with a plain
# molecule-shaped merge to bring the base-sync commit into scope.
git checkout -q main
git merge -q --no-ff --no-edit -m "evolve(task-20260905-a007)" feat/issue-13

# --- Scenario 8: stacked merge between two SUFFIXED integration branches.
#     Issue 20 is answered by two chained PRs; each gets its own branch
#     name, both naming issue 20's sibling issue numbers in the same shape.
mol_8a="task-20260909-a008"
mol_8b="task-20260909-a009"
git checkout -q -b feat/issue-21-session main
git checkout -q -b "feat/$mol_8b"
echo w8b > w8b.txt && git add w8b.txt && git commit -q -m "evolve($mol_8b): work"
git checkout -q feat/issue-21-session
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_8b'" "feat/$mol_8b"

git checkout -q -b feat/issue-20-done main
git checkout -q -b "feat/$mol_8a"
echo w8a > w8a.txt && git add w8a.txt && git commit -q -m "evolve($mol_8a): work"
git checkout -q feat/issue-20-done
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_8a'" "feat/$mol_8a"
git merge -q --no-ff --no-edit \
    -m "Merge branch 'feat/issue-21-session' into feat/issue-20-done" feat/issue-21-session
stacked_suffixed=$(git rev-parse HEAD)

git checkout -q main
git merge -q --no-ff --no-edit \
    -m "Merge pull request #20 from noogram/feat/issue-20-done" feat/issue-20-done
pr_suffixed=$(git rev-parse HEAD)

# --- Scenario 9: suffixed branch carrying a non-clean merge, still rejected.
mol_9="task-20260909-a010"
git checkout -q -b feat/issue-22-status main
git checkout -q -b "feat/$mol_9"
echo w9 > w9.txt && git add w9.txt && git commit -q -m "evolve($mol_9): work"
git checkout -q feat/issue-22-status
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_9'" "feat/$mol_9"

git checkout -q -b rogue-22 main
echo r22 > r22.txt && git add r22.txt && git commit -q -m "unreviewed material"
git checkout -q feat/issue-22-status
git merge -q --no-ff --no-edit -m "fix: sneak it in behind a slug" rogue-22
bad_inner_suffixed=$(git rev-parse HEAD)

git checkout -q main
git merge -q --no-ff --no-edit -m "Merge branch 'feat/issue-22-status'" feat/issue-22-status
local_suffixed_dirty=$(git rev-parse HEAD)

# --- Scenario 10: suffixed branch, PR number disagrees with issue number.
#     Recognised the same way since 2026-09-10; the branch is clean.
git checkout -q -b feat/issue-23-review main
echo w10 > w10.txt && git add w10.txt && git commit -q -m "evolve(task-20260909-a011): work"
git checkout -q main
git merge -q --no-ff --no-edit \
    -m "Merge pull request #98 from noogram/feat/issue-23-review" feat/issue-23-review
pr_suffixed_mismatch=$(git rev-parse HEAD)

# --- Scenario 11: the real 2026-09-09/10 shape. Issue #51 was answered by
#     PRs #62/#63/#64 on branches `feat/issue-51-done`, `-session`,
#     `-status`; #64's landing subject names a PR number the issue never
#     had. The branch is clean, so it lands.
mol_11="task-20260910-a012"
git checkout -q -b feat/issue-51-status main
git checkout -q -b "feat/$mol_11"
echo w11 > w11.txt && git add w11.txt && git commit -q -m "evolve($mol_11): work"
git checkout -q feat/issue-51-status
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_11'" "feat/$mol_11"

git checkout -q main
git merge -q --no-ff --no-edit \
    -m "Merge pull request #64 from owner/feat/issue-51-status" feat/issue-51-status
pr_renumbered=$(git rev-parse HEAD)

# --- Scenario 12: same subject shape, second parent not provenance-clean.
mol_12="task-20260910-a013"
git checkout -q -b feat/issue-52-status main
git checkout -q -b "feat/$mol_12"
echo w12 > w12.txt && git add w12.txt && git commit -q -m "evolve($mol_12): work"
git checkout -q feat/issue-52-status
git merge -q --no-ff --no-edit -m "Merge branch 'feat/$mol_12'" "feat/$mol_12"

git checkout -q -b rogue-52 main
echo r52 > r52.txt && git add r52.txt && git commit -q -m "unreviewed material"
git checkout -q feat/issue-52-status
git merge -q --no-ff --no-edit -m "fix: sneak it in behind a renumbered PR" rogue-52
bad_inner_renumbered=$(git rev-parse HEAD)

git checkout -q main
git merge -q --no-ff --no-edit \
    -m "Merge pull request #65 from owner/feat/issue-52-status" feat/issue-52-status
pr_renumbered_dirty=$(git rev-parse HEAD)

# --- Scenario 13: PR-shaped subject whose branch is not an issue branch.
git checkout -q -b not-an-issue-branch main
echo w13 > w13.txt && git add w13.txt && git commit -q -m "evolve(task-20260910-a014): work"
git checkout -q main
git merge -q --no-ff --no-edit \
    -m "Merge pull request #64 from owner/not-an-issue-branch" not-an-issue-branch
pr_bad_branch=$(git rev-parse HEAD)

out=$(run_gate)

grep -q "^ok    $pr_clean" <<<"$out" && r=0 || r=1
verdict "1. clean integration branch via PR-shaped subject accepted" 0 "$r"

grep -q "^FAIL  $pr_dirty" <<<"$out" && r=0 || r=1
verdict "2a. integration branch with a bad inner merge rejected" 0 "$r"

grep -A3 "^FAIL  $pr_dirty" <<<"$out" | grep -q "$bad_inner" && r=0 || r=1
verdict "2b. rejection names the offending inner commit" 0 "$r"

grep -q "^ok    $local_clean" <<<"$out" && r=0 || r=1
verdict "3. clean landing without GitHub's PR wrapper accepted" 0 "$r"

grep -q "^ok    $pr_stacked" <<<"$out" && r=0 || r=1
verdict "4. stacked integration branches (two-level recursion) accepted" 0 "$r"

grep -q "^ok    $pr_mismatch" <<<"$out" && r=0 || r=1
verdict "5. PR-number/issue-number mismatch still recognised, clean branch accepted" 0 "$r"
grep -q "^ok    $pr_mismatch  (issue-12)" <<<"$out" && r=0 || r=1
verdict "5b. verdict keys on the branch's <N>, not the PR number" 0 "$r"

grep -q "^ok    $basesync_issue" <<<"$out" && r=0 || r=1
verdict "6. base-sync onto feat/issue-<N> accepted" 0 "$r"

grep -q "^ok    $pr_suffixed" <<<"$out" && r=0 || r=1
verdict "8a. suffixed integration branch landed via PR accepted" 0 "$r"
grep -q "^ok    $stacked_suffixed" <<<"$out" && r=0 || r=1
verdict "8b. stacked merge between two suffixed branches accepted" 0 "$r"
grep -q "^ok    $stacked_suffixed  (issue-20)" <<<"$out" && r=0 || r=1
verdict "8c. stacked verdict keys on the target's <N>, not the slug" 0 "$r"

grep -q "^FAIL  $local_suffixed_dirty" <<<"$out" && r=0 || r=1
verdict "9a. suffixed branch with a bad inner merge still rejected" 0 "$r"
grep -A3 "^FAIL  $local_suffixed_dirty" <<<"$out" | grep -q "$bad_inner_suffixed" && r=0 || r=1
verdict "9b. rejection names the offending inner commit" 0 "$r"

grep -q "^ok    $pr_suffixed_mismatch" <<<"$out" && r=0 || r=1
verdict "10a. suffixed branch with a renumbered PR accepted when clean" 0 "$r"
grep -q "^ok    $pr_suffixed_mismatch  (issue-23)" <<<"$out" && r=0 || r=1
verdict "10b. verdict keys on the branch's <N>, not the PR number" 0 "$r"

grep -q "^ok    $pr_renumbered" <<<"$out" && r=0 || r=1
verdict "11. #64 from feat/issue-51-status over a clean second parent accepted" 0 "$r"

grep -q "^FAIL  $pr_renumbered_dirty" <<<"$out" && r=0 || r=1
verdict "12a. same shape over a non-clean second parent still rejected" 0 "$r"
grep -A3 "^FAIL  $pr_renumbered_dirty" <<<"$out" | grep -q "$bad_inner_renumbered" && r=0 || r=1
verdict "12b. rejection names the offending inner commit" 0 "$r"

grep -q "^FAIL  $pr_bad_branch" <<<"$out" && r=0 || r=1
verdict "13a. PR-shaped subject with a non-issue branch still refused" 0 "$r"
grep -A3 "^FAIL  $pr_bad_branch" <<<"$out" | grep -q "subject does not match" && r=0 || r=1
verdict "13b. refused as an ordinary bad subject, not an integration shape" 0 "$r"

if [ "$failed" -ne 0 ]; then
    printf '%s\n' "$out" | sed 's/^/      /'
fi

# ---------------------------------------------------------------------------
# Scenario 7 — old shapes unaffected. Re-run the three existing provenance
# harnesses from inside this one so a single green run of this file also
# certifies that the amendment did not regress the shapes it did not touch.
# ---------------------------------------------------------------------------
cd "$REPO" || exit 2
for other in provenance-gate-test.sh provenance-base-sync-test.sh provenance-residence-test.sh; do
    if bash "$HERE/$other" >/dev/null 2>&1; then
        verdict "7. $other still passes" 0 0
    else
        verdict "7. $other still passes" 0 1
    fi
done

echo
echo "provenance-integration-branch-test: passed=$passed failed=$failed"
[ "$failed" -eq 0 ]
