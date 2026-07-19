#!/usr/bin/env bash
# Hermetic regression tests for render-brew-formula.sh.
#
# The renderer is the single source of truth for the Homebrew tap formula. A
# release re-runs it with real digests; the checked-in snapshot at
# packaging/homebrew-tap/Formula/cosmon.rb is the SAME renderer's output with
# deterministic placeholder digests. These tests lock that contract: the four
# release triples are all served, digests land on the right stanza, the required
# flags are enforced, and the checked-in snapshot is byte-identical to a fresh
# render with the placeholders.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
render="$here/render-brew-formula.sh"
snapshot="$repo_root/packaging/homebrew-tap/Formula/cosmon.rb"

# The placeholder digests the checked-in snapshot is rendered with. Distinct
# 64-hex constants — distinct so a swapped stanza is caught, hex so they pass
# the renderer's shape check exactly as real digests do.
PH_ARM_MACOS="0000000000000000000000000000000000000000000000000000000000000000"
PH_X86_MACOS="1111111111111111111111111111111111111111111111111111111111111111"
PH_X86_LINUX="2222222222222222222222222222222222222222222222222222222222222222"
PH_ARM_LINUX="3333333333333333333333333333333333333333333333333333333333333333"

render_placeholders() {
  "$render" \
    --version 0.1.0 --owner noogram \
    --sha-arm-macos "$PH_ARM_MACOS" \
    --sha-x86-macos "$PH_X86_MACOS" \
    --sha-arm-linux "$PH_ARM_LINUX" \
    --sha-x86-linux "$PH_X86_LINUX"
}

out="$(render_placeholders)"

# 1. All four release triples are served.
for triple in \
  aarch64-apple-darwin x86_64-apple-darwin \
  aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  printf '%s\n' "$out" | grep -q "cosmon-0.1.0-${triple}.tar.gz" \
    || { echo "FAIL: formula does not serve ${triple}" >&2; exit 1; }
done
echo "PASS: all four release triples served"

# 2. Both macOS and Linux carry an on_arm AND an on_intel stanza (the Linux ARM
#    stanza is the regression this whole change closes).
[ "$(printf '%s\n' "$out" | grep -c 'on_arm do')" -eq 2 ] \
  || { echo "FAIL: expected two on_arm stanzas (macOS + Linux)" >&2; exit 1; }
[ "$(printf '%s\n' "$out" | grep -c 'on_intel do')" -eq 2 ] \
  || { echo "FAIL: expected two on_intel stanzas (macOS + Linux)" >&2; exit 1; }
echo "PASS: macOS and Linux each carry on_arm + on_intel"

# 3. Each digest lands on its own stanza's URL (no swap). Check the line that
#    follows each url is the matching sha256.
assert_pair() {
  # $1 = triple, $2 = expected sha
  local got
  got="$(printf '%s\n' "$out" \
    | grep -A1 "cosmon-0.1.0-$1.tar.gz" | grep sha256 | grep -oE '[0-9a-f]{64}')"
  [ "$got" = "$2" ] \
    || { echo "FAIL: $1 sha256 is $got, expected $2" >&2; exit 1; }
}
assert_pair aarch64-apple-darwin        "$PH_ARM_MACOS"
assert_pair x86_64-apple-darwin         "$PH_X86_MACOS"
assert_pair aarch64-unknown-linux-musl  "$PH_ARM_LINUX"
assert_pair x86_64-unknown-linux-musl   "$PH_X86_LINUX"
echo "PASS: each digest is bound to the correct triple"

# 3b. The formula's `license` matches the workspace's effective license. Read
#     from the root Cargo.toml rather than hardcoded here, so a future
#     re-license moves both in one edit instead of silently drifting. The
#     formula previously claimed MIT while the binary shipped AGPL-3.0-only —
#     a false licence claim on the distribution channel users install from.
ws_license="$(grep -m1 -E '^license = "' "$repo_root/Cargo.toml" | sed -E 's/^license = "(.*)"$/\1/')"
[ -n "$ws_license" ] \
  || { echo "FAIL: could not read license from $repo_root/Cargo.toml" >&2; exit 1; }
formula_license="$(printf '%s\n' "$out" | grep -m1 -E '^  license "' | sed -E 's/^  license "(.*)"$/\1/')"
[ "$formula_license" = "$ws_license" ] \
  || { echo "FAIL: formula license is '$formula_license', workspace is '$ws_license'" >&2; exit 1; }
echo "PASS: formula license matches the workspace ($ws_license)"

# 4. The checked-in snapshot is byte-identical to a fresh placeholder render.
if ! diff -u "$snapshot" <(render_placeholders); then
  echo "FAIL: $snapshot has drifted from render-brew-formula.sh." >&2
  echo "Regenerate it: scripts/release/render-brew-formula.sh --version 0.1.0 --owner noogram \\" >&2
  echo "  --sha-arm-macos $PH_ARM_MACOS --sha-x86-macos $PH_X86_MACOS \\" >&2
  echo "  --sha-arm-linux $PH_ARM_LINUX --sha-x86-linux $PH_X86_LINUX > $snapshot" >&2
  exit 1
fi
echo "PASS: checked-in formula matches the renderer"

# 5. Missing a required flag fails loudly.
if "$render" --version 0.1.0 --owner noogram \
    --sha-arm-macos "$PH_ARM_MACOS" --sha-x86-macos "$PH_X86_MACOS" \
    --sha-x86-linux "$PH_X86_LINUX" >/dev/null 2>&1; then
  echo "FAIL: renderer accepted a missing --sha-arm-linux" >&2
  exit 1
fi
echo "PASS: missing required sha flag is rejected"

# 6. A malformed (non-hex / wrong-length) digest is rejected.
if "$render" --version 0.1.0 --owner noogram \
    --sha-arm-macos "not-a-real-sha" --sha-x86-macos "$PH_X86_MACOS" \
    --sha-arm-linux "$PH_ARM_LINUX" --sha-x86-linux "$PH_X86_LINUX" >/dev/null 2>&1; then
  echo "FAIL: renderer accepted a malformed digest" >&2
  exit 1
fi
echo "PASS: malformed digest is rejected"

echo "ALL PASS: render-brew-formula.sh contract holds"
