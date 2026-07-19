#!/usr/bin/env bash
# Assert that EVERY user-facing binary shipped in a cosmon release reports the
# release version from `--version`.
#
# WHY. A user who downloads `0.2.1` and runs a binary that announces `0.3.0`
# reasonably concludes the install is broken — that is exactly what a fresh
# public install of v0.2.1 did (`cs 0.2.1` / `cosmon-remote 0.3.0`). The crate
# versions are now aligned in the manifests, but an alignment nothing checks
# rots silently: both prior packaging defects (gnu-vs-musl, and the missing
# connector) shipped precisely because nothing checked.
#
# The binary list is NOT hand-copied here — it is read from
# packaging/shipped-binaries.txt, the same canon release.yml and the alignment
# test read.
#
# USAGE
#   scripts/release-version-conformance.sh <expected-version> \
#       [--bindir DIR] [--tarball NAME]
#
#   <expected-version>  release version WITHOUT the leading `v` (e.g. 0.2.1).
#                       A leading `v` is tolerated and stripped.
#   --bindir DIR        look for the binaries in DIR instead of on PATH. Used by
#                       the packaging job to check the just-built artifacts
#                       before they are tarred; the brew smoke job omits it and
#                       checks what `brew install` actually placed on PATH.
#   --tarball NAME      restrict to the binaries shipped in one tarball
#                       (`client` or `service`, per the canon's third column).
#                       The brew smoke job installs only the client tarball, so
#                       without this filter the service binaries would report as
#                       legitimately-absent MISSING and redden a healthy release.
#
# EXIT
#   0  every shipped binary reports <expected-version>
#   1  at least one mismatch, or a shipped binary is missing/unrunnable
#   2  usage error

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${SCRIPT_DIR}/.."
CANON="${ROOT}/packaging/shipped-binaries.txt"

expected=""
bindir=""
want_tarball=""

while [ $# -gt 0 ]; do
  case "$1" in
    --bindir)
      [ $# -ge 2 ] || { echo "error: --bindir needs a directory" >&2; exit 2; }
      bindir="$2"; shift 2 ;;
    --tarball)
      [ $# -ge 2 ] || { echo "error: --tarball needs a name" >&2; exit 2; }
      want_tarball="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    -*)
      echo "error: unknown flag $1" >&2; exit 2 ;;
    *)
      [ -z "$expected" ] || { echo "error: unexpected argument $1" >&2; exit 2; }
      expected="$1"; shift ;;
  esac
done

if [ -z "$expected" ]; then
  echo "usage: $(basename "$0") <expected-version> [--bindir DIR]" >&2
  exit 2
fi

# Tolerate a `v`-prefixed tag so callers can pass "$GITHUB_REF_NAME" directly.
expected="${expected#v}"

if [ ! -f "$CANON" ]; then
  echo "error: canon not found: $CANON" >&2
  exit 2
fi

fail=0
checked=0

while read -r binary crate tarball _rest; do
  # Skip comments and blank lines.
  case "${binary:-}" in ''|'#'*) continue ;; esac

  if [ -n "$want_tarball" ] && [ "$tarball" != "$want_tarball" ]; then
    continue
  fi

  if [ -n "$bindir" ]; then
    exe="${bindir}/${binary}"
    if [ ! -x "$exe" ]; then
      echo "MISSING  ${binary} — not executable at ${exe} (crate ${crate}, ${tarball} tarball)" >&2
      fail=1
      continue
    fi
  else
    exe="$(command -v "$binary" 2>/dev/null || true)"
    if [ -z "$exe" ]; then
      echo "MISSING  ${binary} — not on PATH (crate ${crate}, ${tarball} tarball)" >&2
      fail=1
      continue
    fi
  fi

  out="$("$exe" --version 2>&1)" || {
    echo "BROKEN   ${binary} — \`--version\` exited non-zero: ${out}" >&2
    fail=1
    continue
  }

  # clap prints "<name> <version>[ (extra)]"; `cs` appends a git sha and build
  # date on later lines. Take the first whitespace-separated token on the first
  # line that looks like a semver.
  got="$(printf '%s\n' "$out" | head -n1 | tr ' ' '\n' \
         | grep -E '^[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"

  checked=$((checked + 1))

  if [ -z "$got" ]; then
    echo "UNPARSED ${binary} — no semver in \`--version\` output: ${out}" >&2
    fail=1
  elif [ "$got" != "$expected" ]; then
    echo "MISMATCH ${binary} — reports ${got}, release is ${expected} (crate ${crate})" >&2
    echo "         a user who downloaded ${expected} would read this as a broken install." >&2
    echo "         fix: give ${crate} \`version.workspace = true\` in its Cargo.toml." >&2
    fail=1
  else
    echo "ok       ${binary} ${got} (${tarball})"
  fi
done < "$CANON"

# Fail closed on an empty check. A filter typo (`--tarball clients`) would
# otherwise select zero rows and exit 0, reporting green while asserting
# nothing — the same silence that let the original defect ship.
if [ "$checked" -eq 0 ] && [ "$fail" -eq 0 ]; then
  if [ -n "$want_tarball" ]; then
    echo "error: no canon row has tarball '${want_tarball}' — refusing to report success" >&2
  else
    echo "error: canon listed no binaries — refusing to report success" >&2
  fi
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo "release version conformance: FAILED" >&2
  exit 1
fi

scope="${want_tarball:+ (${want_tarball} tarball)}"
echo "release version conformance: all ${checked} shipped binaries${scope} report ${expected}"
