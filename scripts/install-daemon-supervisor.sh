#!/usr/bin/env bash
# install-daemon-supervisor.sh — install / uninstall the cosmon-daemon-supervisor LaunchAgent.
#
# Wraps `launchctl` so the operator has a single, reversible verb-door for
# the meta-supervisor. The template lives at
#   scripts/launchd/com.cosmon.daemon-supervisor.plist
# and is rendered into
#   ~/Library/LaunchAgents/com.cosmon.daemon-supervisor.plist
# with `__HOME__` substituted for the current user's home directory.
#
# Mirrors scripts/install-scheduler.sh by construction: same flow, same
# exit codes, same uninstall symmetry. Both the tick-based scheduler and
# the event-driven supervisor run as resident LaunchAgents under the
# Autonomous regime (ADR-016).
#
# See the supervisor architecture at
#   crates/cosmon-daemon-supervisor/src/lib.rs
#
# Usage:
#   scripts/install-daemon-supervisor.sh install      — render + load the agent
#   scripts/install-daemon-supervisor.sh uninstall    — unload + remove the agent
#   scripts/install-daemon-supervisor.sh reload       — unload (if loaded) then install
#   scripts/install-daemon-supervisor.sh status       — show launchctl state
#   scripts/install-daemon-supervisor.sh print        — print rendered plist to stdout
#
# Exit codes:
#   0 — success
#   1 — operator error (missing template, bad args, unknown command)
#   2 — launchctl error

set -euo pipefail

LABEL="com.cosmon.daemon-supervisor"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${SCRIPT_DIR}/launchd/${LABEL}.plist"
TARGET_DIR="${HOME}/Library/LaunchAgents"
TARGET="${TARGET_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/.cosmon/logs"

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "install-daemon-supervisor: $*" >&2
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
        echo "install-daemon-supervisor: $LABEL already loaded — use 'reload' to replace it"
        return 0
    fi

    render > "$TARGET"
    echo "install-daemon-supervisor: rendered $TARGET"

    if launchctl load "$TARGET"; then
        echo "install-daemon-supervisor: loaded — supervisor runs under launchd with KeepAlive"
        echo "install-daemon-supervisor: logs at $LOG_DIR/cosmon-daemon-supervisor.{out,err}"
        echo "install-daemon-supervisor: config at \$HOME/.config/cosmon/daemons.toml"
    else
        rc=$?
        echo "install-daemon-supervisor: launchctl load failed (rc=$rc)" >&2
        return 2
    fi
}

cmd_uninstall() {
    if [[ -f "$TARGET" ]]; then
        if loaded; then
            if ! launchctl unload "$TARGET"; then
                rc=$?
                echo "install-daemon-supervisor: launchctl unload failed (rc=$rc)" >&2
                return 2
            fi
        fi
        rm -f "$TARGET"
        echo "install-daemon-supervisor: removed $TARGET"
    else
        echo "install-daemon-supervisor: no plist at $TARGET (nothing to do)"
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
        *)         echo "install-daemon-supervisor: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
