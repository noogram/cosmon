#!/usr/bin/env bash
# trace-sidecar.test.sh — behavioural tests for the trace sidecar.
#
# Builds a synthetic .cosmon/state tree with a two-node polymer (a root
# molecule and its decay product), runs the sidecar, and asserts the three
# emitted artifacts capture both nodes, are append-only, and never touch state.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SIDECAR="$HERE/trace-sidecar.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STATE="$WORK/.cosmon/state"
MOLS="$STATE/fleets/default/molecules"
mkdir -p "$MOLS/root-0001" "$MOLS/child-0002"

fail=0
check() { # <desc> <condition-cmd...>
  local desc="$1"; shift
  if "$@"; then echo "ok   - $desc"; else echo "FAIL - $desc"; fail=1; fi
}

# ── synthetic state ─────────────────────────────────────────────────────────
cat >"$MOLS/root-0001/state.json" <<'JSON'
{ "id": "root-0001", "formula_id": "deep-think", "status": "running",
  "current_step": 1, "total_steps": 3, "created_at": "2026-01-01T00:00:00Z",
  "tags": ["temp:hot"],
  "variables": { "topic": "root topic line" },
  "links": [ { "rel": "decay_product", "id": "child-0002" } ] }
JSON
cat >"$MOLS/child-0002/state.json" <<'JSON'
{ "id": "child-0002", "formula_id": "task-work", "status": "completed",
  "current_step": 2, "total_steps": 2, "created_at": "2026-01-01T00:05:00Z",
  "variables": { "topic": "child topic line" },
  "links": [ { "rel": "decayed_from", "id": "root-0001" } ] }
JSON
printf 'child artifact body\n' >"$MOLS/child-0002/synthesis.md"

# Event log: two polymer events, one unrelated event that must be excluded.
cat >"$STATE/events.jsonl" <<'JSON'
{"seq":1,"type":"molecule_nucleated","molecule_id":"root-0001","formula_id":"deep-think"}
{"seq":2,"type":"molecule_nucleated","molecule_id":"child-0002","formula_id":"task-work"}
{"seq":3,"type":"molecule_nucleated","molecule_id":"unrelated-9999","formula_id":"x"}
JSON

# Snapshot the source state before tracing, to prove read-only afterwards.
state_digest() { find "$STATE" -type f -exec shasum -a 256 {} + | sort; }
BEFORE_STATE="$(state_digest)"

# ── run ─────────────────────────────────────────────────────────────────────
OUT="$WORK/trace"
( cd "$WORK" && "$SIDECAR" --mol root-0001 --out "$OUT" ) >/dev/null 2>&1

# ── assertions ──────────────────────────────────────────────────────────────
check "events.jsonl exists"   test -f "$OUT/events.jsonl"
check "briefs.md exists"      test -f "$OUT/briefs.md"
check "hashes.tsv exists"     test -f "$OUT/hashes.tsv"

check "root event captured"   grep -q '"molecule_id":"root-0001"'  "$OUT/events.jsonl"
check "child event captured"  grep -q '"molecule_id":"child-0002"' "$OUT/events.jsonl"
check "unrelated excluded"    bash -c "! grep -q 'unrelated-9999' '$OUT/events.jsonl'"

check "both nodes in briefs"  test "$(grep -c '^## ' "$OUT/briefs.md")" -eq 2
check "child topic in briefs" grep -q 'child topic line' "$OUT/briefs.md"

check "child artifact hashed" grep -q 'synthesis.md' "$OUT/hashes.tsv"
# sha256 + byte count of "child artifact body\n" (20 bytes)
EXP="$(printf 'child artifact body\n' | shasum -a 256 | cut -d' ' -f1)"
check "hash is correct sha256" grep -q "$EXP" "$OUT/hashes.tsv"
check "byte count is 20"       awk -F'\t' '$2=="synthesis.md" && $3==20 {ok=1} END{exit !ok}' "$OUT/hashes.tsv"

# Append-only: a second run adds no duplicate event lines.
BEFORE="$(wc -l <"$OUT/events.jsonl")"
( cd "$WORK" && "$SIDECAR" --mol root-0001 --out "$OUT" ) >/dev/null 2>&1
AFTER="$(wc -l <"$OUT/events.jsonl")"
check "second run is append-only (no dupes)" test "$BEFORE" -eq "$AFTER"

# Read-only: source state files are byte-identical after tracing.
check "source state untouched" test "$BEFORE_STATE" = "$(state_digest)"

# --mol is mandatory.
check "missing --mol errors" bash -c "! '$SIDECAR' --state '$STATE' >/dev/null 2>&1"

if [ "$fail" -ne 0 ]; then echo "TESTS FAILED"; exit 1; fi
echo "ALL TESTS PASSED"
