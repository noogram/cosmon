#!/usr/bin/env bash
# fault-hang.sh — assert that `cs wait` enforces its --timeout even when
# the worker never drives the molecule to a terminal state.
#
# Scenario:
#   - Healthy tackle (FAKE_CLAUDE_MODE=exit-0 + default pane_dead=0).
#   - Harness never calls `cs complete` — mimicking a hung worker.
#   - Harness calls `cs wait <mol> --timeout 5`.
#
# Invariant under test: `cs wait` is kubectl-wait, not kubectl-watch. A
# bounded timeout MUST produce a bounded wall-clock, even when the
# worker is frozen. If this assertion fails, the pilot session can hang
# indefinitely on a dead worker — the "do-not-poll-observe" contract
# described in CLAUDE.md is moot.
#
# We cap elapsed at 15s (3x the --timeout) to account for filesystem
# poll jitter + process-startup overhead.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
FAKES_DIR="$REPO/tests/fakes"

# shellcheck source=_assertions.sh
. "$HERE/_assertions.sh"

CS_BIN="$(smoke_resolve_cs_bin "$REPO")" || exit 2

OUTDIR="${SMOKE_OUTDIR:-$REPO/target/smoke-hang-$$}"
SCRATCH="$OUTDIR/project"
FAKE_TMUX_STATE="$OUTDIR/fake-tmux"
mkdir -p "$SCRATCH" "$FAKE_TMUX_STATE"

export PATH="$FAKES_DIR/fake-tmux:$FAKES_DIR/fake-claude:$PATH"
export FAKE_TMUX_DIR="$FAKE_TMUX_STATE"
export FAKE_TMUX_TRACE="$OUTDIR/fake-tmux.log"
unset COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_RUNTIME_ACTIVE

# Healthy spawn, but no driver — the harness is the "frozen worker".
export FAKE_CLAUDE_MODE=exit-0
export COSMON_READINESS_TIMEOUT_SECS=5

echo "fault-hang: cs=$CS_BIN"
echo "fault-hang: artifacts → $OUTDIR"

smoke_bootstrap_project "$SCRATCH" "$REPO" "$CS_BIN" \
    || { echo "fault-hang: bootstrap failed" >&2; exit 2; }

raw="$(cd "$SCRATCH" && "$CS_BIN" nucleate hello --var topic="fault-hang" --json 2>/dev/null)"
MOL="$(smoke_json_field id "$raw")"
if [ -z "$MOL" ]; then
    _smoke_fail "nucleate — could not extract molecule id"
    smoke_summary "fault-hang"
    exit 1
fi
_smoke_pass "nucleate: $MOL"

# Tackle should succeed in the happy-looking tmux state.
if (cd "$SCRATCH" && "$CS_BIN" tackle "$MOL" > "$OUTDIR/tackle.stdout" 2> "$OUTDIR/tackle.stderr"); then
    tackle_exit=0
else
    tackle_exit=$?
fi
assert_exit_code "$tackle_exit" "0" "tackle (healthy spawn)"

# DO NOT call cs complete — simulate a hung worker.
#
# cs wait --timeout 5 should exit with EXIT_TIMEOUT (124) in ~5–8s.
wait_t0="$(date +%s)"
if (cd "$SCRATCH" && "$CS_BIN" wait "$MOL" --timeout 5 --poll-interval 1 --quiet \
        > "$OUTDIR/wait.stdout" 2> "$OUTDIR/wait.stderr"); then
    wait_exit=0
else
    wait_exit=$?
fi
wait_elapsed=$(($(date +%s) - wait_t0))

# The exit code should be 124 (EXIT_TIMEOUT).
assert_exit_code "$wait_exit" "124" "cs wait exits with timeout code"

# The wall-clock bound: --timeout=5 should not produce a 30s outcome.
# We give 3x budget to absorb CI jitter.
assert_within "$wait_elapsed" 15 "cs wait respects its --timeout budget"

# Cleanup tmux state — otherwise sockets persist past OUTDIR teardown.
(cd "$SCRATCH" && "$CS_BIN" done "$MOL" --force --no-merge > /dev/null 2>&1 || true)

smoke_summary "fault-hang"
