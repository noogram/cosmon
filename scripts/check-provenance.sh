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
#     scope. We check the tip — not the merge commit itself — because
#     `cs done` writes the completion to the on-disk ledger *before*
#     the merge commit, but the ledger is committed by a *separate*
#     `chore(state): track artifacts ...` commit that lands AFTER the
#     merge commit. So the merge commit's tree never contains its own
#     completion line; only the eventual tip does. The local hook
#     reads the working tree (which is always current); the CI mirror
#     reads the tip blob (which is current at push time).
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
elif [ -n "${GITHUB_BASE_REF:-}" ] && [ -n "${GITHUB_SHA:-}" ]; then
    git fetch --no-tags --depth=200 origin "$GITHUB_BASE_REF" 2>/dev/null || true
    base="origin/$GITHUB_BASE_REF"
    head="$GITHUB_SHA"
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

failed=0
checked=0
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
        echo "FAIL  $commit"
        echo "      subject does not match cosmon provenance pattern:"
        echo "      $subject"
        failed=$((failed + 1))
        continue
    fi

    if [ -z "$ledger" ]; then
        echo "skip  $commit  ($mol_id)  no ledger at scope tip"
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

    echo "ok    $commit  ($mol_id)"
done <<< "$merges"

echo
echo "check-provenance: checked=$checked failed=$failed"

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
