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

# Read the ledger ONCE from the scope tip (see header comment for why
# the merge commit's own tree is the wrong place to look).
ledger=$(git show "$head:.cosmon/state/events.jsonl" 2>/dev/null || true)

failed=0
checked=0
while IFS= read -r commit; do
    [ -n "$commit" ] || continue
    checked=$((checked + 1))

    subject=$(git log -1 --format='%s' "$commit")
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
