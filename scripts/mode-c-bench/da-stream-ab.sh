#!/usr/bin/env bash
# da-stream-ab.sh — the D-A experiment: does stream:true make the 500 go away?
#
# The panel could NOT settle from source whether turning on streaming stops
# ollama crashing on a long tool-call, or whether it crashes identically.
# That single measurement decides whether M2 (own-side streaming extraction)
# is worth building. This script IS that measurement:
#
#   POST the identical long-SymPy tool-call payload to the identical ollama
#   binary twice — once {stream:false} (current mode-C shape), once
#   {stream:true} — and record whether HTTP 500 / "error parsing tool call"
#   fires in each arm.
#
# The provocation is a JOINT property of model × server, so the model+endpoint
# are PINNED (BENCH_MODEL / BENCH_OLLAMA in lib.sh). If the pinned endpoint is
# unreachable or the pinned model is absent, the arm is reported UNAVAILABLE
# (never silently substituted). --local-probe additionally fires the same A/B
# against a locally-present model as a LABELLED, non-pinned generalisation
# probe (weaker evidence, explicitly not the pinned result).
#
# Usage:
#   da-stream-ab.sh                 # pinned endpoint/model
#   da-stream-ab.sh --local-probe   # also probe a local model (cross-model)
#   BENCH_MODEL=... BENCH_OLLAMA=... da-stream-ab.sh
#
# Provenance: delib-20260707-df9b §M-BENCH "The D-A experiment (forgemaster
# stream:true A/B)".
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

MISSION="$HERE/provocation/anharmonic-mission.md"

# Build the chat/completions payload for a given model + stream flag. The
# system turn forces the whole SymPy script into ONE run_python tool-call
# argument — the exact shape that trips ollama's server-side parser.
build_payload() { # $1=model  $2=stream(true|false)  → JSON on stdout
  local model="$1" stream="$2"
  local mission; mission="$(cat "$MISSION")"
  jq -n --arg model "$model" --argjson stream "$stream" --arg mission "$mission" '
  {
    model: $model,
    stream: $stream,
    messages: [
      { role: "system",
        content: "You are the `computer` role of a physics fleet. Compute the requested quantities and VERIFY them by calling the run_python tool with a COMPLETE, self-contained SymPy script as the single `code` argument. Emit the ENTIRE multi-line script in one tool call — do not split it across calls." },
      { role: "user", content: $mission }
    ],
    tools: [
      { type: "function", function: {
          name: "run_python",
          description: "Execute a complete Python script and return stdout.",
          parameters: { type: "object",
            properties: { code: { type: "string", description: "A complete, self-contained Python script." } },
            required: ["code"] } } }
    ]
  }'
}

# Fire one arm. Prints:  "<http_code> <fired500:0|1> <note>"
fire() { # $1=endpoint  $2=model  $3=stream
  local ep="$1" model="$2" stream="$3" body code
  body="$(build_payload "$model" "$stream")"
  local tmp; tmp="$(mktemp)"
  code="$(curl -s -m 300 -o "$tmp" -w '%{http_code}' \
        "$ep/v1/chat/completions" -H 'Content-Type: application/json' \
        -d "$body" 2>/dev/null || echo 000)"
  local fired=0 note=""
  if [[ "$code" == "500" ]] && grep -qi 'parsing tool call' "$tmp"; then
    fired=1; note="500 error-parsing-tool-call (THE provocation fired)"
  elif [[ "$code" == "500" ]]; then
    fired=1; note="500 (other 5xx)"
  elif [[ "$code" == "200" ]]; then
    # 200 can still be a mode-C 'miss' (model returned the script as content,
    # or a clean tool_calls field). Distinguish for colour.
    if jq -e '.choices[0].message.tool_calls' "$tmp" >/dev/null 2>&1; then
      note="200 clean tool_calls (no crash — server parsed it)"
    else
      note="200 content-fallthrough (model did not emit a parseable tool_call)"
    fi
  else
    note="http=$code (transport/other)"
  fi
  rm -f "$tmp"
  echo "$code $fired $note"
}

reachable() { # $1=endpoint  → 0 if /api/version answers
  curl -s -m 5 "$1/api/version" >/dev/null 2>&1
}
model_present() { # $1=endpoint $2=model
  curl -s -m 10 "$1/api/tags" 2>/dev/null | jq -e --arg m "$2" '.models[]?.name | select(. == $m)' >/dev/null 2>&1
}

ab_for() { # $1=label $2=endpoint $3=model
  local label="$1" ep="$2" model="$3"
  echo "### A/B [$label]  model=$model  endpoint=$ep"
  if ! reachable "$ep"; then
    echo "   UNAVAILABLE: $ep does not answer /api/version — arm INCONCLUSIVE (not substituted)."
    return 2
  fi
  if ! model_present "$ep" "$model"; then
    echo "   UNAVAILABLE: model '$model' absent at $ep — arm INCONCLUSIVE (not substituted)."
    return 2
  fi
  local a b
  a="$(fire "$ep" "$model" false)"; echo "   stream:false → $a"
  b="$(fire "$ep" "$model" true )"; echo "   stream:true  → $b"
  local fa fb; fa="$(awk '{print $2}' <<<"$a")"; fb="$(awk '{print $2}' <<<"$b")"
  echo "   RESULT: stream:false fired500=$fa | stream:true fired500=$fb"
  if [[ "$fa" == 1 && "$fb" == 0 ]]; then
    echo "   D-A ⇒ streaming AVOIDS the 500 → M2 (own-side SSE extraction) is worth building."
  elif [[ "$fa" == 1 && "$fb" == 1 ]]; then
    echo "   D-A ⇒ streaming crashes IDENTICALLY → M2 streaming path does NOT help; keep string-match / seek 200-with-raw."
  elif [[ "$fa" == 0 ]]; then
    echo "   D-A ⇒ non-stream did not fire → INCONCLUSIVE for this model (escalate the provocation)."
  fi
}

main() {
  echo "== D-A stream:false vs stream:true A/B =="
  echo "pinned mission: $MISSION (sha256 $(shasum -a 256 "$MISSION" | awk '{print $1}'))"
  ab_for "PINNED" "$BENCH_OLLAMA" "$BENCH_MODEL"; local pinned_rc=$?

  if [[ "${1:-}" == "--local-probe" ]]; then
    local lep="${LOCAL_OLLAMA:-http://127.0.0.1:11434}"
    local lm="${LOCAL_MODEL:-qwen2.5-coder:7b}"
    echo
    echo "-- cross-model generalisation probe (NOT the pinned result) --"
    ab_for "LOCAL" "$lep" "$lm" || true
  fi
  return "$pinned_rc"
}
main "$@"
