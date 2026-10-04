#!/usr/bin/env bash
# install-daemon-supervisor.sh — install / uninstall the cosmon daemon supervisor.
#
# Wraps the native per-user service manager so the operator has a single,
# reversible verb-door for the meta-supervisor. The Darwin template lives at
#   scripts/launchd/com.cosmon.daemon-supervisor.plist
# and is rendered into
#   ~/Library/LaunchAgents/com.cosmon.daemon-supervisor.plist
# with `__HOME__` substituted for the current user's home directory. Linux
# renders scripts/systemd/cosmon-daemon-supervisor.service beneath the user's
# XDG config directory.
#
# Mirrors scripts/install-scheduler.sh by construction: same verbs, exit
# codes, and uninstall symmetry. Native user services own process lifetime;
# the event-driven supervisor remains in ADR-016's Autonomous regime.
#
# See the supervisor architecture at
#   crates/cosmon-daemon-supervisor/src/lib.rs
#
# Usage:
#   scripts/install-daemon-supervisor.sh install      — validate, render, enable, start
#   scripts/install-daemon-supervisor.sh uninstall    — stop, disable, remove owned unit
#   scripts/install-daemon-supervisor.sh reload       — validate and replace the service
#   scripts/install-daemon-supervisor.sh status       — show native manager state
#   scripts/install-daemon-supervisor.sh print        — print the rendered service file
#   scripts/install-daemon-supervisor.sh install-binary <source> — copy and sign binary; never restart
#
# Exit codes:
#   0 — success
#   1 — operator error (missing template, bad args, unknown command)
#   2 — service-manager error

set -euo pipefail

LABEL="com.cosmon.daemon-supervisor"
SIGNING_IDENTITY="${COSMON_SUPERVISOR_SIGNING_IDENTITY:-Cosmon Local Signing}"
BIN_DIR="${COSMON_SUPERVISOR_BIN_DIR:-${HOME}/.local/bin}"
BIN="${BIN_DIR}/cosmon-daemon-supervisor"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PLIST_TEMPLATE="${SCRIPT_DIR}/launchd/${LABEL}.plist"
LOG_DIR="${HOME}/.cosmon/logs"
CONFIG="${COSMON_SUPERVISOR_CONFIG:-${HOME}/.config/cosmon/daemons.toml}"
PLATFORM="$(uname -s)"

if [[ "$PLATFORM" == Linux ]]; then
    # shellcheck source=scripts/lib/install-user-service.sh
    source "${SCRIPT_DIR}/lib/install-user-service.sh"
    UNIT_NAME="cosmon-daemon-supervisor.service"
    TEMPLATE="${SCRIPT_DIR}/systemd/${UNIT_NAME}"
    TARGET_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
    TARGET="${TARGET_DIR}/${UNIT_NAME}"
else
    TEMPLATE="$PLIST_TEMPLATE"
    TARGET_DIR="${HOME}/Library/LaunchAgents"
    TARGET="${TARGET_DIR}/${LABEL}.plist"
fi

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "install-daemon-supervisor: $*" >&2
    exit 1
}

render() {
    # Emit the native service definition. Writes to stdout so `print` remains
    # useful even when no service manager is reachable.
    [[ -f "$TEMPLATE" ]] || die "template not found: $TEMPLATE"
    if [[ "$PLATFORM" == Linux ]]; then
        local quoted_bin quoted_config quoted_path quoted_home exec_start service_path
        quoted_bin="$(cosmon_unit_quote "$BIN")" || exit 1
        quoted_config="$(cosmon_unit_quote "$CONFIG")" || exit 1
        service_path="${BIN_DIR}:${HOME}/.local/bin:${HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        quoted_path="$(cosmon_unit_quote "PATH=${service_path}")" || exit 1
        quoted_home="$(cosmon_unit_quote "$HOME")" || exit 1
        # The service manager restricts special characters in argv[0] even
        # when its unit syntax can quote them. env performs a direct exec and
        # lets the binary path remain an ordinary, fully quoted argument.
        exec_start="/usr/bin/env ${quoted_bin} --config ${quoted_config}"
        cosmon_render_unit "$TEMPLATE" "$exec_start" "$quoted_path" "$quoted_home"
    else
        sed "s|__HOME__|${HOME}|g" "$TEMPLATE"
    fi
}

loaded() {
    # Portable check that works across macOS 11+ (`launchctl list`) and
    # macOS 14+ (`launchctl print`). We use `list` because it is stable
    # and its grep-friendly output predates the new subsystem.
    if [[ "$PLATFORM" == Linux ]]; then
        systemctl --user is-active --quiet "$UNIT_NAME"
    else
        launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
    fi
}

sign_binary() {
    local path="${1:-$BIN}"
    [[ -f "$path" ]] || die "binary not found: $path"
    if [[ "$(uname -s)" != Darwin ]]; then
        echo "install-daemon-supervisor: installed $path (code signing applies only on macOS)"
        return 0
    fi
    command -v codesign >/dev/null || die "codesign is required to install the supervisor"
    if security find-identity -p codesigning 2>/dev/null | grep -qF "\"${SIGNING_IDENTITY}\""; then
        codesign --force --sign "$SIGNING_IDENTITY" --identifier "$LABEL" "$path" || return 1
        echo "install-daemon-supervisor: signed $path with $SIGNING_IDENTITY ($LABEL)"
    else
        codesign --force --sign - --identifier "$LABEL" "$path" || return 1
        echo "install-daemon-supervisor: WARNING: $SIGNING_IDENTITY is unavailable; ad-hoc signing uses a content-hash requirement, so TCC grants will not survive rebuilds" >&2
    fi
    local signature
    signature="$(codesign -dv "$path" 2>&1)" || die "could not inspect signature: $path"
    grep -Fqx "Identifier=$LABEL" <<< "$signature" || die "installed binary has the wrong code-signing identifier"
}

cmd_install_binary() {
    [[ $# -eq 1 ]] || die "install-binary requires one source path"
    [[ -f "$1" ]] || die "binary not found: $1"
    mkdir -p "$BIN_DIR"
    local staged
    staged="$(mktemp "${BIN_DIR}/.cosmon-daemon-supervisor.XXXXXX")"
    if ! install "$1" "$staged" || ! sign_binary "$staged" || ! mv -f "$staged" "$BIN"; then
        rm -f "$staged"
        die "could not install signed supervisor binary"
    fi
}

cmd_install() {
    if [[ "$PLATFORM" == Linux ]]; then
        linux_install
        return
    fi

    mkdir -p "$TARGET_DIR" "$LOG_DIR"
    cmd_install_binary "$BIN"

    if loaded; then
        echo "install-daemon-supervisor: $LABEL already loaded; on-disk signature is ready, but the running process was not restarted"
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

linux_preflight() {
    [[ -x "$BIN" ]] || die "binary is not executable: $BIN"
    [[ -s "$CONFIG" ]] || die "config is missing or empty: $CONFIG"
    if ! "$BIN" --config "$CONFIG" --check; then
        die "config validation failed: $CONFIG"
    fi
    cosmon_require_user_manager || exit 1
    mkdir -p "$TARGET_DIR"
    [[ -w "$TARGET_DIR" ]] || die "unit directory is not writable: $TARGET_DIR"
}

linux_install() {
    linux_preflight
    local staging_dir staged
    staging_dir="$(mktemp -d "${TARGET_DIR}/.cosmon-supervisor.XXXXXX")"
    staged="${staging_dir}/${UNIT_NAME}"
    if ! render > "$staged" || ! cosmon_validate_unit "$staged"; then
        rm -rf "$staging_dir"
        die "unit validation failed; existing service was not changed"
    fi
    chmod 0644 "$staged"
    mv -f "$staged" "$TARGET"
    rmdir "$staging_dir"
    systemctl --user daemon-reload || {
        echo "install-daemon-supervisor: daemon-reload failed after writing $TARGET" >&2
        return 2
    }
    systemctl --user enable "$UNIT_NAME" || {
        echo "install-daemon-supervisor: could not enable $UNIT_NAME" >&2
        return 2
    }
    systemctl --user restart "$UNIT_NAME" || {
        echo "install-daemon-supervisor: could not start $UNIT_NAME; inspect 'systemctl --user status $UNIT_NAME'" >&2
        return 2
    }
    echo "install-daemon-supervisor: installed and started $TARGET"
}

cmd_uninstall() {
    if [[ "$PLATFORM" == Linux ]]; then
        cosmon_require_user_manager || exit 1
        if [[ -f "$TARGET" ]] || loaded || systemctl --user is-enabled --quiet "$UNIT_NAME"; then
            if ! systemctl --user disable --now "$UNIT_NAME"; then
                echo "install-daemon-supervisor: could not stop and disable $UNIT_NAME" >&2
                return 2
            fi
        fi
        if [[ -f "$TARGET" ]]; then
            rm -f "$TARGET"
            echo "install-daemon-supervisor: removed $TARGET"
        else
            echo "install-daemon-supervisor: no unit at $TARGET (nothing to remove)"
        fi
        systemctl --user daemon-reload || return 2
        systemctl --user reset-failed "$UNIT_NAME" >/dev/null 2>&1 || true
        return
    fi
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
    if [[ "$PLATFORM" == Linux ]]; then
        linux_install
    else
        cmd_uninstall
        cmd_install
    fi
}

cmd_status() {
    if [[ "$PLATFORM" == Linux ]]; then
        if [[ -f "$TARGET" ]]; then
            echo "unit:    $TARGET"
            cosmon_require_user_manager || exit 1
            systemctl --user show "$UNIT_NAME" \
                -p LoadState -p UnitFileState -p ActiveState -p SubState \
                -p MainPID -p FragmentPath --no-pager
        else
            echo "unit:    (not installed)"
            cosmon_require_user_manager || exit 1
            echo "loaded:  no"
        fi
        return
    fi
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
        install-binary) shift; cmd_install_binary "$@" ;;
        -h|--help|help) usage 0 ;;
        "")        usage 1 ;;
        *)         echo "install-daemon-supervisor: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
