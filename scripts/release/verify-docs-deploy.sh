#!/usr/bin/env bash
# verify-docs-deploy.sh — assert the LIVE docs origin serves the version that
# was just released.
#
# The defect this closes is not "the deploy command failed". It is "the deploy
# command succeeded and the site kept serving last month's pages" — which is
# exactly what a `wrangler pages deploy` exit code cannot distinguish. So the
# check is made against the origin over the network, on content, not on status:
#
#   1. `<base>/version.txt` must equal the released version. The stamp is
#      written into the bundle by build-docs-site.sh at build time, and a
#      Cloudflare Pages deployment is served atomically — a fresh stamp means
#      fresh pages.
#   2. `<base>/` must return a real page containing the book's title, so a
#      deploy that published an empty or half-built bundle fails here instead
#      of quietly turning the site into a 404 farm.
#
# The origin needs a moment to route a new deployment onto the custom domain,
# so (1) is retried; a stale answer is a retry, any other failure is fatal at
# once. On give-up the served value is printed — the whole point is to name
# what the site actually says.
#
# `--base-url` accepts file:// (curl handles it), which is how the offline test
# exercises this script without a network or a live origin.
#
# Usage:
#   scripts/release/verify-docs-deploy.sh --version 0.6.0 \
#       [--base-url https://docs.noogram.org] [--attempts 20] [--delay 15]
set -euo pipefail

version=""
base_url="https://docs.noogram.org"
attempts=20
delay=15

die() { echo "verify-docs-deploy: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version)  version="${2:-}"; shift 2 ;;
    --base-url) base_url="${2:-}"; shift 2 ;;
    --attempts) attempts="${2:-}"; shift 2 ;;
    --delay)    delay="${2:-}"; shift 2 ;;
    -h|--help)  sed -n '2,28p' "$0"; exit 0 ;;
    *)          die "unknown argument: $1" ;;
  esac
done

[ -n "$version" ] || die "--version is required"
version="${version#v}"
base_url="${base_url%/}"

served=""
i=1
while [ "$i" -le "$attempts" ]; do
  if served="$(curl -fsSL --max-time 30 "${base_url}/version.txt" 2>/dev/null)"; then
    served="$(printf '%s' "$served" | tr -d '[:space:]')"
    if [ "$served" = "$version" ]; then
      echo "verify-docs-deploy: ${base_url}/version.txt => ${served} (matches the released tag)"
      break
    fi
    echo "verify-docs-deploy: attempt ${i}/${attempts} — origin still serving '${served}', want '${version}'"
  else
    echo "verify-docs-deploy: attempt ${i}/${attempts} — ${base_url}/version.txt not fetchable yet"
    served=""
  fi
  if [ "$i" -eq "$attempts" ]; then
    die "origin never served version ${version} (last answer: '${served:-<unfetchable>}'). The deploy reported success while ${base_url} serves other content — do NOT treat the release docs as published."
  fi
  sleep "$delay"
  i=$(( i + 1 ))
done

# A correct stamp beside a broken bundle would still be a broken site. Ask for
# /index.html by name rather than the bare root: Pages redirects it to `/` (and
# curl -L follows), while a file:// base — how the offline test drives this —
# resolves it to the actual page instead of a directory listing.
home="$(curl -fsSL --max-time 30 "${base_url}/index.html" )" \
  || die "${base_url}/index.html is not fetchable — the deploy left no reachable home page"
case "$home" in
  *"noogram / cosmon"*) : ;;
  *) die "${base_url}/index.html does not look like the built book (no title marker in the served HTML)" ;;
esac

echo "verify-docs-deploy: ${base_url} is live and serving ${version}"
