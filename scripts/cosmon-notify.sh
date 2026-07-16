#!/usr/bin/env bash
# cosmon-notify — envoie un message sur le canal Telegram interne cosmon (@cosmon_noog_bot).
#
# SENDER partagé du canal bi-directionnel cosmon. Toute session Claude opérant
# dans la galaxie cosmon l'appelle pour pinger l'opérateur (Noogram) — jalons,
# questions atomiques, notifs de build. C'est le pendant "outbound" du poller
# `cosmon-bot` (daemon inbox-only) et du routeur `cosmon-tg-route` (inbound).
#
# Usage:
#   cosmon-notify "texte"
#   echo "texte" | cosmon-notify
#   cosmon-notify --from <session-id> "texte"   # préfixe [sess-<sid8>]
#
# Le préfixe `--from <sid>` désambiguïse QUELLE session parle quand plusieurs
# sessions partagent le canal : le message part sous la forme
#   [sess-df6ccbb6] texte
# et l'opérateur peut répondre en adressant `@df6ccbb6: ...` (le routeur route
# la réponse vers cette session via `cs whisper --to-session`).
#
# Lit le bot_token depuis ~/.cosmon/cosmon-bot-inbox.toml (la SEULE source de
# vérité du token @cosmon_noog_bot — le même fichier que consomme le poller,
# donc pas de dérive de token). chat_id = premier de `inbox_chat_ids`
# (défaut 100000000 = DM Noogram). Texte simple, pas de parse_mode.
#
# Miroir du pattern qsig-notify / project_x-gui-notify (canal interne
# opérateur<->agent), étendu avec l'adressage multi-session --from.
set -euo pipefail

CFG="${COSMON_BOT_INBOX_TOML:-$HOME/.cosmon/cosmon-bot-inbox.toml}"
# Fallback historique : ~/.cosmon/tg-bot.toml (même bot, ancienne source).
CFG_FALLBACK="$HOME/.cosmon/tg-bot.toml"
CHAT_DEFAULT="100000000"

usage() {
  sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
}

FROM=""
while [ $# -gt 0 ]; do
  case "$1" in
    --from) FROM="${2:-}"; shift 2 ;;
    --from=*) FROM="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; break ;;
    -*) echo "cosmon-notify: unknown flag $1" >&2; exit 2 ;;
    *) break ;;
  esac
done

MSG="${1:-}"
if [ -z "$MSG" ]; then
  MSG="$(cat)"
fi
if [ -z "$MSG" ]; then
  echo "cosmon-notify: empty message (nothing on argv or stdin)" >&2
  exit 2
fi

if [ ! -f "$CFG" ]; then
  if [ -f "$CFG_FALLBACK" ]; then
    CFG="$CFG_FALLBACK"
  else
    echo "cosmon-notify: config not found: $CFG (nor $CFG_FALLBACK)" >&2
    exit 1
  fi
fi

TOKEN="$(grep -E '^bot_token' "$CFG" | sed -E 's/.*[=:][[:space:]]*"?([0-9]+:[A-Za-z0-9_-]+)"?.*/\1/' | head -1)"
if [ -z "$TOKEN" ]; then
  echo "cosmon-notify: no bot_token in $CFG" >&2
  exit 1
fi

# chat_id: premier entier de inbox_chat_ids (poller config) ou group_chat_id
# (fallback config), sinon le DM opérateur par défaut.
CHAT="$(grep -E '^inbox_chat_ids' "$CFG" | grep -oE '[0-9]+' | head -1)"
if [ -z "$CHAT" ]; then
  CHAT="$(grep -E '^group_chat_id' "$CFG" | grep -oE '[0-9]+' | head -1)"
fi
CHAT="${CHAT:-$CHAT_DEFAULT}"

if [ -n "$FROM" ]; then
  short="${FROM#session-}"   # strip conventional prefix so the tag is meaningful
  short="${short:0:8}"
  MSG="[sess-${short}] ${MSG}"
fi

curl -s "https://api.telegram.org/bot${TOKEN}/sendMessage" \
  --data-urlencode "chat_id=${CHAT}" \
  --data-urlencode "text=${MSG}" \
  | python3 -c "import sys,json;d=json.load(sys.stdin);print('sent id='+str(d['result']['message_id']) if d.get('ok') else 'ERROR: '+json.dumps(d))"
