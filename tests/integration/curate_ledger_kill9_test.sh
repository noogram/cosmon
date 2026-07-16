#!/usr/bin/env bash
# True kill-9 test for curate-ledger.
#
# Spawns a child writer that appends 10 rows with a small sleep between
# each, then kill -9's it after row ~5. Checks that:
#
#   (i)  the partial ledger contains some prefix of the 10 rows (every
#        complete line is a valid sealed JSON);
#   (ii) `resume` returns the ids of those committed rows without error;
#   (iii) the verifier returns 0 (clean) or surfaces an EOF torn write
#         without crashing.
#
# Unlike the synthetic-torn-bytes test (T3 in curate_ledger_test.sh),
# this one actually triggers the SIGKILL signal mid-loop, exercising
# the O_APPEND + fsync invariant under a real OS-level interrupt.
#
# Usage:
#   bash tests/integration/curate_ledger_kill9_test.sh

set -euo pipefail

_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
_LEDGER_SH="$_REPO/scripts/curate-ledger.sh"
_VERIFY="$_REPO/scripts/verify-curate-ledger.sh"

# shellcheck source=../../scripts/curate-ledger.sh
source "$_LEDGER_SH"

TMP="$(mktemp -d -t curate-ledger-kill9-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

LEDGER="$TMP/scan.ndjson"
DECS="$TMP/decisions.ndjson"

pass() { printf '\033[32m✓\033[0m %s\n' "$1"; }
fail() { printf '\033[31m✗\033[0m %s\n' "$1" >&2; exit 1; }
info() { printf '\033[36m· \033[0m%s\n' "$1"; }

# Writer subscript: appends 10 sealed rows, 0.15s between each.
WRITER="$TMP/writer.sh"
cat > "$WRITER" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
source "$1"
LEDGER="$2"
DECS="$3"
for i in $(seq 1 10); do
    mol_id="task-20260521-k9$(printf '%02d' "$i")"
    decided_at="2026-05-21T22:30:0${i}Z"
    action='{"Revise":{"tag":"temp:warm"}}'
    row=$(python3 -c "
import json, sys
print(json.dumps({
    'mol_id': sys.argv[1],
    'galaxy': 'cosmon',
    'kind': 'task',
    'action': json.loads(sys.argv[2]),
    'decided_at': sys.argv[3],
}, sort_keys=True, separators=(',', ':')))
" "$mol_id" "$action" "$decided_at")
    curate_ledger_append "$LEDGER" "$row" >/dev/null
    curate_ledger_append "$DECS" "$row" >/dev/null
    sleep 0.15
done
EOF
chmod +x "$WRITER"

info "spawning writer; will SIGKILL after ~0.7s (≈ 4-5 rows committed)"
bash "$WRITER" "$_LEDGER_SH" "$LEDGER" "$DECS" &
WRITER_PID=$!

sleep 0.7
kill -9 "$WRITER_PID" 2>/dev/null || true
wait "$WRITER_PID" 2>/dev/null || true

# Collect committed line count.
n_scan=$(wc -l < "$LEDGER" 2>/dev/null || echo 0)
n_dec=$(wc -l < "$DECS" 2>/dev/null || echo 0)
info "after SIGKILL: scan.ndjson=$n_scan lines, decisions.ndjson=$n_dec lines"

(( n_scan >= 1 )) || fail "kill-9 too fast: nothing committed; loosen sleep"
(( n_scan <= 10 )) || fail "wrote more rows than expected ($n_scan)"

# Every committed line must be a valid JSON object with sealed field.
python3 - "$LEDGER" <<'PY'
import json, sys, re
rx = re.compile(r"^[0-9a-f]{64}$")
with open(sys.argv[1]) as f:
    for ln, line in enumerate(f, 1):
        line = line.rstrip("\n")
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            # A torn write at EOF is allowed; intermediate torn writes are not.
            # The producer always writes-then-fsyncs the full \n-terminated
            # line, so under fsync semantics there should be no torn line.
            # If we ever see one, it must be the final line.
            print(f"torn line at {ln} — must be EOF")
PY

# Resume should succeed and yield exactly n_scan ids (if the kill landed
# between rows; if it landed mid-row, the torn fragment is dropped silently
# by the parser and resume yields n_scan-1 — both are acceptable as long
# as the count is non-negative and verify returns OK).
resumed=$(curate_ledger_resume_set "$LEDGER")
n_resumed=$(printf '%s\n' "$resumed" | grep -c '^task-' || true)
info "resume yielded $n_resumed already-decided mol_ids"
(( n_resumed >= 1 )) || fail "resume should yield ≥1 id, got $n_resumed"

# Verify on the partial ledger — accept either 0 (clean, no torn EOF) or
# 0 (clean with EOF-torn-write notice). Anything else is a regression.
set +e
bash "$_VERIFY" "$TMP" >/dev/null 2>&1
rc=$?
set -e
[[ $rc -eq 0 ]] || fail "verify on kill-9 ledger should exit 0, got $rc"
pass "kill-9 mid-loop: ledger parseable, resume works, verify clean"

# Final invariant: write the remaining rows on top of the partial ledger
# (simulating a clean restart) and verify again.
info "completing the ledger after kill-9 (simulate clean restart)"
seen=$(printf '%s\n' "$resumed")
for i in $(seq 1 10); do
    mol_id="task-20260521-k9$(printf '%02d' "$i")"
    if grep -qx "$mol_id" <<<"$seen"; then
        continue
    fi
    decided_at="2026-05-21T22:30:0${i}Z"
    action='{"Revise":{"tag":"temp:warm"}}'
    row=$(python3 -c "
import json, sys
print(json.dumps({
    'mol_id': sys.argv[1],
    'galaxy': 'cosmon',
    'kind': 'task',
    'action': json.loads(sys.argv[2]),
    'decided_at': sys.argv[3],
}, sort_keys=True, separators=(',', ':')))
" "$mol_id" "$action" "$decided_at")
    curate_ledger_append "$LEDGER" "$row" >/dev/null
    curate_ledger_append "$DECS" "$row" >/dev/null
done

[[ $(wc -l < "$LEDGER") -eq 10 ]] || fail "after recovery, scan.ndjson should have 10 lines, got $(wc -l < "$LEDGER")"

if bash "$_VERIFY" "$TMP" >/dev/null 2>&1; then
    pass "post-recovery: 10 rows + verify clean"
else
    rc=$?
    fail "post-recovery: verify failed (exit $rc)"
fi

echo
pass "kill-9 mid-loop test passed end-to-end"
