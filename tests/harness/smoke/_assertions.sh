#!/usr/bin/env bash
# _assertions.sh — shared assertion library for the lifecycle smoke harness.
#
# Sourced by full-lifecycle-smoke.sh and each fault-*.sh variant. Every
# assertion is a single-purpose function that prints a one-line PASS/FAIL
# and increments counters in the caller's scope (SMOKE_PASSED, SMOKE_FAILED).
#
# Design: these functions NEVER exit on their own. The caller decides what
# to do with the counters. This keeps the harness composable — a fault
# script can assert several conditions, log every failure, and exit once
# at the end with an aggregate verdict.
#
# All assertions emit structured lines of the shape:
#   [PASS]  <name> — <detail>
#   [FAIL]  <name> — <detail>
#
# suitable for grep and for CI log inspection.

# shellcheck disable=SC2034
SMOKE_ASSERTIONS_LOADED=1

# ── counters (caller may read) ────────────────────────────────────────────
: "${SMOKE_PASSED:=0}"
: "${SMOKE_FAILED:=0}"
: "${SMOKE_QUIET:=0}"

_smoke_pass() {
    SMOKE_PASSED=$((SMOKE_PASSED + 1))
    if [ "$SMOKE_QUIET" != "1" ]; then
        printf '[PASS]  %s\n' "$*"
    fi
}

_smoke_fail() {
    SMOKE_FAILED=$((SMOKE_FAILED + 1))
    printf '[FAIL]  %s\n' "$*" >&2
}

# ── CS binary resolution ──────────────────────────────────────────────────
# Match run_matrix.sh's approach: prefer the worktree's target/debug/cs,
# fall back to installed cs. Harness never assumes the installed binary
# matches the current source.

smoke_resolve_cs_bin() {
    local repo="$1"
    if [ -n "${CS_BIN:-}" ]; then
        echo "$CS_BIN"
        return 0
    fi
    if [ -x "$repo/target/debug/cs" ]; then
        echo "$repo/target/debug/cs"
        return 0
    fi
    if command -v cs >/dev/null 2>&1; then
        command -v cs
        return 0
    fi
    return 1
}

# ── JSON helpers ──────────────────────────────────────────────────────────
# Parse a single top-level string field from `cs ... --json` output without
# depending on jq (not always installed on fresh CI runners).

smoke_json_field() {
    local field="$1" json="$2"
    # Accept both pretty and compact JSON. Capture the first match only.
    printf '%s' "$json" | tr -d '\n' \
        | grep -o "\"$field\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" \
        | head -1 \
        | sed -E "s/.*\"$field\"[[:space:]]*:[[:space:]]*\"([^\"]*)\".*/\1/"
}

# ── Primary assertions ────────────────────────────────────────────────────

assert_status() {
    # $1 = molecule id, $2 = expected status (lowercase), $3 = cs binary,
    # $4 = project root (scratch dir; cs observe uses walk-up discovery,
    # so we must invoke from the scratch project not the harness cwd).
    local mol="$1" expected="$2" cs="$3" project="$4"
    local observed status
    observed="$(cd "$project" && "$cs" observe "$mol" --json 2>/dev/null)" || {
        _smoke_fail "assert_status($mol) — cs observe failed"
        return 1
    }
    status="$(smoke_json_field status "$observed")"
    if [ "$status" = "$expected" ]; then
        _smoke_pass "assert_status($mol) status=$status"
        return 0
    fi
    _smoke_fail "assert_status($mol) — expected '$expected', got '$status'"
    return 1
}

assert_no_worktree() {
    # $1 = project root (scratch dir), $2 = molecule id
    local project="$1" mol="$2"
    if [ -d "$project/.worktrees/$mol" ]; then
        _smoke_fail "assert_no_worktree($mol) — .worktrees/$mol still exists"
        return 1
    fi
    _smoke_pass "assert_no_worktree($mol)"
    return 0
}

assert_no_branch() {
    # $1 = project root, $2 = molecule id
    local project="$1" mol="$2"
    if (cd "$project" && git branch --list "feat/$mol" 2>/dev/null | grep -q "feat/$mol"); then
        _smoke_fail "assert_no_branch($mol) — feat/$mol still exists"
        return 1
    fi
    _smoke_pass "assert_no_branch($mol)"
    return 0
}

assert_no_tmux_session() {
    # $1 = FAKE_TMUX_DIR (isolated state), $2 = molecule id (session
    # name derives from slug+shortid, so we grep the state dir instead of
    # pattern-matching on a predicted name).
    local fake_dir="$1" mol="$2"
    local leftover
    leftover="$(find "$fake_dir" -name '*.meta' 2>/dev/null | head -5)"
    if [ -n "$leftover" ]; then
        _smoke_fail "assert_no_tmux_session($mol) — leftover meta files: $leftover"
        return 1
    fi
    _smoke_pass "assert_no_tmux_session($mol)"
    return 0
}

assert_merged() {
    # Assert that main contains at least one merge/fast-forward of the
    # worker's branch. We check `git log --all --oneline` for the mol_id
    # string on the main branch side — cs done commits molecule artifacts
    # with the mol_id in the message.
    # $1 = project root, $2 = molecule id
    local project="$1" mol="$2"
    local found
    found="$(cd "$project" && git log --all --oneline -- ".cosmon/state/fleets/default/molecules/$mol" 2>/dev/null | head -1)"
    if [ -n "$found" ]; then
        _smoke_pass "assert_merged($mol)"
        return 0
    fi
    # Fallback: look for the mol id anywhere in the main branch history.
    found="$(cd "$project" && git log --oneline 2>/dev/null | grep -m1 "$mol")"
    if [ -n "$found" ]; then
        _smoke_pass "assert_merged($mol) (by commit message)"
        return 0
    fi
    _smoke_fail "assert_merged($mol) — no trace in git log"
    return 1
}

assert_exit_code() {
    # $1 = actual exit code, $2 = expected spec ("0" | "!0" | <int>), $3 = label
    local actual="$1" spec="$2" label="$3"
    case "$spec" in
        "?") _smoke_pass "$label (exit=$actual, wildcard)" ; return 0 ;;
        "0")
            if [ "$actual" = "0" ]; then
                _smoke_pass "$label (exit=0)"
                return 0
            fi
            _smoke_fail "$label — expected exit=0, got $actual"
            return 1
            ;;
        "!0")
            if [ "$actual" != "0" ]; then
                _smoke_pass "$label (exit=$actual, non-zero as expected)"
                return 0
            fi
            _smoke_fail "$label — expected non-zero exit, got 0"
            return 1
            ;;
        *)
            if [ "$actual" = "$spec" ]; then
                _smoke_pass "$label (exit=$actual)"
                return 0
            fi
            _smoke_fail "$label — expected exit=$spec, got $actual"
            return 1
            ;;
    esac
}

assert_within() {
    # Assert that an elapsed integer seconds is <= bound. Used by the
    # hang fault variant to certify `cs wait` did not block past --timeout.
    # $1 = elapsed, $2 = max_seconds, $3 = label
    local elapsed="$1" bound="$2" label="$3"
    if [ "$elapsed" -le "$bound" ]; then
        _smoke_pass "$label (elapsed=${elapsed}s ≤ ${bound}s)"
        return 0
    fi
    _smoke_fail "$label — elapsed=${elapsed}s exceeds ${bound}s bound"
    return 1
}

# ── Scratch project scaffolding ───────────────────────────────────────────
# Create an isolated cosmon project inside the caller-provided scratch dir.
# Installs the fakes on PATH, unsets inherited COSMON_* env, drops the
# hello formula into .cosmon/formulas/.

smoke_bootstrap_project() {
    # $1 = scratch dir (must exist, empty), $2 = repo root, $3 = cs binary
    local scratch="$1" repo="$2" cs="$3"

    (
        cd "$scratch" || exit 1
        git init -q
        git config user.email smoke@cosmon.test
        git config user.name "smoke-harness"
        git -c commit.gpgsign=false commit --allow-empty -q -m "seed"
    ) || return 1

    # `cs init` scaffolds .cosmon/ + canonical formulas.
    (cd "$scratch" && "$cs" init . >/dev/null 2>&1) || return 1

    # Drop the smoke-test formula next to the canonical ones.
    cp "$repo/tests/fixtures/hello.formula.toml" \
        "$scratch/.cosmon/formulas/hello.formula.toml" || return 1

    return 0
}

# ── Summary ───────────────────────────────────────────────────────────────

smoke_summary() {
    # Print pass/fail tally and return 0 if all passed, 1 otherwise.
    local label="${1:-smoke}"
    local total=$((SMOKE_PASSED + SMOKE_FAILED))
    printf '%s: %d/%d passed (%d failed)\n' \
        "$label" "$SMOKE_PASSED" "$total" "$SMOKE_FAILED"
    if [ "$SMOKE_FAILED" -gt 0 ]; then
        return 1
    fi
    return 0
}
