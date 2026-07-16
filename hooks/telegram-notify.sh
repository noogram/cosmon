#!/usr/bin/env bash
# telegram-notify.sh — Cosmon outbound hook for Telegram notifications.
#
# Reads Cosmon event JSON (one Envelope per line, NDJSON) from stdin and
# sends a formatted notification to a Telegram chat via the Bot API.
#
# Required environment variables:
#   TELEGRAM_BOT_TOKEN  — Bot API token (from @BotFather)
#   TELEGRAM_CHAT_ID    — Target chat/group/channel ID
#
# Optional environment variables:
#   TELEGRAM_PARSE_MODE — Message parse mode (default: "HTML")
#   TELEGRAM_SILENT     — Set to "1" to send without notification sound
#   COSMON_HOOK_FILTER  — Comma-separated event kinds to forward
#                         (default: all events). Example: "error_occurred,worker_terminated"
#
# Usage:
#   echo '{"timestamp":"...","kind":"worker_spawned","worker_id":"quartz","agent":"polecat"}' \
#     | ./hooks/telegram-notify.sh
#
#   cs observe --follow --json mol-abc | ./hooks/telegram-notify.sh
#
# Exit codes:
#   0 — all events sent successfully (or no matching events)
#   1 — missing required environment variable
#   2 — curl failed to reach Telegram API

set -euo pipefail

# ── Configuration ───────────────────────────────────────────────────
readonly PARSE_MODE="${TELEGRAM_PARSE_MODE:-HTML}"
readonly SILENT="${TELEGRAM_SILENT:-0}"
readonly FILTER="${COSMON_HOOK_FILTER:-}"

# ── Validation ──────────────────────────────────────────────────────
if [[ -z "${TELEGRAM_BOT_TOKEN:-}" ]]; then
  echo "telegram-notify: TELEGRAM_BOT_TOKEN is not set" >&2
  exit 1
fi

if [[ -z "${TELEGRAM_CHAT_ID:-}" ]]; then
  echo "telegram-notify: TELEGRAM_CHAT_ID is not set" >&2
  exit 1
fi

readonly API_URL="https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage"

# ── Helpers ─────────────────────────────────────────────────────────

# Check if an event kind passes the filter.
# Returns 0 (pass) if no filter is set or the kind is in the filter list.
passes_filter() {
  local kind="$1"
  if [[ -z "$FILTER" ]]; then
    return 0
  fi
  # Split comma-separated filter and check membership
  local IFS=','
  for allowed in $FILTER; do
    if [[ "$kind" == "$allowed" ]]; then
      return 0
    fi
  done
  return 1
}

# Escape HTML special characters for Telegram's HTML parse mode.
escape_html() {
  local text="$1"
  text="${text//&/&amp;}"
  text="${text//</&lt;}"
  text="${text//>/&gt;}"
  echo "$text"
}

# Format a Cosmon event JSON line into a human-readable Telegram message.
format_message() {
  local json="$1"
  local kind timestamp

  kind=$(echo "$json" | jq -r '.kind // empty')
  timestamp=$(echo "$json" | jq -r '.timestamp // empty')

  if [[ -z "$kind" ]]; then
    echo ""
    return
  fi

  local header
  local body=""

  case "$kind" in
    worker_spawned)
      local worker agent
      worker=$(echo "$json" | jq -r '.worker_id')
      agent=$(echo "$json" | jq -r '.agent')
      header="Worker Spawned"
      body="Worker <b>$(escape_html "$worker")</b> started as <b>$(escape_html "$agent")</b>"
      ;;
    worker_terminated)
      local worker reason
      worker=$(echo "$json" | jq -r '.worker_id')
      reason=$(echo "$json" | jq -r '.reason')
      header="Worker Terminated"
      body="Worker <b>$(escape_html "$worker")</b> stopped: $(escape_html "$reason")"
      ;;
    molecule_dispatched)
      local mol worker
      mol=$(echo "$json" | jq -r '.molecule_id')
      worker=$(echo "$json" | jq -r '.worker_id')
      header="Molecule Dispatched"
      body="Molecule <b>$(escape_html "$mol")</b> assigned to worker <b>$(escape_html "$worker")</b>"
      ;;
    molecule_transitioned)
      local mol from to
      mol=$(echo "$json" | jq -r '.molecule_id')
      from=$(echo "$json" | jq -r '.from')
      to=$(echo "$json" | jq -r '.to')
      header="Molecule Transitioned"
      body="<b>$(escape_html "$mol")</b>: $(escape_html "$from") → $(escape_html "$to")"
      ;;
    step_completed)
      local mol step total
      mol=$(echo "$json" | jq -r '.molecule_id')
      step=$(echo "$json" | jq -r '.step')
      total=$(echo "$json" | jq -r '.total')
      header="Step Completed"
      body="<b>$(escape_html "$mol")</b>: step $((step + 1))/$total"
      ;;
    task_dispatched)
      local title target priority channel
      title=$(echo "$json" | jq -r '.title')
      target=$(echo "$json" | jq -r '.target')
      priority=$(echo "$json" | jq -r '.priority')
      channel=$(echo "$json" | jq -r '.channel')
      header="Task Dispatched"
      body="<b>$(escape_html "$title")</b> → $(escape_html "$target") [$(escape_html "$priority"), $(escape_html "$channel")]"
      ;;
    error_occurred)
      local context message
      context=$(echo "$json" | jq -r '.context')
      message=$(echo "$json" | jq -r '.message')
      header="Error"
      body="$(escape_html "$context"): <b>$(escape_html "$message")</b>"
      ;;
    *)
      header="Event: $(escape_html "$kind")"
      body="<pre>$(escape_html "$json")</pre>"
      ;;
  esac

  local ts_display=""
  if [[ -n "$timestamp" ]]; then
    ts_display="<i>${timestamp}</i>
"
  fi

  echo "${ts_display}<b>[${header}]</b>
${body}"
}

# Send a message to Telegram via the Bot API.
send_telegram() {
  local text="$1"

  local -a curl_args=(
    --silent
    --show-error
    --max-time 10
    --retry 2
    --retry-delay 1
    -X POST
    "$API_URL"
    -d "chat_id=${TELEGRAM_CHAT_ID}"
    -d "parse_mode=${PARSE_MODE}"
    --data-urlencode "text=${text}"
  )

  if [[ "$SILENT" == "1" ]]; then
    curl_args+=(-d "disable_notification=true")
  fi

  local response http_code
  response=$(curl "${curl_args[@]}" -w "\n%{http_code}" 2>&1)
  http_code=$(echo "$response" | tail -1)
  local body
  body=$(echo "$response" | sed '$d')

  if [[ "$http_code" -ge 200 && "$http_code" -lt 300 ]]; then
    return 0
  else
    echo "telegram-notify: API returned HTTP $http_code" >&2
    echo "$body" >&2
    return 2
  fi
}

# ── Main loop ───────────────────────────────────────────────────────

exit_code=0

while IFS= read -r line; do
  # Skip empty lines
  [[ -z "$line" ]] && continue

  # Extract event kind for filtering
  kind=$(echo "$line" | jq -r '.kind // empty' 2>/dev/null) || continue
  [[ -z "$kind" ]] && continue

  # Apply filter
  if ! passes_filter "$kind"; then
    continue
  fi

  # Format and send
  message=$(format_message "$line")
  if [[ -n "$message" ]]; then
    if ! send_telegram "$message"; then
      exit_code=2
    fi
  fi
done

exit "$exit_code"
