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
# ENGINE (corrected 2026-07-27 — read this before overriding it)
# ──────────────────────────────────────────────────────────────
# This bench runs on a dedicated **colima** profile, `cosmon-bench`. Until
# 2026-07-27 it pinned `desktop-linux` and this header asserted that Docker
# Desktop was the tester's engine and that colima was "NOT faithful". The
# tester corrected his own earlier description that day, unprompted, on issue
# #20: his bed is Colima (Lima-based), Ubuntu 24.04.4 LTS, aarch64.
#
# The old claim's factual half — a colima VM runs a different kernel with a
# different user-namespace posture — is TRUE and now measured. Its conclusion
# was inverted. On this machine, under the default seccomp profile:
#
#   colima-cosmon-bench  kernel 6.8.0-100-generic (Ubuntu 24.04.4 LTS)
#                        unshare as uid 10001 -> BLOCKED; virtiofs chown -> SILENTLY IGNORED
#   desktop-linux        kernel 6.10.11-linuxkit (Docker Desktop 27.3.1)
#                        unshare as uid 10001 -> OK;      bind-mount chown -> HONOURED
#
# So Docker Desktop reproduces NEITHER of the tester's two standing findings.
# It was not merely mislabelled; it was the engine that cannot see them. The
# full capture, including the seccomp attribution pass, is in
# `docs/benches/engine-fidelity-2026-07-27.md`, produced by
# `scripts/container-engine-posture.sh`. Re-measure before changing the
# default again — do not swap one unverified fidelity claim for another.
#
# If the bench engine is not running, this script REFUSES with the command to
# start it and exits 2 = INCONCLUSIVE. It never falls back to another context.
#
# Environment overrides:
#   COSMON_BENCH_COLIMA_PROFILE  colima profile (default: cosmon-bench, which
#                                belongs to the benches and to nothing else)
#   COSMON_DOCKER_CONTEXT        explicit context override; still must be
#                                reachable, still no fallback
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

IMAGE="cosmon-container-worker-doors:bench"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# shellcheck source=lib/bench-engine.sh
. "$REPO_ROOT/scripts/lib/bench-engine.sh"
CONTEXT="$(bench_engine_context)"

say() { printf '\n\033[1;34m▶ %s\033[0m\n' "$*"; }
die() { printf '\n\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# Refuses with the start command and exits 2 (INCONCLUSIVE) if the bench
# engine is down. "The discriminating engine was unavailable" is never a pass.
bench_engine_require "$CONTEXT"

say "engine fidelity check (context=$CONTEXT)"
bench_engine_fingerprint "$CONTEXT"

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
