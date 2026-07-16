#!/usr/bin/env bash
# local-default-container-test.sh — host-side driver for the EPHEMERAL
# CONTAINERIZED functional test of cosmon's local-default autonomy loop
# (task-20260601-989e, parent delib-20260530-0877).
#
# It builds an image that contains `cs` but NO `claude` binary, then runs
# a throwaway container that drives a bare `cs tackle` (no --adapter flag)
# to completion against the HOST's Ollama. The container's absence of
# claude makes the local-default flip true BY CONSTRUCTION — see
# docker/local-default-container/Dockerfile and
# docs/guides/local-default-container-test.md.
#
# ISOLATION GUARANTEE: nothing here touches the live cosmon fleet, the
# resident runtime, or the host's .cosmon/state. The galaxy is created
# inside the container's own HOME (`docker run --rm`), on a separate
# filesystem namespace, and is destroyed with the container.
#
# CI discipline: when the host Ollama is unreachable this script SKIPS
# with a clear log and exit 0 (same as scripts/local-default-smoke.sh) so
# it never fails CI for infra-absence — but runs the full assertion suite
# (and goes red on a real regression) when the host model is up.
#
# Usage:
#   ollama serve &                       # host-side, with qwen3:8b pulled
#   scripts/local-default-container-test.sh
#
# Environment overrides:
#   COSMON_LOCAL_MODEL     model to drive (default qwen3:8b; do NOT use
#                          qwen2.5-coder:7b — it emits tool calls as text)
#   COSMON_HOST_OLLAMA     host-visible Ollama URL for the probe
#                          (default http://localhost:11434)
#   COSMON_CONTAINER_OLLAMA  container-visible Ollama URL
#                          (default http://host.docker.internal:11434)
#   COSMON_DOCKER_CONTEXT  docker context to use (default: current context)
#   COSMON_KEEP_IMAGE=1    skip the image rmi at teardown (faster reruns)
set -euo pipefail

MODEL="${COSMON_LOCAL_MODEL:-qwen3:8b}"
HOST_OLLAMA="${COSMON_HOST_OLLAMA:-http://localhost:11434}"
CONTAINER_OLLAMA="${COSMON_CONTAINER_OLLAMA:-http://host.docker.internal:11434}"
IMAGE_TAG="cosmon-local-default-test:ephemeral"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

say()  { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
skip() { printf '\033[1;33m⤼ SKIP: %s\033[0m\n' "$*"; exit 0; }
die()  { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

DOCKER="docker"
if [ -n "${COSMON_DOCKER_CONTEXT:-}" ]; then
  DOCKER="docker --context $COSMON_DOCKER_CONTEXT"
fi

# 0. Skip-with-clear-log gates (infra absence is NOT a test failure) --
command -v docker >/dev/null 2>&1 || skip "docker not found on PATH"
if ! $DOCKER info >/dev/null 2>&1; then
  skip "docker daemon not reachable (context: $($DOCKER context show 2>/dev/null || echo '?')) — start colima/docker"
fi
say "Probing host Ollama at $HOST_OLLAMA ..."
if ! curl -sf -m 3 "$HOST_OLLAMA/api/tags" >/dev/null 2>&1; then
  skip "host Ollama not reachable at $HOST_OLLAMA — run \`ollama serve\` (and \`ollama pull $MODEL\`)"
fi
if ! curl -sf -m 3 "$HOST_OLLAMA/api/tags" 2>/dev/null | jq -e --arg m "$MODEL" \
     '.models[]?.name | select(. == $m or . == ($m + ":latest"))' >/dev/null 2>&1; then
  skip "model '$MODEL' not pulled on host Ollama — run \`ollama pull $MODEL\`"
fi
ok "host Ollama reachable; model '$MODEL' present"

# 1. Build the no-claude image ---------------------------------------
# Build context = repo root (the .dockerignore there excludes target/,
# .git/, .worktrees/). The cs binary is built inside the builder stage,
# so the host toolchain is irrelevant.
say "Building image $IMAGE_TAG (cs + git + tmux, NO claude) ..."
$DOCKER build \
  -f "$REPO_ROOT/docker/local-default-container/Dockerfile" \
  -t "$IMAGE_TAG" \
  "$REPO_ROOT"
ok "image built"

# Teardown: remove the container is automatic (--rm); optionally drop the
# image too so the test leaves NOTHING behind (ephemeral discipline).
cleanup() {
  if [ -z "${COSMON_KEEP_IMAGE:-}" ]; then
    $DOCKER rmi -f "$IMAGE_TAG" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

# 2. Run the ephemeral container -------------------------------------
# --rm           : container + its writable layer destroyed on exit.
# --add-host     : colima/Lima needs the explicit host-gateway mapping
#                  for host.docker.internal to resolve to the Mac host.
# --network-alias none; no volume mounts → no path can resolve to the
#                  host's .cosmon/state. The galaxy lives and dies inside.
say "Running ephemeral container (--rm, no volumes) ..."
$DOCKER run --rm \
  --add-host=host.docker.internal:host-gateway \
  -e COSMON_LOCAL_BASE_URL="$CONTAINER_OLLAMA" \
  -e COSMON_LOCAL_MODEL="$MODEL" \
  "$IMAGE_TAG"

ok "container exited 0 — all assertions passed"
printf '\n\033[1;32m═══ LOCAL-DEFAULT CONTAINER TEST GREEN ═══\033[0m\n'
printf 'The local default activated by construction: a container with no\n'
printf 'claude on PATH drove `cs tackle` to done via the host Ollama, with\n'
printf 'cosmon owning the loop. Ephemeral teardown leaves no container'
[ -z "${COSMON_KEEP_IMAGE:-}" ] && printf ', volume, or image' || printf ' or volume'
printf ' behind.\n'
