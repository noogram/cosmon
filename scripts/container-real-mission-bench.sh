#!/usr/bin/env bash
# container-real-mission-bench.sh — host-side driver for the missing arm of
# the container bench: drive ONE REAL mission through cosmon's production
# dispatch path, inside the external tester's published environment, and
# stop at the credential gate.
#
# WHY THIS EXISTS
# ───────────────
# scripts/container-worker-doors-bench.sh proves the four doors of issue
# #20 open. Every one of its arms stops in front of a file literally named
# PLACEHOLDER-NOT-A-CREDENTIAL, because the credential gate only stat()s
# that file and never opens it. bench/README.md already admits in writing
# that where a probe needs a real authed Claude Code it "degrades to
# asserting the argv/spawn signature and marks that portion INCONCLUSIVE".
#
# So the doors are proven to OPEN, and nothing has ever been proven to walk
# down the corridor. This driver is that walk — as far as a machine may go.
# It builds the tester's image, installs `cs` from THIS tree, nucleates and
# tackles a real molecule through the real `cs tackle --adapter claude`
# path, and halts at door 3 with the gate's own reason quoted verbatim.
#
# A refusal is the EXPECTED, MEASURED outcome for THIS driver, which never
# provisions a credential — a real result, not a failure.
#
# The in-container harness itself grades against whichever world it finds:
# empty-handed it expects the refusal; with a credential present (because a
# human logged in by hand, see below) it expects a spawned, live worker and
# asserts that positively. This driver reports whichever verdict the
# harness reached rather than assuming there is only one.
#
# Usage:
#   scripts/container-real-mission-bench.sh
#   COSMON_KEEP_IMAGE=1 scripts/container-real-mission-bench.sh   # keep image
#                                                                 # so you can
#                                                                 # log in after
#
# Environment overrides:
#   COSMON_DOCKER_CONTEXT   docker context (default: desktop-linux, the
#                           tester's engine — Docker Desktop on macOS
#                           arm64, LinuxKit VM. A colima context runs an
#                           Ubuntu kernel with a DIFFERENT user-namespace
#                           posture and is NOT faithful.)
#   COSMON_KEEP_IMAGE=1     skip the image rmi at teardown
#   MISSION_TOPIC           the payload's topic (target-agnostic by design)
#   MISSION_OUT_DIR         where to copy the container's records
#
# SECRETS: none, ever. Unlike the doors bench, this harness does not even
# mint a placeholder — arriving empty-handed is the measurement. No
# credential is read from the host, mounted, passed as a build arg, put in
# an environment variable, or written to a log. If you find yourself
# needing one to make this script pass, stop: that step is the human's,
# and the script prints the exact command for it.
#
# ISOLATION: `docker run --rm` with no bind mount of any host state. The
# galaxy is created inside the container and dies with it. Nothing here
# touches the live fleet, the resident runtime, or .cosmon/state. The
# image is tagged and removed by name, so no pre-existing image or
# container of yours is ever a candidate for deletion.
#
# Exit status:
#   0  the expected outcome for the world the harness was in was observed
#   1  a finding: it was not
#   2  INCONCLUSIVE — the discriminating step could not run here. Never a
#      silent pass in either world; the reason is printed. (bench/README.md
#      verdict semantics.)
set -uo pipefail

CONTEXT="${COSMON_DOCKER_CONTEXT:-desktop-linux}"
IMAGE="cosmon-container-real-mission:bench"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${MISSION_OUT_DIR:-$REPO_ROOT/bench/out/real-mission}"
CONTAINER="cosmon-real-mission-$$"

say()  { printf '\n\033[1;34m▶ %s\033[0m\n' "$*"; }
die()  { printf '\n\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }
# INCONCLUSIVE is its own exit, distinct from a failure: the harness could
# not run the step that discriminates. Reporting it as green would be the
# one dishonesty this whole molecule exists to prevent.
incon() { printf '\n\033[1;33mVERDICT INCONCLUSIVE — %s\033[0m\n' "$*" >&2; exit 2; }

command -v docker >/dev/null 2>&1 \
  || incon "docker is not on PATH; the container mission cannot be driven here"
docker --context "$CONTEXT" info >/dev/null 2>&1 \
  || incon "docker context '$CONTEXT' is not reachable (start Docker Desktop with \`open -ga Docker\`, or set COSMON_DOCKER_CONTEXT)"

say "engine fidelity check (context=$CONTEXT)"
docker --context "$CONTEXT" info \
  --format 'server={{.ServerVersion}} os={{.OperatingSystem}} arch={{.Architecture}} kernel={{.KernelVersion}}'

cleanup() {
  docker --context "$CONTEXT" rm -f "$CONTAINER" >/dev/null 2>&1 || true
  if [ "${COSMON_KEEP_IMAGE:-0}" != "1" ]; then
    say "removing the bench image (only this tag — nothing else)"
    docker --context "$CONTEXT" image rm -f "$IMAGE" >/dev/null 2>&1 || true
  else
    say "keeping $IMAGE (COSMON_KEEP_IMAGE=1) so you can log in to it yourself"
  fi
}
trap cleanup EXIT

say "building $IMAGE from $REPO_ROOT (cs comes from THIS worktree, not the v0.3.0 tag)"
docker --context "$CONTEXT" build \
  -f "$REPO_ROOT/docker/container-real-mission/Dockerfile" \
  -t "$IMAGE" \
  "$REPO_ROOT" \
  || incon "the image build failed; the mission was never dispatched (see the build output above)"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

say "driving the real mission (docker run --rm-equivalent, no host state mounted)"
# --init reaps the tmux/claude children; the dispatch needs a real pid 1.
# The container is named rather than `--rm`'d inline so its records can be
# `docker cp`'d out (robust to uid mapping) before it is removed in cleanup.
docker --context "$CONTEXT" run --name "$CONTAINER" --init \
  -e IMAGE_REF="$IMAGE" \
  -e COSMON_DOCKER_CONTEXT="$CONTEXT" \
  ${MISSION_TOPIC:+-e MISSION_TOPIC="$MISSION_TOPIC"} \
  "$IMAGE" 2>&1 | tee "$OUT_DIR/transcript.log"
# Never read the exit status through the pipe: `tee` is what $? would name.
RC="${PIPESTATUS[0]}"

say "copying the produced records out of the container"
docker --context "$CONTEXT" cp "$CONTAINER:/out/." "$OUT_DIR/" >/dev/null 2>&1 || true

RECORD="$OUT_DIR/mission-record.json"
if [ ! -f "$RECORD" ]; then
  incon "the container produced no mission-record.json (rc=$RC); see $OUT_DIR/transcript.log"
fi

say "records written under: $OUT_DIR"
ls -1 "$OUT_DIR"

# Report the harness's OWN verdict rather than a verdict this driver
# assumes. The in-container grader knows which world it was in; this one
# does not, and printing "REFUSED-AT-CREDENTIAL-GATE" over a run that
# actually spawned a worker was one of the three defects this harness
# shipped with. `jq` is not required on the host, so the field is read with
# sed from jq's own pretty-printed output.
VERDICT="$(sed -n 's/^ *"verdict": *"\([^"]*\)".*/\1/p' "$RECORD" | head -n1)"
WORLD="$(sed -n 's/^ *"world": *"\([^"]*\)".*/\1/p' "$RECORD" | head -n1)"

case "$RC" in
  0) printf '\n\033[1;32mVERDICT %s (world: %s) — the expected outcome for this world was observed.\033[0m\n' \
       "${VERDICT:-?}" "${WORLD:-?}" ;;
  2) incon "${VERDICT:-INCONCLUSIVE}: the discriminating step could not run; see $RECORD and $OUT_DIR/transcript.log" ;;
  *) die "VERDICT ${VERDICT:-?} (world: ${WORLD:-?}) — not the expected outcome for this world (rc=$RC); read $RECORD and $OUT_DIR/transcript.log" ;;
esac
exit 0
