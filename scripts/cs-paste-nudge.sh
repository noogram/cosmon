#!/usr/bin/env bash
# cs-paste-nudge.sh — one-action watchdog: press Enter on UNSUBMITTED-PASTE.
#
# Design origin: delib-20260414-76c6 (synthesis, 9-persona panel).
# Principle: "The scanner presses Enter on prompts the operator would have
# pressed Enter on — and does nothing else." (jobs)
#
# Temporary safety net. Decommissioned the moment einstein's Propelled
# substates land (see docs/watchdog.md, section "Decommission").
#
# Anti-requirements (hard):
#   - Does NOT authenticate.
#   - Does NOT touch .cosmon/state/.
#   - Does NOT act on UNKNOWN / LOGIN / RATELIMIT / APPROVAL / CRASHED.
#   - Does NOT respawn / collapse / mark anything.
#
# Usage: ./scripts/cs-paste-nudge.sh [flags]
#   --dry-run             default — observe + log, never press Enter
#   --apply               opt-in — send Enter on confirmed UNSUBMITTED-PASTE
#   --socket NAME         tmux socket allowlist (repeatable; default: default)
#   --session-regex RE    session name allowlist (default: ^(fix-|task-|delib-|mission-|idea-|smoke-|temp-))
#   --interval SEC        polling cadence (default 20)
#   --log PATH            NDJSON audit log (default .cosmon/watchdog.log)
#   --once                scan one tick then exit (used by tests)
#   -h|--help             this help

set -euo pipefail

# -------- defaults ----------------------------------------------------------
MODE="dry-run"
SESSION_REGEX='^(fix-|task-|delib-|mission-|idea-|smoke-|temp-)'
INTERVAL=20
LOG_PATH=".cosmon/watchdog.log"
LOG_MAX_BYTES=$((10 * 1024 * 1024))
ONCE=0
declare -a SOCKETS=()

# -------- arg parsing -------------------------------------------------------
while (($#)); do
  case "$1" in
    --dry-run) MODE="dry-run"; shift ;;
    --apply) MODE="apply"; shift ;;
    --socket) SOCKETS+=("$2"); shift 2 ;;
    --session-regex) SESSION_REGEX="$2"; shift 2 ;;
    --interval) INTERVAL="$2"; shift 2 ;;
    --log) LOG_PATH="$2"; shift 2 ;;
    --once) ONCE=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

(( ${#SOCKETS[@]} == 0 )) && SOCKETS=(default)

mkdir -p "$(dirname "$LOG_PATH")"
: >>"$LOG_PATH"

# -------- self-exclusion ----------------------------------------------------
# If the watchdog runs inside tmux, its own pane_id must never be acted upon.
SELF_PANE="${TMUX_PANE:-}"

# -------- ring buffer (per-pane state across ticks) ------------------------
# Keyed by "<socket>|<session>|<pane_id>". Values: "STATE|hash"
declare -A LAST_STATE
declare -A DWELL

# -------- utilities ---------------------------------------------------------
now_iso() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }

log_event() {
  # log_event STATE ACTION SOCKET SESSION PANE DETAIL
  local state="$1" action="$2" socket="$3" session="$4" pane="$5" detail="$6"
  rotate_log_if_needed
  printf '{"ts":"%s","socket":"%s","session":"%s","pane":"%s","state":"%s","action":"%s","detail":%s}\n' \
    "$(now_iso)" "$socket" "$session" "$pane" "$state" "$action" "$detail" \
    >>"$LOG_PATH"
}

rotate_log_if_needed() {
  [ -f "$LOG_PATH" ] || return 0
  local size
  size=$(wc -c <"$LOG_PATH" | tr -d ' ')
  (( size >= LOG_MAX_BYTES )) || return 0
  mv -f "$LOG_PATH" "${LOG_PATH}.1"
  : >"$LOG_PATH"
}

json_escape() {
  # Minimal JSON string escape for the detail field.
  python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' 2>/dev/null \
    || awk 'BEGIN{printf "\""} {gsub(/\\/,"\\\\"); gsub(/"/,"\\\""); gsub(/\t/,"\\t"); gsub(/\r/,"\\r"); printf "%s\\n",$0} END{printf "\""}'
}

classify_capture() {
  # stdin = last-20-line capture; echoes one token:
  #   UNSUBMITTED-PASTE  — bracketed-paste marker still on prompt line
  #   UNKNOWN            — anything else (never actioned)
  local buf
  buf="$(cat)"
  if grep -qE 'Pasted text.*\][[:space:]]*\[Pasted text' <<<"$buf"; then
    echo "UNSUBMITTED-PASTE"; return
  fi
  if grep -qE 'Pasted text.*\+[[:space:]]*[0-9]+[[:space:]]+lines' <<<"$buf"; then
    echo "UNSUBMITTED-PASTE"; return
  fi
  echo "UNKNOWN"
}

scan_socket() {
  local socket="$1"
  # tmux list-panes across sessions: -a -F per-session regex handled below.
  local lines
  lines=$(tmux -L "$socket" list-panes -a \
    -F '#{session_name}	#{pane_id}	#{pane_pid}' 2>/dev/null || true)
  [ -z "$lines" ] && return 0

  while IFS=$'\t' read -r session pane pid; do
    [ -z "$session" ] && continue
    [[ "$session" =~ $SESSION_REGEX ]] || continue
    [ -n "$SELF_PANE" ] && [ "$pane" = "$SELF_PANE" ] && continue

    local key="${socket}|${session}|${pane}"
    local capture state hash
    # Grab the pane, strip trailing blank lines (the region below the cursor),
    # then keep the last 20 lines — the visible prompt region a human would read.
    capture=$(tmux -L "$socket" capture-pane -p -t "${session}:" 2>/dev/null \
      | awk 'BEGIN{RS=""} {print}' \
      | tail -n 20 || true)
    [ -z "$capture" ] && continue

    state=$(printf '%s' "$capture" | classify_capture)
    hash=$(printf '%s' "$capture" | cksum | awk '{print $1}')

    local prev_entry="${LAST_STATE[$key]:-}"
    local prev_state="${prev_entry%%|*}"
    local prev_hash="${prev_entry##*|}"

    if [ "$state" = "UNSUBMITTED-PASTE" ] && [ "$prev_state" = "UNSUBMITTED-PASTE" ] && [ "$prev_hash" = "$hash" ]; then
      DWELL[$key]=$((${DWELL[$key]:-1} + 1))
    else
      DWELL[$key]=1
    fi
    LAST_STATE[$key]="${state}|${hash}"

    local dwell="${DWELL[$key]}"
    local detail
    detail=$(printf 'dwell=%s hash=%s pid=%s' "$dwell" "$hash" "$pid" | json_escape)

    if [ "$state" = "UNSUBMITTED-PASTE" ] && (( dwell >= 2 )); then
      if [ "$MODE" = "apply" ]; then
        tmux -L "$socket" send-keys -t "${session}:" Enter 2>/dev/null || true
        sleep 0.3
        tmux -L "$socket" send-keys -t "${session}:" Enter 2>/dev/null || true
        log_event "$state" "enter2x" "$socket" "$session" "$pane" "$detail"
        # reset dwell so we don't hammer the same pane on next tick
        DWELL[$key]=0
        LAST_STATE[$key]="ACTED|${hash}"
      else
        log_event "$state" "would-enter2x" "$socket" "$session" "$pane" "$detail"
      fi
    else
      log_event "$state" "none" "$socket" "$session" "$pane" "$detail"
    fi
  done <<<"$lines"
}

# -------- main loop ---------------------------------------------------------
trap 'exit 0' INT TERM

while :; do
  for sock in "${SOCKETS[@]}"; do
    scan_socket "$sock" || true
  done
  (( ONCE )) && exit 0
  sleep "$INTERVAL"
done
