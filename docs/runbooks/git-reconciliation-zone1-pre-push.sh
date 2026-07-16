#!/usr/bin/env bash
#
# pre-push — Git reconciliation Zone-1 trip-wire guard.
#
# Installed by task-20260711-c298 (delib-20260711-4733 §C1). The living tree
# (lineage A) and cosmon-private (lineage B, twice-filtered) are DISJOINT
# histories, and a `public` remote (noogram/cosmon) now exists for the eventual
# publish. This hook blocks the two — and ONLY the two — dangerous moves:
#
#   1. refs/heads/main  ->  origin (cosmon-private)   [the accidental trip-wire]
#   2. ANY ref          ->  public (noogram/cosmon)   [premature/ wrong publish]
#
# Everything else is explicitly allowed. In particular the fleet's continuous
# `feat/*  ->  origin` pushes are UNTOUCHED — this hook must never break a live
# worker. It is additive & reversible: `rm .git/hooks/pre-push` disables it.
#
# git passes: $1 = remote name, $2 = remote URL.
# stdin lines: <local ref> <local sha> <remote ref> <remote sha>
set -euo pipefail

remote_name="${1:-}"
remote_url="${2:-}"

# Classify the destination. Order matters: cosmon-private is a superstring of
# noogram/cosmon, so it MUST be tested first. Name-based OR url-based, whichever
# fires — belt and suspenders against a future rename or an odd URL form.
is_public=no
is_priv_origin=no
case "$remote_name" in
  public) is_public=yes ;;
  origin) is_priv_origin=yes ;;
esac
case "$remote_url" in
  *cosmon-private*)  is_priv_origin=yes ;;   # lineage B home (matched first)
  *noogram/cosmon*)  is_public=yes ;;        # the public target
esac

# Rule 2 — refuse ALL pushes to public. Checked first (strictest).
if [ "$is_public" = yes ]; then
  echo "pre-push REFUSED: no push to the public remote ('$remote_name' -> $remote_url)." >&2
  echo "  Publishing is a Zone-3 operator-only act performed on a scratch clone." >&2
  echo "  See docs/runbooks/git-reconciliation-zone1.md. rm .git/hooks/pre-push to override." >&2
  exit 1
fi

# Rule 1 — refuse only refs/heads/main -> origin (cosmon-private).
if [ "$is_priv_origin" = yes ]; then
  while read -r local_ref local_sha remote_ref remote_sha; do
    if [ "$remote_ref" = "refs/heads/main" ]; then
      echo "pre-push REFUSED: refs/heads/main -> origin (cosmon-private, $remote_url)." >&2
      echo "  main (lineage A) and cosmon-private (lineage B) are disjoint; this is the trip-wire." >&2
      echo "  feat/* pushes to origin are allowed; only main is blocked here." >&2
      echo "  See docs/runbooks/git-reconciliation-zone1.md. rm .git/hooks/pre-push to override." >&2
      exit 1
    fi
  done
fi

exit 0
