#!/usr/bin/env bash
# check-no-session-ids.sh — refuse agent-harness session identifiers in the
# surfaces `publish.sh --check` cannot see: commit messages, and a
# pull-request / issue body handed to it.
#
# WHY A SECOND SURFACE EXISTS
# ---------------------------
# `scripts/publish.sh --check` owns the tracked tree and now refuses the same
# class there. A commit message is not in any tree, and a PR body is not even
# in the repository — yet both are permanent public record on a public remote.
# On 2026-09-10 a `https://claude.ai/code/session_<id>` link was found in a PR
# comment on the public repo, and 84 commit messages on public `main` still
# carry a `Claude-Session:` trailer with 54 distinct identifiers. Probed
# anonymously they answer 403, so they are not content leaks — they are durable
# public pointers at private conversations, which is the definition this gate
# refuses.
#
# SCOPE — WHAT A PR ADDS, NEVER WHAT HISTORY CONTAINS
# ---------------------------------------------------
# The 84 historical trailers STAY. Rewriting them means a force-push that
# breaks every clone and invalidates the SHAs cited in ADRs and chronicles, and
# the contributor guide forbids rewriting the development repository in place.
# So the scope is a RANGE, never a walk of history: this gate judges the
# commits a change adds and nothing else. On `main` at its current HEAD, with
# no range to judge, it has nothing in scope and says so.
#
# That is also why the scope is not a go-live DATE, the device
# `check-provenance.sh` uses. Five of the commits merged into main on the day
# this gate was written carry the trailer, so any date boundary loose enough to
# be useful tomorrow is a boundary that reds the trunk today — and the
# alternative, a grandfathering exception list that grows, is the thing the
# mission ruled out.
#
# Scope selection, in order of precedence — the same ladder as
# `check-provenance.sh`, so one mental model covers both:
#   1. explicit revisions:   check-no-session-ids.sh <base> <head>
#   2. GitHub PR env:        GITHUB_BASE_REF + COSMON_SESSION_SCAN_HEAD/GITHUB_SHA
#   3. GitHub push env:      GITHUB_EVENT_BEFORE..GITHUB_SHA
#   4. local fallback:       main..HEAD — what this branch adds
#
# WHAT IT DOES NOT COVER, STATED SO IT IS NOT MISREAD
# ---------------------------------------------------
#   - A pull-request or issue BODY EDITED AFTER the check ran. CI judges the
#     body as it stood when the workflow started; a later edit is not re-judged
#     unless the workflow re-runs. The Assert Guard has this exact limitation
#     and the repository already understands it.
#   - A COMMENT posted on a PR or issue at any time. No commit event fires, so
#     no gate runs. The 2026-09-10 finding was in a comment; this gate would
#     not have caught it. What it catches is the trailer that produced the 84,
#     and the body of the PR that carries them.
#   - Any commit already on the trunk. By construction — see SCOPE above.
#   - Release notes and review comments authored outside a PR event.
#
# WHAT IT REPORTS. The commit, the line number inside the message, and the rule
# name — never the identifier. A gate that prints what it found has moved the
# pointer out of one commit message and into every CI log that ran it. Same
# doctrine as `publish.sh` checks B and F.
#
# NO BYPASS ENVIRONMENT VARIABLE. `check-provenance.sh` has one because a merge
# can be legitimately outside the cosmon discipline. A session identifier in a
# public record never is: the substitute is always available and always better
# (see AGENTS.md §Conventions — provenance is the molecule id and the merge
# shape, ADR-052 §I9). The fix is to amend the message.
#
# Exit codes: 0 clean (or nothing in scope) · 1 findings · 2 setup error.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/session-id-patterns.sh
. "$HERE/lib/session-id-patterns.sh"

# Digest a matched value without ever emitting it; reads stdin so the value
# never becomes an argv entry (argv is world-readable in `ps`).
digest() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 | cut -c1-8
  elif command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -c1-8
  else cat >/dev/null; printf 'nodigest'; fi
}

# Scan one blob of text. $1 a label for the finding, stdin the text; one
# finding line per hit on stdout.
#
# It deliberately does NOT keep a counter. Every call site runs it on the right
# of a pipe or with a redirect, i.e. in a subshell, so a variable incremented
# here would be discarded on return and the gate would report zero hits while
# printing findings. The findings file is the count.
scan_text() {
  local label="$1" lno=0 line rule re d
  while IFS= read -r line; do
    lno=$((lno + 1))
    while IFS=$'\t' read -r rule re; do
      [ -z "$rule" ] && continue
      printf '%s\n' "$line" | grep -qE -e "$re" || continue
      # `|| true`: `head -1` closes the pipe, `grep` dies of SIGPIPE, and
      # under `set -o pipefail` the substitution would abort the whole gate
      # rather than digest one line. A gate that dies is not a gate that
      # refuses.
      d="$(printf '%s' "$line" | grep -oE -e "$re" | head -1 | digest || true)"
      printf '  session-id: %s: line %d: %s (value withheld, sha256:%s)\n' \
        "$label" "$lno" "$rule" "$d"
    done <<<"$SESSION_ID_RULES"
  done
}

# ── Scope ───────────────────────────────────────────────────────────────────
base=""; head_rev=""
if [ "$#" -ge 2 ]; then
  base="$1"; head_rev="$2"
elif [ -n "${GITHUB_BASE_REF:-}" ] && [ -n "${COSMON_SESSION_SCAN_HEAD:-}${GITHUB_SHA:-}" ]; then
  git fetch --no-tags --depth=200 origin "$GITHUB_BASE_REF" 2>/dev/null || true
  base="origin/$GITHUB_BASE_REF"
  # On pull_request events GITHUB_SHA is GitHub's synthetic test-merge commit,
  # which the contributor never wrote. The workflow exports the PR's real HEAD.
  head_rev="${COSMON_SESSION_SCAN_HEAD:-$GITHUB_SHA}"
elif [ -n "${GITHUB_EVENT_BEFORE:-}" ] && [ -n "${GITHUB_SHA:-}" ] \
     && [ "${GITHUB_EVENT_BEFORE:-}" != "0000000000000000000000000000000000000000" ]; then
  base="$GITHUB_EVENT_BEFORE"; head_rev="$GITHUB_SHA"
elif git rev-parse --verify -q main >/dev/null 2>&1; then
  base="main"; head_rev="HEAD"
fi

findings="$(mktemp)" || exit 2
trap 'rm -f "$findings"' EXIT

commits=""
if [ -n "$base" ] && [ -n "$head_rev" ]; then
  commits="$(git log --format='%H' "$base..$head_rev" 2>/dev/null || true)"
fi

echo "check-no-session-ids: scope ${base:-<none>}..${head_rev:-<none>}"

checked=0
if [ -n "$commits" ]; then
  while IFS= read -r commit; do
    [ -z "$commit" ] && continue
    checked=$((checked + 1))
    git log -1 --format='%B' "$commit" | scan_text "$commit" >>"$findings"
  done <<<"$commits"
fi

# ── The PR / issue body, when the caller hands one over ─────────────────────
# A path, so the body never passes through argv or an env value this script
# echoes. Absent is not an error: most invocations have no body.
body_file="${COSMON_SESSION_SCAN_BODY_FILE:-}"
body_checked=0
if [ -n "$body_file" ] && [ -f "$body_file" ]; then
  body_checked=1
  scan_text "pull-request/issue body" <"$body_file" >>"$findings"
fi

hits=$(grep -c . "$findings" 2>/dev/null || true)
hits=${hits:-0}
echo "check-no-session-ids: commits=$checked body_scanned=$body_checked hits=$hits"

if [ "$hits" -eq 0 ]; then
  if [ "$checked" -eq 0 ] && [ "$body_checked" -eq 0 ]; then
    echo "check-no-session-ids: nothing in scope — no commits added, no body handed over"
  fi
  echo "check-no-session-ids: CLEAN"
  exit 0
fi

echo
echo "offending locations:"
sort -u "$findings"
cat >&2 <<'EOF'

Session-identifier gate FAILED.

A harness session identifier — a `<Vendor>-Session:` / `<Vendor>-Thread:`
trailer, or a deep link into a vendor console — names a private conversation
this repository's readers cannot and must not open. A public repository is
permanent, so the identifier is a durable public pointer at something private.

Your harness may instruct you to append one by default. This repository's rule
overrides that default (AGENTS.md §Conventions).

Provenance belongs in the anchor that is public, stable and meaningful: the
molecule id, the merge shape `Merge branch 'feat/<mol_id>'`, the durable
molecule directory and the ledger (ADR-052 §I9). A vendor URL is not
provenance; it is a bookmark in somebody's browser.

Fix: amend the offending commit message (`git rebase -i`, or `git commit
--amend` for the tip) and drop the trailer. There is no bypass variable.
EOF
exit 1
