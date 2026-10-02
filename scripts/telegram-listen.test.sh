#!/usr/bin/env bash
# Exercise the public listener against a loopback-only synthetic endpoint.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(mktemp -d)"
SERVER_PID=""
trap 'if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null || true; fi; rm -rf "$ROOT"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

printf '%s\n' 'bot_token = "synthetic-listener"' > "$ROOT/bot.toml"
cat > "$ROOT/response.json" <<'JSON'
{"ok":true,"result":[{"update_id":41,"message":{"chat":{"id":100000000},"date":1,"from":{"first_name":"Operator","is_bot":false},"text":"synthetic payload"}}]}
JSON

python3 "$HERE/../tests/harness/bot_reader_fake_endpoint.py" \
  --response "$ROOT/response.json" --requests "$ROOT/requests.jsonl" \
  --port-file "$ROOT/port" &
SERVER_PID=$!
for _ in $(seq 1 100); do
  [ -s "$ROOT/port" ] && break
  sleep 0.02
done
[ -s "$ROOT/port" ] || fail "fake endpoint did not start"

output=$(COSMON_BOT_READER_TEST_MODE=1 \
  COSMON_BOT_READER_TEST_ROOT="$ROOT" \
  COSMON_BOT_READER_TEST_ENDPOINT="http://127.0.0.1:$(cat "$ROOT/port")" \
  bash "$HERE/telegram-listen.sh" 2>&1)

[ -f "$ROOT/.cosmon/telegram-inbox/41.json" ] || fail "accepted record was not captured"
[ "$(cat "$ROOT/.cosmon/telegram-offset")" = "41" ] || fail "checkpoint was not advanced"
grep -q '"offset": "1"' "$ROOT/requests.jsonl" || fail "request did not acknowledge from checkpoint zero"
grep -q '"credential_is_synthetic": true' "$ROOT/requests.jsonl" || fail "fake endpoint did not receive synthetic credential"
case "$output$(cat "$ROOT/.cosmon/logs/telegram-listen.log")" in
  *synthetic-listener*|*synthetic\ payload*) fail "outcome leaked a credential or message" ;;
esac
pass "valid response is captured and acknowledged without leaking content"

kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

set +e
network=$(COSMON_BOT_READER_TEST_MODE=1 \
  COSMON_BOT_READER_TEST_ROOT="$ROOT" \
  COSMON_BOT_READER_TEST_ENDPOINT="http://127.0.0.1:$(cat "$ROOT/port")" \
  bash "$HERE/telegram-listen.sh" 2>&1)
network_status=$?
set -e
[ "$network_status" -eq 1 ] || fail "network failure returned an unexpected status"
grep -q 'network_failure' <<<"$network" || fail "network failure was not observable"
[ "$(cat "$ROOT/.cosmon/telegram-offset")" = "41" ] || fail "network failure moved the checkpoint"
pass "network failure is observable and preserves the checkpoint"

printf '%s\n' '{"ok":false,"error_code":409}' > "$ROOT/response.json"
rm -f "$ROOT/port"
python3 "$HERE/../tests/harness/bot_reader_fake_endpoint.py" \
  --response "$ROOT/response.json" --requests "$ROOT/requests.jsonl" \
  --port-file "$ROOT/port" &
SERVER_PID=$!
for _ in $(seq 1 100); do
  [ -s "$ROOT/port" ] && break
  sleep 0.02
done
[ -s "$ROOT/port" ] || fail "conflict endpoint did not start"
set +e
conflict=$(COSMON_BOT_READER_TEST_MODE=1 \
  COSMON_BOT_READER_TEST_ROOT="$ROOT" \
  COSMON_BOT_READER_TEST_ENDPOINT="http://127.0.0.1:$(cat "$ROOT/port")" \
  bash "$HERE/telegram-listen.sh" 2>&1)
conflict_status=$?
set -e
[ "$conflict_status" -eq 1 ] || fail "API conflict returned an unexpected status"
grep -q 'api_conflict' <<<"$conflict" || fail "API conflict was not observable"
[ "$(cat "$ROOT/.cosmon/telegram-offset")" = "41" ] || fail "API conflict moved the checkpoint"
pass "API conflict is observable and preserves the checkpoint"

kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

set +e
refusal=$(COSMON_BOT_READER_TEST_ROOT="$ROOT" \
  COSMON_BOT_READER_TEST_ENDPOINT="http://127.0.0.1:1" \
  bash "$HERE/telegram-listen.sh" 2>&1)
refusal_status=$?
set -e
[ "$refusal_status" -eq 2 ] || fail "ambient endpoint override was not refused"
grep -q 'test_endpoint_refused' <<<"$refusal" || fail "endpoint refusal was not observable"
pass "test endpoint requires explicit isolated mode"

python3 "$HERE/../tests/harness/bot_reader_capture_test.py"
pass "capture ordering and failure outcomes"
