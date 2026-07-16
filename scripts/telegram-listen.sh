#!/bin/bash
# telegram-listen.sh — the inbound Telegram control channel ("channel d'écoute").
#
# Polls Telegram getUpdates, captures operator messages from the DM, and drops
# each into ~/.cosmon/telegram-inbox/ so the fleet/pilot can act on them. Makes
# the Telegram thread BIDIRECTIONAL: the heartbeat posts vitality outbound; the
# operator replies commands inbound; this listener captures them.
#
# CONSTRAINT: getUpdates allows only ONE consumer. This listener and a
# long-polling notification-bot cannot both run. notification-bot is not polling at install
# time. If it is ever restarted, set ~/.cosmon/telegram-listen.off to stand down.
#
# Scheduled by cosmon-scheduler (patrol "cosmon-telegram-listen", ~60s).
# Kill switch: touch ~/.cosmon/telegram-listen.off
set -uo pipefail
export PATH=/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH

KILL="$HOME/.cosmon/telegram-listen.off"
[ -f "$KILL" ] && exit 0
OPERATOR_CHAT="100000000"
INBOX="$HOME/.cosmon/telegram-inbox"
OFFSET_FILE="$HOME/.cosmon/telegram-offset"
LOG="$HOME/.cosmon/logs/telegram-listen.log"
mkdir -p "$INBOX" "$(dirname "$LOG")"

TOKEN=$(grep '^bot_token' "$HOME/.showroom/bot.toml" 2>/dev/null | cut -d'"' -f2)
[ -z "${TOKEN:-}" ] && exit 0
offset=$(cat "$OFFSET_FILE" 2>/dev/null || echo 0)

resp=$(/usr/bin/curl -s --max-time 25 \
  "https://api.telegram.org/bot${TOKEN}/getUpdates?offset=$((offset + 1))&timeout=20&allowed_updates=%5B%22message%22%5D" 2>/dev/null)
[ -z "$resp" ] && exit 0

printf '%s' "$resp" | /usr/bin/python3 - "$OPERATOR_CHAT" "$INBOX" "$OFFSET_FILE" "$LOG" <<'PY'
import sys, json, os, time
resp = sys.stdin.read()
op_chat, inbox, offset_file, log = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
try:
    d = json.loads(resp)
except Exception:
    sys.exit(0)
if not d.get("ok"):
    sys.exit(0)
updates = d.get("result", [])
if not updates:
    sys.exit(0)
max_id = 0
captured = []
for u in updates:
    uid = u.get("update_id", 0)
    max_id = max(max_id, uid)
    m = u.get("message", {})
    chat = str(m.get("chat", {}).get("id", ""))
    text = m.get("text", "")
    frm = m.get("from", {})
    # operator messages only; ignore bot's own / others
    if chat == op_chat and text and not frm.get("is_bot"):
        rec = {"update_id": uid, "date": m.get("date"), "from": frm.get("first_name"), "text": text}
        with open(os.path.join(inbox, f"{uid}.json"), "w") as f:
            json.dump(rec, f, ensure_ascii=False, indent=2)
        captured.append(text)
if max_id:
    with open(offset_file, "w") as f:
        f.write(str(max_id))
if captured:
    with open(log, "a") as f:
        for t in captured:
            f.write(f"[{time.strftime('%FT%TZ', time.gmtime())}] inbound: {t[:200]}\n")
PY
exit 0
