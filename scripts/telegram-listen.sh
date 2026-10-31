#!/bin/bash
# telegram-listen.sh — the inbound bot control channel ("channel d'écoute").
#
# Polls one update batch, captures operator messages, and advances the
# checkpoint only after every accepted record is durable. The reader polls
# only once enrolled (scripts/bot-reader-transfer.py enroll) and only while
# its ownership journal is active; the stop file below is re-checked under
# the per-host lock immediately before any network request. HTTP/API 409
# remains latched in that journal until explicit local reconciliation.
set -uo pipefail
export PATH=/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH

HERE="$(cd "$(dirname "$0")" && pwd)"
OPERATOR_CHAT="100000000"

# Help must never inspect credentials/state or poll, including before enrollment.
if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  cat <<'HELP'
Usage: telegram-listen.sh [--help]
Poll one batch through durable capture, with ownership and stop checks locked.
Recovery states (inspect status on each host before retrying):
  not_enrolled: shut down legacy readers, validate history, then enroll.
  active: may poll unless stopped or latched by api_conflict.
  fenced: drain inbox then export, or cancel a bare fence.
  offered: retry the same offer, or cancel --transfer-id before commit.
  standby: import an offer; staged: activate only with its committed receipt.
  transferred: source cannot resume; use a new reverse transfer.
  stopped: preserve unrelated stop controls; transfer_pending: inspect phase.
  api_conflict: confirm the competitor is gone on that active host, then run
    bot-reader-transfer.py reconcile --competing-consumer-gone.
    No time, later HTTP 200, stop-file removal, cancel or activate clears it.
  indeterminate_in_flight: refuse transfer; establish request completion through
    separately validated recovery. Reconcile does not clear unknown requests.
  inbox_not_empty: consume/archive source work before exporting.
  invalid_state, invalid_checkpoint: investigate trusted state; never reset it.
  credential_failure: operator provisions credentials out of band.
  transport_failure, network_failure: inspect reachability and request state;
    never promote an unreachable source's destination by elapsed time.
  stale_epoch, receipt_mismatch, offer_mismatch: reject replay or mismatched data.
  disk_failure, record_conflict, invalid_manifest: preserve evidence and repair
    the underlying write, content or manifest problem before retrying.
  destination_not_inactive, not_owner, already_committed, offered_elsewhere:
    inspect both hosts; never force ownership or discard a committed transfer.
Transfer success means activation only; verify with a fresh normal capture poll
and source/destination status. No verify verb or UI/remote client equivalent.
Guarantee: enrolled readers under operator control only; absence of 409 proves
no exclusivity against unmanaged consumers. See docs/guides/bot-reader-transfer.md.
HELP
  exit 0
fi

if [ "${COSMON_BOT_READER_TEST_MODE:-}" = "1" ]; then
  TEST_ROOT="${COSMON_BOT_READER_TEST_ROOT:?test root is required}"
  ROOT="$TEST_ROOT/.cosmon"
  TOKEN_FILE="$TEST_ROOT/bot.toml"
  TEST_ENDPOINT=(--test-endpoint "${COSMON_BOT_READER_TEST_ENDPOINT:?test endpoint is required}")
else
  if [ -n "${COSMON_BOT_READER_TEST_ROOT:-}${COSMON_BOT_READER_TEST_ENDPOINT:-}${COSMON_BOT_READER_TEST_INTERRUPT:-}${COSMON_BOT_READER_TEST_PAUSE:-}" ]; then
    printf '%s\n' '{"outcome":"test_endpoint_refused"}' >&2
    exit 2
  fi
  ROOT="$HOME/.cosmon"
  TOKEN_FILE="$HOME/.showroom/bot.toml"
  TEST_ENDPOINT=()
fi

KILL="$ROOT/telegram-listen.off"
if [ -f "$KILL" ]; then
  printf '%s\n' '{"outcome": "stopped"}'
  exit 0
fi

exec /usr/bin/python3 "$HERE/bot_reader.py" \
  --state-root "$ROOT" \
  --token-file "$TOKEN_FILE" \
  --operator-chat "$OPERATOR_CHAT" \
  "${TEST_ENDPOINT[@]}"
