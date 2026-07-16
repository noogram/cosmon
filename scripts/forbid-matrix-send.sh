#!/usr/bin/env bash
# forbid-matrix-send.sh — CI lint for the Matrix ingress bridge.
#
# cosmon-matrix-tick is a READ-ONLY bridge: Matrix → cosmon only. The SDK
# itself exposes `Room::send_*` and friends, and nothing at the protocol
# layer prevents a future contributor from calling them "just to notify
# the operator". This script is the mechanical guard that enforces the
# one-way postman rule.
#
# Rationale:
#   - docs/architectural-invariants.md §8i-extended (semantic acyclicity)
#   - CLAUDE.md §"Channels" (one-way postman rule, JR)
#   - delib-20260422-c4a6 synthesis §C4 "Read-only — no bidirectional bridge"
#
# Bypass: COSMON_SKIP_MATRIX_SEND_LINT=1 (logged, returns 0). Only set
# this on the narrow emergency path; the default posture is enforcement.
#
# Parent deliberation: delib-20260422-c4a6.

set -euo pipefail

if [ "${COSMON_SKIP_MATRIX_SEND_LINT:-}" = "1" ]; then
    echo "forbid-matrix-send: bypassed via COSMON_SKIP_MATRIX_SEND_LINT=1" >&2
    exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE_DIR="$ROOT/crates/cosmon-matrix-tick"

# The bridge crate may not exist yet — this lint ships BEFORE it does, so
# day one any PR that introduces it is already under scrutiny. Treat a
# missing crate directory as a clean pass; fall back to scanning the
# workspace for any stray reintroduction under a different path.
if [ ! -d "$CRATE_DIR" ]; then
    echo "forbid-matrix-send: $CRATE_DIR does not exist yet — clean pass." >&2
    exit 0
fi

# Forbidden call patterns. Each line is an extended-regex fragment that
# grep -E will match literally. Order doesn't matter; we report all hits.
# Keep this list in sync with the delib-20260422-c4a6 synthesis §C4.
FORBIDDEN_PATTERNS=(
    'Client::send\b'
    'Room::send\b'
    'Room::send_raw\b'
    'Room::send_attachment\b'
    '\.send_raw\('
    '\.send_attachment\('
    '\.send_message\('
    '\.send_event\('
    '\.send_state_event\('
    '\bsend_message\('
    '\bsend_event\('
    '\bsend_state_event\('
    '\.send_queue\b'
    'matrix_sdk::send\b'
    'use[[:space:]]+matrix_sdk::[^;]*::send'
)

# Allowed (documented here for review-time clarity; not enforced — these
# just must NOT appear in FORBIDDEN_PATTERNS):
#   - sync_once (the read-side primitive we DO want)
#   - client.matrix_auth() (login / session restore)
#   - room.messages( (history read)

# Scan scope: the crate's src/ and tests/ trees.
SCAN_DIRS=()
for sub in src tests; do
    if [ -d "$CRATE_DIR/$sub" ]; then
        SCAN_DIRS+=("$CRATE_DIR/$sub")
    fi
done

if [ "${#SCAN_DIRS[@]}" -eq 0 ]; then
    echo "forbid-matrix-send: no src/ or tests/ under $CRATE_DIR — clean pass." >&2
    exit 0
fi

# Build a single alternation for a one-pass grep. This keeps the error
# report ordered by file:line rather than by pattern.
alternation="$(IFS='|'; echo "${FORBIDDEN_PATTERNS[*]}")"

hits=""
# grep -n: line numbers. -E: extended regex. --include: Rust only.
# --exclude-dir skips anything vendored. We tolerate grep's rc=1 (no
# match) but surface rc=2 (real error).
set +e
hits=$(grep -rnE --include='*.rs' --exclude-dir=target \
    "$alternation" "${SCAN_DIRS[@]}" 2>/dev/null)
rc=$?
set -e

if [ "$rc" -eq 2 ]; then
    echo "forbid-matrix-send: grep failed (rc=2) — aborting." >&2
    exit 2
fi

if [ -z "$hits" ]; then
    echo "forbid-matrix-send: no forbidden send calls under $CRATE_DIR ✓"
    exit 0
fi

# Re-identify which forbidden pattern matched each hit so the error
# message is self-explanatory.
echo ""
echo "✗ CI LINT FAILED — cosmon-matrix-tick must NEVER post into Matrix."
echo ""
while IFS= read -r line; do
    [ -n "$line" ] || continue
    # grep -n output: <path>:<lineno>:<content>
    file="${line%%:*}"
    rest="${line#*:}"
    lineno="${rest%%:*}"
    content="${rest#*:}"

    matched=""
    for pat in "${FORBIDDEN_PATTERNS[@]}"; do
        if echo "$content" | grep -qE "$pat"; then
            matched="$pat"
            break
        fi
    done

    echo "    File: $file:$lineno"
    echo "    Forbidden: ${matched:-<unknown>}"
    echo "    >>> $content"
    echo ""
done <<< "$hits"

cat <<'EOF'
    Rationale:
    - docs/architectural-invariants.md §8i-extended (semantic acyclicity)
    - CLAUDE.md §"Channels" (one-way postman rule, JR)
    - delib-20260422-c4a6 synthesis §C4 "Read-only — no bidirectional bridge"

    If you need to notify the operator, use cs-api local (Tailscale)
    or a separate "cosmon-speaks" channel — never the ingress room.
EOF

exit 1
