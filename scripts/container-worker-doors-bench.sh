#!/usr/bin/env bash
# container-worker-doors-bench.sh — host-side driver that replays issue
# #20's two container failure scenarios against the local worktree's `cs`.
#
# The tester (@jdthaler) reported two failure modes on the signed v0.3.0
# and then published a secret-free reproduction recipe. Three fixes landed
# on `feat/container-worker-doors`; this bench is what turns "we believe it
# is fixed" into "we ran his scenarios and here is the pane".
#
# Usage:
#   scripts/container-worker-doors-bench.sh                 # build + run
#   scripts/container-worker-doors-bench.sh | tee bench.log
#
# Environment overrides:
#   COSMON_DOCKER_CONTEXT   docker context (default: desktop-linux, which
#                           is the tester's engine — Docker Desktop on
#                           macOS arm64, LinuxKit VM. A colima context
#                           runs an Ubuntu kernel with a DIFFERENT
#                           user-namespace posture and is NOT faithful.)
#   COSMON_KEEP_IMAGE=1     skip the image rmi at teardown
#
# SECRETS: none, ever. No credential is read from the host, mounted, or
# passed in. Arms B and C mint an obviously-invalid PLACEHOLDER
# credentials file inside the container so that the doors BEHIND the
# credential gate become observable. See the in-container script's header.
#
# ISOLATION: `docker run --rm` with no bind mount of any host state. The
# galaxies are created inside the container and die with it. Nothing here
# touches the live fleet, the resident runtime, or .cosmon/state. The
# image is tagged and removed by name, so no pre-existing image or
# container of yours is ever a candidate for deletion.
set -euo pipefail

CONTEXT="${COSMON_DOCKER_CONTEXT:-desktop-linux}"
IMAGE="cosmon-container-worker-doors:bench"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

say() { printf '\n\033[1;34m▶ %s\033[0m\n' "$*"; }
die() { printf '\n\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

docker --context "$CONTEXT" info >/dev/null 2>&1 \
  || die "docker context '$CONTEXT' is not reachable. Start Docker Desktop (open -ga Docker), or set COSMON_DOCKER_CONTEXT."

say "engine fidelity check (context=$CONTEXT)"
docker --context "$CONTEXT" info \
  --format 'server={{.ServerVersion}} os={{.OperatingSystem}} arch={{.Architecture}} kernel={{.KernelVersion}}'

cleanup() {
  if [ "${COSMON_KEEP_IMAGE:-0}" != "1" ]; then
    say "removing the bench image (only this tag — nothing else)"
    docker --context "$CONTEXT" image rm -f "$IMAGE" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

say "building $IMAGE from $REPO_ROOT (cs comes from THIS worktree, not the v0.3.0 tag)"
docker --context "$CONTEXT" build \
  -f "$REPO_ROOT/docker/container-worker-doors/Dockerfile" \
  -t "$IMAGE" \
  "$REPO_ROOT"

say "running the three arms"
# --init reaps the tmux/claude children; the bench needs a real pid 1.
docker --context "$CONTEXT" run --rm --init "$IMAGE"
