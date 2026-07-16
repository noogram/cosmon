#!/usr/bin/env bash
# fault-segfault.sh — assert clean failure when the worker process dies
# from a native crash (SIGSEGV, exit 139).
#
# Scenario (dual fault injection):
#   FAKE_CLAUDE_MODE=segfault    — if forked, kill -SEGV $$ (exit 139)
#   FAKE_TMUX_SESSION_EXITED=1   — session exists but pane is dead + PID=0
#
# This is the production mode task-25c3 debugged: claude crashed during
# startup (auth transient, native-library bug, OOM), and the remain-on-exit
# tmux config left a session-shaped carcass that made the fleet surface
# look healthy. Using FAKE_TMUX_SESSION_EXITED matches that carcass shape
# more precisely than FAKE_TMUX_PANE_DEAD (which is a looser "pane is
# dead" signal).
#
# Contract: cs tackle must fail, tear down, and leave no fleet worker
# entry claiming life. If this assertion breaks we are back in
# task-25c3 territory.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
FAKES_DIR="$REPO/tests/fakes"

# shellcheck source=_assertions.sh
. "$HERE/_assertions.sh"

CS_BIN="$(smoke_resolve_cs_bin "$REPO")" || exit 2

OUTDIR="${SMOKE_OUTDIR:-$REPO/target/smoke-segfault-$$}"
SCRATCH="$OUTDIR/project"
FAKE_TMUX_STATE="$OUTDIR/fake-tmux"
mkdir -p "$SCRATCH" "$FAKE_TMUX_STATE"

export PATH="$FAKES_DIR/fake-tmux:$FAKES_DIR/fake-claude:$PATH"
export FAKE_TMUX_DIR="$FAKE_TMUX_STATE"
export FAKE_TMUX_TRACE="$OUTDIR/fake-tmux.log"
unset COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_RUNTIME_ACTIVE

export FAKE_CLAUDE_MODE=segfault
export FAKE_TMUX_SESSION_EXITED=1
export COSMON_READINESS_TIMEOUT_SECS=5

echo "fault-segfault: cs=$CS_BIN"
echo "fault-segfault: artifacts → $OUTDIR"

smoke_bootstrap_project "$SCRATCH" "$REPO" "$CS_BIN" \
    || { echo "fault-segfault: bootstrap failed" >&2; exit 2; }

raw="$(cd "$SCRATCH" && "$CS_BIN" nucleate hello --var topic="fault-segfault" --json 2>/dev/null)"
MOL="$(smoke_json_field id "$raw")"
if [ -z "$MOL" ]; then
    _smoke_fail "nucleate — could not extract molecule id"
    smoke_summary "fault-segfault"
    exit 1
fi
_smoke_pass "nucleate: $MOL"

if (cd "$SCRATCH" && "$CS_BIN" tackle "$MOL" > "$OUTDIR/tackle.stdout" 2> "$OUTDIR/tackle.stderr"); then
    tackle_exit=0
else
    tackle_exit=$?
fi
assert_exit_code "$tackle_exit" "!0" "tackle must refuse a crashed-worker spawn"

# Fleet ledger must not record a live worker after a failed tackle.
fleet_json="$SCRATCH/.cosmon/state/fleet.json"
if [ -f "$fleet_json" ]; then
    # A truly healthy worker row has `"id":"..."` inside `"workers":{...}`.
    # We're looking for that concrete shape, not just any fleet file.
    if tr -d ' \t\n' < "$fleet_json" | grep -q '"workers":{[^}]*"id":"[^"]*"'; then
        _smoke_fail "fleet.json carries a worker entry after failed tackle"
    else
        _smoke_pass "fleet.json has no live worker entry after failed tackle"
    fi
else
    # Absence of fleet.json is also acceptable — the file was not even
    # initialized, so there's nothing to lie about.
    _smoke_pass "fleet.json absent after failed tackle (no lie possible)"
fi

# Molecule must not be claimed `active` by the surface.
observed="$(cd "$SCRATCH" && "$CS_BIN" observe "$MOL" --json 2>/dev/null)"
status="$(smoke_json_field status "$observed")"
if [ "$status" = "active" ]; then
    _smoke_fail "surface lies: molecule status=active after crashed spawn"
else
    _smoke_pass "surface honest: molecule status='$status'"
fi

smoke_summary "fault-segfault"
