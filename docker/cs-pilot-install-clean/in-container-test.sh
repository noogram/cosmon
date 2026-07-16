#!/usr/bin/env bash
# in-container-test.sh — runs INSIDE the ephemeral container as the
# entrypoint (cs-pilot increment 2, TEST A — task-20260601-070b). Proves a
# freshly-built `cs` boots `cs pilot --experimental` on a VIRGIN box (no
# ~/.cosmon, no ~/.config/cosmon sediment) over a tiny .cosmon fixture,
# driving the HOST's Ollama. Exits non-zero on the first failed assertion.
#
# Environment (set by the host driver via `docker run -e`):
#   COSMON_PILOT_BASE_URL   host Ollama OpenAI endpoint, e.g.
#                           http://host.docker.internal:11434/v1
#   COSMON_PILOT_MODEL      tool-calling model (default qwen3:8b)
set -euo pipefail

# cs pilot reads these env var names directly (crates/cosmon-cli/src/cmd/pilot.rs).
BASE_URL="${COSMON_PILOT_BASE_URL:-http://host.docker.internal:11434/v1}"
MODEL="${COSMON_PILOT_MODEL:-qwen3:8b}"
# The pilot probes /api/tags (Ollama native) at the base host, but the
# OpenAI endpoint carries the /v1 suffix; derive the bare host for the probe.
PROBE_URL="${BASE_URL%/v1}"

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()  { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Virgin-box invariants -------------------------------------------
# The load-bearing assertion: there is NO host sediment. If a future image
# edit bakes in a ~/.cosmon or ~/.config/cosmon, this test stops proving
# "boots from nothing" and must go red immediately.
say "Asserting VIRGIN box — no ~/.cosmon, no ~/.config/cosmon sediment ..."
[ ! -e "$HOME/.cosmon" ] || die "$HOME/.cosmon exists — not a virgin box"
[ ! -e "$HOME/.config/cosmon" ] || die "$HOME/.config/cosmon exists — not a virgin box"
[ ! -e "$HOME/pilot-transcript.md" ] || die "a stale pilot transcript exists — not a virgin box"
ok "virgin box: no host-state sediment of any kind"

command -v cs >/dev/null 2>&1 || die "cs binary not found on PATH"
command -v jq >/dev/null 2>&1 || die "jq not found"
[ -z "${OPENAI_API_KEY:-}" ] || die "OPENAI_API_KEY set — clean room violated"

# Defensive: strip any inherited molecule context leaked via `docker run -e`.
unset COSMON_PARENT_MOL_ID COSMON_MOL_DIR COSMON_STATE_DIR || true

say "Probing host Ollama at $PROBE_URL ..."
curl -sf -m 5 "$PROBE_URL/api/tags" >/dev/null 2>&1 \
  || die "host Ollama not reachable at $PROBE_URL"
ok "host Ollama reachable; model = $MODEL"

# 1. Tiny .cosmon fixture --------------------------------------------
# A throwaway galaxy with ONE molecule, so the pilot's read-only tools
# (observe / peek / ensemble) have something real to inspect. Everything
# lives under the container HOME — the host's .cosmon/state is unreachable
# by construction (separate filesystem namespace, no volumes).
WORK="$HOME/pilot-fixture"
mkdir -p "$WORK"
cd "$WORK"
git init -q
say "cs init (virgin galaxy — first ever write to this HOME) ..."
cs init >/dev/null || die "cs init failed on a virgin box — hidden host-state dependency!"
ok "cs init succeeded from nothing"

say "Nucleating one fixture molecule (gives ensemble/observe real data) ..."
MOL_ID="$(cs nucleate task-work --json \
  --var topic="install-clean fixture molecule" \
  | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1)"
[ -n "$MOL_ID" ] || die "could not nucleate the fixture molecule on a virgin box"
ok "fixture molecule = $MOL_ID"
git add -A && git commit -qm "install-clean fixture" || true

# 2. Boot cs pilot --experimental over the fixture -------------------
# Feed one real operator line + /quit. `timeout` bounds a stuck model turn
# so the test fails loud rather than hanging. The load-bearing assertion is
# that the pilot BOOTS and EXITS 0 on a virgin box; a successful model
# round-trip is a bonus the host-Ollama gate makes likely but not required.
say "Launching cs pilot --experimental (one turn + /quit) ..."
PILOT_LOG="$(mktemp)"
set +e
COSMON_PILOT_BASE_URL="$BASE_URL" COSMON_PILOT_MODEL="$MODEL" \
  timeout 120 bash -c \
  'printf "How many molecules are pending in the backlog?\n/quit\n" | cs pilot --experimental' \
  >"$PILOT_LOG" 2>&1
RC=$?
set -e
sed 's/^/    | /' "$PILOT_LOG"

[ "$RC" -ne 124 ] || die "cs pilot timed out (120s) — the REPL hung on a virgin box"
[ "$RC" -eq 0 ] || die "cs pilot exited $RC on a virgin box — hidden host-state dependency or boot crash"
ok "cs pilot booted and exited 0 on a virgin box"

# 3. Transcript proof -------------------------------------------------
# The REPL records the operator line immediately, so the transcript is
# non-empty even before the model replies — proof the loop actually ran.
TRANSCRIPT="$WORK/pilot-transcript.md"
[ -f "$TRANSCRIPT" ] || die "pilot-transcript.md not written — the REPL never recorded a turn"
[ -s "$TRANSCRIPT" ] || die "pilot-transcript.md is empty — the REPL recorded nothing"
ok "pilot-transcript.md written ($(wc -c < "$TRANSCRIPT") bytes)"

printf '\n\033[1;32m═══ INSTALL-CLEAN GREEN: freshly-built cs pilot booted on a VIRGIN box, no host sediment ═══\033[0m\n'
