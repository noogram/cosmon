#!/usr/bin/env bash
# Install the /cosmon skill into the user-global Claude Code skills directory.
#
# Source of truth: cosmon repo `tools/cosmon-skill/SKILL.md`, generated from
# `cosmon_filestore::project_upgrade::generate_cosmon_skill_md` — the same
# body rendered into every project's `CLAUDE.md`/`AGENTS.md` cosmon section.
# Deploy target:   ~/.claude/skills/cosmon/
#
# Pure copy — re-running is idempotent (overwrites in place). Runs without
# sudo; the target dir is per-user. Installed once here, this skill loads
# in every repository, including one that has never run `cs init`.

set -euo pipefail

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
DEST_DIR="$HOME/.claude/skills/cosmon"

mkdir -p "$DEST_DIR"
cp "$SRC_DIR/SKILL.md" "$DEST_DIR/SKILL.md"

echo "Installed /cosmon skill → $DEST_DIR"
echo
echo "Files:"
ls -la "$DEST_DIR"
