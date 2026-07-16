#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# mutation-falsifier.sh — the "reverting the fix must redden the test" gate.
#
# cosmon has EVERY green gate a suite can have — build, test, clippy, fmt, and
# `cs verify` (which REPLAYS the recorded gates; verify.rs:483). But replay only
# proves the test still passes on the code as written. It never asks the harder
# question: *does the test actually CATCH a change?* A test that never reddens
# when the code it guards is broken is theatre. delib-20260710-95a7 (torvalds +
# feynman) named this the one gate genuinely absent from the tree: zero
# mutation-testing. This script closes it — a thin wrapper on `cargo-mutants`
# that mutates the code and reports any mutant the suite FAILED to kill (a
# "survived"/"missed" mutant is a test hole; the fix could be reverted and
# nothing would go red).
#
# ⚠️ CADENCE — SLOW, PERIODIC, NEVER INLINE (ADR-010:177, feynman). Mutation
# testing rebuilds the workspace once per mutant; it is minutes-to-hours, not
# seconds. It MUST NOT be wired as a hard-abort on the hot path — not in
# `cs done`, not in the per-step `cs evolve` gate, not on the PR fast lane.
# Its home is the nightly CI cron and the periodic `falsifier-audit` formula
# (a `command`/B2 gate step, Step::is_gate()=command.is_some(), formula.rs:801).
# Scope it to the diff (`--in-diff`) so a PR-boundary run stays bounded.
# cargo-test-at-harvest already exists (smoke-test.formula.toml + cs verify) —
# this adds the mutation dimension that neither can express.
#
# WHAT IT DOES — generates a diff (default vs the merge base), hands it to
# `cargo mutants --in-diff`, and parses `mutants.out/` for MISSED mutants.
# Missed mutants → exit 1 with the list (the test holes). All caught (or no
# mutants in scope) → exit 0. cargo-mutants absent → exit 2 (cannot audit) —
# a FINDING for a periodic run, never a silent skip, but by cadence it blocks
# no fast path.
#
# Usage:
#   ./scripts/mutation-falsifier.sh [--base <ref>] [--package <p>] \
#                                   [--whole-tree] [--timeout <secs>] [-- <extra cargo-mutants args>]
#   ./scripts/mutation-falsifier.sh --check       # is cargo-mutants available? (0/2, no run)
#   ./scripts/mutation-falsifier.sh --self-test   # prove the falsifier catches a real hole
#
#   --base <ref>    diff base for --in-diff scoping (default: origin/main, else main)
#   --whole-tree    audit the whole workspace, not just the diff (nightly cron)
#   --package <p>   restrict to one cargo package (-p passthrough)
#   --timeout <s>   per-mutant test timeout (cargo-mutants -t; default 120)
#
# Exit: 0 all mutants caught / none in scope · 1 mutant(s) survived (test hole)
#       · 2 cargo-mutants unavailable or invocation error.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

[ -t 0 ] || exec </dev/null

# Absolute path to this script — the --self-test re-invokes it from inside a
# tmpdir crate, where a relative $0 would no longer resolve (→ exit 127).
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

have_mutants() { command -v cargo-mutants >/dev/null 2>&1 || cargo mutants --version >/dev/null 2>&1; }

# ── --check: availability probe, no run. For CI/formula preflight. ───────────
if [ "${1:-}" = "--check" ]; then
  if have_mutants; then
    echo "mutation-falsifier: cargo-mutants available ($(cargo mutants --version 2>/dev/null))"
    exit 0
  fi
  echo "mutation-falsifier: cargo-mutants NOT installed — install with: cargo install cargo-mutants" >&2
  exit 2
fi

# ── --self-test: falsifiability. Prove the gate reddens on a REAL test hole and
# stays green on a suite that kills its mutants. Builds two throwaway crates in
# a tmpdir and runs cargo-mutants scoped to one function each — bounded, real,
# hermetic. Skips (exit 0, SKIP) when cargo-mutants is absent so the harness
# test-gate never blocks on a missing dev tool. ──────────────────────────────
if [ "${1:-}" = "--self-test" ]; then
  if ! have_mutants; then
    echo "mutation-falsifier self-test: SKIP — cargo-mutants not installed"
    exit 0
  fi
  WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
  fails=0

  make_crate() { # <dir> <test-body>
    local d="$1" testbody="$2"
    mkdir -p "$d/src"
    cat >"$d/Cargo.toml" <<'TOML'
[package]
name = "mf_selftest"
version = "0.0.0"
edition = "2021"
publish = false
TOML
    cat >"$d/src/lib.rs" <<RS
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t() { $testbody }
}
RS
  }

  # (1) A HOLE: the test asserts nothing about add's result → the `a + b`
  #     mutants (e.g. `a - b`, `* b`, replace-with-0) survive → exit 1 expected.
  make_crate "$WORK/hole" 'let _ = add(2, 3);'
  ( cd "$WORK/hole" && "$SELF" --whole-tree --timeout 60 >/dev/null 2>&1 )
  rc=$?
  if [ "$rc" -eq 1 ]; then echo "  ok   [reddens] a test that asserts nothing → survived mutant → exit 1"
  else echo "  FAIL expected exit 1 (survived mutant) on the hole crate, got $rc"; fails=$((fails+1)); fi

  # (2) A REAL test: asserts add(2,3)==5 → mutations to `a + b` break it →
  #     all caught → exit 0 expected.
  make_crate "$WORK/tight" 'assert_eq!(add(2, 3), 5); assert_eq!(add(0, 0), 0); assert_eq!(add(-1, 1), 0);'
  ( cd "$WORK/tight" && "$SELF" --whole-tree --timeout 60 >/dev/null 2>&1 )
  rc=$?
  if [ "$rc" -eq 0 ]; then echo "  ok   [stays green] a suite that kills its mutants → exit 0"
  else echo "  FAIL expected exit 0 (all caught) on the tight crate, got $rc"; fails=$((fails+1)); fi

  if [ "$fails" -eq 0 ]; then echo "mutation-falsifier self-test: PASS"; exit 0
  else echo "mutation-falsifier self-test: $fails FAILED"; exit 1; fi
fi

# ── argument parsing ─────────────────────────────────────────────────────────
BASE=""
WHOLE_TREE=0
PACKAGE=""
TIMEOUT=120
EXTRA=()
while [ $# -gt 0 ]; do
  case "$1" in
    --base)       BASE="${2:-}"; shift 2 ;;
    --whole-tree) WHOLE_TREE=1; shift ;;
    --package)    PACKAGE="${2:-}"; shift 2 ;;
    --timeout)    TIMEOUT="${2:-}"; shift 2 ;;
    --)           shift; EXTRA=("$@"); break ;;
    *) echo "mutation-falsifier: unknown argument '$1'" >&2; exit 2 ;;
  esac
done

if ! have_mutants; then
  echo "mutation-falsifier: cargo-mutants NOT installed — install with: cargo install cargo-mutants" >&2
  echo "  (periodic gate: this is a finding, not a fast-path block — the audit could not run)" >&2
  exit 2
fi

# Default base: origin/main if it resolves, else main. Only used for --in-diff.
if [ -z "$BASE" ]; then
  if git rev-parse --verify -q origin/main >/dev/null 2>&1; then BASE="origin/main"
  else BASE="main"; fi
fi

OUT_DIR="$(mktemp -d)"; trap 'rm -rf "$OUT_DIR"' EXIT
ARGS=(mutants --no-times -t "$TIMEOUT" -o "$OUT_DIR")
[ -n "$PACKAGE" ] && ARGS+=(-p "$PACKAGE")

if [ "$WHOLE_TREE" -eq 0 ]; then
  DIFF_FILE="$OUT_DIR/scope.diff"
  if ! git diff "$BASE"...HEAD > "$DIFF_FILE" 2>/dev/null; then
    # Base unresolvable (e.g. shallow clone with no main) — fall back to
    # whole-tree rather than silently auditing nothing.
    echo "mutation-falsifier: base '$BASE' unresolvable — falling back to --whole-tree" >&2
    WHOLE_TREE=1
  elif [ ! -s "$DIFF_FILE" ]; then
    echo "mutation-falsifier: empty diff vs $BASE — nothing to mutate. CLEAN."
    exit 0
  else
    ARGS+=(--in-diff "$DIFF_FILE")
    echo "mutation-falsifier: mutating the diff vs $BASE (timeout ${TIMEOUT}s/mutant)"
  fi
fi
[ "$WHOLE_TREE" -eq 1 ] && echo "mutation-falsifier: mutating the WHOLE tree (timeout ${TIMEOUT}s/mutant)"

# Run. cargo-mutants' own exit code varies by version; we treat the authoritative
# signal as the MISSED list on disk, not the process code.
cargo "${ARGS[@]}" "${EXTRA[@]}" >/dev/null 2>&1 || true

# cargo-mutants nests its result files under `<-o dir>/mutants.out/`.
RESULT_DIR="$OUT_DIR/mutants.out"
MISSED="$RESULT_DIR/missed.txt"
if [ ! -e "$MISSED" ]; then
  # No missed.txt means cargo-mutants produced no outcome files — either no
  # mutants in scope or a build failure. Distinguish via the caught list.
  if [ -e "$RESULT_DIR/caught.txt" ] || [ -e "$RESULT_DIR/unviable.txt" ]; then
    echo "mutation-falsifier: no surviving mutants — every mutant in scope was caught. CLEAN."
    exit 0
  fi
  echo "mutation-falsifier: no mutants generated in scope (or the baseline build failed). Nothing to falsify." >&2
  exit 0
fi

# `grep -c` prints the count (0 when empty) but exits 1 on no match; `|| true`
# keeps the single "0" without appending a second line.
survivors="$(grep -c . "$MISSED" 2>/dev/null || true)"
survivors="${survivors:-0}"
if [ "$survivors" -eq 0 ]; then
  echo "mutation-falsifier: no surviving mutants — every mutant in scope was caught. CLEAN."
  exit 0
fi

echo "mutation-falsifier: $survivors SURVIVING mutant(s) — the suite did NOT redden when this code was mutated:" >&2
sed 's/^/  /' "$MISSED" >&2
echo >&2
echo "Each line is a change the tests failed to catch: reverting the corresponding fix" >&2
echo "would leave the suite green. Add or tighten a test until the mutant is killed." >&2
exit 1
