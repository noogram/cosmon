#!/usr/bin/env bash
# uninstall-vllm-mlx-launchagent.sh — companion to install-vllm-mlx-launchagent.sh.
#
# Unloads the dev.cosmon.vllm-mlx LaunchAgent and removes the rendered
# plist. Idempotent: safe to run if nothing is loaded.
#
# Does NOT remove logs or the HuggingFace cache — those are operator
# assets. Pass --purge-logs if you want them gone too.
#
# Usage:
#   uninstall-vllm-mlx-launchagent.sh [--purge-logs]

set -euo pipefail

LABEL="dev.cosmon.vllm-mlx"
TARGET="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG_DIR="${HOME}/Library/Logs"
STDOUT_LOG="${LOG_DIR}/cosmon-vllm-mlx.out.log"
STDERR_LOG="${LOG_DIR}/cosmon-vllm-mlx.err.log"
PURGE_LOGS=0

for arg in "$@"; do
    case "$arg" in
        --purge-logs) PURGE_LOGS=1 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "uninstall-vllm-mlx: unknown flag: $arg" >&2; exit 1 ;;
    esac
done

if launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { found=1 } END { exit !found }'; then
    echo "uninstall-vllm-mlx: unloading ${LABEL}"
    launchctl unload "$TARGET" 2>/dev/null || true
fi

if [[ -f "$TARGET" ]]; then
    echo "uninstall-vllm-mlx: removing ${TARGET}"
    rm -f "$TARGET"
fi

if [[ "$PURGE_LOGS" -eq 1 ]]; then
    rm -f "$STDOUT_LOG" "$STDERR_LOG"
    echo "uninstall-vllm-mlx: removed logs"
fi

echo "uninstall-vllm-mlx: done"
