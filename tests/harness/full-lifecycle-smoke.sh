#!/usr/bin/env bash
# full-lifecycle-smoke.sh — end-to-end cosmon lifecycle smoke test.
#
# Exercises the full pilot cycle on a throwaway project against fake-tmux +
# fake-claude shims:
#
#   cs init → cs nucleate → cs tackle → (simulated worker) → cs complete →
#   cs wait → cs observe → cs done
#
# Where the "simulated worker" is the harness itself: after cs tackle
# spawns the session (fake-claude exits cleanly but never drives evolve),
# the harness calls `cs complete` directly — mirroring what a real claude
# worker would do if it received the prompt and concluded.
#
# This is NOT a redundant copy of run_matrix.sh. Run-matrix is a
# boundary-only tuple oracle (5-bit residual lie). Full-lifecycle-smoke
# covers the OTHER half of the pilot protocol: the teardown side
# (`cs wait`, `cs done`, branch merge, worktree removal) which the matrix
# intentionally skips.
#
# Runtime budget: <30s on a modern laptop. If it grows past 60s, shrink
# the readiness/wait timeouts before adding new assertions.
#
# Exit codes:
#   0  all assertions passed (happy path is sound)
#   1  at least one assertion failed — inspect [FAIL] lines
#   2  harness setup error (no cs binary, no fakes, etc.)

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
# tests/ is a sibling of the repo root? No — tests/ lives inside the repo.
# $HERE = tests/harness; $HERE/.. = tests; $HERE/../.. = repo root.
REPO="$(cd "$HERE/../.." && pwd)"
FAKES_DIR="$REPO/tests/fakes"

# shellcheck source=smoke/_assertions.sh
. "$HERE/smoke/_assertions.sh"

CS_BIN="$(smoke_resolve_cs_bin "$REPO")" || {
    echo "smoke: no cs binary found (tried $REPO/target/debug/cs and \$PATH)" >&2
    exit 2
}

if [ ! -x "$FAKES_DIR/fake-tmux/tmux" ] || [ ! -x "$FAKES_DIR/fake-claude/claude" ]; then
    echo "smoke: fakes missing or not executable under $FAKES_DIR" >&2
    exit 2
fi

OUTDIR="${SMOKE_OUTDIR:-$REPO/target/smoke-$$}"
mkdir -p "$OUTDIR"

echo "smoke: cs=$CS_BIN"
echo "smoke: artifacts → $OUTDIR"

SCRATCH="$OUTDIR/project"
FAKE_TMUX_STATE="$OUTDIR/fake-tmux"
mkdir -p "$SCRATCH" "$FAKE_TMUX_STATE"

# Stage fakes on PATH and isolate the tmux state directory so this run
# touches nothing outside $OUTDIR.
export PATH="$FAKES_DIR/fake-tmux:$FAKES_DIR/fake-claude:$PATH"
export FAKE_TMUX_DIR="$FAKE_TMUX_STATE"
export FAKE_TMUX_TRACE="$OUTDIR/fake-tmux.log"

# If we're running inside a cosmon worker, stop cs nucleate from trying
# to auto-link children to a parent molecule that doesn't exist in the
# scratch project.
unset COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_RUNTIME_ACTIVE

# Happy-path fake-claude mode: exit cleanly, no faults.
export FAKE_CLAUDE_MODE=exit-0

# Short readiness timeout — the fake spawns a dummy "Ready" prompt in
# the capture buffer so cosmon should detect ready in <1s.
export COSMON_READINESS_TIMEOUT_SECS=5

t0="$(date +%s)"

# ── 1. Bootstrap project ──────────────────────────────────────────────────
if ! smoke_bootstrap_project "$SCRATCH" "$REPO" "$CS_BIN"; then
    echo "smoke: bootstrap failed" >&2
    exit 2
fi
_smoke_pass "bootstrap: cs init + hello.formula staged"

# ── 2. Nucleate a hello molecule ──────────────────────────────────────────
nuc_json="$(cd "$SCRATCH" && "$CS_BIN" nucleate hello --var topic="smoke-$$" --json 2>"$OUTDIR/nucleate.stderr")"
MOL="$(smoke_json_field id "$nuc_json")"
if [ -z "$MOL" ]; then
    _smoke_fail "nucleate — could not parse molecule id from: $nuc_json"
    smoke_summary "full-lifecycle-smoke"
    exit 1
fi
_smoke_pass "nucleate: $MOL"
echo "$MOL" > "$OUTDIR/molecule_id"

# State immediately after nucleation — should be pending.
assert_status "$MOL" "pending" "$CS_BIN" "$SCRATCH"

# ── 3. Tackle: spawn worktree + tmux + fake-claude ────────────────────────
tackle_log="$OUTDIR/tackle"
if (cd "$SCRATCH" && "$CS_BIN" tackle "$MOL" > "$tackle_log.stdout" 2> "$tackle_log.stderr"); then
    tackle_exit=0
else
    tackle_exit=$?
fi
echo "$tackle_exit" > "$OUTDIR/tackle.exit"
assert_exit_code "$tackle_exit" "0" "tackle"

# The worktree and branch should now exist.
if [ -d "$SCRATCH/.worktrees/$MOL" ]; then
    _smoke_pass "tackle: worktree .worktrees/$MOL exists"
else
    _smoke_fail "tackle: worktree .worktrees/$MOL missing"
fi
if (cd "$SCRATCH" && git branch --list "feat/$MOL" | grep -q "feat/$MOL"); then
    _smoke_pass "tackle: branch feat/$MOL exists"
else
    _smoke_fail "tackle: branch feat/$MOL missing"
fi

# ── 4. Simulate the worker driving to completion ──────────────────────────
# In a real run, claude would inspect the prompt, do the work, and call
# `cs evolve` / `cs complete` from inside the worktree. The fake-claude
# exits immediately, so the harness plays that role.
complete_json="$(cd "$SCRATCH" && "$CS_BIN" complete "$MOL" --reason "smoke-harness simulated worker" --json 2>"$OUTDIR/complete.stderr")"
new_status="$(smoke_json_field new_status "$complete_json")"
if [ "$new_status" = "completed" ]; then
    _smoke_pass "complete: new_status=completed"
else
    _smoke_fail "complete: expected new_status=completed, got '$new_status' (raw: $complete_json)"
fi

# ── 5. cs wait must return promptly since the molecule is terminal ────────
wait_t0="$(date +%s)"
if (cd "$SCRATCH" && "$CS_BIN" wait "$MOL" --timeout 15 --quiet > "$OUTDIR/wait.stdout" 2> "$OUTDIR/wait.stderr"); then
    wait_exit=0
else
    wait_exit=$?
fi
wait_elapsed=$(($(date +%s) - wait_t0))
assert_exit_code "$wait_exit" "0" "wait"
assert_within "$wait_elapsed" 12 "wait returns quickly on terminal molecule"

# ── 6. cs observe says completed ──────────────────────────────────────────
assert_status "$MOL" "completed" "$CS_BIN" "$SCRATCH"

# ── 7. cs done: merge + teardown ──────────────────────────────────────────
if (cd "$SCRATCH" && "$CS_BIN" done "$MOL" --json > "$OUTDIR/done.stdout" 2> "$OUTDIR/done.stderr"); then
    done_exit=0
else
    done_exit=$?
fi
assert_exit_code "$done_exit" "0" "done"

# ── 8. Post-conditions ────────────────────────────────────────────────────
assert_no_worktree "$SCRATCH" "$MOL"
assert_no_branch   "$SCRATCH" "$MOL"
assert_no_tmux_session "$FAKE_TMUX_STATE" "$MOL"

# ── 9. Summary ────────────────────────────────────────────────────────────
elapsed=$(($(date +%s) - t0))
printf '\nsmoke: elapsed=%ds  artifacts=%s\n' "$elapsed" "$OUTDIR"
smoke_summary "full-lifecycle-smoke"
