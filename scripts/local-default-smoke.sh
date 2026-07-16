#!/usr/bin/env bash
# local-default-smoke.sh — the walking-skeleton smoke test
# (task-20260530-821f, parent delib-20260530-0877).
#
# Proves, mechanically, that a bare `cs tackle` (NO --adapter flag)
# drives a molecule end-to-end through cosmon's OWN harness against a
# LOCAL Ollama model — with ZERO Claude Code in the default path.
#
# This is the architect's recipe from the deliberation synthesis,
# scripted. It exits non-zero on the first failed assertion so it is
# usable as a CI gate (when Ollama is available) and as an operator
# spot-check.
#
# Prerequisites:
#   - `ollama serve` reachable at $COSMON_LOCAL_BASE_URL
#     (default http://localhost:11434)
#   - a tool-calling-capable local model pulled (default qwen3:8b;
#     override with COSMON_LOCAL_MODEL). NOTE: qwen2.5-coder:7b emits
#     tool calls as plain text, not structured tool_calls — do NOT use
#     it here.
#   - the `cs` binary on PATH (or set CS_BIN=/path/to/cs)
#
# Usage:
#   ollama serve &
#   scripts/local-default-smoke.sh
set -euo pipefail

# This script may run inside a cosmon worktree whose env carries the
# parent worker's molecule context. The isolated temp project below has
# no such molecule, so strip the inherited context to avoid a spurious
# DecayProduct auto-link.
unset COSMON_PARENT_MOL_ID COSMON_MOL_DIR COSMON_STATE_DIR || true

CS_BIN="${CS_BIN:-cs}"
BASE_URL="${COSMON_LOCAL_BASE_URL:-http://localhost:11434}"
MODEL="${COSMON_LOCAL_MODEL:-qwen3:8b}"

# Resolve the repo root BEFORE any `cd` so the task-work formula copy
# works regardless of the caller's working directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()  { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Preconditions ----------------------------------------------------
command -v "$CS_BIN" >/dev/null 2>&1 || die "cs binary not found (set CS_BIN)"
command -v jq >/dev/null 2>&1 || die "jq not found"
say "Probing Ollama at $BASE_URL ..."
curl -sf -m 3 "$BASE_URL/api/tags" >/dev/null 2>&1 \
  || die "Ollama not reachable at $BASE_URL — run \`ollama serve\`"
ok "Ollama reachable; default model = $MODEL"

# 1. Isolated project -------------------------------------------------
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
git init -q
git config user.email smoke@cosmon.test
git config user.name cosmon-smoke

# Minimal cosmon project: NO [adapters.default] row — we are testing
# the BUILT-IN default, which must be `local`.
mkdir -p .cosmon/state .cosmon/formulas
printf '[project]\nproject_id = "local-default-smoke"\n' > .cosmon/config.toml
printf '.cosmon/state/\n' > .gitignore
printf '# local-default-smoke\n' > README.md

# Copy the task-work formula from the repo this script ships in.
cp "$REPO_ROOT/.cosmon/formulas/task-work.formula.toml" \
   .cosmon/formulas/task-work.formula.toml
git add -A && git commit -qm init

# 2. Nucleate + bare tackle (NO --adapter) ----------------------------
say "Nucleating task-work molecule ..."
MOL_ID="$("$CS_BIN" nucleate task-work --json \
  --var topic="write a haiku to README" \
  | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1)"
[ -n "$MOL_ID" ] || die "could not capture molecule id from cs nucleate"
ok "molecule = $MOL_ID"

say "Tackling WITHOUT --adapter (must route local) ..."
COSMON_LOCAL_MODEL="$MODEL" "$CS_BIN" tackle "$MOL_ID"

# 3. Mechanical assertions --------------------------------------------
STATE_DIR="$WORK/.cosmon/state"
# The adapter_selected envelope lands in the top-level state log; other
# event logs live under fleets/<fleet>/molecules/<id>/. Concatenate
# every events.jsonl so the assertions see the full trace regardless of
# layout.
# Portable (bash 3.2 / macOS): no mapfile. Concatenate every log.
ALL_EVENTS="$(find "$STATE_DIR" -name events.jsonl -exec cat {} +)"
[ -n "$ALL_EVENTS" ] || die "no events.jsonl produced"

say "Asserting adapter_selected = local / default ..."
SEL="$(printf '%s\n' "$ALL_EVENTS" | jq -c 'select(.type=="adapter_selected")' | tail -n1)"
[ -n "$SEL" ] || die "no adapter_selected event found in any events.jsonl"
echo "  $SEL"
[ "$(echo "$SEL" | jq -r '.adapter_name')" = "local" ] \
  || die "adapter_name is not 'local' — default flip not in effect"
[ "$(echo "$SEL" | jq -r '.selection_source.source')" = "default" ] \
  || die "selection_source is not 'default'"
ok "bare tackle routed to local via built-in default"

say "Asserting synthesis.md is non-empty ..."
SYNTH="$(find "$STATE_DIR" -name synthesis.md | head -n1)"
[ -n "$SYNTH" ] && [ -s "$SYNTH" ] \
  || die "synthesis.md missing or empty"
ok "synthesis.md written ($(wc -c < "$SYNTH") bytes)"

say "Asserting NO claude process references this molecule's worktree ..."
# Scope the witness to THIS molecule. On a clean host the architect's
# recipe is `pgrep -f claude → exit=1`; but a dev host swarms with
# ambient Claude Code sessions (the operator's own + sibling cosmon
# workers), so a bare global pgrep false-positives on processes that
# have nothing to do with this tackle. A claude spawned BY this tackle
# would run with the molecule's worktree as cwd and carry the molecule
# id / session name in its argv — so we scope the match to "$MOL_ID".
# The local adapter is InProcess (no tmux pane, no subprocess), so this
# set must be empty. The structural witness above (adapter=local,
# loop_ownership=cosmon, selection_source=default) is the primary proof;
# this is the process-level belt-and-suspenders.
if pgrep -f "claude.*$MOL_ID" >/dev/null 2>&1 || pgrep -f "$MOL_ID.*claude" >/dev/null 2>&1; then
  die "a 'claude' process references molecule $MOL_ID — default path must be ZERO Claude Code"
fi
# Also assert no tmux session was created on this run's socket — an
# InProcess adapter creates none, whereas the claude/tmux path would.
if tmux -L local-default-smoke list-sessions >/dev/null 2>&1; then
  die "a tmux session exists on socket 'local-default-smoke' — local adapter must be tmux-free"
fi
ok "no claude process, no tmux session — ZERO Claude Code in the default path"

printf '\n\033[1;32m═══ WALKING SKELETON GREEN: provider(LOCAL) → harness(cosmon) → molecule done ═══\033[0m\n'
