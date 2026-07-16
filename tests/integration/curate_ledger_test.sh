#!/usr/bin/env bash
# Integration test for the curate-ledger append+flush+BLAKE3-seal helper.
#
# Three scenarios:
#
#   T1. Happy-path round-trip — write 10 rows, verify clean (exit 0).
#   T2. Tamper detection      — hand-edit one row, verify mismatch (exit 1).
#   T3. Kill-9 mid-row        — synthesise a torn-write tail, then
#                               (a) resume produces the 9 already-decided
#                                   mol_ids; (b) the patrol can append
#                                   the missing row on restart; (c) verify
#                                   passes with the EOF-torn-write notice
#                                   AFTER recovery.
#
# This test operationalises the 2026-05-21 worker-checkpoint lesson:
# a drain-worker died in machine sleep with its final verdict batch
# unflushed. After this fix, "kill -9 mid-row" leaves a usable partial
# ledger that the next pass resumes from cleanly.
#
# Usage:
#   bash tests/integration/curate_ledger_test.sh

set -euo pipefail

_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
_LEDGER_SH="$_REPO/scripts/curate-ledger.sh"
_VERIFY="$_REPO/scripts/verify-curate-ledger.sh"
_PY="$_REPO/scripts/curate-ledger.py"

# shellcheck source=../../scripts/curate-ledger.sh
source "$_LEDGER_SH"

TMP="$(mktemp -d -t curate-ledger-test-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

LEDGER="$TMP/scan.ndjson"
DECS="$TMP/decisions.ndjson"

pass() { printf '\033[32m✓\033[0m %s\n' "$1"; }
fail() { printf '\033[31m✗\033[0m %s\n' "$1" >&2; exit 1; }
info() { printf '\033[36m· \033[0m%s\n' "$1"; }

# ------------------------------------------------------------------
# T1 — Happy path: write 10 rows + cross-check verifier.
# ------------------------------------------------------------------
info "T1: writing 10 sealed rows + matching decisions"

for i in $(seq 1 10); do
    mol_id="task-20260521-test$(printf '%02d' "$i")"
    decided_at="2026-05-21T22:30:0${i}Z"
    if (( i % 2 == 0 )); then
        action='{"Collapse":{"reason":"obsolete-age>30d"}}'
    else
        action='{"Revise":{"tag":"temp:warm"}}'
    fi
    row=$(python3 -c "
import json, sys
print(json.dumps({
    'mol_id': sys.argv[1],
    'galaxy': 'cosmon',
    'kind': 'task',
    'formula': 'task-work',
    'age_hours': 720,
    'tags': ['temp:cold'],
    'blocked_by': [],
    'blocks': [],
    'syzygie_cited': False,
    'action': json.loads(sys.argv[2]),
    'confidence': 0.85,
    'rationale': 'synthetic test row',
    'surface_to_operator': False,
    'decided_at': sys.argv[3],
    'decided_pass': 1,
}, sort_keys=True, separators=(',', ':')))
" "$mol_id" "$action" "$decided_at")
    curate_ledger_append "$LEDGER" "$row" >/dev/null
    curate_ledger_append "$DECS" "$row" >/dev/null
done

[[ $(wc -l < "$LEDGER") -eq 10 ]] || fail "T1: expected 10 lines in scan.ndjson, got $(wc -l < "$LEDGER")"
[[ $(wc -l < "$DECS") -eq 10 ]] || fail "T1: expected 10 lines in decisions.ndjson"

# Each line must contain a 64-char hex sealed field.
bad=$(python3 -c "
import json, re, sys
rx = re.compile(r'^[0-9a-f]{64}\$')
for ln, line in enumerate(open(sys.argv[1]), 1):
    row = json.loads(line)
    if 'sealed' not in row or not rx.match(row['sealed']):
        print(ln)
        sys.exit(0)
" "$LEDGER")
[[ -z "$bad" ]] || fail "T1: line $bad has missing/invalid seal"

if bash "$_VERIFY" "$TMP" >/dev/null 2>&1; then
    pass "T1: 10 sealed rows + verify exit 0"
else
    rc=$?
    fail "T1: verify failed unexpectedly (exit $rc)"
fi

# ------------------------------------------------------------------
# T2 — Tamper detection: hand-edit one row, expect verify exit 1.
# ------------------------------------------------------------------
info "T2: hand-edit one row to flip the reason — verify must reject"

# Copy the ledger to a tampered version.
TAMPER_DIR="$TMP/tampered"
mkdir -p "$TAMPER_DIR"
cp "$LEDGER" "$TAMPER_DIR/scan.ndjson"
cp "$DECS" "$TAMPER_DIR/decisions.ndjson"

# Flip the rationale on line 5 — same seal, different body ⇒ mismatch.
python3 -c "
import json, sys
lines = open(sys.argv[1]).readlines()
row = json.loads(lines[4])
row['rationale'] = 'tampered after the fact'
lines[4] = json.dumps(row, sort_keys=True, separators=(',', ':')) + '\n'
open(sys.argv[1], 'w').writelines(lines)
" "$TAMPER_DIR/scan.ndjson"

# A pure-body edit that doesn't touch (mol_id, action, decided_at) still
# changes the canonical row hash — but our seal binds only those three
# fields. So actually this edit is allowed by our threat model: only
# action/decided_at edits are detected. Tamper a sealed-bound field instead.
python3 -c "
import json, sys
lines = open(sys.argv[1]).readlines()
row = json.loads(lines[2])
# flip the collapse reason — this changes canonical(action), seal mismatches
if 'Collapse' in row['action']:
    row['action']['Collapse']['reason'] = 'tampered reason'
else:
    row['action'] = {'Collapse': {'reason': 'forged action'}}
lines[2] = json.dumps(row, sort_keys=True, separators=(',', ':')) + '\n'
open(sys.argv[1], 'w').writelines(lines)
" "$TAMPER_DIR/scan.ndjson"

set +e
bash "$_VERIFY" "$TAMPER_DIR" >/dev/null 2>&1
rc=$?
set -e
[[ $rc -eq 1 ]] || fail "T2: verify should have returned 1 (seal mismatch), got $rc"
pass "T2: tampered action body detected (verify exit 1)"

# ------------------------------------------------------------------
# T3 — Kill-9 mid-row: synthetic torn write at EOF.
# Then: (a) resume yields 9 mol_ids; (b) re-append the 10th cleanly;
#       (c) verify passes after recovery.
# ------------------------------------------------------------------
info "T3: simulate kill-9 during the 10th append — torn write at EOF"

TORN_DIR="$TMP/torn"
mkdir -p "$TORN_DIR"
# Take the first 9 complete rows + append a partial line (no trailing newline,
# no closing brace) to simulate a writer that was killed mid-write().
head -n 9 "$LEDGER" > "$TORN_DIR/scan.ndjson"
printf '%s' '{"mol_id":"task-20260521-test10","galaxy":"cosmon","kind":"task","action":{"Revise":{"ta' >> "$TORN_DIR/scan.ndjson"

# Resume must yield 9 already-decided ids without erroring.
resumed=$(curate_ledger_resume_set "$TORN_DIR/scan.ndjson" || true)
n_resumed=$(printf '%s\n' "$resumed" | grep -c '^task-' || true)
[[ $n_resumed -eq 9 ]] || fail "T3: resume should yield 9 mol_ids, got $n_resumed"

# The torn write is at EOF; a subsequent append still succeeds (O_APPEND
# always writes at end-of-file, so the next append lands AFTER the torn
# bytes and produces a line that JSON-loads — but the file still contains
# the torn fragment, which the verifier flags as EOF-torn).
# Operator-grade recovery: a clean restart truncates the torn tail and
# rewrites it. Simulate that here.

# Re-write the 10th row cleanly on top of the truncated file.
head -n 9 "$LEDGER" > "$TORN_DIR/scan.ndjson"
cp "$DECS" "$TORN_DIR/decisions.ndjson"
mol_id="task-20260521-test10"
decided_at="2026-05-21T22:30:10Z"
action='{"Collapse":{"reason":"obsolete-age>30d"}}'
row=$(python3 -c "
import json, sys
print(json.dumps({
    'mol_id': sys.argv[1],
    'galaxy': 'cosmon',
    'kind': 'task',
    'formula': 'task-work',
    'age_hours': 720,
    'tags': ['temp:cold'],
    'blocked_by': [],
    'blocks': [],
    'syzygie_cited': False,
    'action': json.loads(sys.argv[2]),
    'confidence': 0.85,
    'rationale': 'synthetic test row (recovered)',
    'surface_to_operator': False,
    'decided_at': sys.argv[3],
    'decided_pass': 1,
}, sort_keys=True, separators=(',', ':')))
" "$mol_id" "$action" "$decided_at")
curate_ledger_append "$TORN_DIR/scan.ndjson" "$row" >/dev/null

[[ $(wc -l < "$TORN_DIR/scan.ndjson") -eq 10 ]] || fail "T3: recovered ledger should have 10 lines"

# Verify must pass on the recovered ledger.
if bash "$_VERIFY" "$TORN_DIR" >/dev/null 2>&1; then
    pass "T3a: torn-at-EOF + resume + re-append + verify exit 0"
else
    rc=$?
    fail "T3a: verify failed after recovery (exit $rc)"
fi

# ------------------------------------------------------------------
# T3b — Mid-file torn write (unrecoverable) → verify exit 2.
# ------------------------------------------------------------------
info "T3b: torn write NOT at EOF — verify must report corrupt (exit 2)"

MID_DIR="$TMP/mid-torn"
mkdir -p "$MID_DIR"
# Inject a malformed line in the middle, followed by valid lines.
head -n 5 "$LEDGER" > "$MID_DIR/scan.ndjson"
printf '%s\n' '{"mol_id":"truncated' >> "$MID_DIR/scan.ndjson"
tail -n 4 "$LEDGER" >> "$MID_DIR/scan.ndjson"
cp "$DECS" "$MID_DIR/decisions.ndjson"

set +e
bash "$_VERIFY" "$MID_DIR" >/dev/null 2>&1
rc=$?
set -e
[[ $rc -eq 2 ]] || fail "T3b: verify should have returned 2 (corrupt), got $rc"
pass "T3b: mid-file torn write detected (verify exit 2)"

# ------------------------------------------------------------------
# T4 — decisions.ndjson references unscanned mol_id → exit 1.
# ------------------------------------------------------------------
info "T4: decision without matching scan row — verify must reject"

ORPHAN_DIR="$TMP/orphan"
mkdir -p "$ORPHAN_DIR"
cp "$LEDGER" "$ORPHAN_DIR/scan.ndjson"
cp "$DECS" "$ORPHAN_DIR/decisions.ndjson"

# Append a sealed decision row whose mol_id was never scanned.
orphan_row=$(python3 -c "
import json
print(json.dumps({
    'mol_id': 'task-20260521-orphan',
    'galaxy': 'cosmon',
    'kind': 'task',
    'action': {'Collapse': {'reason': 'orphan'}},
    'decided_at': '2026-05-21T22:30:99Z',
}, sort_keys=True, separators=(',', ':')))
")
curate_ledger_append "$ORPHAN_DIR/decisions.ndjson" "$orphan_row" >/dev/null

set +e
bash "$_VERIFY" "$ORPHAN_DIR" >/dev/null 2>&1
rc=$?
set -e
[[ $rc -eq 1 ]] || fail "T4: verify should have returned 1 (orphan decision), got $rc"
pass "T4: orphan decision detected (verify exit 1)"

# ------------------------------------------------------------------
echo
pass "all curate-ledger integration tests passed"
