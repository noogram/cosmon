#!/usr/bin/env bash
# docker/spore-e2e/build.sh — build the spore-e2e image.
#
# Stages the canonical `math-attack` spore drop into a gitignored build-context
# subdir (the drop is an external, unreviewed asset — never tracked in cosmon),
# then builds the image with the repo ROOT as context. Idempotent; safe to
# re-run. The heavy `cargo build --release` runs here (in step-1 implementation),
# NOT in the 600s smoke-dispatch gate — the gate only `docker run`s the result.
set -euo pipefail

IMAGE="${SPORE_E2E_IMAGE:-cosmon-spore-e2e:latest}"
SPORE_ZIP="${SPORE_ZIP:-/Users/eserie/galaxies/sporarium/drops/math-attack-v2-20260720.zip}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
STAGE="$HERE/_spore_stage"

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

command -v docker >/dev/null 2>&1 || die "docker not on PATH"
[ -f "$SPORE_ZIP" ] || die "spore drop not found: $SPORE_ZIP (fail closed)"
[ -f "$REPO_ROOT/docs/specs/tla2tools.jar" ] || die "docs/specs/tla2tools.jar missing"

say "Staging spore from $SPORE_ZIP ..."
rm -rf "$STAGE"; mkdir -p "$STAGE"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
unzip -oq "$SPORE_ZIP" -d "$TMP"
# The zip holds math-attack/spore.toml at its root; flatten into _spore_stage.
SPORE_DIR="$(dirname "$(find "$TMP" -name spore.toml | head -1)")"
[ -n "$SPORE_DIR" ] || die "spore.toml not found inside the zip"
cp -r "$SPORE_DIR"/. "$STAGE/"
[ -f "$STAGE/spore.toml" ] || die "staging failed — no spore.toml under $STAGE"

say "Building image $IMAGE (context = $REPO_ROOT) ..."
docker build -t "$IMAGE" -f "$HERE/Dockerfile" "$REPO_ROOT"

say "Cleaning staging ..."
rm -rf "$STAGE"

printf '\033[1;32m✓ built %s\033[0m\n' "$IMAGE"
