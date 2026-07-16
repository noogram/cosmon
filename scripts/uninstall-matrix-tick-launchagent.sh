#!/usr/bin/env bash
# uninstall-matrix-tick-launchagent.sh — remove the `matrix-echo-tick`
# LaunchAgent (dev.noogram.cosmon.matrix-tick).
#
# Non-destructive: logs under ~/Library/Logs/ and credentials under
# ~/.config/cosmon-matrix-tick/ are preserved. Only the rendered plist
# and the launchctl registration are removed. Re-install at will.
#
# Usage:
#   uninstall-matrix-tick-launchagent.sh [--yes]
#
# Exit codes:
#   0 — success (or nothing to do)
#   1 — operator error (unknown flag)
#   2 — launchctl error

set -euo pipefail

LABEL="dev.noogram.cosmon.matrix-tick"
TARGET_DIR="${HOME}/Library/LaunchAgents"
TARGET="${TARGET_DIR}/${LABEL}.plist"

YES=0

die() {
    echo "uninstall-matrix-tick: $*" >&2
    exit 1
}

usage() {
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --yes|-y) YES=1; shift ;;
            -h|--help|help) usage 0 ;;
            *) echo "uninstall-matrix-tick: unknown flag: $1" >&2; usage 1 ;;
        esac
    done
}

loaded() {
    launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
}

confirm() {
    [[ "$YES" -eq 1 ]] && return 0
    read -r -p "uninstall-matrix-tick: unload and remove ${LABEL}? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || die "aborted by operator"
}

main() {
    parse_args "$@"

    if [[ ! -f "$TARGET" ]] && ! loaded; then
        echo "uninstall-matrix-tick: no plist at $TARGET and not loaded (nothing to do)"
        exit 0
    fi

    confirm

    if loaded; then
        if ! launchctl unload "$TARGET" 2>/dev/null; then
            # Loaded but target plist already deleted — try unload by
            # label (older macOS) / bootout (newer) before giving up.
            launchctl remove "$LABEL" 2>/dev/null || true
        fi
        if loaded; then
            echo "uninstall-matrix-tick: launchctl still shows $LABEL loaded" >&2
            exit 2
        fi
        echo "uninstall-matrix-tick: unloaded $LABEL"
    fi

    if [[ -f "$TARGET" ]]; then
        rm -f "$TARGET"
        echo "uninstall-matrix-tick: removed $TARGET"
    fi

    echo "uninstall-matrix-tick: done (logs + credentials preserved)"
}

main "$@"
