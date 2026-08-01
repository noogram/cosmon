#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
#
# The same receipt as `ack_hook.py`, written in POSIX sh — because measuring
# the Python version showed that almost all of the receipt's latency is the
# interpreter starting up, not Claude Code dispatching the hook.
#
# Measured on this host: `/usr/bin/env python3 ack_hook.py` costs ~400 ms
# median and over a second at the tail (a pyenv shim, then Python's own
# startup). Claude Code's hook timeout is a few seconds, so under load that
# tail is long enough to lose receipts outright — which is exactly what the
# first matrix saw. This script exists to separate "the mechanism is slow"
# from "our implementation of it is slow".
#
# It is deliberately weaker than the Python version: it records the nonce and
# nothing from the payload, because parsing JSON in sh would cost more than it
# is worth. In production the right shape is neither of these — it is a
# subcommand of the already-compiled `cs` binary, invoked by absolute path.
#
# The same five properties hold:
#   * stdout goes to /dev/null for the whole script, on the first line;
#   * stdin is drained, so Claude Code never writes into a closed pipe;
#   * the receipt is written to a temp file and renamed, so it is never
#     half-read;
#   * the nonce is filtered to a filename-safe alphabet before it is used
#     as one;
#   * every path exits 0 — a receipt hook must never be able to block a prompt.

exec 1>/dev/null

# Drain the payload without storing, parsing, or logging any of it.
cat >/dev/null 2>&1

nonce=$(head -n 1 "${COSMON_RECEIPT_NONCE_FILE:-/nonexistent}" 2>/dev/null |
    tr -cd 'A-Za-z0-9_-' | cut -c1-64)
[ -n "$nonce" ] || nonce="nokey"

dir="${COSMON_RECEIPT_DIR:-}"
if [ -n "$dir" ] && [ -d "$dir" ]; then
    tmp="$dir/.ack-tmp-$$"
    if printf '{"nonce":"%s","event":"UserPromptSubmit"}\n' "$nonce" >"$tmp" 2>/dev/null; then
        mv "$tmp" "$dir/ack-$nonce.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null
    fi
fi

exit 0
