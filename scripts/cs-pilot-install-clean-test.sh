#!/usr/bin/env bash
# cs-pilot-install-clean-test.sh — host-side driver for the install-clean
# smoke (cs-pilot increment 2, TEST A — task-20260601-070b).
#
# Proves a FRESHLY-BUILT `cs` boots `cs pilot --experimental` on a VIRGIN
# box — no ~/.cosmon, no ~/.config/cosmon sediment — over a tiny .cosmon
# fixture, driving the HOST's Ollama. Catches hidden host-state
# dependencies the v0 host smoke (crates/cosmon-pilot/SMOKE.md) could not:
# that smoke ran on the operator's thick machine, where "cs boots from
# nothing" is indistinguishable from "cs boots because a host file it
# silently needs already exists".
#
# THE VIRGINITY IS A THROWAWAY COLIMA VM. The container HOME is already
# clean, but running it on a dedicated ephemeral colima profile
# (cs-pilot-test) extends the virginity below the HOME to the docker
# daemon, image cache, and kernel — the fullest "virgin box" the muscle
# allows. The VM is created, used, and DELETED here: ephemeral, run, tear
# down (no-daemon invariant; no persistent test VM left behind).
#
# CI discipline: SKIPS with a clear log and exit 0 when colima/docker or
# the host Ollama is unreachable, so it never fails CI for infra-absence.
#
# ⚠ This script creates and deletes a colima VM profile (cs-pilot-test). It
# refuses to touch the profile if it already exists, so it can never
# clobber an operator's VM. Spinning a fresh VM + a release cargo build
# takes several minutes — this is an operator-scheduled spot-check, not a
# fast inner-loop gate.
#
# Usage:
#   ollama serve &                       # host-side, with the model pulled
#   scripts/cs-pilot-install-clean-test.sh
#
# Environment overrides:
#   COSMON_PILOT_MODEL     model to drive (default qwen3:8b)
#   COSMON_HOST_OLLAMA     host-visible Ollama URL for the probe
#                          (default http://localhost:11434)
#   COSMON_VM_PROFILE      colima profile name (default cs-pilot-test)
#   COSMON_VM_CPU          colima vCPUs   (default 4)
#   COSMON_VM_MEMORY       colima RAM GiB (default 6)
#   COSMON_VM_DISK         colima disk GiB(default 30)
#   COSMON_REUSE_VM=1      build/run on the CURRENT docker context instead
#                          of spinning a throwaway VM (faster; drops the
#                          below-the-HOME virginity — container HOME only)
set -euo pipefail

MODEL="${COSMON_PILOT_MODEL:-qwen3:8b}"
HOST_OLLAMA="${COSMON_HOST_OLLAMA:-http://localhost:11434}"
CONTAINER_OLLAMA="http://host.docker.internal:11434/v1"
VM_PROFILE="${COSMON_VM_PROFILE:-cs-pilot-test}"
IMAGE_TAG="cosmon-cs-pilot-install-clean:ephemeral"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

say()  { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
skip() { printf '\033[1;33m⤼ SKIP: %s\033[0m\n' "$*"; exit 0; }
die()  { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Skip-with-clear-log gates (probe BEFORE the expensive VM spin) ---
command -v docker >/dev/null 2>&1 || skip "docker not found on PATH"
say "Probing host Ollama at $HOST_OLLAMA ..."
if ! curl -sf -m 3 "$HOST_OLLAMA/api/tags" >/dev/null 2>&1; then
  skip "host Ollama not reachable at $HOST_OLLAMA — run \`ollama serve\` (and \`ollama pull $MODEL\`)"
fi
if ! curl -sf -m 3 "$HOST_OLLAMA/api/tags" 2>/dev/null | jq -e --arg m "$MODEL" \
     '.models[]?.name | select(. == $m or . == ($m + ":latest"))' >/dev/null 2>&1; then
  skip "model '$MODEL' not pulled on host Ollama — run \`ollama pull $MODEL\`"
fi
ok "host Ollama reachable; model '$MODEL' present"

# 1. Provision the throwaway VM (or reuse the current context) -------
DOCKER="docker"
VM_STARTED=""
if [ -n "${COSMON_REUSE_VM:-}" ]; then
  say "COSMON_REUSE_VM set — using the current docker context (container-HOME virginity only)"
  $DOCKER info >/dev/null 2>&1 || skip "docker daemon not reachable — start colima"
else
  command -v colima >/dev/null 2>&1 || skip "colima not found — install it or set COSMON_REUSE_VM=1"
  # Never clobber an existing profile.
  if colima list 2>/dev/null | awk 'NR>1 {print $1}' | grep -qx "$VM_PROFILE"; then
    die "colima profile '$VM_PROFILE' already exists — delete it (\`colima delete $VM_PROFILE\`) or set COSMON_VM_PROFILE"
  fi
  say "Starting throwaway colima VM '$VM_PROFILE' (${COSMON_VM_CPU:-4} cpu / ${COSMON_VM_MEMORY:-6}G / ${COSMON_VM_DISK:-30}G) ..."
  colima start --profile "$VM_PROFILE" \
    --cpu "${COSMON_VM_CPU:-4}" \
    --memory "${COSMON_VM_MEMORY:-6}" \
    --disk "${COSMON_VM_DISK:-30}" \
    || die "colima start failed for profile '$VM_PROFILE'"
  VM_STARTED="$VM_PROFILE"
  DOCKER="docker --context colima-$VM_PROFILE"
  ok "throwaway VM up; docker context = colima-$VM_PROFILE"
fi

cleanup() {
  local rc=$?
  if [ -z "${COSMON_KEEP_IMAGE:-}" ]; then
    $DOCKER rmi "$IMAGE_TAG" >/dev/null 2>&1 || true
  fi
  if [ -n "$VM_STARTED" ]; then
    say "Tearing down throwaway colima VM '$VM_STARTED' ..."
    colima delete --force "$VM_STARTED" >/dev/null 2>&1 \
      || printf '\033[1;33m⚠ could not delete colima VM %s — run `colima delete %s` manually\033[0m\n' "$VM_STARTED" "$VM_STARTED" >&2
  fi
  exit $rc
}
trap cleanup EXIT

# 2. Build cs fresh inside the (virgin) VM ---------------------------
# Build context = repo root (.dockerignore excludes target/, .git/,
# .worktrees/). The cs binary is built inside the builder stage on the VM,
# so "freshly-built on a virgin box" is literal.
say "Building $IMAGE_TAG (cargo build of cs, from source) ..."
$DOCKER build \
  -f "$REPO_ROOT/docker/cs-pilot-install-clean/Dockerfile" \
  -t "$IMAGE_TAG" \
  "$REPO_ROOT"
ok "image built (cs compiled fresh)"

# 3. Run the ephemeral container -------------------------------------
# --add-host: colima/Lima needs the explicit host-gateway mapping for
# host.docker.internal. --rm + no volumes: nothing resolves to host state.
say "Running ephemeral container (--rm, no volumes) ..."
$DOCKER run --rm \
  --add-host=host.docker.internal:host-gateway \
  -e COSMON_PILOT_BASE_URL="$CONTAINER_OLLAMA" \
  -e COSMON_PILOT_MODEL="$MODEL" \
  "$IMAGE_TAG"

ok "container exited 0 — cs pilot booted on a virgin box"
printf '\n\033[1;32m═══ CS-PILOT INSTALL-CLEAN TEST GREEN ═══\033[0m\n'
printf 'A freshly-built cs booted `cs pilot --experimental` from NOTHING —\n'
printf 'no ~/.cosmon, no ~/.config/cosmon — drove the host Ollama over a tiny\n'
printf 'fixture, and exited clean. No hidden host-state dependency. Ephemeral\n'
printf 'teardown leaves no container, image, or VM behind.\n'
