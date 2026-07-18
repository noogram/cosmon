#!/usr/bin/env bash
# Render the Homebrew tap formula from a downloaded release-artifacts tree.
#
# WHY this exists: the release pipeline's `update-tap` job
# (.github/workflows/release.yml) must turn the build matrix's *artifacts* — one
# `cosmon-<version>-<triple>.tar.gz.sha256` file per release triple, laid out
# under the actions/download-artifact tree — into the final formula carrying the
# REAL per-target digests (not the placeholders the checked-in snapshot uses).
#
# That glue (find each sha256 file, read its digest, invoke the renderer) used
# to live inline in the workflow YAML, where nothing could exercise it: a typo
# in the artifact-name pattern or the awk field would only ever surface on a
# tagged release, and only once ENABLE_HOMEBREW_TAP was flipped on. Extracting
# it into a tracked script makes `update-tap` testable end-to-end —
# render-tap-from-artifacts.test.sh builds a synthetic artifacts tree and
# asserts the rendered formula binds each real digest to the correct triple.
#
# This script only resolves digests from the artifacts tree and delegates the
# actual formula text to render-brew-formula.sh (the single source of truth for
# the formula body). The two stay in lockstep: this one owns the artifact
# layout, that one owns the Ruby.
#
# Usage:
#   render-tap-from-artifacts.sh \
#     --version 0.1.0 --owner noogram --artifacts-dir artifacts
#
# All three flags are required. The rendered formula is written to stdout.
set -euo pipefail

version=""
owner=""
artifacts_dir=""

die() {
  echo "render-tap-from-artifacts: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)       version="${2:-}"; shift 2 ;;
    --owner)         owner="${2:-}"; shift 2 ;;
    --artifacts-dir) artifacts_dir="${2:-}"; shift 2 ;;
    -h|--help)
      sed -n '2,27p' "$0"
      exit 0
      ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$version" ]       || die "--version is required"
[ -n "$owner" ]         || die "--owner is required"
[ -n "$artifacts_dir" ] || die "--artifacts-dir is required"
[ -d "$artifacts_dir" ] || die "artifacts dir does not exist: $artifacts_dir"

here="$(cd "$(dirname "$0")" && pwd)"

# Resolve the per-target sha256 from the artifact checksum files. Each file is
# `<64-hex>  cosmon-<version>-<triple>.tar.gz` (sha256sum / shasum -a 256
# format), so the digest is the first whitespace-delimited field. The four
# triples MUST match render-brew-formula.sh's stanzas and release.yml's build
# matrix — the install-lint `triples` job enforces brew ⊆ installer on top.
declare -A sha
for target in aarch64-apple-darwin x86_64-apple-darwin \
              aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  f="$(find "$artifacts_dir" -name "cosmon-${version}-${target}.tar.gz.sha256" | head -1)"
  [ -n "$f" ] || die "no sha256 artifact for ${target} under ${artifacts_dir}"
  sha[$target]="$(awk '{print $1}' "$f")"
done

# Delegate the formula body to the single source of truth. render-brew-formula.sh
# shape-checks every digest, so a truncated/garbled checksum file fails loudly
# here rather than shipping a broken formula to the tap.
exec "$here/render-brew-formula.sh" \
  --version "$version" \
  --owner   "$owner" \
  --sha-arm-macos "${sha[aarch64-apple-darwin]}" \
  --sha-x86-macos "${sha[x86_64-apple-darwin]}" \
  --sha-arm-linux "${sha[aarch64-unknown-linux-musl]}" \
  --sha-x86-linux "${sha[x86_64-unknown-linux-musl]}"
