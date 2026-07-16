#!/usr/bin/env bash
# curate-classify.sh — single-molecule classifier for curate-patrol's
# `classify` step. Implements the 12-row priority-ordered decision
# matrix from delib-20260521-c3cd/synthesis.md as the `classify` step
# logic of `curate-patrol.formula.toml`. v0 = shell + jq, no Rust.
#
# Inputs per molecule:
#   - `state.json` (the molecule's authoritative state file). Either
#     passed via `--state-json <path>` or piped on stdin.
#   - Side-channel context (passed as flags or env vars):
#       --pass <1|2>           kahneman two-pass rule (default 1)
#       --now <RFC3339>        current time (default: now())
#       --galaxy <name>        galaxy label for the verdict (default: "cosmon")
#       --syzygie-cache <path> file of cited mol IDs (one per line)
#       --active-ids <path>    file of mol IDs currently active/running
#                              (one per line; informs row 3 "blocks-active")
#       --worktree-dir <path>  galaxy's .worktrees root (default: PWD/.worktrees)
#       --prompt-md <path>     molecule's prompt.md for first-person scan
#       --briefing-md <path>   molecule's briefing.md for completeness check
#       --events-jsonl <path>  molecule's events.jsonl for "no events" check
#
# Output: ONE line of JSON on stdout — the DrainVerdict for this molecule.
#
#   {"mol_id":"task-...","galaxy":"cosmon","kind":"task",
#    "formula":"task-work","age_hours":216,"tags":[],
#    "blocked_by":[],"blocks":[],"syzygie_cited":false,
#    "action":{"Surface":null},"confidence":0.9,
#    "rationale":"first-person prompt + age >14d + no temp:*",
#    "surface_to_operator":true,"decided_at":"2026-05-22T02:00:00Z",
#    "decided_pass":1,"sealed":null}
#
# The `sealed` field is null on emit — C-CURATE-LEDGER (task-20260521-b807)
# wraps the classifier's output with the BLAKE3 seal + fsync ledger.
#
# Exit codes:
#   0  one verdict line emitted (the patrol classifier should treat any
#      successful run as authoritative for this molecule)
#   1  unrecoverable error (jq parse failure, missing required input)
#   64 usage error
#
# Matrix priority — TOP TO BOTTOM, first match wins. Row 1 (firebreak)
# MUST always be the first check (godel: patrol can never classify its
# own family of molecules).

set -euo pipefail

# ---------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------

PASS=1
NOW=""
GALAXY="cosmon"
STATE_JSON=""
SYZYGIE_CACHE=""
ACTIVE_IDS=""
WORKTREE_DIR=""
PROMPT_MD=""
BRIEFING_MD=""
EVENTS_JSONL=""

# ---------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --state-json)     STATE_JSON="$2"; shift 2 ;;
    --pass)           PASS="$2"; shift 2 ;;
    --now)            NOW="$2"; shift 2 ;;
    --galaxy)         GALAXY="$2"; shift 2 ;;
    --syzygie-cache)  SYZYGIE_CACHE="$2"; shift 2 ;;
    --active-ids)     ACTIVE_IDS="$2"; shift 2 ;;
    --worktree-dir)   WORKTREE_DIR="$2"; shift 2 ;;
    --prompt-md)      PROMPT_MD="$2"; shift 2 ;;
    --briefing-md)    BRIEFING_MD="$2"; shift 2 ;;
    --events-jsonl)   EVENTS_JSONL="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,/^# Exit codes/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown arg: $1" >&2; exit 64 ;;
  esac
done

if [[ "$PASS" != "1" && "$PASS" != "2" ]]; then
  echo "--pass must be 1 or 2 (got '$PASS')" >&2
  exit 64
fi

if [[ -z "$NOW" ]]; then
  NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
fi

# ---------------------------------------------------------------------
# Load state.json (from --state-json <path> or stdin)
# ---------------------------------------------------------------------

if [[ -n "$STATE_JSON" && "$STATE_JSON" != "-" ]]; then
  if [[ ! -f "$STATE_JSON" ]]; then
    echo "state-json not found: $STATE_JSON" >&2
    exit 1
  fi
  STATE_BLOB="$(cat "$STATE_JSON")"
else
  STATE_BLOB="$(cat)"
fi

if ! echo "$STATE_BLOB" | jq -e . >/dev/null 2>&1; then
  echo "state-json is not valid JSON" >&2
  exit 1
fi

# ---------------------------------------------------------------------
# Extract canonical fields. We accept both `state.json` shape
# (formula_id, typed_links with rel/source/target) and
# `cs --json observe` shape (formula, typed_links — same enum).
# ---------------------------------------------------------------------

MOL_ID="$(echo "$STATE_BLOB" | jq -r '.id // ""')"
FORMULA="$(echo "$STATE_BLOB" | jq -r '.formula_id // .formula // ""')"
# `kind` is nullable in state.json (older molecules); fall back to the
# id prefix so row 5 (decision/deliberation) still fires for legacy
# molecules. `delib-` → deliberation; `decision-` → decision.
KIND="$(echo "$STATE_BLOB" | jq -r '.kind // ""')"
if [[ -z "$KIND" ]]; then
  case "$MOL_ID" in
    delib-*)    KIND="deliberation" ;;
    decision-*) KIND="decision" ;;
    task-*)     KIND="task" ;;
    idea-*)     KIND="idea" ;;
    issue-*)    KIND="issue" ;;
    signal-*)   KIND="signal" ;;
    spark-*)    KIND="spark" ;;
  esac
fi
STATUS="$(echo "$STATE_BLOB" | jq -r '.status // ""')"
CREATED_AT="$(echo "$STATE_BLOB" | jq -r '.created_at // ""')"
UPDATED_AT="$(echo "$STATE_BLOB" | jq -r '.updated_at // ""')"

if [[ -z "$MOL_ID" || -z "$FORMULA" ]]; then
  echo "state-json missing required fields (id, formula_id/formula)" >&2
  exit 1
fi

# Tags array → JSON array on stdout, plus a flat space-separated form
# for cheap grep checks.
TAGS_JSON="$(echo "$STATE_BLOB" | jq -c '.tags // []')"
TAGS_FLAT="$(echo "$TAGS_JSON" | jq -r '.[]' | tr '\n' ' ')"

# Typed links → blocked_by + blocks arrays.
BLOCKED_BY_JSON="$(echo "$STATE_BLOB" | jq -c '
  [(.typed_links // [])[] | select(.rel == "blocked_by") | (.source // .target)]
')"
BLOCKS_JSON="$(echo "$STATE_BLOB" | jq -c '
  [(.typed_links // [])[] | select(.rel == "blocks") | (.target // .source)]
')"
HAS_BLOCKED_BY="$(echo "$BLOCKED_BY_JSON" | jq 'length > 0')"
HAS_BLOCKS="$(echo "$BLOCKS_JSON" | jq 'length > 0')"
HAS_DAG_EDGES="false"
if [[ "$HAS_BLOCKED_BY" == "true" || "$HAS_BLOCKS" == "true" ]]; then
  HAS_DAG_EDGES="true"
fi

# ---------------------------------------------------------------------
# Compute age_hours (best-effort: macOS `date -d` unsupported; fall back
# to python -c if both BSD and GNU date fail).
# ---------------------------------------------------------------------

iso_to_epoch() {
  local iso="$1"
  local epoch
  # GNU date
  epoch="$(date -d "$iso" +%s 2>/dev/null || echo "")"
  if [[ -z "$epoch" ]]; then
    # BSD date (macOS) — strip subsecond + timezone offset variants
    local norm
    norm="$(echo "$iso" | sed -E 's/\.[0-9]+//; s/\+00:00$/Z/; s/Z$//')"
    epoch="$(date -ju -f "%Y-%m-%dT%H:%M:%S" "$norm" +%s 2>/dev/null || echo "")"
  fi
  if [[ -z "$epoch" ]]; then
    # Final fallback — python
    epoch="$(python3 -c "
import sys, datetime
s = sys.argv[1].replace('Z', '+00:00')
print(int(datetime.datetime.fromisoformat(s).timestamp()))
" "$iso" 2>/dev/null || echo "")"
  fi
  echo "$epoch"
}

NOW_EPOCH="$(iso_to_epoch "$NOW")"
CREATED_EPOCH="$(iso_to_epoch "$CREATED_AT")"
UPDATED_EPOCH="$(iso_to_epoch "$UPDATED_AT")"

if [[ -z "$NOW_EPOCH" || -z "$CREATED_EPOCH" ]]; then
  # Can't compute age — emit Surface with a low-confidence rationale.
  AGE_HOURS=0
  AGE_DAYS=0
  AGE_UNKNOWN=true
else
  AGE_SECONDS=$(( NOW_EPOCH - CREATED_EPOCH ))
  AGE_HOURS=$(( AGE_SECONDS / 3600 ))
  AGE_DAYS=$(( AGE_SECONDS / 86400 ))
  AGE_UNKNOWN=false
fi

if [[ -z "$UPDATED_EPOCH" || "$AGE_UNKNOWN" == "true" ]]; then
  UPDATE_AGE_DAYS=$AGE_DAYS
else
  UPDATE_AGE_DAYS=$(( ( NOW_EPOCH - UPDATED_EPOCH ) / 86400 ))
fi

# ---------------------------------------------------------------------
# Detect signals
# ---------------------------------------------------------------------

# syzygie_cited: molecule id appears in any peer galaxy's chronicle/ADR cache.
SYZYGIE_CITED="false"
if [[ -n "$SYZYGIE_CACHE" && -f "$SYZYGIE_CACHE" ]]; then
  if grep -Fxq "$MOL_ID" "$SYZYGIE_CACHE"; then
    SYZYGIE_CITED="true"
  fi
fi

# Active/in-flight: status==running OR worktree dir exists for this id.
IN_FLIGHT="false"
if [[ "$STATUS" == "running" || "$STATUS" == "queued" ]]; then
  IN_FLIGHT="true"
fi
if [[ "$IN_FLIGHT" == "false" && -n "$WORKTREE_DIR" && -d "${WORKTREE_DIR%/}/${MOL_ID}" ]]; then
  IN_FLIGHT="true"
fi

# Blocks an active molecule? — any element of BLOCKS_JSON is also in ACTIVE_IDS.
BLOCKS_ACTIVE="false"
if [[ -n "$ACTIVE_IDS" && -f "$ACTIVE_IDS" && "$HAS_BLOCKS" == "true" ]]; then
  while IFS= read -r blocked_id; do
    if [[ -n "$blocked_id" ]] && grep -Fxq "$blocked_id" "$ACTIVE_IDS"; then
      BLOCKS_ACTIVE="true"
      break
    fi
  done < <(echo "$BLOCKS_JSON" | jq -r '.[]')
fi

# Temp:* tag presence.
has_tag() {
  local needle="$1"
  printf ' %s ' "$TAGS_FLAT" | grep -qF " $needle "
}

HAS_TEMP_ANY="false"
for t in temp:hot temp:warm temp:cold temp:frozen; do
  if has_tag "$t"; then HAS_TEMP_ANY="true"; break; fi
done
HAS_TEMP_HOT="false";    has_tag "temp:hot"    && HAS_TEMP_HOT="true"
HAS_TEMP_COLD="false";   has_tag "temp:cold"   && HAS_TEMP_COLD="true"
HAS_TEMP_FROZEN="false"; has_tag "temp:frozen" && HAS_TEMP_FROZEN="true"
# temp:warm is intentionally not extracted — no matrix row reads it.
# The Revise{temp:warm} verdict is an output, not an input signal.

# First-person prompt detection — cheap heuristic over prompt.md.
# Markers: "je veux", "il faut", "j'aimerais", "I want", "I need",
# "we should", "I'd like". Conservative — both languages.
PROMPT_IS_FIRST_PERSON="false"
if [[ -n "$PROMPT_MD" && -f "$PROMPT_MD" ]]; then
  if grep -qiE '\b(je\s+veux|j[''’]aimerais|il\s+faut|i\s+want|i\s+need|we\s+should|i[''’]d\s+like|i\s+would\s+like)\b' "$PROMPT_MD" 2>/dev/null; then
    PROMPT_IS_FIRST_PERSON="true"
  fi
fi

# Briefing complete: briefing.md exists and has more than 0 non-blank lines.
BRIEFING_COMPLETE="false"
if [[ -n "$BRIEFING_MD" && -f "$BRIEFING_MD" ]]; then
  if grep -cvE '^\s*$' "$BRIEFING_MD" >/dev/null 2>&1; then
    if [[ $(grep -cvE '^\s*$' "$BRIEFING_MD") -gt 5 ]]; then
      BRIEFING_COMPLETE="true"
    fi
  fi
fi

# Events: <=3 lines counts as "no events" (creation/nucleation events only).
HAS_NO_EVENTS="true"
if [[ -n "$EVENTS_JSONL" && -f "$EVENTS_JSONL" ]]; then
  event_lines="$(wc -l < "$EVENTS_JSONL" | tr -d ' ')"
  if [[ "$event_lines" -gt 3 ]]; then
    HAS_NO_EVENTS="false"
  fi
fi

# ---------------------------------------------------------------------
# Apply the matrix — top to bottom, first match wins.
# ---------------------------------------------------------------------

# Emit one DrainVerdict line and exit. ACTION_JSON is the `action`
# tagged-enum object (matches the Rust shape: `{"Skip": {"why":...}}`,
# `{"Revise": {"tag":...}}`, `{"Collapse": {"reason":...}}`,
# `"Tackle"` or `"Surface"` for unit variants).
emit_verdict() {
  local action_json="$1"
  local confidence="$2"
  local rationale="$3"
  local surface_flag="$4"

  jq -cn \
    --arg mol_id "$MOL_ID" \
    --arg galaxy "$GALAXY" \
    --arg kind "$KIND" \
    --arg formula "$FORMULA" \
    --argjson age_hours "$AGE_HOURS" \
    --argjson tags "$TAGS_JSON" \
    --argjson blocked_by "$BLOCKED_BY_JSON" \
    --argjson blocks "$BLOCKS_JSON" \
    --argjson syzygie_cited "$SYZYGIE_CITED" \
    --argjson action "$action_json" \
    --argjson confidence "$confidence" \
    --arg rationale "$rationale" \
    --argjson surface "$surface_flag" \
    --arg decided_at "$NOW" \
    --argjson decided_pass "$PASS" \
    '{
       mol_id: $mol_id,
       galaxy: $galaxy,
       kind: $kind,
       formula: $formula,
       age_hours: $age_hours,
       tags: $tags,
       blocked_by: $blocked_by,
       blocks: $blocks,
       syzygie_cited: $syzygie_cited,
       action: $action,
       confidence: $confidence,
       rationale: $rationale,
       surface_to_operator: $surface,
       decided_at: $decided_at,
       decided_pass: $decided_pass,
       sealed: null
     }'
  exit 0
}

# Row 1 — godel firebreak. ALWAYS FIRST. Patrol cannot classify its own
# family (any formula starting with `curate-`).
if [[ "$FORMULA" == curate-* ]]; then
  emit_verdict '{"Skip":{"why":"patrol-self"}}' '0.99' \
    "formula starts with curate- — firebreak (godel)" false
fi

# Defensive pre-filter — terminal molecules (collapsed/completed) are
# already settled; the scan step should have excluded them, but if one
# slips through we Skip rather than re-classify. NOT one of the 12
# matrix rows; this is a safety net consistent with kahneman D1's
# "never re-classify completed work".
case "$STATUS" in
  collapsed|completed)
    emit_verdict '{"Skip":{"why":"terminal"}}' '0.99' \
      "status=$STATUS is terminal — already settled" false
    ;;
esac

# Row 2 — active/propelled (worker live OR worktree exists).
if [[ "$IN_FLIGHT" == "true" ]]; then
  emit_verdict '{"Skip":{"why":"in-flight"}}' '0.95' \
    "status=$STATUS or worktree exists — worker is live" false
fi

# Row 3 — has Blocks edges to active molecules.
if [[ "$BLOCKS_ACTIVE" == "true" ]]; then
  emit_verdict '{"Skip":{"why":"blocks-active"}}' '0.9' \
    "blocks at least one active molecule — touching would orphan it" false
fi

# Row 4 — syzygie_cited (cross-galaxy citation, godel undecidable).
if [[ "$SYZYGIE_CITED" == "true" ]]; then
  emit_verdict '"Surface"' '0.5' \
    "molecule is cited by a peer galaxy — cross-galaxy undecidable" true
fi

# Row 5 — kind ∈ {decision, deliberation} (operator-intent undecidable).
case "$KIND" in
  decision|deliberation)
    emit_verdict '"Surface"' '0.5' \
      "kind=$KIND is operator-intent — undecidable by matrix" true
    ;;
esac

# Row 6 — prompt first-person + age >14d + no temp:*.
if [[ "$PROMPT_IS_FIRST_PERSON" == "true" \
   && "$AGE_DAYS" -gt 14 \
   && "$HAS_TEMP_ANY" == "false" ]]; then
  emit_verdict '"Surface"' '0.5' \
    "first-person prompt + age >14d + no temp:* — domain-private undecidable" true
fi

# Row 7 — pending, age >30d, no temp:*, no DAG edges, no events → Collapse.
# Pass 1 defers; pass 2 fires.
if [[ "$STATUS" == "pending" \
   && "$AGE_DAYS" -gt 30 \
   && "$HAS_TEMP_ANY" == "false" \
   && "$HAS_DAG_EDGES" == "false" \
   && "$HAS_NO_EVENTS" == "true" ]]; then
  if [[ "$PASS" == "2" ]]; then
    emit_verdict '{"Collapse":{"reason":"pending >30d, no temp:*, no edges, no events"}}' \
      '0.9' "row 7 — orphan pending >30d, collapsing on pass 2" false
  else
    emit_verdict '{"Skip":{"why":"pass-2-only"}}' '0.9' \
      "row 7 match but pass 1 (retag-only) — deferred to pass 2" false
  fi
fi

# Row 8 — pending, temp:cold/frozen, no edge activity 14d → Collapse.
if [[ "$STATUS" == "pending" \
   && ( "$HAS_TEMP_COLD" == "true" || "$HAS_TEMP_FROZEN" == "true" ) \
   && "$UPDATE_AGE_DAYS" -gt 14 ]]; then
  if [[ "$PASS" == "2" ]]; then
    reason="pending temp:cold/frozen, no edge activity ${UPDATE_AGE_DAYS}d"
    emit_verdict "{\"Collapse\":{\"reason\":\"$reason\"}}" '0.9' \
      "row 8 — cold/frozen + stagnant, collapsing on pass 2" false
  else
    emit_verdict '{"Skip":{"why":"pass-2-only"}}' '0.9' \
      "row 8 match but pass 1 (retag-only) — deferred to pass 2" false
  fi
fi

# Row 9 — pending, age >7d, no temp:* → Revise{temp:warm}. Pass 1 only
# (this is the retag pass; pass 2 will collapse the survivors via row 7/8).
if [[ "$STATUS" == "pending" \
   && "$AGE_DAYS" -gt 7 \
   && "$HAS_TEMP_ANY" == "false" \
   && "$PASS" == "1" ]]; then
  emit_verdict '{"Revise":{"tag":"temp:warm"}}' '0.9' \
    "pending >7d with no temp:* — retag temp:warm (pass 1)" false
fi

# Row 10 — pending, temp:hot, age >7d, no progress → Surface.
if [[ "$STATUS" == "pending" \
   && "$HAS_TEMP_HOT" == "true" \
   && "$AGE_DAYS" -gt 7 \
   && "$UPDATE_AGE_DAYS" -gt 7 ]]; then
  emit_verdict '"Surface"' '0.6' \
    "temp:hot >7d with no progress — operator decides escalation" true
fi

# Row 11 — temp:hot, age ≤7d, no blockers, briefing complete → Tackle.
# v0 Rung 2 — ALWAYS surface, never auto-execute (kahneman).
if [[ "$HAS_TEMP_HOT" == "true" \
   && "$AGE_DAYS" -le 7 \
   && "$HAS_BLOCKED_BY" == "false" \
   && "$BRIEFING_COMPLETE" == "true" ]]; then
  emit_verdict '"Tackle"' '0.6' \
    "temp:hot, fresh, no blockers, briefing ready — tackle candidate (surfaced in v0)" true
fi

# Row 12 — ambiguous / no row matched → Surface.
emit_verdict '"Surface"' '0.3' \
  "no row matched — ambiguous, surfaced for operator review" true
