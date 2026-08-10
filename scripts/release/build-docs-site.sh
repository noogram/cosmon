#!/usr/bin/env bash
# build-docs-site.sh — build the mdBook site AND stamp it with the version it
# was built from.
#
# Why the stamp exists. Publishing a static site is the one release step whose
# success tells you nothing: `wrangler pages deploy` exits 0 for a bundle that
# is byte-identical to last month's. The v0.6.0 release (2026-08-10) shipped
# four signed binaries in four minutes while docs.noogram.org kept describing
# `cs session`, a verb ADR-175 had renamed — 31 occurrences of the old name
# live, zero of the new one. Nothing was red. The publication mechanism was a
# human remembering to run wrangler by hand, and it forgot on the very release
# that renamed a public verb.
#
# So the built bundle carries `version.txt` (and `version.json`), written from
# the tag being released. After the deploy, verify-docs-deploy.sh fetches that
# file from the live origin and asserts it names the tag. A Cloudflare Pages
# deployment is served as one atomic bundle, so the stamp coming back fresh is
# proof that the *pages* are fresh too — which an exit code is not.
#
# Usage:
#   scripts/release/build-docs-site.sh --version 0.6.0 [--commit <sha>]
#   scripts/release/build-docs-site.sh --stamp-only --out <dir> --version 0.6.0
#
# --stamp-only writes the stamp into an existing directory without invoking
# mdbook. It is what the offline test exercises, so the bytes the release path
# publishes are the bytes a test can assert on without a toolchain.
set -euo pipefail

version=""
commit="${GITHUB_SHA:-}"
stamp_only=0
out=""

die() { echo "build-docs-site: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version)    version="${2:-}"; shift 2 ;;
    --commit)     commit="${2:-}"; shift 2 ;;
    --out)        out="${2:-}"; shift 2 ;;
    --stamp-only) stamp_only=1; shift ;;
    -h|--help)    sed -n '2,30p' "$0"; exit 0 ;;
    *)            die "unknown argument: $1" ;;
  esac
done

[ -n "$version" ] || die "--version is required (the tag being released, without a leading v)"
version="${version#v}"

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
book_dir="$repo_root/docs/book"

if [ "$stamp_only" = 0 ]; then
  command -v mdbook >/dev/null 2>&1 || die "mdbook not on PATH"
  # book.toml declares the mermaid preprocessor; without it mdbook aborts
  # rather than silently emitting diagram-less pages.
  command -v mdbook-mermaid >/dev/null 2>&1 || die "mdbook-mermaid not on PATH (book.toml declares the preprocessor)"
  ( cd "$book_dir" && mdbook build )
  out="$book_dir/book"
fi

[ -n "$out" ] || die "--out is required with --stamp-only"
[ -d "$out" ] || die "output directory does not exist: $out"

if [ "$stamp_only" = 0 ]; then
  # A deploy of an empty bundle is worse than no deploy: it replaces a working
  # site with a 404. Refuse to stamp what is not a book.
  [ -f "$out/index.html" ] || die "mdbook produced no index.html in $out"
fi

printf '%s\n' "$version" > "$out/version.txt"
printf '{"version":"%s","commit":"%s"}\n' "$version" "$commit" > "$out/version.json"

echo "build-docs-site: stamped $out with version ${version} (commit ${commit:-unknown})"
