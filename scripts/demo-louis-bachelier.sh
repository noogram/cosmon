#!/usr/bin/env bash
#
# demo-louis-bachelier.sh — rehearsable script for the 2026-04-22 demo
# at Institut Louis Bachelier.
#
# Runs the full cosmon-saas thin-client flow end-to-end. Safe to execute
# on localhost (defaults to http://127.0.0.1:8080). Pass the server URL
# and API key via env vars to hit a tunnel.
#
# Usage (localhost rehearsal):
#   COSMON_SAAS_API_KEY="$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')" \
#     ./scripts/demo-louis-bachelier.sh
#
# Usage (tunnel, jour J):
#   CS_SERVER=https://cosmon-demo.noogram.dev \
#   COSMON_SAAS_API_KEY=<clé-du-jour> \
#     ./scripts/demo-louis-bachelier.sh

set -euo pipefail

SERVER="${CS_SERVER:-http://127.0.0.1:8080}"
KEY="${COSMON_SAAS_API_KEY:?COSMON_SAAS_API_KEY must be set}"
DEFAULT_TOPIC="Écrire une page wikipedia-like sur le lemme d Itô (<=300 mots, niveau M1 maths appliquées)"
TOPIC="${CS_DEMO_TOPIC:-$DEFAULT_TOPIC}"
ART_DIR="${CS_ART_DIR:-./cosmon-artifacts}"

# Prefer local release binary; fall back to PATH.
CS_CLIENT="${CS_CLIENT:-./target/release/cs-client}"
if [[ ! -x "$CS_CLIENT" ]]; then
  CS_CLIENT="cs-client"
fi

narrate() { printf '\n\033[36m# %s\033[0m\n' "$*"; }

narrate "Check server liveness — ~250 ms"
"$CS_CLIENT" --server "$SERVER" --api-key "$KEY" healthz

narrate "Full pilot cycle: nucleate → tackle → wait → done → fetch"
narrate "Topic: $TOPIC"
"$CS_CLIENT" --server "$SERVER" --api-key "$KEY" \
  --artifacts-dir "$ART_DIR" \
  run task-work --var "topic=$TOPIC"

narrate "Fetched artifacts — only markdown, no state.json, no formulas"
LATEST_DIR="$(ls -dt "$ART_DIR"/task-* 2>/dev/null | head -1 || true)"
if [[ -z "$LATEST_DIR" ]]; then
  echo "no artifacts fetched — check server logs" >&2
  exit 1
fi
ls -la "$LATEST_DIR"

narrate "Show the synthesis (or prompt if synthesis missing)"
if [[ -f "$LATEST_DIR/synthesis.md" ]]; then
  head -40 "$LATEST_DIR/synthesis.md"
elif [[ -f "$LATEST_DIR/prompt.md" ]]; then
  head -20 "$LATEST_DIR/prompt.md"
else
  echo "(no synthesis.md nor prompt.md found)"
fi

narrate "Done. Tarball artefacts in $LATEST_DIR"
