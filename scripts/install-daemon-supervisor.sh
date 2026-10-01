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
#   scripts/install-daemon-supervisor.sh install-binary <source> — copy and sign binary; never restart
#
# Exit codes:
#   0 — success
#   1 — operator error (missing template, bad args, unknown command)
#   2 — launchctl error

set -euo pipefail

LABEL="com.cosmon.daemon-supervisor"
SIGNING_IDENTITY="${COSMON_SUPERVISOR_SIGNING_IDENTITY:-Cosmon Local Signing}"
BIN_DIR="${COSMON_SUPERVISOR_BIN_DIR:-${HOME}/.local/bin}"
BIN="${BIN_DIR}/cosmon-daemon-supervisor"
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
        install-binary) shift; cmd_install_binary "$@" ;;
        -h|--help|help) usage 0 ;;
        "")        usage 1 ;;
        *)         echo "install-daemon-supervisor: unknown command: $1" >&2; usage 1 ;;
    esac
}

main "$@"
