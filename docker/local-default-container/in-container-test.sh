#!/usr/bin/env bash
# in-container-test.sh — runs INSIDE the ephemeral container as the
# entrypoint (task-20260601-989e). Drives cosmon's local-default
# autonomy loop end-to-end against the HOST's Ollama and asserts the
# clean-room invariants mechanically. Exits non-zero on the first
# failed assertion so the host driver (and CI) can gate on it.
#
# This is the containerized sibling of scripts/local-default-smoke.sh:
# same assertions, but the isolation is structural (fresh container HOME,
# no claude binary anywhere) rather than scoped-by-process-name. Because
# claude is not installed at all, the "ZERO Claude Code" proof collapses
# from "no claude process referencing this molecule" to "claude is not on
# PATH" — a stronger, by-construction guarantee.
#
# Environment (set by the host driver via `docker run -e`):
#   COSMON_LOCAL_BASE_URL   host Ollama, e.g. http://host.docker.internal:11434
#   COSMON_LOCAL_MODEL      tool-calling model (default qwen3:8b)
set -euo pipefail

BASE_URL="${COSMON_LOCAL_BASE_URL:-http://host.docker.internal:11434}"
MODEL="${COSMON_LOCAL_MODEL:-qwen3:8b}"

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()  { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Structural preconditions ----------------------------------------
# The load-bearing assertion of the whole image: claude must be absent.
# We check it FIRST and FAIL CLOSED — if a future image edit reintroduces
# claude on PATH, this test stops being a structural autonomy proof and
# must go red immediately.
say "Asserting claude / aider / codex are ABSENT from PATH (structural autonomy proof) ..."
for forbidden in claude aider codex; do
  if command -v "$forbidden" >/dev/null 2>&1; then
    die "'$forbidden' is on PATH inside the container — the no-other-path guarantee is broken"
  fi
done
ok "no claude/aider/codex binary anywhere — exec→claude escape is structurally impossible"

command -v cs >/dev/null 2>&1 || die "cs binary not found on PATH"
command -v jq >/dev/null 2>&1 || die "jq not found"

# Clean-room invariant: no host cosmon config can be visible.
[ ! -e "$HOME/.config/cosmon/config.toml" ] \
  || die "$HOME/.config/cosmon/config.toml exists — clean room violated"
# No cloud key in the container env.
[ -z "${OPENAI_API_KEY:-}" ] || die "OPENAI_API_KEY is set in the container — clean room violated"
ok "clean room: no ~/.config/cosmon/config.toml, no OPENAI_API_KEY"

# Strip any inherited molecule context (defensive — the image carries
# none, but `docker run -e` could leak the host worker's vars).
unset COSMON_PARENT_MOL_ID COSMON_MOL_DIR COSMON_STATE_DIR || true

say "Probing host Ollama at $BASE_URL ..."
curl -sf -m 5 "$BASE_URL/api/tags" >/dev/null 2>&1 \
  || die "host Ollama not reachable at $BASE_URL"
ok "host Ollama reachable; model = $MODEL"

# 1. Throwaway galaxy via `cs init` ----------------------------------
# Everything happens under the container's HOME — the host's .cosmon/state
# is unreachable by construction (separate filesystem namespace).
WORK="$HOME/throwaway-galaxy"
mkdir -p "$WORK"
cd "$WORK"
git init -q
say "cs init (seeds canonical formulas, incl. task-work) ..."
cs init >/dev/null
# Critical: the freshly-seeded config must NOT carry an [adapters.default]
# row. If it did, the bare-tackle would route to whatever it names instead
# of the built-in `local` floor — defeating the test.
if grep -q '\[adapters.default\]' .cosmon/config.toml 2>/dev/null; then
  die ".cosmon/config.toml carries [adapters.default] — built-in floor would be bypassed"
fi
ok "throwaway galaxy initialized, no [adapters.default] override"
git add -A && git commit -qm "init throwaway galaxy" || true

# 2. Nucleate + bare tackle (NO --adapter) ---------------------------
say "Nucleating task-work molecule ..."
MOL_ID="$(cs nucleate task-work --json \
  --var topic="write a haiku to README" \
  | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1)"
[ -n "$MOL_ID" ] || die "could not capture molecule id from cs nucleate"
ok "molecule = $MOL_ID"

say "Tackling WITHOUT --adapter (must route to built-in local floor) ..."
COSMON_LOCAL_MODEL="$MODEL" COSMON_LOCAL_BASE_URL="$BASE_URL" cs tackle "$MOL_ID"

# 3. Mechanical assertions -------------------------------------------
STATE_DIR="$WORK/.cosmon/state"
ALL_EVENTS="$(find "$STATE_DIR" -name events.jsonl -exec cat {} +)"
[ -n "$ALL_EVENTS" ] || die "no events.jsonl produced"

say "Asserting adapter_selected = local / default / cosmon-owned loop ..."
SEL="$(printf '%s\n' "$ALL_EVENTS" | jq -c 'select(.type=="adapter_selected")' | tail -n1)"
[ -n "$SEL" ] || die "no adapter_selected event found"
echo "  $SEL"
[ "$(echo "$SEL" | jq -r '.adapter_name')" = "local" ] \
  || die "adapter_name is not 'local' — default flip not in effect"
[ "$(echo "$SEL" | jq -r '.selection_source.source')" = "default" ] \
  || die "selection_source is not 'default' (built-in floor)"
[ "$(echo "$SEL" | jq -r '.loop_ownership')" = "cosmon" ] \
  || die "loop_ownership is not 'cosmon' — the harness loop is not cosmon's"
ok "bare tackle routed to local, via built-in default, cosmon owns the loop"

say "Asserting synthesis.md is non-empty ..."
SYNTH="$(find "$STATE_DIR" -name synthesis.md | head -n1)"
[ -n "$SYNTH" ] && [ -s "$SYNTH" ] || die "synthesis.md missing or empty"
ok "synthesis.md written ($(wc -c < "$SYNTH") bytes)"

say "Asserting ZERO claude process and no claude tmux session ..."
# Structural: claude is not installed, so this is belt-and-suspenders.
if pgrep -f claude >/dev/null 2>&1; then
  die "a 'claude' process exists in the container — impossible unless the image was tampered with"
fi
# The local adapter is InProcess: it creates no tmux session at all.
if tmux list-sessions >/dev/null 2>&1; then
  die "a tmux session exists — the local InProcess adapter must be tmux-free"
fi
ok "no claude process, no tmux session — ZERO Claude Code in the default path"

printf '\n\033[1;32m═══ CONTAINER GREEN: provider(LOCAL host Ollama) → harness(cosmon) → molecule done, no-claude-by-construction ═══\033[0m\n'
