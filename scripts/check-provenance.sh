#!/usr/bin/env bash
# check-provenance.sh — CI mirror of the .cosmon/hooks/pre-merge-commit
# hook. Walks every merge commit added in the current scope and rejects
# any whose subject does not match the cosmon provenance pattern.
#
# Scope selection (in order of precedence):
#   1. $1 / $2 explicit revisions:   check-provenance.sh <base> <head>
#   2. GitHub PR env vars:           GITHUB_BASE_REF / GITHUB_SHA
#   3. GitHub push env var:          GITHUB_EVENT_BEFORE..GITHUB_SHA
#   4. Fallback:                     merges on HEAD since GO_LIVE date
#                                    (default 2026-04-19, the day this
#                                    gate was introduced — see ADR-052).
#                                    Override with COSMON_PROVENANCE_SINCE.
#
# Why the CI gate exists:
#   The local hook protects laptops; the CI gate protects the remote.
#   Without it, a force-pushed merge on the server cannot be caught
#   from inside cosmon (ADR-052 §I9 Enforceability — out-of-band).
#
# What it checks:
#   - Subject matches: Merge branch 'feat/<mol_id>' | evolve(<mol_id>)
#                     | done(<mol_id>) | auto-merge(<mol_id>)
#   - mol_id has a recorded molecule_completed or molecule_collapsed
#     event in .cosmon/state/events.jsonl AT THE TIP COMMIT of the
#     scope — WHEN the ledger is tracked in git at all. We check the
#     tip — not the merge commit itself — because `cs done` writes the
#     completion to the on-disk ledger *before* the merge commit, but
#     the ledger is committed by a *separate* `chore(state): track
#     artifacts ...` commit that lands AFTER the merge commit. So the
#     merge commit's tree never contains its own completion line; only
#     the eventual tip does. The local hook reads the working tree
#     (which is always current); the CI mirror reads the tip blob.
#
# Ledger residence — why the second check is not always available
# (task-20260729-dc53):
#   ADR-055 gives a galaxy a *residence* for its narration. Under the
#   `solo` residence `.cosmon/state/` is tracked, the ledger is in
#   every tree, and both halves of this gate run. Under the `team`
#   (and `remote`) residence `.cosmon/state/` is entirely gitignored —
#   narration lives on the orphan branch `cosmon/state`, or on a
#   server — and no tree of the working branch ever contains the
#   ledger. cosmon-the-repo itself is a `team` residence, so on this
#   repo `git show <tip>:.cosmon/state/events.jsonl` has always
#   resolved to nothing and the ledger half has never once bound.
#
#   That is not a regression to repair by re-enabling the check; it is
#   what ADR-052 §D5 asked for. Read the table there: the *hook* is the
#   gate that refuses "a mol_id with no `cs done` event in the ledger",
#   and it can, because it reads a working tree. The *CI check* is
#   assigned exactly one refusal — "merges into main that lack the
#   `(<mol_id>)` provenance line in the merge commit". Subject shape.
#   The ledger half here is a bonus that fires under `solo`, never a
#   mandate.
#
#   What this script owes the reader is therefore honesty, not a
#   restored check. Two rules follow:
#     - Ledger untracked at the tip (`git ls-tree .cosmon/state` empty)
#       ⇒ expected under team/remote residence. Say so ONCE, up front,
#       and label every verdict `shape` so no summary line can be
#       misread as "N merges were ledger-verified".
#     - Ledger tracked but absent/unreadable ⇒ NOT expected. Something
#       removed the ledger from a residence that keeps it under git.
#       That is a hard FAIL, where it used to be a silent `skip`.
#
# Bypass: COSMON_SKIP_PROVENANCE=1  (logged, returns 0 immediately).
#
# References: docs/adr/052-one-ledger-one-writer-one-witness.md §I9, §D5.

set -euo pipefail

if [ "${COSMON_SKIP_PROVENANCE:-}" = "1" ]; then
    echo "check-provenance: bypassed via COSMON_SKIP_PROVENANCE=1" >&2
    exit 0
fi

# Resolve base..head range.
if [ "$#" -ge 2 ]; then
    base="$1"
    head="$2"
elif [ -n "${GITHUB_BASE_REF:-}" ] && [ -n "${COSMON_PROVENANCE_HEAD:-}${GITHUB_SHA:-}" ]; then
    git fetch --no-tags --depth=200 origin "$GITHUB_BASE_REF" 2>/dev/null || true
    base="origin/$GITHUB_BASE_REF"
    # On pull_request events GITHUB_SHA is the synthetic test-merge commit
    # GitHub fabricates for the PR ("Merge <head> into <base>"). Cosmon did
    # not write it, its subject can never match, and judging it would block
    # every external PR (PR #42). The workflow exports the PR's real HEAD as
    # COSMON_PROVENANCE_HEAD; prefer it so the gate walks the commits the
    # contributor actually wrote.
    head="${COSMON_PROVENANCE_HEAD:-$GITHUB_SHA}"
elif [ -n "${GITHUB_EVENT_BEFORE:-}" ] && [ -n "${GITHUB_SHA:-}" ] \
        && [ "${GITHUB_EVENT_BEFORE:-}" != "0000000000000000000000000000000000000000" ]; then
    base="$GITHUB_EVENT_BEFORE"
    head="$GITHUB_SHA"
else
    # Fallback — scan merges on HEAD since the gate's go-live date so we
    # do not retroactively flag the historical c1cb-class merges that
    # motivated this gate's existence in the first place.
    base=""
    head="HEAD"
fi

# Default: since the day this gate landed. Force midnight so git's
# --since does not interpret the bare date as "today's wall-clock time"
# and silently skip same-day commits.
since="${COSMON_PROVENANCE_SINCE:-2026-04-19 00:00:00}"

if [ -n "$base" ]; then
    range="$base..$head"
    merges=$(git log --merges --format='%H' "$range" 2>/dev/null || true)
else
    merges=$(git log --merges --format='%H' --since="$since" "$head" 2>/dev/null || true)
fi

if [ -z "$merges" ]; then
    echo "check-provenance: no merge commits in scope — nothing to check"
    exit 0
fi

MOL_ID_RE='([a-z]+-[0-9]{8}-[a-f0-9]+)'
PATTERNS=(
    "^Merge branch [\"']feat/${MOL_ID_RE}[\"']"
    "^evolve\(${MOL_ID_RE}\)"
    "^done\(${MOL_ID_RE}\)"
    "^auto-merge\(${MOL_ID_RE}\)"
)

# Base-sync: `git merge main` run INSIDE a molecule's worktree, before
# `cs done`, so the fold does not have to resolve a pile of conflicts
# against a trunk that moved underneath it. Git writes the subject
# itself, hence the fixed shape.
#
# Why this is a separate class, and why accepting it does not weaken
# the gate:
#
#   - It is interior to a tracked molecule. The commit that actually
#     lands the work on main is the molecule's own fold merge
#     (`Merge branch 'feat/<mol_id>'`), which goes through the full
#     check above, ledger included. The base-sync is an ancestor of
#     that fold, not an independent entry point.
#
#   - It contributes NOTHING new. Its incoming side is a commit that
#     already sits on the trunk's first-parent chain, so every line it
#     carries was gated when it landed there. We verify that
#     structurally, per commit (see trunk_has below) — the subject
#     string alone is never taken as proof. A merge that *claims* to be
#     a base-sync but whose second parent is off-trunk is still FAIL.
#
#   - The ledger check is deliberately not applied here. At base-sync
#     time the molecule is by construction still running, so it has no
#     completion event; demanding one would make the practice
#     impossible rather than safe. The completion is demanded of the
#     fold merge, which is where it belongs.
BASE_SYNC_RE="^Merge branch [\"']main[\"'] into feat/${MOL_ID_RE}\$"

# Durable base-sync marker (delib-20260720-cff4, Phase 1). `cs sync` stamps
# an explicit `Base-Sync: <base>..<branch>` trailer on the merge it creates,
# so recognition no longer depends solely on the subject direction heuristic
# — a string git writes and nobody signs. The trailer is a superset signal:
# a merge is treated as a base-sync candidate if EITHER its subject matches
# BASE_SYNC_RE OR it carries a Base-Sync trailer whose branch names a
# molecule. Either way the SAME structural safety check applies (incoming
# side must sit on the trunk's first-parent chain), so this only hardens
# recognition, it never relaxes the gate.
BASE_SYNC_TRAILER_RE="^Base-Sync:[[:space:]]*[^[:space:]]+\.\.feat/${MOL_ID_RE}[[:space:]]*\$"

# First-parent trunk commits, used to prove a base-sync's incoming side
# is already-gated trunk material. Built lazily on first use, from the
# scope head AND the scope base: in a PR scope the head is the feature
# branch (whose first-parent chain follows the branch, not the trunk),
# so the trunk chain has to come from the base ref.
trunk_fp=""
trunk_has() {
    if [ -z "$trunk_fp" ]; then
        trunk_fp="$TMPDIR_PROV/trunk-fp"
        {
            git rev-list --first-parent "$head" 2>/dev/null || true
            [ -n "$base" ] && (git rev-list --first-parent "$base" 2>/dev/null || true)
        } > "$trunk_fp"
    fi
    grep -qx "$1" "$trunk_fp"
}

TMPDIR_PROV="$(mktemp -d -t cosmon-provenance-XXXXXX)"
trap 'rm -rf "$TMPDIR_PROV"' EXIT

# Read the ledger ONCE from the scope tip (see header comment for why
# the merge commit's own tree is the wrong place to look).
ledger=$(git show "$head:.cosmon/state/events.jsonl" 2>/dev/null || true)

# Residence probe — the oracle ADR-055 itself names in its verification
# table: "`git ls-files .cosmon/state/` returns 0 paths on the current
# branch" IS the definition of the team/remote residence. We ask the same
# question of the scope tip's tree rather than the index, so the answer is
# a property of the commit under test and not of whoever's checkout we run
# in. Empty ⇒ the ledger is absent BY DESIGN; non-empty ⇒ this residence
# keeps its narration under git and a missing ledger is a real defect.
state_tracked=$(git ls-tree -r --name-only "$head" -- .cosmon/state 2>/dev/null | head -n 1)

if [ -n "$ledger" ]; then
    ledger_mode="present"
elif [ -z "$state_tracked" ]; then
    ledger_mode="absent-by-residence"
    echo "check-provenance: NOTE — .cosmon/state/ is untracked at the scope tip."
    echo "  This galaxy keeps its narration off the working branch (ADR-055"
    echo "  team/remote residence), so no tree here can carry events.jsonl and"
    echo "  the ADR-052 §I9 ledger half of this gate CANNOT run. What runs below"
    echo "  is the subject-shape check — which is precisely the refusal §D5"
    echo "  assigns to CI. The ledger half is enforced by"
    echo "  .cosmon/hooks/pre-merge-commit, which reads the working tree."
    echo "  Do not cite a green run of this gate as evidence that a merge's"
    echo "  molecule reached a terminal state."
    echo
else
    ledger_mode="missing"
    echo "check-provenance: .cosmon/state/ IS tracked at the scope tip" >&2
    echo "  ($state_tracked, …) but .cosmon/state/events.jsonl is absent or" >&2
    echo "  unreadable there. Under a residence that keeps narration under" >&2
    echo "  git, that is a removed ledger, not an expected absence." >&2
    echo
fi

failed=0
checked=0
ledger_verified=0
shape_only=0
waived=0
while IFS= read -r commit; do
    [ -n "$commit" ] || continue
    checked=$((checked + 1))

    subject=$(git log -1 --format='%s' "$commit")

    # Base-sync class — checked before the general patterns because it
    # carries its own, structural, evidence requirement. Recognised via the
    # subject direction heuristic OR the durable `Base-Sync:` trailer stamped
    # by `cs sync` (delib-20260720-cff4). The trailer widens recognition; the
    # structural check below is identical for both, so it cannot weaken the
    # gate.
    base_sync_mol=""
    if [[ "$subject" =~ $BASE_SYNC_RE ]]; then
        base_sync_mol="${BASH_REMATCH[1]}"
    else
        trailer_line=$(git log -1 --format='%(trailers:key=Base-Sync,valueonly)' "$commit" \
            | head -n 1)
        # Reconstruct the full trailer line for the regex (valueonly drops the key).
        [ -n "$trailer_line" ] && trailer_line="Base-Sync: $trailer_line"
        if [ -n "$trailer_line" ] && [[ "$trailer_line" =~ $BASE_SYNC_TRAILER_RE ]]; then
            base_sync_mol="${BASH_REMATCH[1]}"
        fi
    fi
    if [ -n "$base_sync_mol" ]; then
        mol_id="$base_sync_mol"
        p2=$(git rev-parse --verify "$commit^2" 2>/dev/null || true)
        if [ -n "$p2" ] && trunk_has "$p2"; then
            # Structurally verified, deliberately not ledger-verified
            # (see the BASE_SYNC_RE comment): counted with the shape-only
            # verdicts so the summary never overstates ledger coverage.
            shape_only=$((shape_only + 1))
            echo "ok    $commit  ($mol_id)  base-sync from trunk"
        else
            echo "FAIL  $commit  ($mol_id)"
            echo "      subject claims a base-sync from main, but the"
            echo "      incoming side is not on the trunk's first-parent"
            echo "      chain — this merge carries ungated material"
            echo "      $subject"
            failed=$((failed + 1))
        fi
        continue
    fi

    mol_id=""
    for re in "${PATTERNS[@]}"; do
        if [[ "$subject" =~ $re ]]; then
            mol_id="${BASH_REMATCH[1]}"
            break
        fi
    done

    if [ -z "$mol_id" ]; then
        # Last chance before FAIL: an explicit, per-commit, reasoned waiver.
        # Read from the scope tip's tree rather than the working checkout, so
        # the verdict is a property of the commit under test.
        # `|| true`: the file is optional, `grep -v` returns 1 on an all-comment
        # file, and under `set -euo pipefail` either would abort the whole gate
        # rather than fail this one commit — a gate that dies is not a gate that
        # refuses, and the summary line would never print.
        waiver=$( { git show "$head:docs/provenance-waivers.tsv" 2>/dev/null \
            | grep -v '^[[:space:]]*#' \
            | awk -F'\t' -v c="$commit" '$1==c {print $2; exit}'; } || true )
        if [ -n "$waiver" ]; then
            waived=$((waived + 1))
            echo "WAIVE $commit"
            echo "      $subject"
            echo "      reason: $waiver"
            continue
        fi
        echo "FAIL  $commit"
        echo "      subject does not match cosmon provenance pattern:"
        echo "      $subject"
        echo "      (a merge genuinely outside the discipline takes a line in"
        echo "      docs/provenance-waivers.tsv with a written reason, not a"
        echo "      widened pattern and not COSMON_SKIP_PROVENANCE=1)"
        failed=$((failed + 1))
        continue
    fi

    # No ledger to consult. Which of the two absences is it?
    if [ "$ledger_mode" = "absent-by-residence" ]; then
        # Expected. The subject-shape refusal — the one §D5 assigns to CI —
        # has already passed above. Say `shape`, never `skip`: the commit
        # WAS checked, against everything this surface can check.
        shape_only=$((shape_only + 1))
        echo "shape $commit  ($mol_id)  subject ok; ledger off-branch (ADR-055)"
        continue
    elif [ "$ledger_mode" = "missing" ]; then
        echo "FAIL  $commit  ($mol_id)"
        echo "      .cosmon/state/ is tracked at the scope tip but the"
        echo "      ledger is not there — it cannot be consulted, so this"
        echo "      merge's completion cannot be proven (ADR-052 §I9)"
        failed=$((failed + 1))
        continue
    fi

    # Accept any of:
    #   - molecule_completed / molecule_collapsed (the I9 invariant proper)
    #   - merge_dispatched (proves `cs done` was the merge caller — cs
    #     done refuses non-terminal molecules without --force, so this
    #     is a strong proxy for "the state machine signed off on the
    #     transition"; the c1cb-class pilot-inline merges have neither)
    # NOTE: do NOT `exit` early from awk under `set -o pipefail` — the
    # upstream printf would die from SIGPIPE (rc 141) and the whole
    # pipeline would return the SIGPIPE code, masking the actual match.
    # Instead, scan the full ledger and report at END.
    rc=0
    awk -v id="$mol_id" '
        ((index($0, "\"molecule_id\":\"" id "\"") > 0) \
            || (index($0, "\"molecule\":\"" id "\"") > 0)) \
            && (index($0, "\"kind\":\"molecule_completed\"") > 0 \
             || index($0, "\"type\":\"molecule_completed\"") > 0 \
             || index($0, "\"kind\":\"molecule_collapsed\"") > 0 \
             || index($0, "\"type\":\"molecule_collapsed\"") > 0 \
             || index($0, "\"type\":\"merge_dispatched\"") > 0) {
            found = 1
        }
        END { exit found ? 0 : 1 }
    ' <<< "$ledger" || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL  $commit  ($mol_id)"
        echo "      no molecule_completed / molecule_collapsed in the"
        echo "      ledger at this commit — c1cb pathology (ADR-052 §I9)"
        failed=$((failed + 1))
        continue
    fi

    ledger_verified=$((ledger_verified + 1))
    echo "ok    $commit  ($mol_id)"
done <<< "$merges"

# Report the two coverages separately. A bare `checked=94 failed=0` reads
# as "94 merges were validated against the ledger", which on a team
# residence is false for all 94 of them.
echo
echo "check-provenance: checked=$checked ledger_verified=$ledger_verified" \
     "shape_only=$shape_only waived=$waived failed=$failed"
if [ "$ledger_verified" -eq 0 ] && [ "$checked" -gt 0 ]; then
    echo "check-provenance: ZERO merges were ledger-verified on this surface." \
         "The ADR-052 §I9 ledger invariant is enforced elsewhere" \
         "(.cosmon/hooks/pre-merge-commit), not here."
fi

if [ "$failed" -ne 0 ]; then
    cat >&2 <<EOF

Provenance gate FAILED. ADR-052 §I9: every merge commit must trace
back to a tracked molecule with a recorded completion in the ledger.

If a failing merge is genuinely outside the cosmon discipline (emergency
hotfix, external rebase), set COSMON_SKIP_PROVENANCE=1 in the workflow
or amend the merge commit to use the documented subject form.

Reference: docs/adr/052-one-ledger-one-writer-one-witness.md §I9, §D5.
EOF
    exit 1
fi
