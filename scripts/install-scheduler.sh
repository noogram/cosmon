#!/usr/bin/env bash
# install-scheduler.sh — install / uninstall the cosmon scheduler service.
#
# Wraps the native per-user service manager so the operator has a single,
# reversible verb-door for the unified patrol scheduler. Darwin renders the
# launchd template beneath ~/Library/LaunchAgents. Linux renders a one-shot
# service and its external timer beneath the user's XDG config directory.
#
# AbandonProcessGroup on Darwin and KillMode=process on Linux are load-bearing:
# the scheduler tick exits after dispatch, while detached patrols must remain
# alive. Status reports the native manager state; application state remains in
# the scheduler's configured state file.
#
# Usage:
#   scripts/install-scheduler.sh install      — validate, render, enable, start
#   scripts/install-scheduler.sh uninstall    — stop, disable, remove owned units
#   scripts/install-scheduler.sh reload       — validate and replace the service
#   scripts/install-scheduler.sh status       — show native manager state
#   scripts/install-scheduler.sh print        — print rendered service definitions
#
# Exit codes:
#   0 — success
#   1 — operator error (missing template, bad args, unknown command)
#   2 — service-manager error

set -euo pipefail

LABEL="com.cosmon.scheduler"
BIN_DIR="${COSMON_SCHEDULER_BIN_DIR:-${HOME}/.local/bin}"
BIN="${BIN_DIR}/cosmon-scheduler"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PLIST_TEMPLATE="${SCRIPT_DIR}/launchd/${LABEL}.plist"
LOG_DIR="${HOME}/.cosmon/logs"
CONFIG="${COSMON_SCHEDULER_CONFIG:-${HOME}/.config/cosmon/patrols.toml}"
PLATFORM="$(uname -s)"

if [[ "$PLATFORM" == Linux ]]; then
    # shellcheck source=scripts/lib/install-user-service.sh
    source "${SCRIPT_DIR}/lib/install-user-service.sh"
    SERVICE_NAME="cosmon-scheduler.service"
    TIMER_NAME="cosmon-scheduler.timer"
    SERVICE_TEMPLATE="${SCRIPT_DIR}/systemd/${SERVICE_NAME}"
    TIMER_TEMPLATE="${SCRIPT_DIR}/systemd/${TIMER_NAME}"
    TARGET_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
    SERVICE_TARGET="${TARGET_DIR}/${SERVICE_NAME}"
    TIMER_TARGET="${TARGET_DIR}/${TIMER_NAME}"
else
    TEMPLATE="$PLIST_TEMPLATE"
    TARGET_DIR="${HOME}/Library/LaunchAgents"
    TARGET="${TARGET_DIR}/${LABEL}.plist"
fi

usage() {
    sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "install-scheduler: $*" >&2
    exit 1
}

render() {
    if [[ "$PLATFORM" == Linux ]]; then
        local quoted_bin quoted_config quoted_path exec_start service_path
        quoted_bin="$(cosmon_unit_quote "$BIN")" || exit 1
        quoted_config="$(cosmon_unit_quote "$CONFIG")" || exit 1
        service_path="${BIN_DIR}:${HOME}/.local/bin:${HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        quoted_path="$(cosmon_unit_quote "PATH=${service_path}")" || exit 1
        exec_start="/usr/bin/env ${quoted_bin} tick --config ${quoted_config}"
        cosmon_render_unit "$SERVICE_TEMPLATE" "$exec_start" "$quoted_path" "%h"
        return
    fi
    [[ -f "$TEMPLATE" ]] || die "template not found: $TEMPLATE"
    sed "s|__HOME__|${HOME}|g" "$TEMPLATE"
}

render_timer() {
    [[ -f "$TIMER_TEMPLATE" ]] || die "template not found: $TIMER_TEMPLATE"
    cat "$TIMER_TEMPLATE"
}

require_abandon_process_group() {
    local plist="$1"
    if ! /usr/bin/plutil -extract AbandonProcessGroup raw -o - -- "$plist" \
        2>/dev/null | grep -qx 'true'; then
        die "rendered plist lacks AbandonProcessGroup=true — detached patrols would be SIGKILLed on every tick. Restore the key in $TEMPLATE."
    fi
}

loaded() {
    if [[ "$PLATFORM" == Linux ]]; then
        systemctl --user is-active --quiet "$TIMER_NAME"
    else
        launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
    fi
}

linux_preflight() {
    [[ -x "$BIN" ]] || die "binary is not executable: $BIN"
    [[ -s "$CONFIG" ]] || die "config is missing or empty: $CONFIG"
    if ! "$BIN" validate --config "$CONFIG"; then
        die "config validation failed: $CONFIG"
    fi
    cosmon_require_user_manager || exit 1
    mkdir -p "$TARGET_DIR"
    [[ -w "$TARGET_DIR" ]] || die "unit directory is not writable: $TARGET_DIR"
}

linux_install() {
    linux_preflight
    local staging_dir staged_service staged_timer
    staging_dir="$(mktemp -d "${TARGET_DIR}/.cosmon-scheduler.XXXXXX")"
    staged_service="${staging_dir}/${SERVICE_NAME}"
    staged_timer="${staging_dir}/${TIMER_NAME}"
    if ! render > "$staged_service" || ! render_timer > "$staged_timer" ||
        ! cosmon_validate_unit "$staged_service" "$staged_timer"; then
        rm -rf "$staging_dir"
        die "unit validation failed; existing scheduler units were not changed"
    fi
    chmod 0644 "$staged_service" "$staged_timer"
    mv -f "$staged_service" "$SERVICE_TARGET"
    mv -f "$staged_timer" "$TIMER_TARGET"
    rmdir "$staging_dir"
    systemctl --user daemon-reload || {
        echo "install-scheduler: daemon-reload failed after writing scheduler units" >&2
        return 2
    }
    systemctl --user enable "$TIMER_NAME" || {
        echo "install-scheduler: could not enable $TIMER_NAME" >&2
        return 2
    }
    systemctl --user restart "$TIMER_NAME" || {
        echo "install-scheduler: could not start $TIMER_NAME; inspect 'systemctl --user status $TIMER_NAME'" >&2
        return 2
    }
    echo "install-scheduler: installed $SERVICE_TARGET and $TIMER_TARGET"
    echo "install-scheduler: timer fires the one-shot scheduler every 60s"
}

cmd_install() {
    if [[ "$PLATFORM" == Linux ]]; then
        linux_install
        return
    fi
    mkdir -p "$TARGET_DIR" "$LOG_DIR"
    if loaded; then
        echo "install-scheduler: $LABEL already loaded — use 'reload' to replace it"
        return 0
    fi
    local staged
    staged="$(mktemp -t install-scheduler)"
    render > "$staged"
    require_abandon_process_group "$staged"
    mv -f "$staged" "$TARGET"
    echo "install-scheduler: rendered $TARGET"
    if launchctl load "$TARGET"; then
        echo "install-scheduler: loaded — tick fires every 60s"
        echo "install-scheduler: logs at $LOG_DIR/cosmon-scheduler.{out,err}"
    else
        local rc=$?
        echo "install-scheduler: launchctl load failed (rc=$rc)" >&2
        return 2
    fi
}

cmd_uninstall() {
    if [[ "$PLATFORM" == Linux ]]; then
        cosmon_require_user_manager || exit 1
        if [[ -f "$TIMER_TARGET" ]] || loaded || systemctl --user is-enabled --quiet "$TIMER_NAME"; then
            if ! systemctl --user disable --now "$TIMER_NAME"; then
                echo "install-scheduler: could not stop and disable $TIMER_NAME" >&2
                return 2
            fi
        fi
        if [[ -f "$SERVICE_TARGET" ]] && ! systemctl --user stop "$SERVICE_NAME"; then
            echo "install-scheduler: could not stop $SERVICE_NAME" >&2
            return 2
        fi
        local removed=0
        if [[ -f "$SERVICE_TARGET" ]]; then rm -f "$SERVICE_TARGET"; removed=1; fi
        if [[ -f "$TIMER_TARGET" ]]; then rm -f "$TIMER_TARGET"; removed=1; fi
        if [[ "$removed" == 1 ]]; then
            echo "install-scheduler: removed owned scheduler units"
        else
            echo "install-scheduler: no scheduler units installed (nothing to remove)"
        fi
        systemctl --user daemon-reload || return 2
        systemctl --user reset-failed "$SERVICE_NAME" "$TIMER_NAME" >/dev/null 2>&1 || true
        return
    fi
    if [[ -f "$TARGET" ]]; then
        if loaded && ! launchctl unload "$TARGET"; then
            local rc=$?
            echo "install-scheduler: launchctl unload failed (rc=$rc)" >&2
            return 2
        fi
        rm -f "$TARGET"
        echo "install-scheduler: removed $TARGET"
    else
        echo "install-scheduler: no plist at $TARGET (nothing to do)"
    fi
}

cmd_reload() {
    if [[ "$PLATFORM" == Linux ]]; then linux_install; else cmd_uninstall; cmd_install; fi
}

cmd_status() {
    if [[ "$PLATFORM" == Linux ]]; then
        if [[ -f "$SERVICE_TARGET" && -f "$TIMER_TARGET" ]]; then
            echo "service: $SERVICE_TARGET"
            echo "timer:   $TIMER_TARGET"
            cosmon_require_user_manager || exit 1
            systemctl --user show "$TIMER_NAME" -p LoadState -p UnitFileState \
                -p ActiveState -p SubState -p NextElapseUSecRealtime -p FragmentPath --no-pager
            systemctl --user show "$SERVICE_NAME" -p LoadState -p ActiveState \
                -p SubState -p MainPID -p KillMode -p FragmentPath --no-pager
        else
            echo "service: (not installed)"
            echo "timer:   (not installed)"
            cosmon_require_user_manager || exit 1
            echo "loaded:  no"
        fi
        return
    fi
    if [[ -f "$TARGET" ]]; then
        echo "plist:   $TARGET"
        if /usr/bin/plutil -extract AbandonProcessGroup raw -o - -- "$TARGET" \
            2>/dev/null | grep -qx 'true'; then
            echo "abandon: yes (detached patrols survive the tick)"
        else
            echo "abandon: NO — this installed plist predates the fix."
            echo "         Every detached patrol is SIGKILLed when the tick exits."
            echo "         Repair: $0 reload"
        fi
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
    if [[ "$PLATFORM" == Linux ]]; then
        echo "# ${SERVICE_NAME}"
        render
        echo "# ${TIMER_NAME}"
        render_timer
    else
        render
    fi
}

main() {
    case "${1:-}" in
        install) shift; cmd_install "$@" ;;
        uninstall) shift; cmd_uninstall "$@" ;;
        reload) shift; cmd_reload "$@" ;;
        status) shift; cmd_status "$@" ;;
        print) shift; cmd_print "$@" ;;
        -h|--help|help) usage 0 ;;
        "") usage 1 ;;
        *) echo "install-scheduler: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
