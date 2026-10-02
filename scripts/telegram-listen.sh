#!/bin/bash
# telegram-listen.sh — the inbound bot control channel ("channel d'écoute").
#
# Polls one update batch, captures operator messages, and advances the
# checkpoint only after every accepted record is durable.
set -uo pipefail
export PATH=/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH

HERE="$(cd "$(dirname "$0")" && pwd)"
OPERATOR_CHAT="100000000"

if [ "${COSMON_BOT_READER_TEST_MODE:-}" = "1" ]; then
  TEST_ROOT="${COSMON_BOT_READER_TEST_ROOT:?test root is required}"
  ROOT="$TEST_ROOT/.cosmon"
  TOKEN_FILE="$TEST_ROOT/bot.toml"
  TEST_ENDPOINT=(--test-endpoint "${COSMON_BOT_READER_TEST_ENDPOINT:?test endpoint is required}")
else
  if [ -n "${COSMON_BOT_READER_TEST_ROOT:-}${COSMON_BOT_READER_TEST_ENDPOINT:-}${COSMON_BOT_READER_TEST_INTERRUPT:-}" ]; then
    printf '%s\n' '{"outcome":"test_endpoint_refused"}' >&2
    exit 2
  fi
  ROOT="$HOME/.cosmon"
  TOKEN_FILE="$HOME/.showroom/bot.toml"
  TEST_ENDPOINT=()
fi

KILL="$ROOT/telegram-listen.off"
[ -f "$KILL" ] && exit 0

exec /usr/bin/python3 "$HERE/bot_reader.py" \
  --token-file "$TOKEN_FILE" \
  --operator-chat "$OPERATOR_CHAT" \
  --inbox "$ROOT/telegram-inbox" \
  --checkpoint "$ROOT/telegram-offset" \
  --log "$ROOT/logs/telegram-listen.log" \
  "${TEST_ENDPOINT[@]}"
