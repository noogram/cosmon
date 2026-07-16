#!/usr/bin/env bash
# assert-hits.sh — G_inject, the fail-closed zero-hit injection gate (ADR-134).
#
# A post-processing injection/transform step (stamp a favicon into every page,
# splice a nav bar, rewrite canonical URLs, shim MathJax…) has a KNOWN-NONZERO
# expected hit count: it is dispatched precisely because there are N>0 targets
# it is supposed to touch. The pathology this helper kills is the
# silent-fail-open shape — the step prints `injected: 0` and then `exit 0`, so
# the absence of work is reported as the successful completion of work. The
# print line scrolls past; the EXIT CODE is the only channel the calling
# pipeline and the operator's CI surface actually read. G_inject moves the
# signal from the print line to the exit code.
#
# It is the SIGN-FLIPPED TWIN of the D7 publish-gate ban-list (ADR-128):
#   * ban-list  — abort when a forbidden thing is PRESENT  (count > 0 is bad)
#   * must-hit  — abort when a required thing is ABSENT     (count == 0 is bad)
# Same fail-closed spine (the gate BLOCKS, never merely warns), opposite
# polarity. See ADR-134 and docs/guides/fail-closed-injection.md.
#
# Two shapes, mirroring the gate-primitive family (G_visual / G_latex):
#
#   1. count-in-hand — the step already returns its hit count:
#        hits=$(inject_favicons ...)
#        assert_hits "favicons" "$hits"          # echoes hits; exit 2 if 0
#
#   2. stream — the step's stdout is one line per hit, grep-counted in a pipe:
#        inject_navbars ... | assert_hits_stream "nav bars" --min 1
#
# Both echo the hit count on success and FAIL CLOSED (exit 2) when the count
# is below the expected minimum (default 1).
#
# Source it to get the functions, or call it directly as a CLI:
#   source scripts/assert-hits.sh
#   assert_hits "favicons" "$hits"
#   inject_navbars | assert_hits_stream "nav bars" --min 1
#
#   scripts/assert-hits.sh favicons 12 --min 1
#   inject_navbars | scripts/assert-hits.sh --stream "nav bars" --min 1
#
# Exit / return codes (mirroring scripts/latex-audit.sh and `lumen
# visual-audit`):
#   0   PASS — count >= min; the count is echoed on stdout.
#   2   FAIL — count <  min; loud `FAIL: <label>: ...` on stderr. The gate
#       blocks. Exit 2 distinguishes a gate FAIL from a generic exit-1 error.
#   64  usage error (missing label/count, non-integer count or --min) — an
#       EX_USAGE-class mistake in the CALLING pipeline, kept distinct from a
#       gate FAIL so the caller can tell "you mis-wired me" from "the
#       injection found nothing".
#
# NOTE: this file is designed to be SOURCED, so it deliberately does NOT mutate
# the caller's shell options (`set -e`/`-u`) at the top level — the functions
# are written defensively instead. Shell options are set only inside the CLI
# dispatch branch at the bottom.

# _assert_hits_usage — print the calling convention to stderr.
_assert_hits_usage() {
  cat >&2 <<'EOF'
usage:
  assert_hits <label> <count> [--min N]          # count already in hand
  <producer> | assert_hits_stream <label> [--min N]   # count = lines on stdin

  CLI form:
    assert-hits.sh <label> <count> [--min N]
    <producer> | assert-hits.sh --stream <label> [--min N]
EOF
}

# _assert_hits_is_uint <str> — succeed iff <str> is a non-negative integer.
_assert_hits_is_uint() {
  [[ "${1:-}" =~ ^[0-9]+$ ]]
}

# assert_hits <label> <count> [--min N]
#
# The core gate. Echoes <count> on success; on a sub-minimum count it prints
# `FAIL: <label>: expected >=N hits, got <count>` to stderr and returns 2.
# Returns 64 on a usage error (missing/garbage args). Default minimum is 1.
assert_hits() {
  local min=1
  local -a pos=()
  while (($#)); do
    case "$1" in
      --min)
        shift
        if (($# == 0)); then
          echo "assert_hits: --min requires a value" >&2
          return 64
        fi
        min="$1"
        ;;
      --min=*) min="${1#--min=}" ;;
      --)
        shift
        while (($#)); do pos+=("$1"); shift; done
        break
        ;;
      -*)
        echo "assert_hits: unknown flag: $1" >&2
        _assert_hits_usage
        return 64
        ;;
      *) pos+=("$1") ;;
    esac
    shift
  done

  local label="${pos[0]:-}"
  local count="${pos[1]:-}"

  if [[ -z "$label" || ${#pos[@]} -lt 2 ]]; then
    echo "assert_hits: missing label or count" >&2
    _assert_hits_usage
    return 64
  fi
  if ! _assert_hits_is_uint "$count"; then
    echo "assert_hits: count must be a non-negative integer, got: '$count'" >&2
    return 64
  fi
  if ! _assert_hits_is_uint "$min"; then
    echo "assert_hits: --min must be a non-negative integer, got: '$min'" >&2
    return 64
  fi

  if ((count < min)); then
    echo "FAIL: ${label}: expected >=${min} hits, got ${count}" >&2
    return 2
  fi

  echo "$count"
  return 0
}

# assert_hits_stream <label> [--min N]
#
# Stream-mode wrapper for the `inject ... | assert_hits_stream` shape. Consumes
# stdin, counts lines (one line == one hit), then delegates to assert_hits with
# that count. Line counting uses awk's NR so a final line with no trailing
# newline is still counted. Echoes the count on success; same exit codes as
# assert_hits.
assert_hits_stream() {
  local label="${1:-}"
  if [[ -z "$label" || "$label" == -* ]]; then
    echo "assert_hits_stream: missing label" >&2
    _assert_hits_usage
    return 64
  fi
  shift

  local count
  count=$(awk 'END { print NR }')

  assert_hits "$label" "$count" "$@"
}

# CLI dispatch — only when executed directly, never when sourced. Shell options
# are scoped here so sourcing the file does not mutate the caller's shell.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  set -uo pipefail
  if [[ "${1:-}" == "--stream" ]]; then
    shift
    assert_hits_stream "$@"
    exit $?
  fi
  assert_hits "$@"
  exit $?
fi
