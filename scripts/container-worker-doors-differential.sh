#!/usr/bin/env bash
# container-worker-doors-differential.sh — runs the FINAL issue-#20 bench,
# unchanged, against TWO different builds of `cs`, and nothing else.
#
# WHY THIS EXISTS
# ───────────────
# The door-4 fix (73c4b2a) turned arm C from red to green on the container
# bench. But the bench itself was repaired in the same commit, on two
# observation points that the new fail-closed behaviour had removed or
# moved. So "same harness, byte for byte, red then green" was not true of
# the development history, and a reader who only has the file cannot tell a
# real fix from a loosened instrument.
#
# This driver makes that claim true after the fact. It takes the harness in
# its FINAL state — one file, one hash, printed below — and runs it twice,
# changing exactly ONE thing between the two runs: which commit `cs` is
# built from.
#
#   (a) 4c41738  the parent of the fix   → arm C must be RED
#   (b) 73c4b2a  the fix                 → arm C must be GREEN
#
# If the final harness still finds the defect on the parent, its repairs did
# not blunt it. If it went green on the parent, the repairs blinded it and
# the whole file has to be reopened. Both outcomes are informative. Nothing
# here is tuned to produce the first.
#
# THE ONE THING THIS DRIVER ADDS, AND WHY IT IS OUTSIDE THE HARNESS
# ─────────────────────────────────────────────────────────────────
# The bench's own provenance block greps three fix-only strings out of the
# shipped binary. All three are ALREADY present at 4c41738 — they came from
# fixes 1-3, which landed before the parent. They therefore cannot tell the
# two builds under test apart. The discriminant for door 4 specifically is
# `COSMON_READINESS_TRACE`, the env var of `cosmon_transport::readiness_trace`,
# a module that exists only at 73c4b2a.
#
# That extra grep is run HERE, against the image, and NOT added to the bench
# script — because adding it would change the harness and destroy the very
# property this driver exists to establish. The harness stays frozen; the
# driver does the extra reading.
#
# HOW THE "SAME HARNESS" CLAIM IS ENFORCED, NOT JUST ASSERTED
# ────────────────────────────────────────────────────────────
# The final harness lives on a branch NEWER than the parent commit, so a
# plain checkout of the parent silently restores the OLD harness — and the
# comparison would then vary two things while believing it varied one. That
# failure is invisible after the fact, so it is checked twice, ahead of each
# pass, and a mismatch is fatal rather than repaired:
#
#   1. after the copy into the build context, the file's SHA-256 must equal
#      the reference harness's;
#   2. after the build, the SHA-256 is read back OUT OF THE IMAGE, from
#      /usr/local/bin/container-worker-doors-bench — because what a `COPY`
#      line, a stale layer or a `.dockerignore` actually delivered is not
#      the same fact as what sat in the context directory.
#
# Both values are printed for both passes and belong in the report. An
# invalid pass is re-run, never patched up. Likewise an unexpected verdict
# is a FINDING to report — never a reason to adjust the bench.
#
# Usage:
#   scripts/container-worker-doors-differential.sh
#
# Environment overrides:
#   COSMON_DOCKER_CONTEXT   docker context (default: desktop-linux, the
#                           reporter's engine)
#   COSMON_DIFF_OUT         directory for the raw logs (default: a temp dir,
#                           printed at the end)
#   COSMON_KEEP_IMAGE=1     skip the image rmi at teardown
#
# SECRETS: none, ever — the same discipline as the bench it drives. The only
# credential material anywhere in this pipeline is the literal string
# `PLACEHOLDER-NOT-A-CREDENTIAL-…` minted inside the container.
set -euo pipefail

CONTEXT="${COSMON_DOCKER_CONTEXT:-desktop-linux}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${COSMON_DIFF_OUT:-$(mktemp -d "${TMPDIR:-/tmp}/cwd-differential.XXXXXX")}"
mkdir -p "$OUT"
# Build contexts live OUTSIDE the artefact directory: a git worktree is not
# an artefact, and a report directory that also holds two checkouts of the
# repository is a trap for whoever archives it.
TREES="$(mktemp -d "${TMPDIR:-/tmp}/cwd-diff-trees.XXXXXX")"

# The two heads. `PARENT` must NOT carry the door-4 fix; `FIXED` must.
PARENT_SHA="${COSMON_DIFF_PARENT:-4c41738}"
FIXED_SHA="${COSMON_DIFF_FIXED:-73c4b2a}"

# The harness, in its final state. Both runs get THESE bytes.
HARNESS_SRC="$REPO_ROOT/docker/container-worker-doors/in-container-bench.sh"
DOCKERFILE_SRC="$REPO_ROOT/docker/container-worker-doors/Dockerfile"

say() { printf '\n\033[1;34m▶ %s\033[0m\n' "$*"; }
die() { printf '\n\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

sha() { shasum -a 256 "$1" | awk '{print $1}'; }

docker --context "$CONTEXT" info >/dev/null 2>&1 \
  || die "docker context '$CONTEXT' is not reachable. Start Docker Desktop (open -ga Docker), or set COSMON_DOCKER_CONTEXT."

# ── The pinned environment, recorded ONCE and shared by both passes ──────
ENV_FILE="$OUT/environment.txt"
{
  echo "=== differential run environment (identical for both passes) ==="
  echo "date_utc            $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "host_uname          $(uname -a)"
  echo "docker_context      $CONTEXT"
  echo "docker_client       $(docker --context "$CONTEXT" version --format '{{.Client.Version}}')"
  docker --context "$CONTEXT" info \
    --format 'docker_server      {{.ServerVersion}}
engine_os           {{.OperatingSystem}}
engine_arch         {{.Architecture}}
engine_kernel       {{.KernelVersion}}'
  echo "harness_path        docker/container-worker-doors/in-container-bench.sh"
  echo "harness_sha256      $(sha "$HARNESS_SRC")"
  echo "harness_bytes       $(wc -c <"$HARNESS_SRC" | tr -d ' ')"
  echo "dockerfile_sha256   $(sha "$DOCKERFILE_SRC")"
  echo "parent_sha          $(git -C "$REPO_ROOT" rev-parse "$PARENT_SHA")"
  echo "fixed_sha           $(git -C "$REPO_ROOT" rev-parse "$FIXED_SHA")"
  echo "placeholder_cred    PLACEHOLDER-NOT-A-CREDENTIAL-cosmon-bench-issue-20 (literal; door 3 stats, never reads)"
} | tee "$ENV_FILE"

WORKTREES=()
IMAGES=()
cleanup() {
  for wt in "${WORKTREES[@]:-}"; do
    [ -n "$wt" ] || continue
    git -C "$REPO_ROOT" worktree remove --force "$wt" >/dev/null 2>&1 || true
  done
  if [ "${COSMON_KEEP_IMAGE:-0}" != "1" ]; then
    for img in "${IMAGES[@]:-}"; do
      [ -n "$img" ] || continue
      docker --context "$CONTEXT" image rm -f "$img" >/dev/null 2>&1 || true
    done
  fi
  rmdir "$TREES" 2>/dev/null || true
}
trap cleanup EXIT

# run_head <label> <sha>
#
# Builds `cs` from <sha>, with the FINAL harness dropped on top, then runs
# the bench. The build context is a detached git worktree at <sha>, so the
# Rust sources are exactly that commit's and nothing of the current branch
# leaks in except the two harness files, which are overwritten on purpose
# and whose hashes are re-verified after the copy.
run_head() {
  local label="$1" sha_ref="$2"
  local wt="$TREES/tree-$label" img="cosmon-cwd-diff:$label"
  local resolved
  resolved="$(git -C "$REPO_ROOT" rev-parse "$sha_ref")"

  say "[$label] checking out $resolved into a detached worktree"
  git -C "$REPO_ROOT" worktree add --detach "$wt" "$resolved" >/dev/null
  WORKTREES+=("$wt")

  say "[$label] overwriting the harness with its FINAL bytes"
  cp "$HARNESS_SRC" "$wt/docker/container-worker-doors/in-container-bench.sh"
  cp "$DOCKERFILE_SRC" "$wt/docker/container-worker-doors/Dockerfile"
  cp "$REPO_ROOT/.dockerignore" "$wt/.dockerignore"
  local h d
  h="$(sha "$wt/docker/container-worker-doors/in-container-bench.sh")"
  d="$(sha "$wt/docker/container-worker-doors/Dockerfile")"
  [ "$h" = "$(sha "$HARNESS_SRC")" ] || die "[$label] harness hash mismatch after copy"
  [ "$d" = "$(sha "$DOCKERFILE_SRC")" ] || die "[$label] Dockerfile hash mismatch after copy"
  echo "  harness sha256    $h"
  echo "  dockerfile sha256 $d"
  # What the checkout differs from HEAD by, in the harness's own directory:
  # must be nothing but the two files we just forced.
  say "[$label] worktree divergence from the final harness (expect: clean)"
  git -C "$wt" status --porcelain -- docker/container-worker-doors || true

  say "[$label] building $img"
  docker --context "$CONTEXT" build \
    -f "$wt/docker/container-worker-doors/Dockerfile" \
    -t "$img" "$wt" >"$OUT/build-$label.log" 2>&1 \
    || { tail -40 "$OUT/build-$label.log"; die "[$label] docker build failed (full log: $OUT/build-$label.log)"; }
  IMAGES+=("$img")

  # ── The hash of the harness AS BAKED INTO THE IMAGE ───────────────────
  # Hashing the file on disk proves what was copied into the build context.
  # It does not prove what the container will execute — a `COPY` line, a
  # stale layer or a `.dockerignore` could still put different bytes at
  # /usr/local/bin/container-worker-doors-bench. So the hash is read back
  # out of the image, immediately before the pass runs, and a mismatch is
  # fatal: an invalid pass is restarted, never patched up.
  local baked
  baked="$(docker --context "$CONTEXT" run --rm --entrypoint /bin/sh "$img" \
    -c 'sha256sum /usr/local/bin/container-worker-doors-bench | cut -d" " -f1')"
  echo "  harness sha256 IN THE IMAGE, read back before the pass: $baked"
  [ "$baked" = "$h" ] \
    || die "[$label] the harness baked into the image is NOT the final harness ($baked != $h) — this pass is invalid; restart it, do not adjust it"

  # ── Image identity + the door-4 provenance discriminant ───────────────
  {
    echo "=== [$label] image identity ==="
    echo "commit_under_test   $resolved"
    echo "image_tag           $img"
    echo "image_id            $(docker --context "$CONTEXT" image inspect "$img" --format '{{.Id}}')"
    echo "image_created       $(docker --context "$CONTEXT" image inspect "$img" --format '{{.Created}}')"
    echo "harness_sha256_disk $h"
    echo "harness_sha256_image $baked"
    # BuildKit does not keep the bases as local images, so the resolved
    # digests are read from the build log, where `FROM …@sha256:…` records
    # exactly which manifest each stage was built on.
    grep -aoE 'docker\.io/library/rust:[^ @]+@sha256:[0-9a-f]+' "$OUT/build-$label.log" \
      | sort -u | sed 's/^/base_image          /'
    echo
    echo "--- binary provenance, read from /usr/local/bin/cs in the image ---"
    echo "(the first three exist ALREADY at the parent — fixes 1-3 predate it —"
    echo " so only the fourth distinguishes the two builds under test)"
    docker --context "$CONTEXT" run --rm --entrypoint /bin/sh "$img" -c '
      for m in "no usable Claude Code credential" "hasTrustDialogAccepted" "awaiting-human" "COSMON_READINESS_TRACE"; do
        if grep -aqF "$m" /usr/local/bin/cs; then
          printf "PRESENT  %s\n" "$m"
        else
          printf "ABSENT   %s\n" "$m"
        fi
      done
      printf "cs_sha256  %s\n" "$(sha256sum /usr/local/bin/cs | cut -d" " -f1)"
      printf "cs_version %s\n" "$(cs --version 2>&1)"
      printf "claude_version %s\n" "$(claude --version 2>&1)"
    '
  } 2>&1 | tee "$OUT/provenance-$label.txt"

  say "[$label] running the bench (raw output → $OUT/bench-$label.log)"
  set +e
  docker --context "$CONTEXT" run --rm --init "$img" 2>&1 | tee "$OUT/bench-$label.log"
  local rc=${PIPESTATUS[0]}
  set -e
  echo "bench_exit_status   $rc" | tee -a "$OUT/bench-$label.log"

  say "[$label] verdicts"
  grep '^VERDICT ' "$OUT/bench-$label.log" | tee "$OUT/verdicts-$label.txt" || true
}

run_head parent "$PARENT_SHA"
run_head fixed  "$FIXED_SHA"

say "DIFFERENTIAL SUMMARY"
printf '\n--- arm C on the PARENT (%s) — expected RED ---\n' "$PARENT_SHA"
grep '^VERDICT c:' "$OUT/bench-parent.log" || echo "(no arm-C verdict line)"
printf '\n--- arm C on the FIX (%s) — expected GREEN ---\n' "$FIXED_SHA"
grep '^VERDICT c:' "$OUT/bench-fixed.log" || echo "(no arm-C verdict line)"

printf '\nall artefacts: %s\n' "$OUT"
ls -1 "$OUT"
