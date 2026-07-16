#!/usr/bin/env bash
# install-git-hooks.sh — wire the canonical hooks under .cosmon/hooks/
# into .git/hooks/. Idempotent: re-running replaces the symlink.
#
# Today this installs:
#   commit-msg        ADR-052 §I9 provenance gate (rejects merges that
#                     bypass the cosmon state machine). The gate runs
#                     in `commit-msg` rather than `pre-merge-commit`
#                     because the merge subject and MERGE_HEAD are
#                     only available after fmt-merge-msg writes them
#                     — see the hook's header for the full rationale.
#
# Usage:
#   bash scripts/install-git-hooks.sh           # install for this repo
#   bash scripts/install-git-hooks.sh --check   # verify without modifying
#
# References:
#   - docs/adr/052-one-ledger-one-writer-one-witness.md §I9, §D5
#   - .cosmon/hooks/pre-merge-commit (the hook source)

set -euo pipefail

CHECK_ONLY=0
if [ "${1:-}" = "--check" ]; then
    CHECK_ONLY=1
fi

REPO=$(git rev-parse --show-toplevel)
GITDIR=$(git rev-parse --git-common-dir)
SRC_DIR="$REPO/.cosmon/hooks"
DST_DIR="$GITDIR/hooks"

HOOKS=(
    "commit-msg"
)

mkdir -p "$DST_DIR"

failed=0
for hook in "${HOOKS[@]}"; do
    src="$SRC_DIR/$hook"
    dst="$DST_DIR/$hook"

    if [ ! -f "$src" ]; then
        echo "install-git-hooks: source missing: $src" >&2
        failed=$((failed + 1))
        continue
    fi

    chmod +x "$src"

    if [ "$CHECK_ONLY" -eq 1 ]; then
        if [ -L "$dst" ] && [ "$(readlink "$dst")" = "$src" ]; then
            echo "ok      $hook"
        elif [ -f "$dst" ] && cmp -s "$src" "$dst"; then
            echo "ok      $hook (copy)"
        else
            echo "missing $hook → run: bash scripts/install-git-hooks.sh"
            failed=$((failed + 1))
        fi
        continue
    fi

    if [ -e "$dst" ] || [ -L "$dst" ]; then
        rm -f "$dst"
    fi
    ln -s "$src" "$dst"
    echo "linked  $hook → $src"
done

if [ "$failed" -ne 0 ]; then
    exit 1
fi
