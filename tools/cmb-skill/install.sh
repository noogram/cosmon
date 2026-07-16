#!/usr/bin/env bash
# Install the /cmb skill into the user-global Claude Code skills directory.
#
# Source of truth: cosmon repo `tools/cmb-skill/`.
# Deploy target:   ~/.claude/skills/cmb/
#
# Pure copy — re-running is idempotent (overwrites in place).
# Runs without sudo; the target dir is per-user.

set -euo pipefail

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
DEST_DIR="$HOME/.claude/skills/cmb"

mkdir -p "$DEST_DIR"

# Copy SKILL.md and cmb.sh; preserve executable bit on the script.
cp "$SRC_DIR/SKILL.md" "$DEST_DIR/SKILL.md"
cp "$SRC_DIR/cmb.sh"   "$DEST_DIR/cmb.sh"
chmod +x "$DEST_DIR/cmb.sh"

echo "Installed /cmb skill → $DEST_DIR"
echo
echo "Files:"
ls -la "$DEST_DIR"
echo
echo "Smoke test (run from any cosmon worktree):"
echo "  ~/.claude/skills/cmb/cmb.sh | head -20"
