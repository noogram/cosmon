#!/usr/bin/env bash
# cosmon-tg-route.test.sh — routing + idempotency test for the receptionist.
#
# Builds an isolated state dir with fake presence sessions and a fake Telegram
# inbox, stubs `cs` with a recorder, and asserts the router aims each message
# at the right ensemble primitive:
#   - addressed @<sid-prefix>:  → cs whisper --to-session <full-sid>
#   - addressed @<mol-id>:      → cs whisper <mol-id>  (positional)
#   - @all:                     → whisper to every fresh session
#   - unaddressed               → freshest session
#   - unaddressed, no session   → cs drop
#   - marker prevents re-processing (idempotent tick)
#
# Usage: ./scripts/cosmon-tg-route.test.sh   (exit 0 pass, non-zero fail)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROUTER="$HERE/cosmon-tg-route.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STATE="$WORK/state"
INBOX="$WORK/inbox"
MARKER="$WORK/marker"
CSLOG="$WORK/cs-calls.log"
STUB="$WORK/cs-stub"
mkdir -p "$STATE/presence" "$INBOX"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# ── cs stub: record argv, one line per call ──────────────────────────────────
cat > "$STUB" <<STUBEOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$CSLOG"
exit 0
STUBEOF
chmod +x "$STUB"

# ── Fresh presence sessions (heartbeat = now) ────────────────────────────────
now_iso() { python3 -c 'from datetime import datetime,timezone;print(datetime.now(timezone.utc).isoformat())'; }
old_iso() { python3 -c 'from datetime import datetime,timezone,timedelta;print((datetime.now(timezone.utc)-timedelta(hours=3)).isoformat())'; }

mk_session() { # <sid> <iso>
  cat > "$STATE/presence/$1.json" <<JSON
{"session_id":"$1","galaxy":"cosmon","cwd":"/x","pid":1,"started_at":"$2","heartbeat_at":"$2","headline":""}
JSON
}

# session-aaaa1111 is the freshest; session-bbbb2222 fresh but older.
mk_session "session-bbbb2222" "$(python3 -c 'from datetime import datetime,timezone,timedelta;print((datetime.now(timezone.utc)-timedelta(seconds=60)).isoformat())')"
sleep 0.01
mk_session "session-aaaa1111" "$(now_iso)"

mk_inbox() { # <ts-basename> <text>
  cat > "$INBOX/$1.txt" <<TXT
from: Noogram
at: $(now_iso)
chat_id: 100000000
text: $2
TXT
}

run_router() {
  COSMON_TG_ROUTE_STATE_DIR="$STATE" \
  COSMON_TG_ROUTE_INBOX="$INBOX" \
  COSMON_TG_ROUTE_MARKER="$MARKER" \
  COSMON_TG_ROUTE_CS="$STUB" \
  COSMON_TG_ROUTE_FRESH_SECONDS=900 \
  HOME="$WORK" \
    bash "$ROUTER" "$@" 2>/dev/null
}

# ── T1: unaddressed → freshest session (aaaa1111) ────────────────────────────
mk_inbox "20260101T000001Z-1" "hello there"
run_router
grep -q -- "whisper --config $STATE --to-session session-aaaa1111 -m hello there" "$CSLOG" \
  || fail "T1 unaddressed did not route to freshest session; log:$(cat "$CSLOG")"
pass "T1 unaddressed → freshest session"

# ── T2: idempotency — re-running does NOT re-route T1 ────────────────────────
: > "$CSLOG"
run_router
[ -s "$CSLOG" ] && fail "T2 re-run re-processed messages (marker broken): $(cat "$CSLOG")"
pass "T2 marker prevents re-processing"

# ── T3: addressed @<sid-prefix>: → that session ──────────────────────────────
: > "$CSLOG"
mk_inbox "20260101T000002Z-2" "@bbbb2222: ping you specifically"
run_router
grep -q -- "whisper --config $STATE --to-session session-bbbb2222 -m ping you specifically" "$CSLOG" \
  || fail "T3 sid-prefix routing failed; log:$(cat "$CSLOG")"
pass "T3 @sid-prefix → matching session"

# ── T4: addressed @<mol-id>: → positional whisper ────────────────────────────
: > "$CSLOG"
mk_inbox "20260101T000003Z-3" "@task-20260711-b0a4: check your worktree"
run_router
grep -q -- "whisper --config $STATE task-20260711-b0a4 -m check your worktree" "$CSLOG" \
  || fail "T4 mol-id routing failed; log:$(cat "$CSLOG")"
pass "T4 @mol-id → positional whisper (worker pane)"

# ── T5: @all: → broadcast to every fresh session ─────────────────────────────
: > "$CSLOG"
mk_inbox "20260101T000004Z-4" "@all: standup in 5"
run_router
grep -q -- "--to-session session-aaaa1111 -m standup in 5" "$CSLOG" \
  || fail "T5 broadcast missed aaaa1111; log:$(cat "$CSLOG")"
grep -q -- "--to-session session-bbbb2222 -m standup in 5" "$CSLOG" \
  || fail "T5 broadcast missed bbbb2222; log:$(cat "$CSLOG")"
pass "T5 @all → broadcast to all fresh sessions"

# ── T6: unaddressed with NO fresh session → cs drop ──────────────────────────
: > "$CSLOG"
rm -f "$STATE"/presence/session-*.json     # no sessions at all
mk_inbox "20260101T000005Z-5" "nobody home"
run_router
grep -q -- "drop --config $STATE --tag source:telegram nobody home" "$CSLOG" \
  || fail "T6 no-session fallback did not cs drop; log:$(cat "$CSLOG")"
pass "T6 unaddressed + no session → cs drop fallback"

# ── T7: --dry-run does NOT advance the marker (repeatable) ────────────────────
: > "$CSLOG"
rm -f "$MARKER"
mk_session "session-cccc3333" "$(now_iso)"
mk_inbox "20260101T000006Z-6" "dry probe"
run_router --dry-run
[ -f "$MARKER" ] && fail "T7 dry-run advanced the marker"
pass "T7 dry-run leaves marker untouched"

echo "ALL PASS"
