#!/usr/bin/env bash
# synapse-e2e-demo.sh — end-to-end automation of the matrix-echo-tick
# pipeline against a disposable local Synapse homeserver. Proof that
# bot provisioning + room creation + credential minting + whisper
# materialisation runs fully unattended on a homeserver we control, so
# the same flow can later be aimed at AWS Synapse behind Tailscale
# (task-20260422-1916) by swapping a single URL.
#
# Required tooling: docker (with Compose v2), curl, jq, openssl, and
# the already-installed matrix-echo-tick binary (~/.local/bin by
# default; override with MATRIX_ECHO_TICK=…).
#
# Usage:
#   scripts/synapse-e2e-demo.sh          # full run, tears Synapse down
#   scripts/synapse-e2e-demo.sh --keep   # leave Synapse up for debug
#
# See crates/cosmon-matrix-tick/docs/local-demo.md for the operator
# guide (troubleshooting, AWS adaptation).

set -euo pipefail

KEEP=0
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    -h|--help)
      sed -n '2,18p' "$0"
      exit 0
      ;;
    *)
      echo "unknown flag: $arg" >&2
      exit 64
      ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE_DIR="$REPO_ROOT/docker/synapse-e2e"
DATA_DIR="$COMPOSE_DIR/synapse-data"
OVERLAY_SRC="$COMPOSE_DIR/homeserver.overlay.yaml"

HOMESERVER_URL="http://127.0.0.1:8008"
HS_NAME="localhost"
BOT_LOCALPART="cosmon-e2e-bot"
HUMAN_LOCALPART="human-test-e2e"
BOT_MXID="@${BOT_LOCALPART}:${HS_NAME}"
HUMAN_MXID="@${HUMAN_LOCALPART}:${HS_NAME}"

MATRIX_ECHO_TICK="${MATRIX_ECHO_TICK:-$HOME/.local/bin/matrix-echo-tick}"

# Work dir for creds, state, inbox. Temporary — teardown nukes it unless --keep.
WORK_DIR="$(mktemp -d -t cosmon-synapse-e2e-XXXXXXXX)"
INBOX_DIR="$WORK_DIR/inbox"
STATE_DIR="$WORK_DIR/state"
COSMON_STATE="$WORK_DIR/cosmon-state"
CREDS_FILE="$WORK_DIR/bot-credentials.toml"
HUMAN_CREDS_FILE="$WORK_DIR/human-credentials.toml"

BOT_PASSWORD="$(openssl rand -hex 16)"
HUMAN_PASSWORD="$(openssl rand -hex 16)"
SHARED_SECRET="$(openssl rand -hex 32)"

# ---------- logging ----------
START_EPOCH="$(date +%s)"
step_log() {
  local now elapsed
  now="$(date +%s)"
  elapsed=$((now - START_EPOCH))
  printf '[%3ds] %s\n' "$elapsed" "$*"
}
fail() {
  step_log "FAIL: $*"
  exit 1
}

# ---------- teardown trap ----------
teardown() {
  local code=$?
  if [[ "$KEEP" -eq 1 && "$code" -eq 0 ]]; then
    step_log "--keep: Synapse left running at $HOMESERVER_URL"
    step_log "--keep: work dir preserved at $WORK_DIR"
    step_log "--keep: stop with: docker compose -f $COMPOSE_DIR/docker-compose.yml down -v"
    return
  fi
  step_log "teardown: stopping Synapse"
  (cd "$COMPOSE_DIR" && docker compose down -v --remove-orphans >/dev/null 2>&1) || true
  rm -rf "$DATA_DIR" || true
  rm -rf "$WORK_DIR" || true
}
trap teardown EXIT

# ---------- preflight ----------
step_log "preflight: check docker + compose + tooling"
command -v docker >/dev/null || fail "docker not found"
docker compose version >/dev/null 2>&1 || fail "docker compose v2 not available"
command -v curl >/dev/null || fail "curl not found"
command -v jq >/dev/null || fail "jq not found"
command -v openssl >/dev/null || fail "openssl not found"
[[ -x "$MATRIX_ECHO_TICK" ]] || fail "matrix-echo-tick binary not executable at $MATRIX_ECHO_TICK"

# Port 8008 must be free on 127.0.0.1.
if lsof -nP -iTCP@127.0.0.1:8008 -sTCP:LISTEN 2>/dev/null | grep -q LISTEN; then
  fail "port 127.0.0.1:8008 is already in use — stop the other service first"
fi

# Any dangling container from a previous run.
if docker ps -a --format '{{.Names}}' | grep -q '^synapse-e2e-homeserver$'; then
  step_log "preflight: removing stale container synapse-e2e-homeserver"
  docker rm -f synapse-e2e-homeserver >/dev/null
fi

# ---------- 1. generate Synapse config (first-boot bootstrap) ----------
step_log "1. generate Synapse homeserver.yaml"
mkdir -p "$DATA_DIR"
rm -rf "$DATA_DIR"/homeserver.yaml "$DATA_DIR"/*.signing.key "$DATA_DIR"/*.db 2>/dev/null || true

docker run --rm \
  -v "$DATA_DIR:/data" \
  -e SYNAPSE_SERVER_NAME="$HS_NAME" \
  -e SYNAPSE_REPORT_STATS=no \
  -e UID="$(id -u)" \
  -e GID="$(id -g)" \
  matrixdotorg/synapse:v1.132.0 generate >/dev/null

[[ -f "$DATA_DIR/homeserver.yaml" ]] || fail "synapse generate did not produce homeserver.yaml"

# Append overlay with the shared secret substituted in.
python3 - "$DATA_DIR/homeserver.yaml" "$OVERLAY_SRC" "$SHARED_SECRET" <<'PY'
import sys
yaml_path, overlay_path, secret = sys.argv[1], sys.argv[2], sys.argv[3]
with open(overlay_path) as f:
    overlay = f.read().replace("__SHARED_SECRET__", secret)
# Remove any existing keys the overlay sets (so re-runs stay idempotent).
stripped = []
skip_keys = {
    "suppress_key_server_warning", "enable_registration",
    "registration_requires_token", "registration_shared_secret",
    "federation_domain_whitelist", "allow_public_rooms_over_federation",
    "allow_public_rooms_without_auth", "serve_server_wellknown",
    "rc_message", "rc_registration", "rc_login", "rc_admin_redaction",
    "rc_joins", "presence",
}
with open(yaml_path) as f:
    skipping = False
    for line in f:
        if not line.strip() or line.startswith('#'):
            stripped.append(line)
            skipping = False
            continue
        if not line.startswith((' ', '\t')):
            key = line.split(':', 1)[0].strip()
            skipping = key in skip_keys
        if not skipping:
            stripped.append(line)
with open(yaml_path, 'w') as f:
    f.writelines(stripped)
    if not stripped or not stripped[-1].endswith('\n'):
        f.write('\n')
    f.write('\n# --- synapse-e2e-demo overlay ---\n')
    f.write(overlay)
PY

# ---------- 2. start Synapse ----------
step_log "2. docker compose up"
(cd "$COMPOSE_DIR" && docker compose up -d >/dev/null)

# ---------- 3. wait for healthy versions endpoint ----------
step_log "3. wait for homeserver /versions"
deadline=$(( $(date +%s) + 60 ))
while :; do
  if curl -fsS "$HOMESERVER_URL/_matrix/client/versions" >/dev/null 2>&1; then
    break
  fi
  if [[ "$(date +%s)" -ge "$deadline" ]]; then
    docker compose -f "$COMPOSE_DIR/docker-compose.yml" logs --tail 60 synapse || true
    fail "Synapse /versions not reachable within 60s"
  fi
  sleep 1
done

# ---------- HMAC admin register helper ----------
# Synapse admin register HMAC: sha1(nonce\0user\0password\0(admin|notadmin))
admin_register() {
  local localpart="$1" password="$2" is_admin="${3:-false}"
  local nonce admin_flag mac body resp
  nonce="$(curl -fsS "$HOMESERVER_URL/_synapse/admin/v1/register" | jq -r .nonce)"
  [[ -n "$nonce" && "$nonce" != "null" ]] || fail "failed to fetch admin-register nonce"
  if [[ "$is_admin" == "true" ]]; then
    admin_flag="admin"
  else
    admin_flag="notadmin"
  fi
  mac="$(printf '%s\0%s\0%s\0%s' "$nonce" "$localpart" "$password" "$admin_flag" \
      | openssl dgst -sha1 -hmac "$SHARED_SECRET" -hex | awk '{print $NF}')"
  body="$(jq -cn \
    --arg nonce "$nonce" \
    --arg user "$localpart" \
    --arg pw "$password" \
    --arg mac "$mac" \
    --argjson admin "$is_admin" \
    '{nonce:$nonce,username:$user,password:$pw,admin:$admin,mac:$mac}')"
  resp="$(curl -fsS -X POST -H 'Content-Type: application/json' \
    --data "$body" \
    "$HOMESERVER_URL/_synapse/admin/v1/register")"
  echo "$resp"
}

# ---------- 4. register bot ----------
step_log "4. register bot $BOT_MXID (admin HMAC)"
BOT_RESP="$(admin_register "$BOT_LOCALPART" "$BOT_PASSWORD" false)"
BOT_ACCESS_TOKEN="$(jq -r .access_token <<<"$BOT_RESP")"
BOT_DEVICE_ID="$(jq -r .device_id <<<"$BOT_RESP")"
BOT_USER_ID="$(jq -r .user_id <<<"$BOT_RESP")"
[[ -n "$BOT_ACCESS_TOKEN" && "$BOT_ACCESS_TOKEN" != "null" ]] || fail "bot registration did not return an access_token"

# ---------- 5. register human ----------
step_log "5. register human $HUMAN_MXID (admin HMAC)"
HUMAN_RESP="$(admin_register "$HUMAN_LOCALPART" "$HUMAN_PASSWORD" false)"
HUMAN_ACCESS_TOKEN="$(jq -r .access_token <<<"$HUMAN_RESP")"
HUMAN_USER_ID="$(jq -r .user_id <<<"$HUMAN_RESP")"
[[ -n "$HUMAN_ACCESS_TOKEN" && "$HUMAN_ACCESS_TOKEN" != "null" ]] || fail "human registration did not return an access_token"

# ---------- 6. create private, encryption-disabled room (as human) + invite bot ----------
step_log "6. human creates room + invites bot"
ROOM_RESP="$(curl -fsS -X POST \
  -H "Authorization: Bearer $HUMAN_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  --data "$(jq -cn \
    --arg invitee "$BOT_USER_ID" \
    '{preset:"private_chat", name:"cosmon-e2e-whispers", invite:[$invitee], initial_state:[]}')" \
  "$HOMESERVER_URL/_matrix/client/v3/createRoom")"
ROOM_ID="$(jq -r .room_id <<<"$ROOM_RESP")"
[[ -n "$ROOM_ID" && "$ROOM_ID" != "null" ]] || fail "createRoom did not return a room_id (resp=$ROOM_RESP)"

# ---------- 7. bot joins ----------
step_log "7. bot joins $ROOM_ID"
ROOM_ID_ENC="$(jq -rn --arg r "$ROOM_ID" '$r|@uri')"
curl -fsS -X POST \
  -H "Authorization: Bearer $BOT_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  --data '{}' \
  "$HOMESERVER_URL/_matrix/client/v3/rooms/$ROOM_ID_ENC/join" >/dev/null

# ---------- 8. write credentials TOML ----------
step_log "8. write bot credentials TOML (0600)"
mkdir -p "$WORK_DIR"
cat >"$CREDS_FILE" <<EOF
user_id = "$BOT_USER_ID"
access_token = "$BOT_ACCESS_TOKEN"
device_id = "$BOT_DEVICE_ID"
homeserver = "$HOMESERVER_URL"
EOF
chmod 0600 "$CREDS_FILE"

cat >"$HUMAN_CREDS_FILE" <<EOF
user_id = "$HUMAN_USER_ID"
access_token = "$HUMAN_ACCESS_TOKEN"
homeserver = "$HOMESERVER_URL"
EOF
chmod 0600 "$HUMAN_CREDS_FILE"

# ---------- 9. write nucleon map for the human ----------
step_log "9. write nucleon map for $HUMAN_MXID (scope=peer)"
NUCLEON_DIR="$COSMON_STATE/nucleons/test-human"
mkdir -p "$NUCLEON_DIR"
cat >"$NUCLEON_DIR/matrix-identity.toml" <<EOF
mxid = "$HUMAN_MXID"
homeserver = "$HS_NAME"
verified_at = "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
verification_method = "e2e-demo-registration"
scope = "peer"
EOF

# ---------- 10. human sends message ----------
step_log "10. human sends test whisper"
WHISPER_BODY="hello-from-e2e-$(date +%s)-$(openssl rand -hex 4)"
TXN_ID="e2e-$(date +%s)-$(openssl rand -hex 6)"
SEND_RESP="$(curl -fsS -X PUT \
  -H "Authorization: Bearer $HUMAN_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  --data "$(jq -cn --arg b "$WHISPER_BODY" '{msgtype:"m.text", body:$b}')" \
  "$HOMESERVER_URL/_matrix/client/v3/rooms/$ROOM_ID_ENC/send/m.room.message/$TXN_ID")"
EVENT_ID="$(jq -r .event_id <<<"$SEND_RESP")"
[[ -n "$EVENT_ID" && "$EVENT_ID" != "null" ]] || fail "send did not return event_id (resp=$SEND_RESP)"

# ---------- 11. run matrix-echo-tick ----------
step_log "11. invoke matrix-echo-tick"
mkdir -p "$INBOX_DIR" "$STATE_DIR"
TICK_LOG="$WORK_DIR/tick.log"
set +e
"$MATRIX_ECHO_TICK" \
  --homeserver "$HOMESERVER_URL" \
  --user "$BOT_USER_ID" \
  --credentials-file "$CREDS_FILE" \
  --room "$ROOM_ID" \
  --inbox "$INBOX_DIR" \
  --state "$STATE_DIR" \
  --cosmon-state "$COSMON_STATE" \
  --trusted-homeservers "$HS_NAME" \
  --timeout-secs 15 \
  --max-events 50 \
  >"$TICK_LOG" 2>&1
TICK_RC=$?
set -e
if [[ "$TICK_RC" -ne 0 ]]; then
  sed 's/^/    tick> /' "$TICK_LOG" >&2
  fail "matrix-echo-tick exited with code $TICK_RC"
fi

# ---------- 12. verify whisper materialised ----------
step_log "12. verify whisper file in inbox"
MATCH="$(grep -rl "$WHISPER_BODY" "$INBOX_DIR" 2>/dev/null || true)"
if [[ -z "$MATCH" ]]; then
  step_log "tick stdout follows:"
  sed 's/^/    tick> /' "$TICK_LOG" >&2
  ls -la "$INBOX_DIR" >&2 || true
  fail "whisper body '$WHISPER_BODY' not found in $INBOX_DIR"
fi

# ---------- summary ----------
TOTAL=$(( $(date +%s) - START_EPOCH ))
echo
echo "=========================================================================="
echo " synapse-e2e-demo: SUCCESS in ${TOTAL}s"
echo "--------------------------------------------------------------------------"
printf " homeserver   : %s\n" "$HOMESERVER_URL"
printf " bot          : %s\n" "$BOT_USER_ID"
printf " human        : %s\n" "$HUMAN_USER_ID"
printf " room         : %s\n" "$ROOM_ID"
printf " whisper body : %s\n" "$WHISPER_BODY"
printf " materialised : %s\n" "$MATCH"
echo "=========================================================================="

if [[ "$TOTAL" -gt 120 ]]; then
  step_log "WARN: total runtime ${TOTAL}s exceeds 120s target"
fi
