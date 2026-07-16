#!/usr/bin/env bash
# install-vllm-mlx-launchagent.sh — install the vllm-mlx HTTP sidecar
# LaunchAgent (dev.cosmon.vllm-mlx).
#
# Long-lived local LLM server bound to 127.0.0.1:8000. Exposes both
# OpenAI `/v1/*` and Anthropic `/v1/messages` from one process;
# cosmon-provider::openai and cosmon-provider::anthropic point at this
# endpoint when the operator picks the local-inference path.
#
# Ship date 2026-06-15 — Claude Code billing flip. Path B was chosen
# over the full-Rust internalization on 5/5 panel convergence in
# delib-20260519-f6c3.
#
# ADR-016 posture: vllm-mlx is côté hôte (host-side), not a daemon in
# the Transactional Core. Cosmon consumes it through the OpenAI /
# Anthropic wire schemas — that is the seam discipline ADR-082 demands,
# satisfied by contract rather than uniform language.
#
# Idempotent: re-running with the same arguments leaves the system
# unchanged; re-running with different arguments re-renders + reloads.
#
# Usage:
#   install-vllm-mlx-launchagent.sh \
#       [--model mlx-community/Qwen3-7B-Instruct-4bit] \
#       [--host 127.0.0.1]   # 127.0.0.1 by default (loopback only)
#       [--port 8000] \
#       [--binary /abs/path/to/vllm-mlx] \
#       [--hf-home $HOME/.cache/huggingface] \
#       [--working-dir $HOME/galaxies/cosmon] \
#       [--continuous-batching|--no-continuous-batching] \
#       [--enable-prefix-cache|--no-enable-prefix-cache] \
#       [--max-num-seqs N] \
#       [--extra-arg "--auto-unload-idle-seconds=900"]   # repeatable
#       [--skip-load]                # render only, do not launchctl load
#       [--no-prompt]                # fail instead of prompting
#
# Defaults match the Path B v0 recommendation in the chronicle:
#   - model: mlx-community/Qwen3-7B-Instruct-4bit
#   - host:  127.0.0.1 (loopback; cosmon is a single-operator system)
#   - port:  8000
#   - continuous batching: on
#   - prefix cache: on
#
# Exit codes:
#   0 — success
#   1 — operator error (missing argument, unknown flag)
#   2 — install / launchctl failure
#
# Governing docs:
#   - ADR-016, ADR-082, ADR-103
#   - docs/guides/vllm-mlx-offramp.md (operator runbook)

set -euo pipefail

LABEL="dev.cosmon.vllm-mlx"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
TEMPLATE="${REPO_ROOT}/docker/launchd/${LABEL}.plist"
TARGET_DIR="${HOME}/Library/LaunchAgents"
TARGET="${TARGET_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/Library/Logs"
STDOUT_LOG="${LOG_DIR}/cosmon-vllm-mlx.out.log"
STDERR_LOG="${LOG_DIR}/cosmon-vllm-mlx.err.log"

# Arguments (filled by parse_args).
MODEL=""
HOST=""
PORT=""
BINARY=""
HF_HOME_DIR=""
WORKING_DIR=""
CONTINUOUS_BATCHING=1
ENABLE_PREFIX_CACHE=1
MAX_NUM_SEQS=""
EXTRA_ARGS=()
SKIP_LOAD=0
NO_PROMPT=0

die() {
    echo "install-vllm-mlx: $*" >&2
    exit 1
}

usage() {
    sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --model)                       MODEL="$2";                shift 2 ;;
            --host)                        HOST="$2";                 shift 2 ;;
            --port)                        PORT="$2";                 shift 2 ;;
            --binary)                      BINARY="$2";               shift 2 ;;
            --hf-home)                     HF_HOME_DIR="$2";          shift 2 ;;
            --working-dir)                 WORKING_DIR="$2";          shift 2 ;;
            --continuous-batching)         CONTINUOUS_BATCHING=1;     shift ;;
            --no-continuous-batching)      CONTINUOUS_BATCHING=0;     shift ;;
            --enable-prefix-cache)         ENABLE_PREFIX_CACHE=1;     shift ;;
            --no-enable-prefix-cache)      ENABLE_PREFIX_CACHE=0;     shift ;;
            --max-num-seqs)                MAX_NUM_SEQS="$2";         shift 2 ;;
            --extra-arg)                   EXTRA_ARGS+=("$2");        shift 2 ;;
            --skip-load)                   SKIP_LOAD=1;               shift ;;
            --no-prompt)                   NO_PROMPT=1;               shift ;;
            -h|--help|help)                usage 0 ;;
            *) echo "install-vllm-mlx: unknown flag: $1" >&2; usage 1 ;;
        esac
    done
}

prompt_if_missing() {
    local varname="$1" label="$2" default="${3:-}"
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

resolve_binary() {
    if [[ -n "$BINARY" ]]; then
        [[ -x "$BINARY" ]] || die "binary not executable: $BINARY"
        return 0
    fi

    if command -v vllm-mlx >/dev/null 2>&1; then
        BINARY="$(command -v vllm-mlx)"
        return 0
    fi

    die "vllm-mlx not on PATH — run 'uv tool install vllm-mlx --python 3.13' first (or pass --binary)"
}

resolve_defaults() {
    [[ -n "$MODEL" ]] || MODEL="mlx-community/Qwen3-7B-Instruct-4bit"
    [[ -n "$HOST"  ]] || HOST="127.0.0.1"
    [[ -n "$PORT"  ]] || PORT="8000"

    [[ -n "$WORKING_DIR" ]] || WORKING_DIR="${HOME}/galaxies/cosmon"
    [[ -d "$WORKING_DIR" ]] || die "working directory does not exist: $WORKING_DIR"

    [[ -n "$HF_HOME_DIR" ]] || HF_HOME_DIR="${HOME}/.cache/huggingface"
    mkdir -p "$HF_HOME_DIR"
    mkdir -p "$LOG_DIR"
}

write_extra_args_file() {
    # Compose the variable-length list of `vllm-mlx serve` flags as one
    # block of `<string>…</string>` plist lines, one per line, to a temp
    # file. The render step pastes those lines in place of the
    # __EXTRA_ARGS_LINES__ marker. This sidesteps the macOS awk
    # newline-in-variable limitation: we read the block from disk.
    local out="$1"
    : > "$out"
    if [[ "$CONTINUOUS_BATCHING" -eq 1 ]]; then
        printf '        <string>--continuous-batching</string>\n' >> "$out"
    fi
    if [[ "$ENABLE_PREFIX_CACHE" -eq 1 ]]; then
        printf '        <string>--enable-prefix-cache</string>\n' >> "$out"
    fi
    if [[ -n "$MAX_NUM_SEQS" ]]; then
        printf '        <string>--max-num-seqs</string>\n' >> "$out"
        printf '        <string>%s</string>\n' "$MAX_NUM_SEQS" >> "$out"
    fi
    local arg
    for arg in "${EXTRA_ARGS[@]:-}"; do
        [[ -z "$arg" ]] && continue
        printf '        <string>%s</string>\n' "$arg" >> "$out"
    done
}

render() {
    [[ -f "$TEMPLATE" ]] || die "template not found: $TEMPLATE"

    local extra_file
    extra_file="$(mktemp -t vllm-mlx-extra.XXXXXX)"
    write_extra_args_file "$extra_file"

    # awk paste: on lines containing __EXTRA_ARGS_LINES__, splice the
    # extras file in place (stripped of any trailing newline). On all
    # other lines, do the scalar substitutions and emit. This keeps
    # newlines out of awk's -v assignment, which macOS awk forbids.
    awk -v home="$HOME" \
        -v binary="$BINARY" \
        -v model="$MODEL" \
        -v host="$HOST" \
        -v port="$PORT" \
        -v hfhome="$HF_HOME_DIR" \
        -v wd="$WORKING_DIR" \
        -v stdout_log="$STDOUT_LOG" \
        -v stderr_log="$STDERR_LOG" \
        -v extra_file="$extra_file" \
    '{
        if (index($0, "__EXTRA_ARGS_LINES__")) {
            line = ""
            while ((getline tmp < extra_file) > 0) {
                if (line == "") { line = tmp } else { line = line "\n" tmp }
            }
            close(extra_file)
            sub(/__EXTRA_ARGS_LINES__/, line)
            print
            next
        }
        gsub(/__HOME__/, home);
        gsub(/__BINARY__/, binary);
        gsub(/__MODEL__/, model);
        gsub(/__HOST__/, host);
        gsub(/__PORT__/, port);
        gsub(/__HF_HOME__/, hfhome);
        gsub(/__WORKING_DIRECTORY__/, wd);
        gsub(/__STDOUT_LOG__/, stdout_log);
        gsub(/__STDERR_LOG__/, stderr_log);
        print
    }' "$TEMPLATE"

    rm -f "$extra_file"
}

loaded() {
    launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'
}

unload_if_loaded() {
    if loaded; then
        echo "install-vllm-mlx: unloading previous instance"
        launchctl unload "$TARGET" 2>/dev/null || true
    fi
}

load_agent() {
    if [[ "$SKIP_LOAD" -eq 1 ]]; then
        echo "install-vllm-mlx: --skip-load — rendered plist at $TARGET (not loaded)"
        return 0
    fi

    if ! launchctl load "$TARGET"; then
        local rc=$?
        echo "install-vllm-mlx: launchctl load failed (rc=$rc)" >&2
        exit 2
    fi

    if loaded; then
        echo "install-vllm-mlx: loaded — listening on ${HOST}:${PORT}"
        echo "install-vllm-mlx: logs at ${STDOUT_LOG} / ${STDERR_LOG}"
    else
        echo "install-vllm-mlx: loaded but launchctl list does not show it" >&2
        exit 2
    fi
}

probe_models() {
    # Best-effort. Model load can take 60s+ for cold cache; we don't
    # fail hard if it's not up yet — the operator's `cs vllm-mlx health`
    # command (or the chronicle's `curl /v1/models` snippet) is the
    # canonical readiness probe.
    if [[ "$SKIP_LOAD" -eq 1 ]]; then return 0; fi
    if ! command -v curl >/dev/null 2>&1; then return 0; fi

    local waited=0
    while [[ $waited -lt 15 ]]; do
        if curl -fsS "http://${HOST}:${PORT}/v1/models" >/dev/null 2>&1; then
            echo "install-vllm-mlx: /v1/models ok on ${HOST}:${PORT}"
            return 0
        fi
        sleep 2
        waited=$((waited + 2))
    done

    cat >&2 <<EOF
install-vllm-mlx: /v1/models not responding after 15s on ${HOST}:${PORT}
    → cold model load can take longer; tail ${STDERR_LOG} for progress.
    → re-probe later with: curl http://${HOST}:${PORT}/v1/models
EOF
}

main() {
    parse_args "$@"
    resolve_defaults
    resolve_binary

    mkdir -p "$TARGET_DIR"
    unload_if_loaded
    render > "$TARGET"
    chmod 0644 "$TARGET"
    load_agent
    probe_models

    cat <<EOF

install-vllm-mlx: install complete
  label:    ${LABEL}
  binary:   ${BINARY}
  model:    ${MODEL}
  bind:     ${HOST}:${PORT}
  HF_HOME:  ${HF_HOME_DIR}
  logs:     ${STDOUT_LOG}
            ${STDERR_LOG}

To verify health:
  cs vllm-mlx health           # (subcommand introduced by task-20260519-2a75)
  curl http://${HOST}:${PORT}/v1/models | jq

Cosmon config (point providers at the local endpoint):
  export OPENAI_BASE_URL=http://${HOST}:${PORT}
  export OPENAI_API_KEY=local-llama        # any non-empty string
  export ANTHROPIC_BASE_URL=http://${HOST}:${PORT}
  export ANTHROPIC_API_KEY=local-llama

Uninstall:
  scripts/uninstall-vllm-mlx-launchagent.sh
EOF
}

main "$@"
