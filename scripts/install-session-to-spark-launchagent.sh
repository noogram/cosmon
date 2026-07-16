#!/usr/bin/env bash
# install-session-to-spark-launchagent.sh — install / uninstall the
# `session-to-spark` LaunchAgent that consumes session notes beginning
# with `!spark `, nucleates a `spark` per qualifying note, and writes
# a sidecar marker under `.cosmon/state/sessions/.promoted/`.
#
# Mirror of `scripts/install-whisper-to-spark-launchagent.sh` — same
# verb-door shape so the operator does not have to learn a new ceremony.
#
# Template lives at
#   scripts/launchd/dev.noogram.cosmon.session-to-spark.plist
# and is rendered into
#   ~/Library/LaunchAgents/dev.noogram.cosmon.session-to-spark.plist
# with `__HOME__` and `__COSMON_ROOT__` substituted.
#
# Usage:
#   scripts/install-session-to-spark-launchagent.sh install [--cosmon-root DIR]
#   scripts/install-session-to-spark-launchagent.sh uninstall
#   scripts/install-session-to-spark-launchagent.sh reload [--cosmon-root DIR]
#   scripts/install-session-to-spark-launchagent.sh status
#   scripts/install-session-to-spark-launchagent.sh print [--cosmon-root DIR]
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

LABEL="dev.noogram.cosmon.session-to-spark"
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
    echo "install-session-to-spark: $*" >&2
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
    [[ -x "$root/scripts/session-to-spark-tick.sh" ]] \
        || die "tick script not executable: $root/scripts/session-to-spark-tick.sh"
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
        echo "install-session-to-spark: $LABEL already loaded — use 'reload' to replace it"
        return 0
    fi

    render "$root" > "$TARGET"
    echo "install-session-to-spark: rendered $TARGET (cosmon-root=$root)"

    if launchctl load "$TARGET"; then
        echo "install-session-to-spark: loaded — tick fires every 300s (promotes !spark-prefixed notes)"
        echo "install-session-to-spark: logs at $LOG_DIR/session-to-spark.{out,err}"
        echo "install-session-to-spark: for targeted promotion, use 'cs session promote <note_ts>'"
    else
        rc=$?
        echo "install-session-to-spark: launchctl load failed (rc=$rc)" >&2
        return 2
    fi
}

cmd_uninstall() {
    if [[ -f "$TARGET" ]]; then
        if loaded; then
            if ! launchctl unload "$TARGET"; then
                rc=$?
                echo "install-session-to-spark: launchctl unload failed (rc=$rc)" >&2
                return 2
            fi
        fi
        rm -f "$TARGET"
        echo "install-session-to-spark: removed $TARGET"
    else
        echo "install-session-to-spark: no plist at $TARGET (nothing to do)"
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
        *)         echo "install-session-to-spark: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
