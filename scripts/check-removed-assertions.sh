#!/usr/bin/env bash
# check-removed-assertions.sh — the decision behind the CI "Assert Guard".
#
# Usage: check-removed-assertions.sh <base-rev> <head-rev>
#   PR_BODY  (environment) the pull-request description, treated as data only.
#
# Fails (exit 1) when the pull request removes Rust test assertions and its
# description does not carry `assert-guard: reviewed`. Exit 2 on a usage error.
#
# The comparison runs from the MERGE BASE of the two revisions (three-dot
# diff), so only what the pull request itself removes is counted. A two-dot
# diff between two tips also reports everything another pull request merged
# into the base after this branch forked, as "removed" (measured on #221,
# 2026-11-16: 22 false removals, all additions of #219).
#
# The body is never expanded as source: it is only piped through printf '%s'.
# The workflow step passes it through the environment for the same reason
# (see scripts/check-workflow-yaml.sh).
set -uo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <base-rev> <head-rev>" >&2
  exit 2
fi
BASE="$1"
HEAD="$2"

# ERE, so it runs with BSD grep too. The prefix stands for a word boundary:
# `debug_assert!` must not match as `assert!`, and an identifier ending in
# `assert!` is not an assertion macro.
PATTERN='^-(.*[^A-Za-z0-9_])?(assert!|assert_eq!|assert_ne!|debug_assert!|debug_assert_eq!|debug_assert_ne!)|^-.*#\[should_panic'

DIFF=$(git diff "$BASE"..."$HEAD" -- '*.rs') || {
  echo "check-removed-assertions: git diff $BASE...$HEAD failed" >&2
  exit 2
}
REMOVED=$(printf '%s\n' "$DIFF" | grep -v '^---' | grep -E "$PATTERN" || true)

if [ -z "$REMOVED" ]; then
  echo "No test assertions removed. ✓"
  exit 0
fi

echo "::warning::Test assertions were removed in this PR. Review required."
echo ""
echo "The following assertion lines were removed:"
echo "$REMOVED"
echo ""
echo "If these removals are intentional, add 'assert-guard: reviewed' to the PR description."

if printf '%s' "${PR_BODY:-}" | grep -q 'assert-guard: reviewed'; then
  echo ""
  echo "Override marker found in PR description. Proceeding."
  exit 0
fi
exit 1
