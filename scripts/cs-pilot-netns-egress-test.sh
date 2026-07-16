#!/usr/bin/env bash
# cs-pilot-netns-egress-test.sh — host-side driver for the netns
# egress-guard test (cs-pilot increment 2, TEST B — task-20260601-070b,
# parent shelf items of the cs-pilot build; closes the verification gap
# from task-20260530-d8bc).
#
# THE ONE TEST THAT REQUIRES LINUX. On macOS the autonomy egress guard
# (cosmon-core::egress, COSMON_EGRESS_POLICY) is only ADVISORY — it cannot
# actually block network. The real deny-external behaviour (a strict-local
# worker physically unable to reach a remote API) only exercises inside a
# netns-capable Linux kernel. colima provides that kernel.
#
# This driver builds an ephemeral Linux image that compiles the workspace's
# `exec_command_netns_e2e` test, runs a throwaway container that exercises
# the REAL ExecCommand → EgressJail → `unshare --net` path with the
# COSMON_NETNS_E2E gate set, then tears the container (and optionally the
# image) down. No persistent test daemon — ephemeral, run, tear down
# (no-daemon invariant).
#
# CI discipline: SKIPS with a clear log and exit 0 when docker is
# unreachable, so it never fails CI for infra-absence — but runs the full
# assertion suite (and goes red on a real regression) when colima is up.
#
# Usage:
#   colima start                 # or any docker context with a Linux kernel
#   scripts/cs-pilot-netns-egress-test.sh
#
# Environment overrides:
#   COSMON_DOCKER_CONTEXT  docker context to use (default: current context)
#   COSMON_KEEP_IMAGE=1    skip the image rmi at teardown (faster reruns)
set -euo pipefail

IMAGE_TAG="cosmon-cs-pilot-netns:ephemeral"

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
  skip "docker daemon not reachable (context: $($DOCKER context show 2>/dev/null || echo '?')) — start colima"
fi
# The docker daemon must run a Linux kernel (it always does under colima;
# guard anyway so a Docker-Desktop-on-macOS-with-a-weird-backend still
# reports clearly rather than failing deep in the test).
KERNEL_OS="$($DOCKER info --format '{{.OSType}}' 2>/dev/null || echo '?')"
[ "$KERNEL_OS" = "linux" ] || skip "docker daemon kernel is '$KERNEL_OS', not linux — netns test needs a Linux kernel"
ok "docker reachable; Linux kernel (netns enforcement available)"

# 1. Build the test image --------------------------------------------
# Build context = repo root (.dockerignore excludes target/, .git/,
# .worktrees/). cs + the netns test are compiled inside the builder, so
# the host toolchain is irrelevant.
say "Building $IMAGE_TAG (compiles exec_command_netns_e2e) ..."
$DOCKER build \
  -f "$REPO_ROOT/docker/cs-pilot-netns/Dockerfile" \
  -t "$IMAGE_TAG" \
  "$REPO_ROOT"
ok "image built (test pre-compiled)"

cleanup() {
  if [ -z "${COSMON_KEEP_IMAGE:-}" ]; then
    say "Removing image $IMAGE_TAG ..."
    $DOCKER rmi "$IMAGE_TAG" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

# 2. Run the ephemeral container -------------------------------------
# --rm destroys the container + its writable layer on exit. No volumes,
# so nothing can resolve to the host's .cosmon/state. The container needs
# normal egress so the BASELINE probe (AllowAll) reaches 1.1.1.1:443 —
# the netns is created *inside* the container by the test, per-command.
say "Running ephemeral container (--rm, no volumes) ..."
$DOCKER run --rm "$IMAGE_TAG"

ok "container exited 0 — netns egress guard enforced"
printf '\n\033[1;32m═══ CS-PILOT NETNS EGRESS TEST GREEN ═══\033[0m\n'
printf 'deny-external is a REFUSED SYSCALL on a Linux kernel, not a label:\n'
printf 'the strict-local worker physically cannot reach a remote API, while\n'
printf 'local commands still run. The macOS Advisory-only gap is closed.\n'
