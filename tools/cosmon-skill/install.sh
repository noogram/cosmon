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
#
# The same run wires the presence hook into ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/settings.json
# (via `cs sessions hook install`), so a pilot session registers itself with no
# further gesture. The hook is appended beside any hook already in that file,
# re-running changes nothing, and unrelated settings are never rewritten.
#   --no-hook     install the skill only
#   --uninstall   remove the skill and the hook, leaving the rest of settings.json

set -euo pipefail

link=false
hook=true
uninstall=false
usage() {
    printf 'Usage: %s [--link] [--no-hook] [--uninstall]\n' "$0"
}
for arg in "$@"; do
    case "$arg" in
        --link) link=true ;;
        --no-hook) hook=false ;;
        --uninstall) uninstall=true ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
DEST_DIR="$CONFIG_DIR/skills/cosmon"
TARGET="$DEST_DIR/SKILL.md"
SETTINGS="$CONFIG_DIR/settings.json"

# Wire or unwire the presence hook. Never fatal: the skill is the deliverable,
# the hook is an addition, and a missing `cs` or an unparsable settings file
# is reported and left alone rather than overwritten.
wire_hook() {
    verb="$1"
    if ! command -v cs >/dev/null 2>&1; then
        echo "cs not on PATH; presence hook not ${verb}ed (run 'cs sessions hook ${verb} --provider claude' later)" >&2
        return 0
    fi
    if ! cs sessions hook "$verb" --provider claude --settings "$SETTINGS" \
        --cs-bin "$(command -v cs)"; then
        echo "presence hook not ${verb}ed in $SETTINGS (left untouched)" >&2
    fi
}

if [ "$uninstall" = true ]; then
    wire_hook uninstall
    rm -f "$TARGET"
    rmdir "$DEST_DIR" 2>/dev/null || true
    echo "Removed /cosmon skill and presence hook from $CONFIG_DIR"
    exit 0
fi

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

if [ "$hook" = true ]; then
    wire_hook install
fi

echo "Installed /cosmon skill → $DEST_DIR"
echo
echo "Files:"
ls -la "$DEST_DIR"
