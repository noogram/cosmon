#!/usr/bin/env bash
# run_matrix.sh — spawn-matrix harness for cosmon (task-20260416-03b5).
#
# For each (tmux_mode, claude_mode) combination in MATRIX, run a full
# `cs nucleate && cs tackle` cycle against fake-tmux + fake-claude shims,
# then compute the 5-bit shannon tuple (S, P, B, W, F) and check a small
# set of turing invariants. Emits a per-row report on stdout and writes
# artifacts into $OUTDIR for post-mortem.
#
# Not exhaustive (2^many combos); covers strategically selected failure
# modes observed in production (task-4046 zombie, task-25c3 auth, …) plus
# synthetic scenarios that gave the panel deliberation (delib-3879) its
# evidence.
#
# Exit codes:
#   0  all rows meet their expected invariants
#   1  at least one row violated an invariant it was expected to hold
#   2  harness setup / toolchain error
#
# See docs/testing/mock-strategy.md for philosophy.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
FAKES_DIR="$REPO/tests/fakes"
OUTDIR="${MATRIX_OUTDIR:-$REPO/target/matrix-$$}"

# ── CS binary resolution ───────────────────────────────────────────────────
# Prefer ./target/debug/cs from the worktree being tested, fall back to the
# installed binary. Never assume the live system binary matches the source.

CS_BIN="${CS_BIN:-}"
if [ -z "$CS_BIN" ]; then
    if [ -x "$REPO/target/debug/cs" ]; then
        CS_BIN="$REPO/target/debug/cs"
    elif command -v cs >/dev/null 2>&1; then
        CS_BIN="$(command -v cs)"
    else
        echo "matrix: no cs binary found (tried $REPO/target/debug/cs and \$PATH)" >&2
        exit 2
    fi
fi

echo "matrix: using cs=$CS_BIN"
echo "matrix: artifacts → $OUTDIR"
mkdir -p "$OUTDIR"

# Shared PATH for every row: fakes MUST appear first so `tmux` and
# `claude` invocations hit the shims. We resolve this at the harness
# level (not inside cs) — the binary we're testing should be oblivious.
export PATH="$FAKES_DIR/fake-tmux:$FAKES_DIR/fake-claude:$PATH"

# If the harness is itself running inside a cosmon worker (COSMON_MOL_DIR
# set, COSMON_PARENT_MOL_ID set), scratch-project `cs nucleate` calls will
# try to auto-link children to the parent molecule and fail because the
# scratch project doesn't know about it. Strip those so nucleation is
# self-contained.
unset COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_RUNTIME_ACTIVE

# ── Matrix ────────────────────────────────────────────────────────────────
# Format: "row_name|tmux_env|claude_env|expected_tuple"
#
# The expected tuple is a 5-bit string "SPBWF" (S=session, P=pane_alive,
# B=branch_created, W=worktree_created, F=fleet_entry). "?" means don't-care
# on that bit. Entries with "expected=X" mark expected tackle exit codes.

MATRIX=(
    # Expected tuple bits: S=session P=pane_alive B=branch W=worktree F=fleet_entry
    # "?" means don't-care. Expected exit: "0" | "!0" | "?".
    #
    # Note on F (fleet_entry): cosmon creates a fleet worker entry ONLY
    # when the tmux spawn succeeds. FAKE_TMUX_NEW_SESSION_EXIT!=0 or
    # FAKE_TMUX_NO_SERVER=1 short-circuit before the fleet write, so F=0
    # on those rows. PANE_DEAD / SESSION_EXITED only affect liveness
    # reporting, not whether the fleet entry was committed.

    # Healthy baseline — all surfaces should agree.
    "healthy                    |                                     |FAKE_CLAUDE_MODE=exit-0         |11111|expected=0"

    # Claude CLI-level failures (process-boundary). The spawn surface
    # commits regardless of claude's eventual exit code, so the 5-tuple
    # still reads as healthy — which is precisely the "residual lie" the
    # harness was built to quantify.
    "claude-exit-42             |                                     |FAKE_CLAUDE_MODE=exit-42        |11111|expected=0"
    "claude-exit-delayed        |                                     |FAKE_CLAUDE_MODE=exit-delayed   |11111|expected=0"
    "claude-hang                |                                     |FAKE_CLAUDE_MODE=hang           |11111|expected=0"
    "claude-segfault            |                                     |FAKE_CLAUDE_MODE=segfault       |11111|expected=0"
    "claude-auth                |                                     |FAKE_CLAUDE_MODE=auth-prompt    |11111|expected=0"
    "claude-partial             |                                     |FAKE_CLAUDE_MODE=partial-output |11111|expected=0"

    # Tmux-level failures (oracle boundary).
    "tmux-new-fail              |FAKE_TMUX_NEW_SESSION_EXIT=2          |FAKE_CLAUDE_MODE=exit-0         |00110|expected=!0"
    "tmux-no-server             |FAKE_TMUX_NO_SERVER=1                 |FAKE_CLAUDE_MODE=exit-0         |?????|expected=?"
    "tmux-pane-dead             |FAKE_TMUX_PANE_DEAD=1                 |FAKE_CLAUDE_MODE=exit-0         |10111|expected=0"
    "tmux-session-exited        |FAKE_TMUX_SESSION_EXITED=1            |FAKE_CLAUDE_MODE=exit-0         |10111|expected=0"
    "tmux-list-empty            |FAKE_TMUX_LIST_EMPTY=1                |FAKE_CLAUDE_MODE=exit-0         |11110|expected=!0"
    "tmux-has-miss              |FAKE_TMUX_HAS_SESSION_MISS=1          |FAKE_CLAUDE_MODE=exit-0         |11111|expected=0"

    # Cartesian strategic combinations.
    "pane-dead+hang             |FAKE_TMUX_PANE_DEAD=1                 |FAKE_CLAUDE_MODE=hang           |10111|expected=0"
    "new-fail+auth              |FAKE_TMUX_NEW_SESSION_EXIT=1          |FAKE_CLAUDE_MODE=auth-prompt    |00110|expected=!0"
    "list-empty+segfault        |FAKE_TMUX_LIST_EMPTY=1                |FAKE_CLAUDE_MODE=segfault       |11110|expected=!0"
    "no-server+exit-42          |FAKE_TMUX_NO_SERVER=1                 |FAKE_CLAUDE_MODE=exit-42        |?????|expected=?"
    "session-exited+partial     |FAKE_TMUX_SESSION_EXITED=1            |FAKE_CLAUDE_MODE=partial-output |10111|expected=0"
    "pane-dead+exit-delayed     |FAKE_TMUX_PANE_DEAD=1                 |FAKE_CLAUDE_MODE=exit-delayed   |10111|expected=0"
    "has-miss+hang              |FAKE_TMUX_HAS_SESSION_MISS=1          |FAKE_CLAUDE_MODE=hang           |11111|expected=0"
    "pane-dead+auth             |FAKE_TMUX_PANE_DEAD=1                 |FAKE_CLAUDE_MODE=auth-prompt    |10111|expected=0"
)

# ── Utilities ──────────────────────────────────────────────────────────────

bit() {
    # Echo 1 if $1 is truthy (non-empty, non-zero), else 0.
    if [ -n "$1" ] && [ "$1" != "0" ] && [ "$1" != "false" ]; then
        echo 1
    else
        echo 0
    fi
}

compare_tuple() {
    # $1 = actual 5-bit, $2 = expected mask with ? as wildcard
    local actual="$1" expected="$2" i
    if [ "${#actual}" -ne 5 ] || [ "${#expected}" -ne 5 ]; then
        return 1
    fi
    for i in 0 1 2 3 4; do
        local a="${actual:$i:1}" e="${expected:$i:1}"
        if [ "$e" = "?" ]; then
            continue
        fi
        if [ "$a" != "$e" ]; then
            return 1
        fi
    done
    return 0
}

compare_exit() {
    # $1 = actual int, $2 = expected spec: "0" | "!0" | "?"
    local actual="$1" spec="$2"
    case "$spec" in
        "?") return 0 ;;
        "0") [ "$actual" = "0" ] ;;
        "!0") [ "$actual" != "0" ] ;;
        *) [ "$actual" = "$spec" ] ;;
    esac
}

# ── Per-row harness ────────────────────────────────────────────────────────

run_row() {
    local row_name="$1" tmux_env="$2" claude_env="$3" expected_tuple="$4" expected_exit_spec="$5"

    local row_dir="$OUTDIR/$row_name"
    mkdir -p "$row_dir"

    # Isolated scratch project per row. Each gets its own git repo, own
    # .cosmon/, own FAKE_TMUX_DIR. Nothing the row does touches the
    # cosmon repo being tested.
    local scratch="$row_dir/project"
    mkdir -p "$scratch"
    (
        cd "$scratch" || exit 1
        git init -q
        git config user.email test@example.com
        git config user.name test
        echo "# scratch" > README.md
        git add README.md
        git -c commit.gpgsign=false commit -q -m "seed"
    ) || return 1

    # Isolated fake-tmux state directory per row.
    local fake_state="$row_dir/fake-tmux-state"
    mkdir -p "$fake_state"

    # `cs init` scaffolds .cosmon/ + canonical formulas.
    (
        cd "$scratch" || exit 1
        "$CS_BIN" init . >/dev/null 2>&1
    ) || return 1

    # Nucleate a leaf task-work molecule. The exact formula doesn't matter —
    # we're testing the spawn surface, not formula semantics.
    local mol_id
    mol_id="$(cd "$scratch" && "$CS_BIN" nucleate task-work --var topic="matrix row $row_name" --json 2>/dev/null | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)"
    if [ -z "$mol_id" ]; then
        echo "  [!] could not nucleate molecule in $scratch"
        return 1
    fi
    echo "$mol_id" > "$row_dir/molecule_id"

    # ── Tackle with fakes + fault injection ────────────────────────────
    local tackle_exit=0
    (
        cd "$scratch" || exit 1
        # Per-row env layer: fault injection + isolated state dir.
        # shellcheck disable=SC2086
        env FAKE_TMUX_DIR="$fake_state" \
            FAKE_TMUX_TRACE="$row_dir/fake-tmux.log" \
            COSMON_READINESS_TIMEOUT_SECS=5 \
            $tmux_env \
            $claude_env \
            "$CS_BIN" tackle "$mol_id" \
                > "$row_dir/tackle.stdout" \
                2> "$row_dir/tackle.stderr"
    )
    tackle_exit=$?
    echo "$tackle_exit" > "$row_dir/tackle.exit"

    # ── Observe the physical + logical state ───────────────────────────
    #
    # S: session exists in fake-tmux state dir?
    local s_bit=0
    local meta_file=""
    if ls "$fake_state"/*/*.meta >/dev/null 2>&1; then
        s_bit=1
        meta_file="$(ls "$fake_state"/*/*.meta 2>/dev/null | head -1)"
    fi

    # P: pane alive? Ask fake-tmux directly via list-panes. The fault
    # injection (FAKE_TMUX_PANE_DEAD / FAKE_TMUX_SESSION_EXITED) flips
    # the reported pane_dead bit. When the session doesn't exist at all,
    # P is 0 regardless.
    local p_bit=0
    if [ "$s_bit" = "1" ]; then
        # Find the socket dir and session name from the meta file path.
        local socket session pane_dead
        socket="$(basename "$(dirname "$meta_file")")"
        session="$(basename "$meta_file" .meta)"
        # shellcheck disable=SC2086
        pane_dead="$(env FAKE_TMUX_DIR="$fake_state" $tmux_env \
            "$FAKES_DIR/fake-tmux/tmux" -L "$socket" \
            list-panes -t "$session" -F '#{pane_dead}' 2>/dev/null | head -1)"
        if [ "$pane_dead" = "0" ]; then
            p_bit=1
        fi
    fi

    # B: a worker branch was created on the scratch repo?
    # cs tackle uses the `feat/<mol-id>` naming scheme.
    local b_bit=0
    if (cd "$scratch" && git branch --list 'feat/*' 2>/dev/null | grep -q .); then
        b_bit=1
    fi

    # W: a worktree was materialized?
    local w_bit=0
    if [ -d "$scratch/.worktrees" ] && ls "$scratch/.worktrees"/ >/dev/null 2>&1; then
        if [ "$(ls -A "$scratch/.worktrees" 2>/dev/null | wc -l)" -gt 0 ]; then
            w_bit=1
        fi
    fi

    # F: a fleet worker entry was registered in .cosmon/state/fleet.json?
    # Detect by absence of the empty-workers literal `"workers": {}`.
    local f_bit=0
    if [ -f "$scratch/.cosmon/state/fleet.json" ] \
       && [ -n "$(tr -d ' \t\n' < "$scratch/.cosmon/state/fleet.json" 2>/dev/null | grep -o '"workers":{[^}]*"id"' || true)" ]; then
        f_bit=1
    fi

    local actual_tuple="${s_bit}${p_bit}${b_bit}${w_bit}${f_bit}"

    # ── Record findings ─────────────────────────────────────────────────
    {
        echo "row=$row_name"
        echo "tmux_env=$tmux_env"
        echo "claude_env=$claude_env"
        echo "tackle_exit=$tackle_exit"
        echo "expected_tuple=$expected_tuple"
        echo "actual_tuple=$actual_tuple"
        echo "S(session)=$s_bit"
        echo "P(pane_alive)=$p_bit"
        echo "B(branch)=$b_bit"
        echo "W(worktree)=$w_bit"
        echo "F(fleet_entry)=$f_bit"
    } > "$row_dir/observation.txt"

    # ── Invariant check: actual vs expected ─────────────────────────────
    local tuple_ok=1 exit_ok=1
    if ! compare_tuple "$actual_tuple" "$expected_tuple"; then
        tuple_ok=0
    fi
    if ! compare_exit "$tackle_exit" "$expected_exit_spec"; then
        exit_ok=0
    fi

    local verdict="PASS"
    if [ "$tuple_ok" = "0" ] || [ "$exit_ok" = "0" ]; then
        verdict="FAIL"
    fi
    printf '  %-28s tuple=%s expected=%s exit=%s expect=%s → %s\n' \
        "$row_name" "$actual_tuple" "$expected_tuple" "$tackle_exit" "$expected_exit_spec" "$verdict"

    # Cleanup worktrees + branches from the scratch repo so the next row
    # starts clean (not strictly needed since scratch is row-local, but
    # saves disk).
    rm -rf "$scratch/.worktrees" 2>/dev/null || true

    if [ "$verdict" = "PASS" ]; then
        return 0
    fi
    return 1
}

# ── Main ──────────────────────────────────────────────────────────────────

total=0
fail=0
start_epoch="$(date +%s)"

echo "matrix: running ${#MATRIX[@]} combos"
echo "───────────────────────────────────────────────────────────────────────"

for entry in "${MATRIX[@]}"; do
    # Fields are pipe-separated, surrounding whitespace is tolerated.
    IFS='|' read -r row_name tmux_env claude_env expected_tuple expected_exit <<EOF
$entry
EOF
    # Trim leading/trailing whitespace from each field.
    row_name="$(echo "$row_name" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    tmux_env="$(echo "$tmux_env" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    claude_env="$(echo "$claude_env" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    expected_tuple="$(echo "$expected_tuple" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    expected_exit="${expected_exit#expected=}"
    expected_exit="$(echo "$expected_exit" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"

    total=$((total + 1))
    if ! run_row "$row_name" "$tmux_env" "$claude_env" "$expected_tuple" "$expected_exit"; then
        fail=$((fail + 1))
    fi
done

end_epoch="$(date +%s)"
elapsed=$((end_epoch - start_epoch))

echo "───────────────────────────────────────────────────────────────────────"
printf 'matrix: %d/%d passed  (%ds)  artifacts=%s\n' \
    "$((total - fail))" "$total" "$elapsed" "$OUTDIR"

if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
