#!/usr/bin/env bash
# Regression experiment for task-20260416-ef31 / delib-20260416-3879.
#
# Two failure modes, one invariant:
#
#   Mode A: fake-claude exits 42 at startup (silent exec failure analogue
#           of task-4046).
#   Mode B: real claude process is kill -9'd ~0.5s after `cs tackle`
#           returns (mid-spawn crash race).
#
# Invariant: in both modes, cosmon must leave ZERO surface lies —
# molecule NOT Running, fleet has no dangling worker, tmux carcass
# purged, `feat/<mol-id>` branch deleted.
#
# This script exits 0 when the invariant holds, non-zero otherwise. CI
# runs it on every PR (see `.github/workflows`). The measurement gate
# from the deliberation demands two weeks of clean runs before the
# reconciler spike (idea-20260416-c4e7) can be retired.

set -uo pipefail

# The parent-worker environment leaks COSMON_PARENT_MOL_ID + COSMON_MOL_DIR
# into any `cs nucleate` call, which would try to auto-link fixture
# molecules to the outer worker's molecule. Scrub them for the whole run.
unset COSMON_PARENT_MOL_ID COSMON_MOL_DIR

CS_BIN="${CS_BIN:-cs}"
TMPROOT="$(mktemp -d -t cosmon-zombie-XXXXXX)"
trap 'rm -rf "$TMPROOT"' EXIT

FAILED=0
fail() { echo "FAIL: $*" >&2; FAILED=1; }
ok()   { echo "ok:   $*"; }

have_tmux() { command -v tmux >/dev/null 2>&1; }
if ! have_tmux; then
  echo "skip: tmux not available" >&2
  exit 0
fi
if ! command -v "$CS_BIN" >/dev/null 2>&1; then
  echo "skip: $CS_BIN not on PATH" >&2
  exit 0
fi

run_scenario() {
  local name="$1" fake_script="$2"
  local project="$TMPROOT/$name"
  local fakebin="$TMPROOT/$name-bin"
  mkdir -p "$project" "$fakebin"

  "$CS_BIN" init --yes "$project" >/dev/null
  (
    cd "$project"
    git config user.email "test@test.local"
    git config user.name  "cosmon-test"
    git add -A
    git commit -q -m "init"
  )

  # Nucleate a throwaway task-work molecule. `--json` gives us the id.
  local mol_id
  mol_id="$(cd "$project" && "$CS_BIN" --json nucleate task-work --var "topic=zombie-$name" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
  if [ -z "$mol_id" ]; then fail "$name: could not nucleate"; return; fi

  # Plant the fake claude.
  printf '%s\n' "$fake_script" > "$fakebin/claude"
  chmod +x "$fakebin/claude"

  # Attempt the tackle under the hostile claude. We capture-but-ignore
  # the exit code here because for mode B the surface-lie vs not is
  # what we actually test, not the exit code.
  (cd "$project" && PATH="$fakebin:$PATH" "$CS_BIN" tackle "$mol_id" >/dev/null 2>&1) || true

  # Give mode B's `kill -9` a chance to fire, then sanity-wait.
  sleep 1

  # --- Surface assertions ---
  local state_file="$project/.cosmon/state/fleets/default/molecules/$mol_id/state.json"
  local status
  status="$(sed -n 's/.*"status"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$state_file" | head -n1)"
  if [ "$status" = "running" ]; then
    fail "$name: molecule is Running but worker died"
  else
    ok "$name: molecule status = $status (not running)"
  fi

  local fleet_file="$project/.cosmon/state/fleet.json"
  if [ -f "$fleet_file" ] && grep -q "\"$mol_id\"" "$fleet_file"; then
    fail "$name: fleet.json mentions $mol_id"
  else
    ok "$name: fleet.json clean"
  fi

  local project_id
  project_id="$(sed -n 's/^project_id[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$project/.cosmon/config.toml" | head -n1)"
  if [ -n "$project_id" ]; then
    local panes
    panes="$(tmux -L "$project_id" list-panes -a -F '#{session_name}|#{pane_dead}' 2>/dev/null || true)"
    if echo "$panes" | grep -q "zombie-$name"; then
      fail "$name: tmux pane for zombie-$name still present: $panes"
    else
      ok "$name: no tmux carcass"
    fi
    tmux -L "$project_id" kill-server 2>/dev/null || true
  fi

  if (cd "$project" && git branch --list "feat/$mol_id" | grep -q .); then
    fail "$name: orphan branch feat/$mol_id"
  else
    ok "$name: no orphan branch"
  fi
}

# ── Mode A: fake-claude-exits-42 ──────────────────────────────────────
run_scenario "A-exit42" '#!/bin/sh
exit 42'

# ── Mode B: real-looking claude that gets SIGKILLed mid-startup ───────
# We simulate the "spawned, about to come alive, then killed" path with
# a script that forks a background self-terminator and then would
# otherwise stay alive. `kill -9 $$` reliably terminates the pane.
run_scenario "B-sigkill" '#!/bin/sh
( sleep 0.5; kill -9 $PPID ) &
exec sleep 60'

if [ "$FAILED" -eq 0 ]; then
  echo "PASS: no surface lie in any scenario"
  exit 0
else
  echo "FAIL: experiment detected a surface lie" >&2
  exit 1
fi
