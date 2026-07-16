#!/usr/bin/env bash
# install-cs-api-launchagent.sh — install the `cs-api` LaunchAgent
# (dev.noogram.cosmon.cs-api).
#
# Long-lived local HTTP adapter. `KeepAlive = true` so launchd re-spawns
# it on crash; `RunAtLoad = true` so it starts at login/boot. Unlike
# `matrix-tick` (one-shot, launchd-as-alarm), this agent is resident.
#
# ADR-016 posture: `cs-api` is an adapter (HTTP shell-out to the `cs`
# CLI), NOT a daemon in the Transactional Core. It holds no state; every
# request spawns a fresh `cs` process. Keeping it resident is operator
# convenience for native pilots (mac-pilot, ios-pilot, tailnet tablets).
#
# This script stitches together:
#
#   - operator-supplied arguments (bind address, cosmon state, etc.)
#   - the committed plist template (docker/launchd/*.plist)
#   - the rendered, operator-specific plist in ~/Library/LaunchAgents/
#   - `launchctl unload` / `launchctl load` to (re)activate it
#
# The script is idempotent: running it twice with the same arguments
# leaves the system in the same state. Re-running with different
# arguments re-renders the plist and reloads it. Existing logs are
# never touched.
#
# Usage:
#   install-cs-api-launchagent.sh \
#       [--bind 127.0.0.1:4222]      # loopback by default; Tailscale for remote
#       [--cs-path /abs/path/to/cs]  # default: $(command -v cs)
#       [--cosmon-state /abs/path]   # default: $HOME/galaxies/cosmon/.cosmon/state
#       [--whispers-inbox /abs/path] # default: <cosmon-state parent>/whispers/inbox
#       [--galaxies-root /abs/path]  # default: $HOME/galaxies
#       [--working-dir /abs/path]    # default: $HOME/galaxies/cosmon (walk-up root)
#       [--binary /abs/path/to/cs-api]
#       [--verbose]                  # enable --verbose on the binary
#       [--skip-load]                # render only, do not launchctl load
#       [--no-prompt]                # fail instead of prompting for missing args
#
# Bind address prompt:
#   Default `127.0.0.1:4222` — loopback only, unreachable from other
#   machines. For tailnet-accessible deployment, pass e.g.
#   `--bind 0.0.0.0:4222` (listen on all interfaces; access is gated by
#   the tailnet/firewall) or a specific Tailscale IP like
#   `--bind 100.x.x.x:4222`. See docs/guides/cs-api.md §Security.
#
# Exit codes:
#   0 — success
#   1 — operator error (missing argument, unknown flag)
#   2 — build / install failure (cargo, launchctl)
#
# Governing docs:
#   - ADR-016 — autonomy regimes (cs-api is an adapter, not a daemon)
#   - docs/guides/cs-api.md (endpoint surface + security invariants)
#   - docs/guides/launchagent-cs-api.md (operator runbook)

set -euo pipefail

LABEL="dev.noogram.cosmon.cs-api"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
TEMPLATE="${REPO_ROOT}/docker/launchd/${LABEL}.plist"
TARGET_DIR="${HOME}/Library/LaunchAgents"
TARGET="${TARGET_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/Library/Logs"
STDOUT_LOG="${LOG_DIR}/cosmon-cs-api.out.log"
STDERR_LOG="${LOG_DIR}/cosmon-cs-api.err.log"

# Arguments (filled by parse_args).
BIND=""
CS_PATH=""
COSMON_STATE=""
WHISPERS_INBOX=""
GALAXIES_ROOT=""
WORKING_DIR=""
BINARY=""
VERBOSE=0
SKIP_LOAD=0
NO_PROMPT=0

die() {
    echo "install-cs-api: $*" >&2
    exit 1
}

usage() {
    sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --bind)            BIND="$2";            shift 2 ;;
            --cs-path)         CS_PATH="$2";         shift 2 ;;
            --cosmon-state)    COSMON_STATE="$2";    shift 2 ;;
            --whispers-inbox)  WHISPERS_INBOX="$2";  shift 2 ;;
            --galaxies-root)   GALAXIES_ROOT="$2";   shift 2 ;;
            --working-dir)     WORKING_DIR="$2";     shift 2 ;;
            --binary)          BINARY="$2";          shift 2 ;;
            --verbose)         VERBOSE=1;            shift ;;
            --skip-load)       SKIP_LOAD=1;          shift ;;
            --no-prompt)       NO_PROMPT=1;          shift ;;
            -h|--help|help)    usage 0 ;;
            *) echo "install-cs-api: unknown flag: $1" >&2; usage 1 ;;
        esac
    done
}

prompt_if_missing() {
    local varname="$1" label="$2" default="${3:-}"
    # shellcheck disable=SC3028  # indirect expansion is bash-only, which is our shebang.
    local current="${!varname}"
    if [[ -n "$current" ]]; then
        return 0
    fi
    if [[ "$NO_PROMPT" -eq 1 ]]; then
        if [[ -n "$default" ]]; then
            printf -v "$varname" '%s' "$default"
            return 0
        fi
        die "missing required argument: $label (and --no-prompt was set)"
    fi
    if [[ -n "$default" ]]; then
        read -r -p "$label [$default]: " value
        value="${value:-$default}"
    else
        read -r -p "$label: " value
    fi
    [[ -n "$value" ]] || die "empty value for: $label"
    printf -v "$varname" '%s' "$value"
}

ensure_binary() {
    if [[ -n "$BINARY" ]]; then
        [[ -x "$BINARY" ]] || die "binary not executable: $BINARY"
        return 0
    fi

    if command -v cs-api >/dev/null 2>&1; then
        BINARY="$(command -v cs-api)"
        return 0
    fi

    echo "install-cs-api: cs-api not in PATH — building from source"
    (
        cd "$REPO_ROOT"
        cargo build --release -p cosmon-api --bin cs-api
    ) || { echo "install-cs-api: cargo build failed" >&2; exit 2; }

    local built="${REPO_ROOT}/target/release/cs-api"
    [[ -x "$built" ]] || die "build succeeded but binary not found at $built"

    if [[ -d "${HOME}/.local/bin" ]]; then
        echo "install-cs-api: installing binary to \$HOME/.local/bin"
        install -m 0755 "$built" "${HOME}/.local/bin/cs-api"
        BINARY="${HOME}/.local/bin/cs-api"
    else
        echo "install-cs-api: using freshly-built $built (no \$HOME/.local/bin)"
        BINARY="$built"
    fi
}

resolve_cs_path() {
    if [[ -n "$CS_PATH" ]]; then
        [[ -x "$CS_PATH" ]] || die "cs binary not executable: $CS_PATH"
        return 0
    fi

    if command -v cs >/dev/null 2>&1; then
        CS_PATH="$(command -v cs)"
        return 0
    fi

    die "cs binary not found on PATH — pass --cs-path explicitly (install cosmon-cli first)"
}

resolve_defaults() {
    # Working directory: default to cosmon checkout so walk-up finds
    # `.cosmon/` when --cosmon-state is omitted.
    [[ -n "$WORKING_DIR" ]] || WORKING_DIR="${HOME}/galaxies/cosmon"
    [[ -d "$WORKING_DIR" ]] || die "working directory does not exist: $WORKING_DIR"

    [[ -n "$COSMON_STATE"   ]] || COSMON_STATE="${WORKING_DIR}/.cosmon/state"
    [[ -n "$WHISPERS_INBOX" ]] || WHISPERS_INBOX="${WORKING_DIR}/.cosmon/whispers/inbox"
    [[ -n "$GALAXIES_ROOT"  ]] || GALAXIES_ROOT="${HOME}/galaxies"

    # Parent dirs we can create up-front (idempotent, mkdir -p never
    # clobbers). State dir may legitimately not exist yet on a fresh
    # workspace — don't error.
    [[ -d "$GALAXIES_ROOT" ]] || die "galaxies root does not exist: $GALAXIES_ROOT"
}

render() {
    [[ -f "$TEMPLATE" ]] || die "template not found: $TEMPLATE"

    # Verbose flag is either the literal `<string>--verbose</string>`
    # element or an empty string — plist arrays can't hold a conditional,
    # so we inject a full XML element or nothing.
    local verbose_frag=""
    if [[ "$VERBOSE" -eq 1 ]]; then
        verbose_frag="<string>--verbose</string>"
    fi

    # `sed -e` with separate expressions keeps each substitution readable
    # and lets any value contain `/` safely (we use `|` as the delimiter).
    sed \
        -e "s|__HOME__|${HOME}|g" \
        -e "s|__BINARY__|${BINARY}|g" \
        -e "s|__BIND__|${BIND}|g" \
        -e "s|__CS_PATH__|${CS_PATH}|g" \
        -e "s|__COSMON_STATE__|${COSMON_STATE}|g" \
        -e "s|__WHISPERS_INBOX__|${WHISPERS_INBOX}|g" \
        -e "s|__GALAXIES_ROOT__|${GALAXIES_ROOT}|g" \
        -e "s|__WORKING_DIRECTORY__|${WORKING_DIR}|g" \
        -e "s|__VERBOSE_FLAG__|${verbose_frag}|g" \
        -e "s|__STDOUT_LOG__|${STDOUT_LOG}|g" \
        -e "s|__STDERR_LOG__|${STDERR_LOG}|g" \
        "$TEMPLATE"
}

loaded() {
    launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
}

unload_if_loaded() {
    if loaded; then
        echo "install-cs-api: unloading previous instance"
        launchctl unload "$TARGET" 2>/dev/null || true
    fi
}

load_agent() {
    if [[ "$SKIP_LOAD" -eq 1 ]]; then
        echo "install-cs-api: --skip-load — rendered plist at $TARGET (not loaded)"
        return 0
    fi

    if ! launchctl load "$TARGET"; then
        local rc=$?
        echo "install-cs-api: launchctl load failed (rc=$rc)" >&2
        exit 2
    fi

    if loaded; then
        echo "install-cs-api: loaded — listening on ${BIND}"
        echo "install-cs-api: logs at ${STDOUT_LOG} / ${STDERR_LOG}"
    else
        echo "install-cs-api: loaded but launchctl list does not show it" >&2
        exit 2
    fi
}

probe_healthz() {
    # Non-blocking verification. KeepAlive + RunAtLoad means the agent
    # is live within a few seconds; `/healthz` is the canonical probe.
    # We don't fail hard if it's not up yet — the operator can `tail`
    # the logs themselves.
    if [[ "$SKIP_LOAD" -eq 1 ]]; then
        return 0
    fi

    if ! command -v curl >/dev/null 2>&1; then
        echo "install-cs-api: curl not found — skipping health probe"
        return 0
    fi

    # Strip bind's `0.0.0.0` / `100.x` and probe localhost on the same
    # port; the agent listens on all interfaces for `0.0.0.0` anyway.
    local port
    port="${BIND##*:}"
    [[ "$port" =~ ^[0-9]+$ ]] || { echo "install-cs-api: cannot parse port from --bind ${BIND}"; return 0; }

    local waited=0
    while [[ $waited -lt 10 ]]; do
        if curl -fsS "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1; then
            echo "install-cs-api: /healthz ok on 127.0.0.1:${port}"
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done

    cat >&2 <<EOF
install-cs-api: /healthz not responding after 10s on 127.0.0.1:${port}
    → check ${STDERR_LOG}. Common errors: bind already in use (pick
      another port), cs not on PATH (pass --cs-path), or the binary
      crashed on startup. See docs/guides/launchagent-cs-api.md
      troubleshooting table.
EOF
}

main() {
    parse_args "$@"

    # Interactive fill. Bind is the one choice worth confirming because
    # it drives security posture (loopback vs tailnet).
    prompt_if_missing BIND "Bind address (loopback: 127.0.0.1:4222 / tailnet: 0.0.0.0:4222)" "127.0.0.1:4222"

    resolve_defaults
    ensure_binary
    resolve_cs_path

    mkdir -p "$TARGET_DIR" "$LOG_DIR"

    unload_if_loaded

    local rendered
    rendered="$(render)"
    printf '%s\n' "$rendered" > "$TARGET"
    echo "install-cs-api: rendered $TARGET"

    load_agent
    probe_healthz
}

main "$@"
