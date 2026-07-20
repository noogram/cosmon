#!/usr/bin/env bash
# run-judge.sh — LLM-as-judge harness (null context, second opinion).
#
# Hands a FRESH cosmon worker the SAME six-issue mission (JUDGE_MISSION.md)
# against a pristine v0.2.1 tree, with NO access to this molecule's context and
# NO access to the tester's report. The worker independently reproduces and
# scores each issue; its per-issue `judge_verdict` is merged into report.json
# as a second column.
#
# Dispatch backends, in preference order:
#   1. cosmon-remote `do` (the real fleet) if COSMON_JUDGE_REMOTE=1 and the
#      client is authed — a genuine fresh worker, fully null-context.
#   2. explicit COSMON_JUDGE_CMD (any command that reads the mission on stdin
#      and writes the JSON contract to stdout).
# When neither is configured, the harness is WIRED but records the judge column
# as INCONCLUSIVE ("judge not dispatched") for every row — no silent fill.
#
# Usage:
#   bench/judge/run-judge.sh            # merge judge verdicts into report.json
#
# This Phase-0 deliverable wires the harness and records the baseline judge
# column against pristine v0.2.1.

set -euo pipefail
JUDGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$JUDGE_DIR/../lib/common.sh"

MISSION="$JUDGE_DIR/JUDGE_MISSION.md"
REPORT_JSON="$OUT_DIR/report.json"
JUDGE_RAW="$OUT_DIR/judge-verdicts.json"

[[ -f "$REPORT_JSON" ]] || die "no report.json yet — run bench/run.sh first"

# Materialise a pristine tree for the judge to inspect (null-context: it only
# sees released source, never this bench's evidence or the tester's report).
UUT="$OUT_DIR/uut-judge"
checkout_v021 "$UUT"

dispatched=0
if [[ "${COSMON_JUDGE_REMOTE:-0}" == "1" ]] && has cosmon-remote; then
  log "dispatching judge via cosmon-remote (fresh worker, null context)"
  # The judge worker gets ONLY the mission + the source path. Real fleet call.
  if cosmon-remote do free-form \
        --topic "$(cat "$MISSION")

Source tree to inspect: $UUT" \
        --kind task --json > "$OUT_DIR/judge-dispatch.json" 2>>"$OUT_DIR/judge.log"; then
    # Fetch the deliverable and extract the JSON contract.
    MID="$(jq -r '.molecule_id // .id // empty' "$OUT_DIR/judge-dispatch.json")"
    if [[ -n "$MID" ]]; then
      cosmon-remote molecule result "$MID" --json > "$OUT_DIR/judge-result.json" 2>>"$OUT_DIR/judge.log" || true
      jq -r '.result // .body // empty' "$OUT_DIR/judge-result.json" 2>/dev/null \
        | sed -n '/^{/,/^}/p' > "$JUDGE_RAW" || true
      [[ -s "$JUDGE_RAW" ]] && dispatched=1
    fi
  fi
elif [[ -n "${COSMON_JUDGE_CMD:-}" ]]; then
  log "dispatching judge via COSMON_JUDGE_CMD"
  if COSMON_UUT="$UUT" bash -c "$COSMON_JUDGE_CMD" < "$MISSION" > "$JUDGE_RAW" 2>>"$OUT_DIR/judge.log"; then
    [[ -s "$JUDGE_RAW" ]] && dispatched=1
  fi
fi

if [[ "$dispatched" -ne 1 ]]; then
  warn "judge not dispatched (set COSMON_JUDGE_REMOTE=1 or COSMON_JUDGE_CMD) — recording judge column as INCONCLUSIVE"
  jq -n '{
    unit_under_test: "'"$COSMON_TAG"'",
    rows: [
      "issue-1-cs-verify","issue-2-build-deps","issue-3-dag-orphan",
      "issue-4-local-ollama","issue-5-paper-cuts","issue-6-claude-adapter"
    ] | map({id: ., judge_verdict: "INCONCLUSIVE", reason: "judge not dispatched in this run"})
  }' > "$JUDGE_RAW"
fi

# Write the judge verdict back into each per-probe record (the single source of
# truth aggregate.sh reads), so re-aggregating cannot wipe the judge column.
while IFS= read -r id; do
  [[ -f "$PROBES_OUT/$id.json" ]] || continue
  jv="$(jq -r --arg id "$id" '.rows[] | select(.id==$id) | .judge_verdict' "$JUDGE_RAW")"
  tmp="$(mktemp)"
  jq --arg jv "$jv" '.judge_verdict = $jv' "$PROBES_OUT/$id.json" > "$tmp"
  mv "$tmp" "$PROBES_OUT/$id.json"
done < <(jq -r '.rows[].id' "$JUDGE_RAW")

# Re-aggregate so report.json + report.md carry the judge column.
bash "$BENCH_DIR/aggregate.sh" >/dev/null

log "judge column merged into $REPORT_JSON (dispatched=$dispatched)"
rm -rf "$UUT"
