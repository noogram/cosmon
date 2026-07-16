#!/usr/bin/env bash
# install-scheduler.sh — install / uninstall the cosmon-scheduler LaunchAgent.
#
# Wraps `launchctl` so the operator has a single, reversible verb-door for
# the unified patrol scheduler. The template lives at
#   scripts/launchd/com.noogram.cosmon-scheduler.plist
# and is rendered into
#   ~/Library/LaunchAgents/com.noogram.cosmon-scheduler.plist
# with `__HOME__` substituted for the current user's home directory.
#
# See the governing plan at
#   .cosmon/state/fleets/default/molecules/idea-20260417-b52d/plan.md
# and the scheduler architecture at
#   crates/cosmon-scheduler/src/lib.rs
#
# Usage:
#   scripts/install-scheduler.sh install      — render + load the agent
#   scripts/install-scheduler.sh uninstall    — unload + remove the agent
#   scripts/install-scheduler.sh reload       — unload (if loaded) then install
#   scripts/install-scheduler.sh status       — show launchctl state
#   scripts/install-scheduler.sh print        — print rendered plist to stdout
#
# Exit codes:
#   0 — success
#   1 — operator error (missing template, bad args, unknown command)
#   2 — launchctl error

set -euo pipefail

LABEL="com.cosmon.scheduler"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${SCRIPT_DIR}/launchd/${LABEL}.plist"
TARGET_DIR="${HOME}/Library/LaunchAgents"
TARGET="${TARGET_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/.cosmon/logs"

usage() {
    sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "install-scheduler: $*" >&2
    exit 1
}

render() {
    # Emit the plist with __HOME__ substituted. Read as bytes; $HOME is
    # trusted (set by login shell). Writes to stdout so callers can pipe
    # or redirect as they see fit.
    [[ -f "$TEMPLATE" ]] || die "template not found: $TEMPLATE"
    sed "s|__HOME__|${HOME}|g" "$TEMPLATE"
}

loaded() {
    # Portable check that works across macOS 11+ (`launchctl list`) and
    # macOS 14+ (`launchctl print`). We use `list` because it is stable
    # and its grep-friendly output predates the new subsystem.
    launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
}

cmd_install() {
    mkdir -p "$TARGET_DIR" "$LOG_DIR"

    if loaded; then
        echo "install-scheduler: $LABEL already loaded — use 'reload' to replace it"
        return 0
    fi

    render > "$TARGET"
    echo "install-scheduler: rendered $TARGET"

    if launchctl load "$TARGET"; then
        echo "install-scheduler: loaded — tick fires every 60s"
        echo "install-scheduler: logs at $LOG_DIR/cosmon-scheduler.{out,err}"
    else
        rc=$?
        echo "install-scheduler: launchctl load failed (rc=$rc)" >&2
        return 2
    fi
}

cmd_uninstall() {
    if [[ -f "$TARGET" ]]; then
        if loaded; then
            if ! launchctl unload "$TARGET"; then
                rc=$?
                echo "install-scheduler: launchctl unload failed (rc=$rc)" >&2
                return 2
            fi
        fi
        rm -f "$TARGET"
        echo "install-scheduler: removed $TARGET"
    else
        echo "install-scheduler: no plist at $TARGET (nothing to do)"
    fi
}

cmd_reload() {
    cmd_uninstall
    cmd_install
}

cmd_status() {
    if [[ -f "$TARGET" ]]; then
        echo "plist:   $TARGET"
    else
        echo "plist:   (not installed)"
    fi

    if loaded; then
        echo "loaded:  yes"
        launchctl list "$LABEL" 2>/dev/null || true
    else
        echo "loaded:  no"
    fi
}

cmd_print() {
    render
}

main() {
    case "${1:-}" in
        install)   shift; cmd_install "$@" ;;
        uninstall) shift; cmd_uninstall "$@" ;;
        reload)    shift; cmd_reload "$@" ;;
        status)    shift; cmd_status "$@" ;;
        print)     shift; cmd_print "$@" ;;
        -h|--help|help) usage 0 ;;
        "")        usage 1 ;;
        *)         echo "install-scheduler: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
