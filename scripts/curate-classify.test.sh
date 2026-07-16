#!/usr/bin/env bash
# curate-classify.test.sh — synthetic backlog regression tests for
# `scripts/curate-classify.sh`. Each test case constructs a fake
# molecule state.json and asserts the expected DrainVerdict.
#
# The matrix is priority-ordered (top-to-bottom, first match wins),
# so test cases pin one matrix row each and assert no higher-priority
# row spuriously fires.
#
# Usage:
#   scripts/curate-classify.test.sh           # run all cases
#   scripts/curate-classify.test.sh -v        # verbose (print each verdict)
#
# Exit codes:
#   0  all cases pass
#   1  at least one case failed

set -euo pipefail

VERBOSE=0
if [[ "${1:-}" == "-v" ]]; then
  VERBOSE=1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLASSIFIER="${SCRIPT_DIR}/curate-classify.sh"

if [[ ! -x "$CLASSIFIER" ]]; then
  echo "classifier not executable: $CLASSIFIER" >&2
  exit 1
fi

TMP_DIR="$(mktemp -d /tmp/curate-classify-test.XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT

# A canonical "now" so age computations are deterministic.
NOW="2026-05-22T02:00:00Z"

# Helper: write a state.json from named-arg JSON snippets and run the
# classifier with the supplied flags. Echoes the raw verdict line.
classify_case() {
  local case_name="$1"; shift
  local state_json="$1"; shift
  local case_dir="${TMP_DIR}/${case_name}"
  mkdir -p "$case_dir"
  echo "$state_json" > "$case_dir/state.json"
  "$CLASSIFIER" --state-json "$case_dir/state.json" --now "$NOW" "$@"
}

# Helper: assert a path in the verdict equals an expected value.
assert_jq() {
  local case_name="$1"
  local verdict="$2"
  local path="$3"
  local expected="$4"
  local actual
  actual="$(echo "$verdict" | jq -rc "$path")"
  if [[ "$actual" != "$expected" ]]; then
    echo "  FAIL $case_name: $path → expected '$expected', got '$actual'"
    echo "       verdict: $verdict"
    return 1
  fi
  [[ $VERBOSE -eq 1 ]] && echo "  ok   $case_name: $path = $actual"
  return 0
}

PASS_COUNT=0
FAIL_COUNT=0

run_case() {
  local name="$1"
  echo "▸ $name"
  if "_case_$name"; then
    PASS_COUNT=$((PASS_COUNT + 1))
  else
    FAIL_COUNT=$((FAIL_COUNT + 1))
  fi
}

# -----------------------------------------------------------------
# Row 1 — godel firebreak: formula starts with curate-
# -----------------------------------------------------------------
_case_row1_firebreak() {
  local state
  state="$(jq -cn '{
    id: "patrol-20260520-aaaa",
    formula_id: "curate-patrol",
    kind: null,
    status: "pending",
    created_at: "2026-05-20T00:00:00Z",
    updated_at: "2026-05-20T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case row1 "$state" --pass 2)"
  assert_jq row1 "$v" '.action.Skip.why' "patrol-self" || return 1
  assert_jq row1 "$v" '.surface_to_operator' "false" || return 1
}

# -----------------------------------------------------------------
# Row 2 — active/propelled (status=running)
# -----------------------------------------------------------------
_case_row2_running() {
  local state
  state="$(jq -cn '{
    id: "task-20260521-bbbb",
    formula_id: "task-work",
    kind: "task",
    status: "running",
    created_at: "2026-05-21T00:00:00Z",
    updated_at: "2026-05-21T01:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case row2 "$state" --pass 2)"
  assert_jq row2 "$v" '.action.Skip.why' "in-flight" || return 1
}

# -----------------------------------------------------------------
# Row 3 — blocks an active molecule
# -----------------------------------------------------------------
_case_row3_blocks_active() {
  local active_file="${TMP_DIR}/active.txt"
  printf 'task-99999999-ffff\n' > "$active_file"
  local state
  state="$(jq -cn '{
    id: "task-20260510-cccc",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-05-10T00:00:00Z",
    updated_at: "2026-05-10T00:00:00Z",
    tags: ["temp:hot"],
    typed_links: [
      {rel: "blocks", target: "task-99999999-ffff"}
    ]
  }')"
  local v
  v="$(classify_case row3 "$state" --pass 2 --active-ids "$active_file")"
  assert_jq row3 "$v" '.action.Skip.why' "blocks-active" || return 1
}

# -----------------------------------------------------------------
# Row 4 — syzygie_cited
# -----------------------------------------------------------------
_case_row4_syzygie() {
  local cache="${TMP_DIR}/syz.txt"
  printf 'task-20260501-dddd\n' > "$cache"
  local state
  state="$(jq -cn '{
    id: "task-20260501-dddd",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-05-01T00:00:00Z",
    updated_at: "2026-05-01T00:00:00Z",
    tags: ["temp:warm"],
    typed_links: []
  }')"
  local v
  v="$(classify_case row4 "$state" --pass 2 --syzygie-cache "$cache")"
  assert_jq row4 "$v" '.action' "Surface" || return 1
  assert_jq row4 "$v" '.surface_to_operator' "true" || return 1
  assert_jq row4 "$v" '.syzygie_cited' "true" || return 1
}

# -----------------------------------------------------------------
# Row 5 — kind ∈ {decision, deliberation}
# -----------------------------------------------------------------
_case_row5_kind() {
  local state
  state="$(jq -cn '{
    id: "delib-20260501-eeee",
    formula_id: "deep-think",
    kind: "deliberation",
    status: "pending",
    created_at: "2026-05-01T00:00:00Z",
    updated_at: "2026-05-01T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case row5 "$state" --pass 2)"
  assert_jq row5 "$v" '.action' "Surface" || return 1
  assert_jq row5 "$v" '.surface_to_operator' "true" || return 1
}

# -----------------------------------------------------------------
# Row 7 — pending >30d, no temp:*, no DAG, no events — Collapse on pass 2
# -----------------------------------------------------------------
_case_row7_collapse_pass2() {
  local state
  state="$(jq -cn '{
    id: "task-20260301-ffff",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-03-01T00:00:00Z",
    updated_at: "2026-03-01T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case row7 "$state" --pass 2)"
  assert_jq row7 "$v" '.action.Collapse.reason' \
    "pending >30d, no temp:*, no edges, no events" || return 1
  assert_jq row7 "$v" '.surface_to_operator' "false" || return 1
}

# -----------------------------------------------------------------
# Row 7 on pass 1 — kahneman: never collapse on pass 1
# -----------------------------------------------------------------
_case_row7_pass1_defer() {
  local state
  state="$(jq -cn '{
    id: "task-20260301-1111",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-03-01T00:00:00Z",
    updated_at: "2026-03-01T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case row7_p1 "$state" --pass 1)"
  assert_jq row7_p1 "$v" '.action.Skip.why' "pass-2-only" || return 1
}

# -----------------------------------------------------------------
# Row 8 — temp:cold + no edge activity 14d — Collapse on pass 2
# -----------------------------------------------------------------
_case_row8_cold_stagnant() {
  local state
  state="$(jq -cn '{
    id: "task-20260420-2222",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-04-20T00:00:00Z",
    updated_at: "2026-05-01T00:00:00Z",
    tags: ["temp:cold"],
    typed_links: []
  }')"
  local v
  v="$(classify_case row8 "$state" --pass 2)"
  assert_jq row8 "$v" '.action.Collapse | type' "object" || return 1
}

# -----------------------------------------------------------------
# Row 9 — pending >7d, no temp:* → Revise{temp:warm} (pass 1 only)
# -----------------------------------------------------------------
_case_row9_revise_warm() {
  local state
  state="$(jq -cn '{
    id: "task-20260510-3333",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-05-10T00:00:00Z",
    updated_at: "2026-05-10T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case row9 "$state" --pass 1)"
  assert_jq row9 "$v" '.action.Revise.tag' "temp:warm" || return 1
  assert_jq row9 "$v" '.surface_to_operator' "false" || return 1
}

# -----------------------------------------------------------------
# Row 10 — temp:hot, age >7d, no progress → Surface
# -----------------------------------------------------------------
_case_row10_hot_stagnant() {
  local state
  state="$(jq -cn '{
    id: "task-20260510-4444",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-05-10T00:00:00Z",
    updated_at: "2026-05-10T00:00:00Z",
    tags: ["temp:hot"],
    typed_links: []
  }')"
  local v
  v="$(classify_case row10 "$state" --pass 2)"
  assert_jq row10 "$v" '.action' "Surface" || return 1
  assert_jq row10 "$v" '.surface_to_operator' "true" || return 1
}

# -----------------------------------------------------------------
# Row 11 — temp:hot, age ≤7d, no blockers, briefing complete → Tackle
# (v0 = always surface; surface_to_operator=true)
# -----------------------------------------------------------------
_case_row11_tackle_surface() {
  local case_dir="${TMP_DIR}/row11"
  mkdir -p "$case_dir"
  cat > "$case_dir/briefing.md" <<'BRF'
# task briefing

Step 1: do work.
Step 2: verify.
Step 3: ship.

This is a non-empty briefing intended to pass the >5 non-blank-line
heuristic for "briefing complete".
BRF
  local state
  state="$(jq -cn '{
    id: "task-20260520-5555",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-05-20T00:00:00Z",
    updated_at: "2026-05-21T00:00:00Z",
    tags: ["temp:hot"],
    typed_links: []
  }')"
  echo "$state" > "$case_dir/state.json"
  local v
  v="$("$CLASSIFIER" --state-json "$case_dir/state.json" --now "$NOW" --pass 2 \
       --briefing-md "$case_dir/briefing.md")"
  assert_jq row11 "$v" '.action' "Tackle" || return 1
  assert_jq row11 "$v" '.surface_to_operator' "true" || return 1
}

# -----------------------------------------------------------------
# Row 12 — ambiguous fallback → Surface
# -----------------------------------------------------------------
_case_row12_ambiguous() {
  # A molecule that doesn't match any earlier row: pending, temp:warm,
  # age <7d, no signals.
  local state
  state="$(jq -cn '{
    id: "task-20260521-6666",
    formula_id: "task-work",
    kind: "task",
    status: "pending",
    created_at: "2026-05-21T00:00:00Z",
    updated_at: "2026-05-21T00:00:00Z",
    tags: ["temp:warm"],
    typed_links: []
  }')"
  local v
  v="$(classify_case row12 "$state" --pass 2)"
  assert_jq row12 "$v" '.action' "Surface" || return 1
  assert_jq row12 "$v" '.surface_to_operator' "true" || return 1
}

# -----------------------------------------------------------------
# kind fallback — legacy molecule with kind:null but id starts with `delib-`
# should still hit row 5
# -----------------------------------------------------------------
_case_kind_fallback_legacy_delib() {
  local state
  state="$(jq -cn '{
    id: "delib-20260101-9abc",
    formula_id: "deep-think",
    kind: null,
    status: "pending",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case kindfb "$state" --pass 2)"
  assert_jq kindfb "$v" '.action' "Surface" || return 1
  assert_jq kindfb "$v" '.kind' "deliberation" || return 1
}

# -----------------------------------------------------------------
# Terminal-status pre-filter (defensive — should never reach matrix)
# -----------------------------------------------------------------
_case_terminal_status_skip() {
  local state
  state="$(jq -cn '{
    id: "task-20260101-dead",
    formula_id: "task-work",
    kind: "task",
    status: "collapsed",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case term "$state" --pass 2)"
  assert_jq term "$v" '.action.Skip.why' "terminal" || return 1
}

# -----------------------------------------------------------------
# Priority sanity — Row 1 (curate-) beats Row 5 (deliberation)
# -----------------------------------------------------------------
_case_priority_row1_over_row5() {
  local state
  state="$(jq -cn '{
    id: "delib-curate-9999-7777",
    formula_id: "curate-patrol",
    kind: "deliberation",
    status: "pending",
    created_at: "2026-05-21T00:00:00Z",
    updated_at: "2026-05-21T00:00:00Z",
    tags: [],
    typed_links: []
  }')"
  local v
  v="$(classify_case pri15 "$state" --pass 2)"
  # Firebreak wins.
  assert_jq pri15 "$v" '.action.Skip.why' "patrol-self" || return 1
}

# -----------------------------------------------------------------
# Run all
# -----------------------------------------------------------------
run_case row1_firebreak
run_case row2_running
run_case row3_blocks_active
run_case row4_syzygie
run_case row5_kind
run_case row7_collapse_pass2
run_case row7_pass1_defer
run_case row8_cold_stagnant
run_case row9_revise_warm
run_case row10_hot_stagnant
run_case row11_tackle_surface
run_case row12_ambiguous
run_case kind_fallback_legacy_delib
run_case terminal_status_skip
run_case priority_row1_over_row5

echo
echo "passed: $PASS_COUNT  failed: $FAIL_COUNT"
if [[ "$FAIL_COUNT" -gt 0 ]]; then
  exit 1
fi
exit 0
