#!/usr/bin/env bash
# codeberg-mirror.sh — idempotent push-mirror of galaxy git repos to a
# non-US sovereign mirror (Codeberg by default, or any Forgejo host).
#
# WHY: GitHub is a US company subject to the CLOUD Act. A second, EU-hosted
# remote turns "GitHub blocked / subpoenaed / DMCA-struck" from a work-stopping
# event into a non-event — the bytes are already mirrored under EU jurisdiction.
# The git history is sovereign by construction (content-addressed, distributed);
# this script just keeps a warm second copy outside US reach.
#
# This script is PREPARED, not armed. It is a DRY-RUN by default: it prints
# exactly what it would do and changes nothing on any remote. It NEVER creates
# the Codeberg organisation (that is a manual operator gesture — see the note
# docs/codeberg-mirror.md §"Create the org"). To actually push, pass --confirm.
#
# Usage:
#   scripts/codeberg-mirror.sh                 # dry-run over default galaxies
#   scripts/codeberg-mirror.sh --confirm       # actually wire remotes + push
#   scripts/codeberg-mirror.sh /srv/cosmon/foo  # operate on an explicit repo list
#
# Environment (all optional, sensible defaults):
#   CODEBERG_HOST   git host            (default: codeberg.org)
#   CODEBERG_OWNER  user or org slug    (default: you)
#   REMOTE_NAME     name of 2nd remote  (default: codeberg)
#   MIRROR_MODE     all-tags | mirror   (default: all-tags — non-destructive)
#
# Exit codes: 0 success / dry-run ok · 1 a push failed · 2 usage error.
set -euo pipefail

CODEBERG_HOST="${CODEBERG_HOST:-codeberg.org}"
CODEBERG_OWNER="${CODEBERG_OWNER:-you}"
REMOTE_NAME="${REMOTE_NAME:-codeberg}"
MIRROR_MODE="${MIRROR_MODE:-all-tags}"

CONFIRM=0
REPOS=()

for arg in "$@"; do
  case "$arg" in
    --confirm) CONFIRM=1 ;;
    --help|-h)
      grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -32
      exit 0 ;;
    -*)
      echo "unknown flag: $arg" >&2; exit 2 ;;
    *) REPOS+=("$arg") ;;
  esac
done

# Default repo set: the two galaxies named in the molecule brief. Both carry a
# single origin → github.com:noogram/<name>.git remote (verified 2026-06-14).
if [[ ${#REPOS[@]} -eq 0 ]]; then
  REPOS=(
    "$HOME/galaxies/cosmon"
    "$HOME/galaxies/noogram"
  )
fi

say() { printf '%s\n' "$*"; }
run() {
  # Echo every mutating command; execute only when --confirm is set.
  if [[ $CONFIRM -eq 1 ]]; then
    say "    \$ $*"
    "$@"
  else
    say "    (dry-run) $*"
  fi
}

if [[ $CONFIRM -eq 0 ]]; then
  say "=== DRY-RUN === (no remote is contacted; pass --confirm to arm)"
fi
say "host=${CODEBERG_HOST} owner=${CODEBERG_OWNER} remote=${REMOTE_NAME} mode=${MIRROR_MODE}"
say ""

rc=0
for repo in "${REPOS[@]}"; do
  if [[ ! -d "$repo/.git" ]]; then
    say "SKIP  $repo  (not a git repo)"
    continue
  fi
  name="$(basename "$repo")"
  mirror_url="git@${CODEBERG_HOST}:${CODEBERG_OWNER}/${name}.git"
  say "REPO  $repo"
  say "      origin → $(git -C "$repo" remote get-url origin 2>/dev/null || echo '(none)')"
  say "      ${REMOTE_NAME} → ${mirror_url}"

  # Idempotent remote wiring: add if absent, re-point if present.
  if git -C "$repo" remote get-url "$REMOTE_NAME" >/dev/null 2>&1; then
    cur="$(git -C "$repo" remote get-url "$REMOTE_NAME")"
    if [[ "$cur" != "$mirror_url" ]]; then
      run git -C "$repo" remote set-url "$REMOTE_NAME" "$mirror_url"
    else
      say "      (remote ${REMOTE_NAME} already correct)"
    fi
  else
    run git -C "$repo" remote add "$REMOTE_NAME" "$mirror_url"
  fi

  # Push. all-tags = non-destructive (never deletes remote branches);
  # mirror = exact replica (force-syncs, deletes remote refs absent locally).
  case "$MIRROR_MODE" in
    mirror)
      run git -C "$repo" push --mirror "$REMOTE_NAME" || rc=1 ;;
    all-tags)
      run git -C "$repo" push "$REMOTE_NAME" --all || rc=1
      run git -C "$repo" push "$REMOTE_NAME" --tags || rc=1 ;;
    *)
      say "      ERROR unknown MIRROR_MODE=${MIRROR_MODE}"; rc=2 ;;
  esac
  say ""
done

if [[ $CONFIRM -eq 0 ]]; then
  say "=== DRY-RUN complete — nothing was changed. Re-run with --confirm to arm. ==="
fi
exit $rc
