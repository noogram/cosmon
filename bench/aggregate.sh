#!/usr/bin/env bash
# aggregate.sh — merge per-probe records into the bench's machine-readable and
# human-readable status reports.
#
# Inputs : bench/out/probes/*.json  (one record per probe, emit_probe schema)
# Outputs: bench/out/report.json    (array of probe records + meta)
#          bench/out/report.md      (human table)
#
# The report is the bench's deliverable: one row per issue with
#   {id, name, adapter, verdict, captured_signature, evidence_path,
#    judge_verdict, note}
# aggregate.sh is pure: it never re-runs probes and never fabricates a row.

source "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

REPORT_JSON="$OUT_DIR/report.json"
REPORT_MD="$OUT_DIR/report.md"

shopt -s nullglob
FILES=("$PROBES_OUT"/*.json)
if [[ ${#FILES[@]} -eq 0 ]]; then
  die "no probe records in $PROBES_OUT — run probes first"
fi

# The six issue ids the bench is contracted to report on, in report order.
EXPECTED_IDS=(
  issue-1-cs-verify
  issue-2-build-deps
  issue-3-dag-orphan
  issue-4-local-ollama
  issue-5-paper-cuts
  issue-6-claude-adapter
)

# Build the ordered array; note any missing expected rows (no silent gaps).
TAG_FOR_META="$COSMON_TAG"
jq -n \
  --arg tag "$TAG_FOR_META" \
  --argjson expected "$(printf '%s\n' "${EXPECTED_IDS[@]}" | jq -R . | jq -s .)" \
  --slurpfile all <(cat "${FILES[@]}" | jq -s '.') \
  '
   ($all[0]) as $records
   | ($expected | map(. as $id | ($records[] | select(.id == $id)) // {id:$id, missing:true})) as $ordered
   | {
       unit_under_test: $tag,
       probe_count: ($ordered | length),
       populated: ($ordered | map(select(has("missing") | not)) | length),
       verdict_tally: ($ordered | map(.verdict // "MISSING") | group_by(.) | map({(.[0]): length}) | add),
       rows: $ordered
     }
  ' > "$REPORT_JSON"

log "wrote $REPORT_JSON"

# Human report.
{
  echo "# cosmon $COSMON_TAG regression bench — status report"
  echo
  echo "Unit under test: **$COSMON_TAG** (compiled/scanned from source, unmutated)."
  echo
  POP="$(jq -r '.populated' "$REPORT_JSON")"
  CNT="$(jq -r '.probe_count' "$REPORT_JSON")"
  echo "Populated rows: **$POP / $CNT**"
  echo
  echo "Verdict tally: \`$(jq -c '.verdict_tally' "$REPORT_JSON")\`"
  echo
  echo "| # | issue | adapter | verdict | judge | signature | evidence |"
  echo "|---|-------|---------|---------|-------|-----------|----------|"
  jq -r '.rows[] |
    "| \(.id // "?") | \(.name // "(missing)") | \(.adapter // "-") | \(.verdict // "MISSING") | \(.judge_verdict // "-") | `\(.captured_signature // "-")` | \(.evidence_path // "-") |"' \
    "$REPORT_JSON"
  echo
  echo "## Notes"
  echo
  jq -r '.rows[] | select(.note != null and .note != "") | "- **\(.id)**: \(.note)"' "$REPORT_JSON"
} > "$REPORT_MD"

log "wrote $REPORT_MD"

# Echo a one-line summary for callers.
jq -r '"aggregate: \(.populated)/\(.probe_count) rows populated; tally \(.verdict_tally)"' "$REPORT_JSON"
