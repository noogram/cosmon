#!/usr/bin/env bash
# docs-deploy.test.sh — offline contract test for the docs-publication pair
# (build-docs-site.sh --stamp-only + verify-docs-deploy.sh).
#
# These two scripts only ever run inside a tag push, on a runner, against a
# live origin — the worst place to discover they are wrong. The property that
# matters is not "the verifier runs" but "the verifier REDDENS on a stale
# origin", because a verifier that greens on stale content restores the exact
# defect it was written to close. So the stale case is asserted first, and by
# construction: `file://` origins standing in for docs.noogram.org, no network,
# no toolchain, no mdbook.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="$here/build-docs-site.sh"
verify="$here/verify-docs-deploy.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fails=0

check() {  # check <label> <expected-rc> <cmd...>
  local label="$1" want="$2"; shift 2
  local rc=0
  "$@" >"$tmp/out" 2>&1 || rc=$?
  if [ "$rc" = "$want" ]; then
    echo "  ok   — $label"
  else
    echo "  FAIL — $label (wanted rc=$want, got rc=$rc)"
    sed 's/^/         /' "$tmp/out"
    fails=$(( fails + 1 ))
  fi
}

echo "docs-deploy.test.sh"

# ── a released bundle, as build-docs-site.sh stamps it ───────────────────────
fresh="$tmp/fresh"; mkdir -p "$fresh"
printf '<html><head><title>noogram / cosmon</title></head><body>hi</body></html>\n' > "$fresh/index.html"
check "stamp-only writes the stamp without mdbook" 0 \
  "$build" --stamp-only --out "$fresh" --version v0.6.0

[ "$(cat "$fresh/version.txt")" = "0.6.0" ] \
  || { echo "  FAIL — version.txt should hold the tag with the v stripped"; fails=$(( fails + 1 )); }
grep -q '"version":"0.6.0"' "$fresh/version.json" \
  || { echo "  FAIL — version.json should hold the version"; fails=$(( fails + 1 )); }

check "stamp-only refuses a directory that does not exist" 1 \
  "$build" --stamp-only --out "$tmp/nope" --version 0.6.0
check "a version is mandatory" 1 \
  "$build" --stamp-only --out "$fresh"

# ── the origin serves the release: green ─────────────────────────────────────
check "verifier accepts an origin serving the released version" 0 \
  "$verify" --version 0.6.0 --base-url "file://$fresh" --attempts 1 --delay 0

check "verifier tolerates a leading v on the tag" 0 \
  "$verify" --version v0.6.0 --base-url "file://$fresh" --attempts 1 --delay 0

# ── THE case: origin up, deploy "succeeded", content is last month's ─────────
stale="$tmp/stale"; mkdir -p "$stale"
cp "$fresh/index.html" "$stale/index.html"
printf '0.5.0\n' > "$stale/version.txt"
check "verifier REDDENS on a stale origin (the v0.6.0 defect)" 1 \
  "$verify" --version 0.6.0 --base-url "file://$stale" --attempts 2 --delay 0
grep -q "0.5.0" "$tmp/out" \
  || { echo "  FAIL — the failure must name what the origin actually served"; fails=$(( fails + 1 )); }

# ── origin with no stamp at all (never deployed / wrong project) ─────────────
bare="$tmp/bare"; mkdir -p "$bare"
check "verifier reddens when the origin has no stamp" 1 \
  "$verify" --version 0.6.0 --base-url "file://$bare" --attempts 2 --delay 0

# ── stamp fresh but bundle broken: right version, no book ────────────────────
broken="$tmp/broken"; mkdir -p "$broken"
printf '0.6.0\n' > "$broken/version.txt"
printf '<html><body>404</body></html>\n' > "$broken/index.html"
check "verifier reddens when the stamp is fresh but the bundle is not a book" 1 \
  "$verify" --version 0.6.0 --base-url "file://$broken" --attempts 1 --delay 0

if [ "$fails" != 0 ]; then
  echo "docs-deploy.test.sh: $fails failure(s)"
  exit 1
fi
echo "docs-deploy.test.sh: OK"
