#!/usr/bin/env bash
# cs-paste-nudge.test.sh — 30s integration test for the watchdog.
#
# Spawns a fake claude pane on an isolated tmux socket, seeds it with a
# bracketed-paste prompt, runs the watchdog in --apply mode, and asserts
# the watchdog pressed Enter by observing a sentinel echo appended to the
# captured buffer.
#
# Usage: ./scripts/cs-paste-nudge.test.sh
# Exit: 0 on pass, non-zero on failure.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
NUDGE="$HERE/cs-paste-nudge.sh"

SOCKET="paste-nudge-test-$$"
SESSION="task-fake-$$"
WORK="$(mktemp -d)"
LOG="$WORK/watchdog.log"
SENTINEL_FILE="$WORK/sentinel"

cleanup() {
  tmux -L "$SOCKET" kill-server >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# 1) Start an isolated tmux server + fake-claude pane.
#    The shell inside waits on `read` — pressing Enter unblocks it and
#    triggers an echo that writes to $SENTINEL_FILE. That's our oracle.
tmux -L "$SOCKET" new-session -d -s "$SESSION" \
  "bash -c 'printf \"Pasted text #1 +42 lines][Pasted text #2 +17 lines]\n\"; read _dummy; echo ENTER_RECEIVED > \"$SENTINEL_FILE\"; sleep 2'"

sleep 0.5

# Sanity: the pane shows the bracketed-paste shape.
tmux -L "$SOCKET" capture-pane -p -t "${SESSION}:" | grep -q 'Pasted text' \
  || fail "fake pane did not render the bracketed-paste seed"
pass "fake pane seeded with bracketed-paste marker"

# 2) Run the watchdog as a short-interval background loop so dwell crosses 2.
"$NUDGE" --apply --socket "$SOCKET" --session-regex "^task-fake-" \
  --interval 1 --log "$LOG" >/dev/null 2>&1 &
WATCHDOG_PID=$!

# 3) Wait up to 10s for the sentinel to appear.
for _ in $(seq 1 40); do
  [ -f "$SENTINEL_FILE" ] && break
  sleep 0.25
done

# Allow the watchdog a moment to finish its log_event after the keystrokes.
sleep 1
kill "$WATCHDOG_PID" 2>/dev/null || true
wait "$WATCHDOG_PID" 2>/dev/null || true

[ -f "$SENTINEL_FILE" ] || fail "sentinel not written — watchdog did not press Enter"
grep -q 'ENTER_RECEIVED' "$SENTINEL_FILE" || fail "sentinel content mismatch"
pass "watchdog pressed Enter on the bracketed-paste pane"

# 4) The audit log must contain a structured enter2x record.
grep -q '"action":"enter2x"' "$LOG" || fail "no enter2x action recorded in audit log"
grep -q '"state":"UNSUBMITTED-PASTE"' "$LOG" || fail "UNSUBMITTED-PASTE state not recorded"
pass "NDJSON audit contains enter2x action for UNSUBMITTED-PASTE"

echo "OK — cs-paste-nudge.sh integration test passed"
