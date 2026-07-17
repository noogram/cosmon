#!/usr/bin/env bash
# fault-exit-42.sh — assert that `cs tackle` fails cleanly when claude
# would have exited 42 and tmux would have surfaced a dead pane.
#
# Scenario (dual fault injection):
#   FAKE_CLAUDE_MODE=exit-42    — if forked, fake-claude exits 42
#   FAKE_TMUX_PANE_DEAD=1       — fake-tmux reports the pane as dead
#
# Why both? fake-tmux does not fork subprocesses by default, so
# FAKE_CLAUDE_MODE alone has no observable effect on the pane_dead bit.
# FAKE_TMUX_PANE_DEAD=1 is the reality we want cosmon to observe:
# "the pane is dead, whatever caused it." Together they model the
# task-4046 production carcass: remain-on-exit swallowed claude's
# exit-42 and left a `[exited]` session; with `list_sessions` now
# filtering on `pane_dead`, `is_alive` returns false → the post-ef31
# `observe_spawn_postcondition` must fail → tackle must return non-zero.
#
# Pre-ef31, tackle would have returned 0 and the fleet surface would
# have claimed a healthy worker. This script is the boundary sentinel
# that prevents that regression from ever coming back.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
FAKES_DIR="$REPO/tests/fakes"

# shellcheck source=_assertions.sh
. "$HERE/_assertions.sh"

CS_BIN="$(smoke_resolve_cs_bin "$REPO")" || exit 2

OUTDIR="${SMOKE_OUTDIR:-$REPO/target/smoke-exit42-$$}"
# The scratch project lives OUTSIDE the repo: the public tree ships
# .cosmon/config.toml (project-root marker), and cs init refuses to
# nest a galaxy under an existing one — walk-up discovery would break.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/cosmon-smoke-XXXXXX")/project"
FAKE_TMUX_STATE="$OUTDIR/fake-tmux"
mkdir -p "$SCRATCH" "$FAKE_TMUX_STATE"

export PATH="$FAKES_DIR/fake-tmux:$FAKES_DIR/fake-claude:$PATH"
export FAKE_TMUX_DIR="$FAKE_TMUX_STATE"
export FAKE_TMUX_TRACE="$OUTDIR/fake-tmux.log"
unset COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_RUNTIME_ACTIVE

# Dual fault injection.
export FAKE_CLAUDE_MODE=exit-42
export FAKE_TMUX_PANE_DEAD=1
export COSMON_READINESS_TIMEOUT_SECS=5

echo "fault-exit-42: cs=$CS_BIN"
echo "fault-exit-42: artifacts → $OUTDIR"

smoke_bootstrap_project "$SCRATCH" "$REPO" "$CS_BIN" \
    || { echo "fault-exit-42: bootstrap failed" >&2; exit 2; }

raw="$(cd "$SCRATCH" && "$CS_BIN" nucleate hello --var topic="fault-exit-42" --json 2>/dev/null)"
MOL="$(smoke_json_field id "$raw")"
if [ -z "$MOL" ]; then
    _smoke_fail "nucleate — could not extract molecule id"
    smoke_summary "fault-exit-42"
    exit 1
fi
_smoke_pass "nucleate: $MOL"

# ── tackle must fail ──────────────────────────────────────────────────────
if (cd "$SCRATCH" && "$CS_BIN" tackle "$MOL" > "$OUTDIR/tackle.stdout" 2> "$OUTDIR/tackle.stderr"); then
    tackle_exit=0
else
    tackle_exit=$?
fi
assert_exit_code "$tackle_exit" "!0" "tackle must refuse a dead-pane spawn"

# ── the stderr must carry the diagnostic pointing to capture-pane ─────────
# Structural test: the user needs to know WHY the spawn failed and
# WHERE to look. If we ever drop the diagnostic, this catches it.
if grep -q "spawn postcondition failed" "$OUTDIR/tackle.stderr"; then
    _smoke_pass "tackle stderr contains spawn-postcondition diagnostic"
else
    _smoke_fail "tackle stderr missing postcondition diagnostic (saw: $(head -1 "$OUTDIR/tackle.stderr" 2>/dev/null || true))"
fi

# ── the surface must not lie about a healthy worker ───────────────────────
# After a failed tackle, the molecule may stay pending or be marked as
# stuck. What MUST NOT happen is: molecule reads as status=active AND
# fleet.json holds a healthy worker entry — that's the task-4046 lie.
observed="$(cd "$SCRATCH" && "$CS_BIN" observe "$MOL" --json 2>/dev/null)"
status="$(smoke_json_field status "$observed")"
case "$status" in
    active)
        _smoke_fail "surface lies: molecule status=active after failed tackle"
        ;;
    pending|stuck|collapsed|"")
        _smoke_pass "surface honest: molecule status='$status' (not active)"
        ;;
    *)
        _smoke_pass "surface honest: molecule status='$status'"
        ;;
esac

smoke_summary "fault-exit-42"
