#!/usr/bin/env bash
# install-cosmon-letter-launchagent.sh — install / uninstall the Monday
# letter LaunchAgent (godin ship-moment #1, delib-20260423-95fe).
#
# Mirror of `scripts/install-session-to-spark-launchagent.sh` — same
# verb-door shape so the operator does not learn a new ceremony.
#
# Template lives at
#   scripts/launchd/dev.noogram.cosmon.letter-monday.plist
# and is rendered into
#   ~/Library/LaunchAgents/dev.noogram.cosmon.letter-monday.plist
# with `__HOME__` and `__COSMON_ROOT__` substituted.
#
# Usage:
#   scripts/install-cosmon-letter-launchagent.sh install [--cosmon-root DIR]
#   scripts/install-cosmon-letter-launchagent.sh uninstall
#   scripts/install-cosmon-letter-launchagent.sh reload [--cosmon-root DIR]
#   scripts/install-cosmon-letter-launchagent.sh status
#   scripts/install-cosmon-letter-launchagent.sh print [--cosmon-root DIR]
#
# `--cosmon-root` defaults to the directory containing this script's
# parent (i.e. the repo checkout). Pass an explicit root when installing
# from outside the checkout.
#
# Exit codes:
#   0 — success
#   1 — operator error (missing template, bad args, unknown command)
#   2 — launchctl error

set -euo pipefail

LABEL="dev.noogram.cosmon.letter-monday"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${SCRIPT_DIR}/launchd/${LABEL}.plist"
TARGET_DIR="${HOME}/Library/LaunchAgents"
TARGET="${TARGET_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/.cosmon/logs"
DEFAULT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

usage() {
    sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "install-cosmon-letter: $*" >&2
    exit 1
}

parse_root() {
    local root=""
    while (($#)); do
        case "$1" in
            --cosmon-root) root="$2"; shift 2 ;;
            *) shift ;;
        esac
    done
    [[ -z "$root" ]] && root="$DEFAULT_ROOT"
    printf '%s\n' "$(cd "$root" && pwd -P)"
}

render() {
    local root="$1"
    [[ -f "$TEMPLATE" ]] || die "template not found: $TEMPLATE"
    [[ -x "$root/scripts/cosmon-letter-monday.sh" ]] \
        || die "letter script not executable: $root/scripts/cosmon-letter-monday.sh"
    sed -e "s|__HOME__|${HOME}|g" \
        -e "s|__COSMON_ROOT__|${root}|g" \
        "$TEMPLATE"
}

loaded() {
    launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
}

cmd_install() {
    local root; root="$(parse_root "$@")"
    mkdir -p "$TARGET_DIR" "$LOG_DIR"

    if loaded; then
        echo "install-cosmon-letter: $LABEL already loaded — use 'reload' to replace it"
        return 0
    fi

    render "$root" > "$TARGET"
    echo "install-cosmon-letter: rendered $TARGET (cosmon-root=$root)"

    if launchctl load "$TARGET"; then
        echo "install-cosmon-letter: loaded — fires Monday 08:30 local time"
        echo "install-cosmon-letter: logs at $LOG_DIR/cosmon-letter-monday.{out,err}"
        echo "install-cosmon-letter: dry-run anytime with 'scripts/cosmon-letter-monday.sh --dry-run'"
    else
        rc=$?
        echo "install-cosmon-letter: launchctl load failed (rc=$rc)" >&2
        return 2
    fi
}

cmd_uninstall() {
    if [[ -f "$TARGET" ]]; then
        if loaded; then
            if ! launchctl unload "$TARGET"; then
                rc=$?
                echo "install-cosmon-letter: launchctl unload failed (rc=$rc)" >&2
                return 2
            fi
        fi
        rm -f "$TARGET"
        echo "install-cosmon-letter: removed $TARGET"
    else
        echo "install-cosmon-letter: no plist at $TARGET (nothing to do)"
    fi
}

cmd_reload() {
    cmd_uninstall
    cmd_install "$@"
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
    local root; root="$(parse_root "$@")"
    render "$root"
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
        *)         echo "install-cosmon-letter: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
