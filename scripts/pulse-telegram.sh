#!/bin/bash
# pulse-telegram.sh — push the cosmon runtime vitality (`cs pulse`) to Telegram.
#
# The continuous heartbeat. Operator maxim (2026-06-26): "absence de signal =
# signal de mort." A green/amber/red pulse arrives on a regular cadence; if it
# STOPS arriving, the engine (or the Mac) is down — silence is the death signal.
# Because this patrol itself depends on the launchd scheduler + an awake Mac,
# its own silence is structurally the same signal it exists to deliver.
#
# Scheduled by cosmon-scheduler via ~/.config/cosmon/patrols.toml (patrol
# "cosmon-pulse-telegram"). Kill switch: touch ~/.cosmon/pulse-telegram.off
set -uo pipefail
export PATH=/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH

KILL="$HOME/.cosmon/pulse-telegram.off"
[ -f "$KILL" ] && exit 0
cd "$HOME/galaxies/cosmon" 2>/dev/null || exit 1

JSON=$(cs pulse --json 2>/dev/null)
[ -z "$JSON" ] && exit 0

msg=$(printf '%s' "$JSON" | /usr/bin/python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
state = d.get("state", "?")
rpm = d.get("rpm", 0.0)
word = d.get("headline_word")
fuel = d.get("fuel_pct", 0.0)
scanned = d.get("scanned", 0)
voy = d.get("voyants", {}) or {}
dot = {"green": "\U0001F7E2", "amber": "\U0001F7E1", "red": "\U0001F534"}.get(state, "⚪")
# name any non-green organs so a red light is legible at a glance
bad = [k for k, v in voy.items() if v != "green"]
head = word if word else f"{rpm:.1f} rpm"
line = f"{dot} cosmon {head}"
if bad:
    marks = " ".join(f"{k}:{voy[k]}" for k in bad)
    line += f" | ⚠️ {marks}"
else:
    line += " | 6 voyants verts"
line += f" | fuel {fuel*100:.0f}% | scanned {scanned}"
print(line)
' 2>/dev/null)
[ -z "$msg" ] && exit 0

TOKEN=$(grep '^bot_token' "$HOME/.showroom/bot.toml" 2>/dev/null | cut -d'"' -f2)
[ -n "$TOKEN" ] && /usr/bin/curl -s "https://api.telegram.org/bot${TOKEN}/sendMessage" \
  --data-urlencode "chat_id=100000000" --data-urlencode "text=${msg}" >/dev/null 2>&1
exit 0
