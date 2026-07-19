#!/usr/bin/env bash
# Release-path integration test for the `update-tap` job.
#
# The update-tap job in .github/workflows/release.yml renders the tap formula
# from the downloaded build artifacts. render-tap-from-artifacts.sh is that
# job's glue, extracted so it can be exercised without a tagged release. This
# test reconstructs the artifacts tree actions/download-artifact produces (one
# per-triple directory, each holding a `cosmon-<v>-<triple>.tar.gz.sha256`
# checksum file in sha256sum format) and asserts the rendered formula binds each
# REAL digest to the correct stanza — the end-to-end path that only ever ran on
# a real release before.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
render_from_artifacts="$here/render-tap-from-artifacts.sh"

VERSION="9.9.9"
OWNER="noogram"

# Distinct real-shaped digests, one per triple, so a swapped or misread stanza
# is caught. Hex + 64 chars so they pass render-brew-formula.sh's shape check.
SHA_ARM_MACOS="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
SHA_X86_MACOS="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
SHA_ARM_LINUX="cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
SHA_X86_LINUX="dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"

# Build a synthetic artifacts tree matching actions/download-artifact's layout:
# each build leg uploads under a per-stem directory, so the checksum files land
# one level down. render-tap-from-artifacts.sh must `find` them regardless.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
artifacts="$work/artifacts"

emit_checksum() {
  # $1 = triple, $2 = sha. The stem dir mirrors upload-artifact's `name`.
  local triple="$1" sha="$2"
  local stem="cosmon-${VERSION}-${triple}"
  mkdir -p "$artifacts/$stem"
  # sha256sum format: "<digest>  <filename>". The renderer reads field 1.
  printf '%s  %s.tar.gz\n' "$sha" "$stem" > "$artifacts/$stem/${stem}.tar.gz.sha256"
  # A sibling .bin.sha256 (also emitted by the release job) must NOT be picked
  # up in place of the tarball checksum — its name doesn't match the pattern.
  printf '%s  cs\n' "$sha" > "$artifacts/$stem/${stem}.bin.sha256"
}

emit_checksum aarch64-apple-darwin       "$SHA_ARM_MACOS"
emit_checksum x86_64-apple-darwin        "$SHA_X86_MACOS"
emit_checksum aarch64-unknown-linux-musl "$SHA_ARM_LINUX"
emit_checksum x86_64-unknown-linux-musl  "$SHA_X86_LINUX"

out="$("$render_from_artifacts" \
  --version "$VERSION" --owner "$OWNER" --artifacts-dir "$artifacts")"

# 1. All four release triples are served with the release version.
for triple in \
  aarch64-apple-darwin x86_64-apple-darwin \
  aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  printf '%s\n' "$out" | grep -q "cosmon-${VERSION}-${triple}.tar.gz" \
    || { echo "FAIL: rendered formula does not serve ${triple}" >&2; exit 1; }
done
echo "PASS: all four release triples served with real version"

# 2. Each REAL digest lands on its own stanza's URL (no swap, no placeholder).
assert_pair() {
  # $1 = triple, $2 = expected sha
  local got
  got="$(printf '%s\n' "$out" \
    | grep -A1 "cosmon-${VERSION}-$1.tar.gz" | grep sha256 | grep -oE '[0-9a-f]{64}')"
  [ "$got" = "$2" ] \
    || { echo "FAIL: $1 sha256 is $got, expected $2" >&2; exit 1; }
}
assert_pair aarch64-apple-darwin        "$SHA_ARM_MACOS"
assert_pair x86_64-apple-darwin         "$SHA_X86_MACOS"
assert_pair aarch64-unknown-linux-musl  "$SHA_ARM_LINUX"
assert_pair x86_64-unknown-linux-musl   "$SHA_X86_LINUX"
echo "PASS: each real digest is bound to the correct triple"

# 2b. The formula published by the release path declares the workspace's
#     effective license. Read from the root Cargo.toml so a re-license moves
#     both together; a hardcoded copy here would just be a second thing to
#     forget. The tap is the channel users install from — a wrong `license`
#     line is a false claim about the binary they receive.
ws_license="$(grep -m1 -E '^license = "' "$repo_root/Cargo.toml" | sed -E 's/^license = "(.*)"$/\1/')"
[ -n "$ws_license" ] \
  || { echo "FAIL: could not read license from $repo_root/Cargo.toml" >&2; exit 1; }
formula_license="$(printf '%s\n' "$out" | grep -m1 -E '^  license "' | sed -E 's/^  license "(.*)"$/\1/')"
[ "$formula_license" = "$ws_license" ] \
  || { echo "FAIL: rendered license is '$formula_license', workspace is '$ws_license'" >&2; exit 1; }
echo "PASS: released formula license matches the workspace ($ws_license)"

# 3. A missing per-triple artifact fails loudly (not silently rendering an empty
#    digest). This is the failure the original inline glue swallowed.
partial="$work/partial"
mkdir -p "$partial/cosmon-${VERSION}-aarch64-apple-darwin"
printf '%s  cosmon-%s-aarch64-apple-darwin.tar.gz\n' "$SHA_ARM_MACOS" "$VERSION" \
  > "$partial/cosmon-${VERSION}-aarch64-apple-darwin/cosmon-${VERSION}-aarch64-apple-darwin.tar.gz.sha256"
if "$render_from_artifacts" \
    --version "$VERSION" --owner "$OWNER" --artifacts-dir "$partial" >/dev/null 2>&1; then
  echo "FAIL: renderer accepted an artifacts tree missing three triples" >&2
  exit 1
fi
echo "PASS: a missing per-triple artifact is rejected"

# 4. A non-existent artifacts dir fails loudly.
if "$render_from_artifacts" \
    --version "$VERSION" --owner "$OWNER" --artifacts-dir "$work/nope" >/dev/null 2>&1; then
  echo "FAIL: renderer accepted a non-existent artifacts dir" >&2
  exit 1
fi
echo "PASS: a non-existent artifacts dir is rejected"

# 5. A required flag omitted fails loudly.
if "$render_from_artifacts" \
    --owner "$OWNER" --artifacts-dir "$artifacts" >/dev/null 2>&1; then
  echo "FAIL: renderer accepted a missing --version" >&2
  exit 1
fi
echo "PASS: missing required flag is rejected"

echo "ALL PASS: update-tap artifact→formula path holds"
