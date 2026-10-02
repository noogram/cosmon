#!/usr/bin/env bash
# Exercise the bot-reader transfer over its remote-execution route.
#
# A stand-in `ssh` on PATH maps each alias to an isolated host root and runs
# the requested command with a scrubbed environment, so the coordinator's
# remote path is exercised end to end without a network, a real host, a real
# credential, or a real message.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(mktemp -d)"
SERVER_PID=""
trap 'if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null || true; fi; rm -rf "$ROOT"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

mkdir -p "$ROOT/bin" "$ROOT/hosts/host-a/.cosmon" "$ROOT/hosts/host-b"
for host in host-a host-b; do
  printf '%s\n' 'bot_token = "synthetic-4242:SYNTHETICSECRETPART"' > "$ROOT/hosts/$host/bot.toml"
done
printf '%s\n' 41 > "$ROOT/hosts/host-a/.cosmon/telegram-offset"

cat > "$ROOT/bin/ssh" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_SSH_LOG"
while [ "$1" != "--" ]; do shift; done
shift
alias="$1"; shift
[ -d "$FAKE_SSH_HOSTS/$alias" ] || { echo "ssh: connect to host $alias: refused" >&2; exit 255; }
exec env -i PATH="$PATH" HOME="$HOME" \
  COSMON_BOT_READER_TEST_MODE=1 COSMON_BOT_READER_TEST_ROOT="$FAKE_SSH_HOSTS/$alias" "$@"
SHIM
chmod +x "$ROOT/bin/ssh"

transfer() {
  PATH="$ROOT/bin:$PATH" FAKE_SSH_LOG="$ROOT/ssh.log" FAKE_SSH_HOSTS="$ROOT/hosts" \
    COSMON_BOT_READER_TEST_MODE=1 COSMON_BOT_READER_TEST_ROOT="$ROOT/hosts/host-a" \
    python3 "$HERE/bot-reader-transfer.py" "$@"
}

transfer enroll --host-id host-a >/dev/null || fail "source could not enroll"

set +e
unreachable=$(transfer transfer --source local --destination ssh:host-gone \
  --destination-host-id host-gone --remote-script "$HERE/bot-reader-transfer.py")
unreachable_status=$?
set -e
[ "$unreachable_status" -eq 1 ] || fail "unreachable destination returned an unexpected status"
grep -q '"outcome": "transport_failure"' <<<"$unreachable" || fail "unreachable destination was not reported"
[ ! -e "$ROOT/hosts/host-a/.cosmon/telegram-listen.off" ] || fail "an unreachable destination fenced the source"
pass "an unreachable destination refuses before the source is fenced"

output=$(transfer transfer --source local --destination ssh:host-b \
  --destination-host-id host-b --remote-script "$HERE/bot-reader-transfer.py") \
  || fail "transfer over the remote route failed: $output"
grep -q '"outcome": "transferred"' <<<"$output" || fail "transfer did not complete"
transfer_id=$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["transfer_id"])' <<<"$output")

grep -q 'BatchMode=yes' "$ROOT/ssh.log" || fail "remote calls may prompt"
grep -q 'StrictHostKeyChecking=yes' "$ROOT/ssh.log" || fail "remote calls skip host-key checking"
! grep -q "$transfer_id" "$ROOT/ssh.log" || fail "manifest data reached a remote command line"
grep -q SYNTHETICSECRETPART "$ROOT/ssh.log" "$ROOT/hosts/host-b/.cosmon/telegram-reader.json" \
  && fail "credential reached a command line or journal"
grep -q SYNTHETICSECRETPART <<<"$output" && fail "credential reached the transfer report"
pass "transfer runs over batch-mode ssh with manifests on stdin only"

printf '%s\n' '{"ok":true,"result":[]}' > "$ROOT/response.json"
python3 "$HERE/../tests/harness/bot_reader_fake_endpoint.py" \
  --response "$ROOT/response.json" --requests "$ROOT/requests.jsonl" \
  --port-file "$ROOT/port" &
SERVER_PID=$!
for _ in $(seq 1 100); do
  [ -s "$ROOT/port" ] && break
  sleep 0.02
done
[ -s "$ROOT/port" ] || fail "fake endpoint did not start"
listen() {
  COSMON_BOT_READER_TEST_MODE=1 COSMON_BOT_READER_TEST_ROOT="$ROOT/hosts/$1" \
    COSMON_BOT_READER_TEST_ENDPOINT="http://127.0.0.1:$(cat "$ROOT/port")" \
    bash "$HERE/telegram-listen.sh" 2>&1
}
rm -f "$ROOT/hosts/host-a/.cosmon/telegram-listen.off"
grep -q transfer_pending <<<"$(listen host-a)" || fail "source polled after transfer"
grep -q '"outcome": "empty"' <<<"$(listen host-b)" || fail "destination did not poll"
[ "$(wc -l < "$ROOT/requests.jsonl")" -eq 1 ] || fail "server did not see exactly one poll"
grep -q '"offset": "42"' "$ROOT/requests.jsonl" || fail "destination did not resume after the checkpoint"
pass "only the destination polls, from the transferred checkpoint"

python3 "$HERE/../tests/harness/bot_reader_transfer_test.py"
pass "offer, commit, cancel, activation and recovery"
