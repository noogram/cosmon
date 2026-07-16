#!/usr/bin/env bash
# negative-control.sh — proof-of-power for the pinning test.
#
# A bench that only ever goes GREEN proves nothing: maybe the stimulus is
# inert and every fix "passes". The negative control replays the SAME
# committed pinning test (openai_tool_parse_recovery.rs) across the fix
# boundary and demands it flips:
#
#   pre-fix  (e48d9a0de, parent of the acute recovery 643c6ae7d)  → RED
#   post-fix (HEAD)                                                → GREEN
#
# RED pre-fix has two admissible flavours, both valid disproof that the
# stimulus is inert:
#   * compile-red  — the recovery machinery (is_tool_parse_error_signal,
#     telemetry_for, the re-inject arm) did not yet exist, so the test that
#     asserts it cannot even build. This is the flavour expected here: the
#     fix + its test landed together in 643c6ae7d.
#   * runtime-red  — it built but a 500-then-200 assertion returned Err.
#
# Deterministic, no ollama, no cs. Cost = up to two cargo builds; bounded by
# TIMEOUT (default 900s each). Provenance: delib-20260707-df9b §M-BENCH
# "negative control (proof of power)".
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
TEST_FILE="crates/cosmon-provider/tests/openai_tool_parse_recovery.rs"
TEST_NAME="openai_tool_parse_recovery"
PREFIX_COMMIT="${PREFIX_COMMIT:-e48d9a0de}"
TIMEOUT="${TIMEOUT:-900}"

run_one() { # $1=label $2=cargo-cwd  → prints "GREEN" | "RED:runtime" | "RED:compile"
  local label="$1" cwd="$2" out rc
  out="$(cd "$cwd" && timeout "$TIMEOUT" cargo test -p cosmon-provider --test "$TEST_NAME" 2>&1)"
  rc=$?
  # persist full log next to the report consumer
  printf '%s\n' "$out" > "${NC_LOGDIR:-/tmp}/negctl-$label.log"
  if (( rc == 0 )) && grep -qE 'test result: ok' <<<"$out"; then
    echo "GREEN"
  elif grep -qE 'error\[E[0-9]+\]|could not compile|cannot find (function|type|method)' <<<"$out"; then
    echo "RED:compile"
  else
    echo "RED:runtime"
  fi
}

main() {
  echo "== negative control: pinning test across the fix boundary =="
  echo "pre-fix commit : $PREFIX_COMMIT"
  echo "post-fix       : $(git -C "$REPO" rev-parse --short HEAD)"

  # ── post-fix (HEAD, current worktree) — expect GREEN ──
  echo "-- post-fix (HEAD) --"
  local post; post=$(run_one "postfix" "$REPO")
  echo "   post-fix: $post"

  # ── pre-fix worktree — expect RED ──
  local wt; wt="$(mktemp -d)/prefix-checkout"
  echo "-- pre-fix ($PREFIX_COMMIT) in $wt --"
  git -C "$REPO" worktree add --quiet --detach "$wt" "$PREFIX_COMMIT" 2>/dev/null || {
    echo "   ERROR: could not create pre-fix worktree" >&2; return 3; }
  # Vendor the HEAD pinning test onto the pre-fix tree (the file did not exist
  # pre-fix; a compile-red is exactly the point).
  mkdir -p "$wt/$(dirname "$TEST_FILE")"
  git -C "$REPO" show "HEAD:$TEST_FILE" > "$wt/$TEST_FILE" 2>/dev/null || cp "$REPO/$TEST_FILE" "$wt/$TEST_FILE"
  local pre; pre=$(run_one "prefix" "$wt")
  echo "   pre-fix: $pre"
  git -C "$REPO" worktree remove --force "$wt" 2>/dev/null || true

  echo "== verdict =="
  if [[ "$post" == GREEN && "$pre" == RED:* ]]; then
    echo "NEGATIVE-CONTROL: PASS  (post=$post, pre=$pre — the stimulus discriminates the fix)"
    return 0
  fi
  echo "NEGATIVE-CONTROL: INCONCLUSIVE  (post=$post, pre=$pre — expected post=GREEN, pre=RED:*)"
  return 1
}
main "$@"
