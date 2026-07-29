#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# ─────────────────────────────────────────────────────────────────────────────
# source-provenance.test.sh — the falsifier R3 shipped without.
#
# WHAT WAS MISSING, AND WHY IT MATTERED
# ─────────────────────────────────────
# `container-nonroot-pilot-bench.sh` re-samples HEAD and the porcelain status
# AFTER `docker build` and refuses if either moved. That re-check is the control
# that makes the stamped `COSMON_SOURCE_SHA` true: the image `COPY`s the tree
# MID-build, so a pre-build sample fires one instant before the interesting
# failure can happen. It shipped with no test — remove it and nothing went red —
# so the committee ruled it must be reported "unfalsified rather than
# confirmed".
#
# This closes that. Two axes, because either alone can be satisfied by a fake:
#
#   1. BEHAVIOUR — the predicates refuse a tree that moved. Constructed in a
#      throwaway repository, both ways a tree can move: a commit landing (HEAD
#      changes, tree stays clean) and an edit landing (HEAD stays, tree dirties).
#      Neither implies the other and the image takes the bytes either way.
#   2. CALL SITE — the bench still calls the POST-build guard, after the build.
#      A behaviour test alone stays green while someone deletes the call, which
#      is exactly the regression the committee named. Position is the property:
#      a guard that runs before `docker build` is the pre-check, and the
#      pre-check is not what closes the window.
#
# No credential, no network, no docker. Everything is a local throwaway repo.
#
# Usage: ./scripts/source-provenance.test.sh
# Exit:  0 all cases behaved · 1 a case did not · 2 harness error
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB="$ROOT/scripts/lib/source-provenance.sh"
BENCH="$ROOT/scripts/container-nonroot-pilot-bench.sh"
[ -f "$LIB" ] || { echo "source-provenance.test: $LIB missing" >&2; exit 2; }
[ -f "$BENCH" ] || { echo "source-provenance.test: $BENCH missing" >&2; exit 2; }

# shellcheck source=lib/source-provenance.sh
. "$LIB"

WORK="$(mktemp -d)" || exit 2
trap 'rm -rf "$WORK"' EXIT

fails=0
ok()  { printf '  \033[32m%s\033[0m %s\n' "OK" "$1"; }
bad() { printf '  \033[31m%s\033[0m %s\n' "XX" "$1"; fails=$((fails + 1)); }

echo "source-provenance.test: constructing moved trees"

# A real repository with one commit.
r="$WORK/repo"
mkdir -p "$r" || exit 2
git -C "$r" init -q 2>/dev/null || exit 2
git -C "$r" config user.email t@example.com
git -C "$r" config user.name test
printf 'one\n' >"$r/a.txt"
git -C "$r" add -A >/dev/null 2>&1
git -C "$r" commit -qm one >/dev/null 2>&1 || exit 2

BEFORE="$(git -C "$r" rev-parse HEAD)"

# ── 1. the baseline. Without it, every refusal below could be a guard that
# refuses everything, which is decoration in the other direction.
if source_unmoved "$r" "$BEFORE"; then
  ok "an unmoved clean tree passes the guard"
else
  bad "an unmoved clean tree was refused - the guard refuses everything"
fi

# ── 2. an EDIT lands during the build. HEAD is untouched, the tree dirties,
# and the COPY already took the edited bytes.
printf 'two\n' >>"$r/a.txt"
if source_unmoved "$r" "$BEFORE"; then
  bad "a DIRTIED tree passed - the stamped commit does not describe the bytes"
else
  ok "an edit landing during the build is refused"
fi
state="$(source_tree_state "$r")"
case "$state" in
  *" dirty") ok "source_tree_state reports the dirty tree as dirty" ;;
  *) bad "source_tree_state said '$state' for a dirtied tree" ;;
esac

# ── 3. a COMMIT lands during the build. HEAD moves and the tree is clean
# again, so a dirtiness check alone would pass it. This is why the guard is
# a conjunction and not one half of one.
git -C "$r" add -A >/dev/null 2>&1
git -C "$r" commit -qm two >/dev/null 2>&1 || exit 2
if [ "$(git -C "$r" rev-parse HEAD)" = "$BEFORE" ]; then
  bad "harness: HEAD did not move, case 3 proves nothing"
elif source_unmoved "$r" "$BEFORE"; then
  bad "a MOVED HEAD passed while the tree was clean - half the guard is dead"
else
  ok "a commit landing during the build is refused, clean tree notwithstanding"
fi

# ── 4. a path that is not a repository at all resolves visibly, never to an
# empty string a caller would stamp as if it were a commit.
mkdir -p "$WORK/notarepo"
case "$(source_tree_state "$WORK/notarepo")" in
  unknown*) ok "a non-repository resolves to 'unknown', not to an empty stamp" ;;
  *) bad "a non-repository resolved to '$(source_tree_state "$WORK/notarepo")'" ;;
esac

# ── 5. THE CALL SITE. The behaviour above stays green while someone deletes
# the post-build call, so the position of the call is asserted separately.
# `docker build` must come BEFORE `verify_source_unmoved`: a guard that runs
# only before the build is the pre-check, and the pre-check cannot see an edit
# that lands mid-build, which is the whole failure this control exists for.
build_line="$(grep -n 'docker --context "\$CTX" build' "$BENCH" | head -1 | cut -d: -f1)"
guard_line="$(grep -n '^verify_source_unmoved$' "$BENCH" | head -1 | cut -d: -f1)"
if [ -z "$build_line" ]; then
  bad "call site: no 'docker build' invocation found in the bench"
elif [ -z "$guard_line" ]; then
  bad "call site: the bench no longer CALLS verify_source_unmoved - the post-build re-check was removed"
elif [ "$guard_line" -le "$build_line" ]; then
  bad "call site: verify_source_unmoved (line $guard_line) runs BEFORE docker build (line $build_line) - that is the pre-check, which cannot see a mid-build edit"
else
  ok "the bench calls verify_source_unmoved AFTER docker build (lines $build_line -> $guard_line)"
fi

echo "----------------------------------------------------------------------------"
if [ "$fails" -eq 0 ]; then
  printf '\033[32msource-provenance.test: all cases behaved - the guard refuses every moved tree it claims to.\033[0m\n'
  exit 0
fi
printf '\033[31msource-provenance.test: %s case(s) did not behave.\033[0m\n' "$fails"
exit 1
