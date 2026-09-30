#!/usr/bin/env bash
# Install the /cosmon skill into the user-global Claude Code skills directory.
#
# Source of truth: cosmon repo `tools/cosmon-skill/SKILL.md`, generated from
# `cosmon_filestore::project_upgrade::generate_cosmon_skill_md` — the same
# body rendered into every project's `CLAUDE.md`/`AGENTS.md` cosmon section.
# Deploy target:   ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/cosmon/
#
# Pure copy — re-running is idempotent (overwrites in place). Runs without
# sudo; the target dir is per-user. Installed once here, this skill loads
# in every repository, including one that has never run `cs init`.

set -euo pipefail

if [ "$#" -gt 0 ]; then
    case "$1" in
        --link)
            link=true
            shift
            ;;
        -h|--help)
            printf 'Usage: %s [--link]\n' "$0"
            exit 0
            ;;
        *)
            printf 'Usage: %s [--link]\n' "$0" >&2
            exit 2
            ;;
    esac
fi
if [ "$#" -ne 0 ]; then
    printf 'Usage: %s [--link]\n' "$0" >&2
    exit 2
fi

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
DEST_DIR="$CONFIG_DIR/skills/cosmon"
TARGET="$DEST_DIR/SKILL.md"
link="${link:-false}"

mkdir -p "$DEST_DIR"
if [ "$link" = true ]; then
    rm -f "$TARGET"
    ln -s "$SRC_DIR/SKILL.md" "$TARGET"
else
    if [ -L "$TARGET" ]; then
        rm -f "$TARGET"
    fi
    cp "$SRC_DIR/SKILL.md" "$TARGET"
fi

echo "Installed /cosmon skill → $DEST_DIR"
echo
echo "Files:"
ls -la "$DEST_DIR"
