#!/usr/bin/env bash
# dev-mcp-harness.sh — serve the cosmon `/mcp` connector locally on a real
# port (tailnet), fronted by a test IdP that mints throwaway JWTs, so you can
# smoke-test the connector from Claude Desktop.
# (task-20260709-6672 — "Harnais dev: servir /mcp en local sur un vrai port
# (tailnet) avec JWT de test, pour smoke-test Claude Desktop".)
#
# What it wires, and why each piece exists
# ----------------------------------------
# The production `/mcp` surface is the THIRD projection of the cosmon core,
# nested on the rpp-adapter's single listener behind a bearer gate
# (delib-20260709-943e). To reach it you need the whole admission chain live:
#
#   Claude Desktop ──HTTP+Bearer──▶ cosmon-rpp-adapter  :$ADAPTER_PORT (tailnet)
#                                        │  validates JWT against JWKS
#                                        │  resolves (iss,sub,aud) → noyau
#                                        ▼
#                                   cs-oidc-mock         :$MOCK_PORT (localhost)
#                                        JWKS + POST /issue (embedded RSA test key)
#
# This script stands up both processes, provisions the trust seam between them
# (a `trusted-issuers.toml` allowlist + one sealed `oidc-identity.toml`
# nucleon binding), mints a test JWT, curl-smokes the `/mcp` initialize
# handshake, then prints the exact Claude Desktop connector config.
#
#   ⚠️  DEMO ONLY. `cs-oidc-mock` signs with a private RSA key committed in
#   plaintext to this repo — every token it mints is trivially forgeable.
#   Never run this outside your own tailnet, never with `posture=active`
#   against real state. Point it at a scratch state dir (the default).
#
# Usage
# -----
#   scripts/dev-mcp-harness.sh                 # build, boot, mint, print, wait
#   scripts/dev-mcp-harness.sh --bind 127.0.0.1:8443   # localhost only
#   scripts/dev-mcp-harness.sh --keep          # reuse the dev state dir
#   scripts/dev-mcp-harness.sh --no-build      # skip cargo build (fast reboot)
#   scripts/dev-mcp-harness.sh --help
#
# Env overrides (all optional):
#   COSMON_DEV_MCP_HOME   dev state root      (default: $TMPDIR/cosmon-dev-mcp)
#   ADAPTER_PORT          adapter port        (default: 8443)
#   MOCK_PORT             oidc-mock port      (default: 8444)
#   NOYAU                 tenant galaxy slot  (default: dev)
#   SUB                   JWT subject         (default: dev-operator)
#   TOKEN_LIFETIME        JWT lifetime (secs) (default: 86400)
#
# Ctrl-C tears both processes down. `--keep` preserves the state dir so a
# second run reuses the galaxy tree and bindings.
set -euo pipefail

# ── styling ──────────────────────────────────────────────────────────────
say()  { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m! %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }
hr()   { printf '\033[2m%s\033[0m\n' "────────────────────────────────────────────────────────────"; }

# ── config ───────────────────────────────────────────────────────────────
DEV_HOME="${COSMON_DEV_MCP_HOME:-${TMPDIR:-/tmp}/cosmon-dev-mcp}"
DEV_HOME="${DEV_HOME%/}"
ADAPTER_PORT="${ADAPTER_PORT:-8443}"
MOCK_PORT="${MOCK_PORT:-8444}"
NOYAU="${NOYAU:-dev}"
SUB="${SUB:-dev-operator}"
TOKEN_LIFETIME="${TOKEN_LIFETIME:-86400}"

# These MUST match cs-oidc-mock's compiled-in defaults (DEFAULT_ISSUER /
# DEFAULT_AUDIENCE / DEFAULT_KID in crates/cosmon-oidc-testkit/src/mock.rs).
# The mock burns `iss` into every token and pins the JWKS `kid`; the adapter
# matches both byte-for-byte, so a drift here is an instant deny.
ISSUER="https://idp.test.cosmon-oidc-testkit"
AUDIENCE="cosmon-rpp-test"

BIND_OVERRIDE=""
DO_BUILD=1
KEEP_STATE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --bind)     BIND_OVERRIDE="${2:?--bind needs an addr}"; shift 2 ;;
    --bind=*)   BIND_OVERRIDE="${1#*=}"; shift ;;
    --keep)     KEEP_STATE=1; shift ;;
    --no-build) DO_BUILD=0; shift ;;
    -h|--help)  grep -E '^# ' "$0" | sed -E 's/^# ?//'; exit 0 ;;
    *)          die "unknown flag: $1 (see --help)" ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── 0. preconditions ─────────────────────────────────────────────────────
command -v cargo >/dev/null 2>&1 || die "cargo not found"
command -v curl  >/dev/null 2>&1 || die "curl not found"
command -v jq    >/dev/null 2>&1 || die "jq not found (brew install jq)"

# ── 1. resolve the bind address (tailnet by default) ─────────────────────
# The whole point of the harness is a *real* port reachable from the device
# running Claude Desktop, so we bind the adapter on the Tailscale IP when we
# can find one. Fall back to localhost (Claude Desktop on the same machine).
resolve_bind() {
  if [ -n "$BIND_OVERRIDE" ]; then
    printf '%s' "$BIND_OVERRIDE"; return
  fi
  if command -v tailscale >/dev/null 2>&1; then
    local ip; ip="$(tailscale ip -4 2>/dev/null | head -n1 || true)"
    if [ -n "$ip" ]; then printf '%s:%s' "$ip" "$ADAPTER_PORT"; return; fi
  fi
  warn "no Tailscale IPv4 found — binding on 127.0.0.1 (localhost only)."
  printf '127.0.0.1:%s' "$ADAPTER_PORT"
}
ADAPTER_BIND="$(resolve_bind)"
ADAPTER_HOST="${ADAPTER_BIND%:*}"

# ── 2. build binaries ────────────────────────────────────────────────────
if [ "$DO_BUILD" -eq 1 ]; then
  say "Building cs, cosmon-rpp-adapter, cs-oidc-mock (debug) ..."
  ( cd "$REPO_ROOT" && cargo build \
      -p cosmon-cli --bin cs \
      -p cosmon-rpp-adapter --bin cosmon-rpp-adapter \
      -p cosmon-oidc-testkit --bin cs-oidc-mock ) \
    || die "cargo build failed"
  ok "binaries built"
fi
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug"
CS_BIN="$TARGET_DIR/cs"
ADAPTER_BIN="$TARGET_DIR/cosmon-rpp-adapter"
MOCK_BIN="$TARGET_DIR/cs-oidc-mock"
for b in "$CS_BIN" "$ADAPTER_BIN" "$MOCK_BIN"; do
  [ -x "$b" ] || die "missing binary $b — run without --no-build"
done

# ── 3. provision the dev state dir ───────────────────────────────────────
# Layout the adapter expects under state_dir:
#   security/trusted-issuers.toml    — authn allowlist (which JWKS to fetch)
#   nucleons/<id>/oidc-identity.toml — authz binding ((iss,sub,aud) → noyau)
# The galaxy tree under galaxies_root/<noyau>/ is materialised by the
# adapter's own boot-time image_init (eager B2), so we don't `cs init` here.
STATE_DIR="$DEV_HOME/state"
GALAXIES_ROOT="$DEV_HOME/galaxies"
CONFIG_PATH="$DEV_HOME/rpp.toml"

if [ "$KEEP_STATE" -eq 0 ] && [ -d "$DEV_HOME" ]; then
  say "Resetting dev state dir $DEV_HOME (use --keep to preserve) ..."
  rm -rf "$DEV_HOME"
fi
mkdir -p "$STATE_DIR/security" "$STATE_DIR/nucleons/$NOYAU" "$GALAXIES_ROOT"

# 3a. trusted-issuers allowlist. `iss` matches the token; `jwks_uri` is the
#     real fetch target (the mock, on localhost — adapter and mock share the
#     host, so no split-DNS). audiences move here off the wire JWKS.
cat > "$STATE_DIR/security/trusted-issuers.toml" <<EOF
# dev-mcp-harness — trust the local cs-oidc-mock ONLY. DEMO key, embargoed.
[[issuer]]
iss = "$ISSUER"
jwks_uri = "http://127.0.0.1:$MOCK_PORT/jwks"
audiences = ["$AUDIENCE"]
EOF

# 3b. the sealed nucleon binding, rendered by the adapter's own audited
#     `nucleon render` path (never hand-authored — the render validates the
#     four-tuple and BLAKE3-seals on load).
"$ADAPTER_BIN" nucleon render \
  --noyau "$NOYAU" \
  --sub "$SUB" \
  --iss "$ISSUER" \
  --aud "$AUDIENCE" \
  --scope "cosmon:molecule:read" \
  --scope "cosmon:molecule:write" \
  > "$STATE_DIR/nucleons/$NOYAU/oidc-identity.toml" \
  || die "nucleon render failed"
ok "provisioned trusted-issuers + nucleon binding (noyau=$NOYAU sub=$SUB)"

# 3c. operator config. Bind on the tailnet; state + galaxies on the scratch
#     tree; prepared posture so long-lived dev JWTs are warned, not refused.
cat > "$CONFIG_PATH" <<EOF
# dev-mcp-harness — throwaway config. Regenerated each run unless --keep.
bind_addr = "$ADAPTER_BIND"
posture = "prepared"
cs_path = "$CS_BIN"
state_dir = "$STATE_DIR"
galaxies_root = "$GALAXIES_ROOT"
whispers_inbox_root = "$STATE_DIR/whispers/inbox"
subprocess_timeout_sec = 30
EOF

# ── 4. launch mock + adapter ─────────────────────────────────────────────
LOG_DIR="$DEV_HOME/logs"; mkdir -p "$LOG_DIR"
MOCK_LOG="$LOG_DIR/oidc-mock.log"
ADAPTER_LOG="$LOG_DIR/rpp-adapter.log"
MOCK_PID=""; ADAPTER_PID=""

cleanup() {
  [ -n "$ADAPTER_PID" ] && kill "$ADAPTER_PID" 2>/dev/null || true
  [ -n "$MOCK_PID" ]    && kill "$MOCK_PID"    2>/dev/null || true
  wait 2>/dev/null || true
  hr
  if [ "$KEEP_STATE" -eq 1 ]; then
    say "Harness stopped. State kept at $DEV_HOME (--keep)."
  else
    rm -rf "$DEV_HOME"
    say "Harness stopped. State dir removed."
  fi
}
trap cleanup INT TERM EXIT

wait_http() { # url attempts label
  local url="$1" tries="${2:-40}" label="${3:-service}" i
  for ((i=0; i<tries; i++)); do
    curl -sf -m 2 "$url" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  die "$label did not come up at $url — see the log"
}

say "Starting cs-oidc-mock on 127.0.0.1:$MOCK_PORT ..."
"$MOCK_BIN" --bind "127.0.0.1:$MOCK_PORT" \
  --issuer "$ISSUER" --audience "$AUDIENCE" \
  >"$MOCK_LOG" 2>&1 &
MOCK_PID=$!
wait_http "http://127.0.0.1:$MOCK_PORT/healthz" 40 "cs-oidc-mock"
ok "cs-oidc-mock live (JWKS + /issue)"

say "Starting cosmon-rpp-adapter on $ADAPTER_BIND ..."
"$ADAPTER_BIN" --config "$CONFIG_PATH" \
  >"$ADAPTER_LOG" 2>&1 &
ADAPTER_PID=$!
wait_http "http://$ADAPTER_BIND/healthz" 80 "cosmon-rpp-adapter"
ok "cosmon-rpp-adapter live"

# ── 5. mint a test JWT ───────────────────────────────────────────────────
say "Minting test JWT (sub=$SUB, lifetime=${TOKEN_LIFETIME}s) ..."
ISSUE_URL="http://127.0.0.1:$MOCK_PORT/issue?sub=$SUB&aud=$AUDIENCE&scope=cosmon:molecule:read+cosmon:molecule:write&lifetime=$TOKEN_LIFETIME"
TOKEN="$(curl -sf -X POST "$ISSUE_URL" | jq -r '.access_token')"
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || die "JWT mint failed (see $MOCK_LOG)"
ok "JWT minted"

# ── 6. smoke-check the /mcp handshake ────────────────────────────────────
# The gate must (a) reject anonymous and (b) admit our JWT. A valid JWT that
# passes the gate reaches the Streamable-HTTP transport; whatever it answers,
# it is NOT our 401. We assert both halves.
say "Smoke-checking /mcp gate ..."
MCP_URL="http://$ADAPTER_BIND/mcp"
INIT_BODY='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"dev-harness","version":"0.0.0"}}}'

anon_code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$MCP_URL" \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d "$INIT_BODY")"
[ "$anon_code" = "401" ] || warn "anonymous /mcp returned $anon_code (expected 401)"
[ "$anon_code" = "401" ] && ok "anonymous /mcp correctly refused (401)"

auth_code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$MCP_URL" \
  -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d "$INIT_BODY")"
if [ "$auth_code" = "401" ]; then
  die "authenticated /mcp returned 401 — the JWT was rejected. See $ADAPTER_LOG"
fi
ok "authenticated /mcp passed the gate (HTTP $auth_code)"

# ── 7. print the Claude Desktop connector config ─────────────────────────
# `mcp-remote` refuses a non-HTTPS URL unless the host is localhost
# ("Non-HTTPS URLs only allowed for localhost"), so a tailnet HTTP endpoint
# needs `--allow-http`. And because the adapter speaks Streamable HTTP (not
# the legacy SSE transport), pin `--transport http-only` so the bridge does
# not waste a probe on SSE first. Both flags are harmless for a localhost
# bind, so we emit them unconditionally.
CONFIG_JSON="$DEV_HOME/claude-desktop-connector.json"
cat > "$CONFIG_JSON" <<EOF
{
  "mcpServers": {
    "cosmon-dev": {
      "command": "npx",
      "args": [
        "-y", "mcp-remote",
        "http://$ADAPTER_BIND/mcp",
        "--allow-http",
        "--transport", "http-only",
        "--header", "Authorization: Bearer $TOKEN"
      ]
    }
  }
}
EOF

hr
ok "Cosmon /mcp connector is live for smoke-testing."
echo
echo "  MCP endpoint : http://$ADAPTER_BIND/mcp"
echo "  Bearer token : (${TOKEN_LIFETIME}s lifetime, sub=$SUB, noyau=$NOYAU)"
echo "  Logs         : $ADAPTER_LOG"
echo "                 $MOCK_LOG"
echo
echo "  Claude Desktop — add to claude_desktop_config.json (mcp-remote bridge):"
echo "    written to → $CONFIG_JSON"
echo
sed 's/^/    /' "$CONFIG_JSON"
echo
if [ "$ADAPTER_HOST" != "127.0.0.1" ]; then
  echo "  Reachable from any tailnet device at http://$ADAPTER_BIND/mcp."
  echo "  For a public HTTPS front (optional): tailscale serve --bg $ADAPTER_PORT"
else
  echo "  Localhost bind — Claude Desktop must run on THIS machine."
  echo "  For a tailnet bind, pass --bind <tailscale-ip>:$ADAPTER_PORT."
fi
echo
echo "  Copy-paste curl to re-drive the handshake:"
echo "    curl -sN -X POST http://$ADAPTER_BIND/mcp \\"
echo "      -H 'authorization: Bearer $TOKEN' \\"
echo "      -H 'content-type: application/json' \\"
echo "      -H 'accept: application/json, text/event-stream' \\"
echo "      -d '$INIT_BODY'"
hr
say "Serving. Press Ctrl-C to tear down."
wait
