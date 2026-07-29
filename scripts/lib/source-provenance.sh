#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# ─────────────────────────────────────────────────────────────────────────────
# source-provenance.sh — the two-sample guard that makes a stamped commit true.
#
# WHY THIS IS A LIBRARY AND NOT TEN LINES INSIDE THE BENCH
# ───────────────────────────────────────────────────────
# The bench that stamps `COSMON_SOURCE_SHA` into an image samples the tree
# twice: once before `docker build` (cheap refusal — do not spend ten minutes
# on a build that was doomed) and once after (the one that actually closes the
# window, because `COPY` happens MID-build and takes whatever is on disk at
# that instant).
#
# The post-build sample shipped with NO test. It was correct and it was
# unfalsified — nothing went red when it was removed — so the committee ruled
# it must be reported "unfalsified rather than confirmed". A control nobody can
# break on purpose is a control nobody knows still works. Moving the two
# predicates behind a named boundary is what makes them reachable from
# `scripts/source-provenance.test.sh`, which constructs the moved-tree case in
# a throwaway repository and requires the guard to refuse it.
#
# Nothing here prints or exits: the caller owns its own reporting vocabulary.
# These are predicates, and a predicate that calls `exit` cannot be tested.
# ─────────────────────────────────────────────────────────────────────────────

# Echo `<sha> <clean|dirty>` for the tree at $1.
#
# `unknown` for the sha when the path is not a repository, so a caller that
# stamps the result stamps a value that is visibly not a commit rather than an
# empty string that reads as "no finding".
source_tree_state() {
  local root="$1" sha state
  sha="$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)"
  if [ -z "$(git -C "$root" status --porcelain 2>/dev/null)" ]; then
    state="clean"
  else
    state="dirty"
  fi
  printf '%s %s\n' "$sha" "$state"
}

# 0 when the tree at $1 is still at commit $2 AND still clean; 1 otherwise.
#
# Both halves are load-bearing and neither implies the other. An edit that is
# committed during the build moves HEAD while leaving the tree clean; an edit
# that is not committed leaves HEAD alone and dirties the tree. The image took
# the bytes either way, so the stamped commit describes something else.
source_unmoved() {
  local root="$1" expected="$2" now
  now="$(source_tree_state "$root")"
  [ "$now" = "$expected clean" ]
}
