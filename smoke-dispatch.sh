#!/usr/bin/env bash
# smoke-dispatch.sh (worktree ROOT) — prove ONE real dispatch through the
# spore-e2e producer's production path and record the produced artifacts
# beneath $MOLECULE_DIR/dispatch-output/ (producer-work, task-20260720-5537).
#
# THE PRODUCER
#   An end-to-end validation harness that germinates the `math-attack` v2 spore
#   on a TRIVIAL conjecture inside a Linux container running the FIXED cosmon,
#   and drives the whole 14-node polymer to a terminal verdict on the sovereign
#   LOCAL (Ollama) adapter — no Claude, no cloud, no auth. See
#   docker/spore-e2e/{Dockerfile,germinate-and-drive.sh,build.sh}.
#
# THE PRODUCTION DISPATCH PATH (what this script runs)
#   `docker run` of the pre-built spore-e2e image. Inside: cs init; unzip the
#   spore; `cs spore validate`; `cs spore run` (germinate, TLC-sealed or honest
#   unchecked); `cs run` (drive the DAG to drain via the local adapter); assert
#   the LLM firewall (no PROVED verdict with backend=none); emit verdict.json.
#   Nothing is fabricated or doubled — every record is emitted by a real run of
#   the real binary against a real local model.
#
# NOT a preflight check: it drives the actual germination + DAG walk and copies
# the produced verdict + transcript + per-node artifacts into the molecule's
# durable dispatch-output directory, then asserts a real terminal verdict landed.
#
# The heavy `cargo build --release` lives in the image build (run once, in
# step-1 implementation, via docker/spore-e2e/build.sh) — NOT here — so this
# gate stays inside its 600s budget: it only runs the pre-built container.
# Fail closed if the image, docker, or the local provider is unavailable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

IMAGE="${SPORE_E2E_IMAGE:-cosmon-spore-e2e:latest}"

# Resolve the molecule directory: the fleet injects $MOLECULE_DIR; fall back to
# this molecule's canonical resolved path.
MOLECULE_DIR="${MOLECULE_DIR:-/Users/eserie/galaxies/cosmon/.cosmon/state/fleets/default/molecules/task-20260720-5537}"
DISPATCH_OUT="$MOLECULE_DIR/dispatch-output"
rm -rf "$DISPATCH_OUT"; mkdir -p "$DISPATCH_OUT"

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m[smoke][fatal] %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Preconditions — fail closed on any missing runner. ------------------
command -v docker >/dev/null 2>&1 || die "docker not on PATH — the production path's runner is unavailable"
docker image inspect "$IMAGE" >/dev/null 2>&1 \
  || die "image '$IMAGE' not built — run docker/spore-e2e/build.sh first (heavy build is out-of-gate by design)"

# The container reaches the HOST's Ollama; the local provider must be up.
BASE_URL="${COSMON_LOCAL_BASE_URL:-http://localhost:11434}"
MODEL="${COSMON_LOCAL_MODEL:-qwen2.5:0.5b}"
curl -sf -m 5 "$BASE_URL/api/tags" >/dev/null 2>&1 \
  || die "host Ollama not reachable at $BASE_URL — the local provider is down (fail closed)"

# 1. Drive the real production path inside the container. ----------------
CONTAINER="spore-e2e-smoke-$$"
say "docker run $IMAGE (spore-e2e germination + local-adapter drive) ..."
set +e
docker run --name "$CONTAINER" \
  --add-host host.docker.internal:host-gateway \
  -e COSMON_LOCAL_BASE_URL="http://host.docker.internal:11434" \
  -e COSMON_LOCAL_MODEL="$MODEL" \
  -e SPORE_SUBJECT="trivial-zero" \
  -e SPORE_PROBLEM="0 = 0, to be PROVEN or REFUTED, not assumed" \
  -e SPORE_BACKEND="none" \
  -e OUT_DIR="/out" \
  "$IMAGE"
RUN_RC=$?
set -e

# 2. Extract the produced records (docker cp is robust to uid mapping). --
say "copying produced records out of the container ..."
docker cp "$CONTAINER:/out/." "$DISPATCH_OUT/" 2>/dev/null || true
docker logs "$CONTAINER" > "$DISPATCH_OUT/container.log" 2>&1 || true
docker rm -f "$CONTAINER" >/dev/null 2>&1 || true

[ "$RUN_RC" -eq 0 ] || die "container run exited $RUN_RC — the production dispatch failed (see container.log)"

# 3. Assert a real terminal verdict landed + the firewall was honored. ---
VERDICT="$DISPATCH_OUT/verdict.json"
[ -f "$VERDICT" ] || die "no verdict.json produced under $DISPATCH_OUT"

GERM="$(jq -r '.germinated_nodes // 0' "$VERDICT")"
TERM="$(jq -r '.terminal_nodes // 0' "$VERDICT")"
FW="$(jq -r '.llm_firewall_honored // false' "$VERDICT")"
LEAN="$(jq -r '.lean_leg // "?"' "$VERDICT")"
BACKEND="$(jq -r '.formal_backend // "?"' "$VERDICT")"
VERD="$(jq -r '.verdict // "?"' "$VERDICT")"

[ "$GERM" -ge 1 ] || die "verdict reports no germinated nodes"
[ "$TERM" -ge 1 ] || die "verdict reports no node reached a terminal state"
[ "$FW" = "true" ] || die "verdict reports the LLM firewall was NOT honored"
# Firewall coherence: backend=none MUST leave the Lean leg SKIPPED (no kernel).
if [ "$BACKEND" = "none" ] && [ "$LEAN" != "SKIPPED" ]; then
  die "incoherent: backend=none but lean_leg='$LEAN' (a kernel leg where none can exist)"
fi

say "OK: germinated=$GERM terminal=$TERM firewall_honored=$FW backend=$BACKEND lean=$LEAN verdict=$VERD"
say "records written under: $DISPATCH_OUT"
ls -1 "$DISPATCH_OUT"
printf '%s\n' "$VERDICT"
