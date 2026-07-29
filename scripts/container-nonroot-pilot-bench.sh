#!/usr/bin/env bash
# container-nonroot-pilot-bench.sh — host-side driver for the non-root pilot
# replay (ADR-165).
#
# WHAT IT MEASURES
# ────────────────
# The claim is NOT "the final owners are correct" — that is the neighbouring
# property and it is compatible with an ownership repair having fired and
# landed on the owner a path already had. The claim is:
#
#   THE NOMINAL PATH INVOKED NO OWNERSHIP REPAIR AT ALL.
#
# and, alongside it, that two consecutive dispatches under one non-root uid
# produce two artefacts that are verified as commit OBJECTS in the
# repository — not merely as files on disk, which is the distinction the
# external tester's own finding turned on.
#
# HOW THE ABSENCE IS OBSERVED
# ───────────────────────────
# Instrumentation, not final state, and not strace: the engine's seccomp
# posture refuses ptrace, so a tracing route would be cause-not-isolated
# (docs/benches/engine-fidelity-2026-07-27.md). `cs` counts every ENTRY into
# the ownership-repair path — before any precondition — into the file named
# by COSMON_OWNERSHIP_TRANSFER_JOURNAL, one line per entry, each carrying the
# writing pid. One journal file per dispatch makes each number attributable
# to one dispatch; the pid makes it attributable to one process.
#
# THE INVOCATION UNDER TEST
# ─────────────────────────
# The full one, because a partial one measures the neighbouring property:
#
#   docker exec -u 10001:10001 -e HOME=/home/cosmon-worker \
#     -e CLAUDE_CONFIG_DIR=… -e LC_ALL=C.UTF-8 -w … <container> bash
#
# Changing only `-u` while leaving HOME=/root puts the worker back behind a
# 0700 directory it cannot read — defect #2 of the four, reintroduced by an
# incomplete command. The in-container harness asserts HOME rather than
# trusting it.
#
# SECRETS: none, ever. No credential is created, mounted, read or logged;
# the config dir the dispatches use is virgin by construction. Do not add
# ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, or a credential mount.
#
# Usage:
#   scripts/container-nonroot-pilot-bench.sh
#
# Environment overrides:
#   COSMON_BENCH_COLIMA_PROFILE  colima profile carrying the bench engine
#   COSMON_DOCKER_CONTEXT        explicit docker context override
#   COSMON_KEEP_IMAGE=1          skip the image rmi at teardown
#   NONROOT_OUT_DIR              where to copy the container's records
#
# Exit status:
#   0  the bar was met
#   1  a finding
#   2  INCONCLUSIVE — a discriminating step could not run
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The two provenance predicates, behind a named boundary so
# `scripts/source-provenance.test.sh` can construct the moved-tree case and
# require them to refuse it. They shipped inline and untested (R3); a control
# nobody can break on purpose is a control nobody knows still works.
# shellcheck source=lib/source-provenance.sh
. "$REPO_ROOT/scripts/lib/source-provenance.sh"
# shellcheck source=lib/bench-engine.sh
. "$REPO_ROOT/scripts/lib/bench-engine.sh"

say() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
sub() { printf '\033[0;36m  · %s\033[0m\n' "$*"; }

CTX="$(bench_engine_context)"
bench_engine_require "$CTX" || exit 2
FINGERPRINT="$(bench_engine_fingerprint "$CTX")"
say "engine"
sub "docker context : $CTX"
sub "fingerprint    : $FINGERPRINT"

# ── Provenance of the bytes ────────────────────────────────────────────
# Read here and baked into the image, so the harness reports the commit the
# binary was built from rather than the commit whoever writes the report
# believes it was built from. A dirty tree is recorded as dirty; the
# in-container harness raises it as a finding, because then the commit does
# not describe the bytes.
SOURCE_STATE="$(source_tree_state "$REPO_ROOT")"
SOURCE_SHA="${SOURCE_STATE%% *}"
SOURCE_CLEAN="${SOURCE_STATE##* }"
say "provenance"
sub "source commit  : $SOURCE_SHA"
sub "source tree    : $SOURCE_CLEAN"

# Refuse to build from a tree that is not clean, rather than stamping a
# commit onto bytes it does not describe.
#
# THIS SAMPLE IS THE *PRE*-CONDITION ONLY, and it is worth being exact about
# what it can and cannot see. A sample taken here observes the tree as it was
# before `docker build` ran. It cannot, by construction, observe an edit that
# lands DURING the build — which is the interesting failure, because the
# `COPY` inside the Dockerfile happens mid-build and takes whatever is on
# disk at that instant. The post-condition sample after the build is what
# closes that window; see `verify_source_unmoved` below. Keeping both is not
# redundancy: the pre-check saves a ten-minute build that was doomed anyway,
# the post-check is the one that makes the stamped commit true.
if [ "$SOURCE_CLEAN" != "clean" ]; then
  printf '\n\033[1;33mVERDICT INCONCLUSIVE — the working tree is dirty.\033[0m\n'
  printf '  Commit or stash first. A capture that names a commit it was not\n'
  printf '  built from is worse than no capture.\n'
  exit 2
fi

# Re-sample HEAD and the porcelain status AFTER the build and refuse if
# either moved. This is the control that actually covers the measured
# failure: start clean, edit while `cargo build` runs inside the image, and
# the COPY takes the edited files while the capture goes on reporting the old
# commit as clean. A pre-build sample fires one instant before that can
# happen; this one fires after it has.
verify_source_unmoved() {
  local after
  if source_unmoved "$REPO_ROOT" "$SOURCE_SHA"; then
    sub "source verified: unmoved across the build ($SOURCE_SHA, clean)"
    return 0
  fi
  after="$(source_tree_state "$REPO_ROOT")"
  printf '\n\033[1;33mVERDICT INCONCLUSIVE — the source moved during the build.\033[0m\n'
  printf '  before : %s (clean)\n' "$SOURCE_SHA"
  printf '  after  : %s\n' "$after"
  printf '  The image COPYs the tree mid-build, so the bytes inside it are\n'
  printf '  not the bytes this capture would name. Re-run from a tree nobody\n'
  printf '  is editing.\n'
  exit 2
}

IMAGE="cosmon-nonroot-pilot:bench"
OUT_DIR="${NONROOT_OUT_DIR:-$REPO_ROOT/docs/benches/nonroot-pilot-$(date +%Y-%m-%d)}"
mkdir -p "$OUT_DIR"

say "build"
docker --context "$CTX" build \
  -f docker/container-real-mission/Dockerfile \
  --build-arg COSMON_SOURCE_SHA="$SOURCE_SHA" \
  --build-arg COSMON_SOURCE_CLEAN="$SOURCE_CLEAN" \
  -t "$IMAGE" "$REPO_ROOT" || {
  printf '\n\033[1;33mVERDICT INCONCLUSIVE — the image did not build.\033[0m\n'
  exit 2
}

# The post-condition. Runs before anything reads the image, so a moved tree
# is a refusal and never a report.
say "provenance (post-build)"
verify_source_unmoved

# ── The credential, by route (a) ───────────────────────────────────────
# The operator's own OAuth token, supplied by him for this replay. It is
# mounted read-only and read by the harness into CLAUDE_CODE_OAUTH_TOKEN in
# one gesture.
#
# Why a staged copy and not the original: the token is 0600 and owned by the
# operator, so a raw bind mount is unreadable by uid 10001 inside. The stage
# is mode 444 INSIDE a 0700 directory — the directory is what protects it on
# the host — and it is shredded on exit.
#
# Why the stage lives under $HOME and not in `mktemp -d`: MEASURED — colima
# shares only $HOME and /tmp/colima into the VM, and macOS `mktemp -d`
# returns a path under /var/folders. A bind mount of a path the VM cannot
# see does not fail; it silently presents an EMPTY file, and the run then
# looks like a credential-less one for thirty minutes.
#
# The value is never echoed here. It is passed to `docker run` as a mount,
# never as `-e`, so it does not appear in this host's process arguments
# either.
TOKEN_SRC="${COSMON_OAT_TOKEN_FILE:-$HOME/.cosmon/claude-oat.token}"
TOKEN_STAGE=""
if [ -r "$TOKEN_SRC" ]; then
  STAGE_DIR="$HOME/.cosmon/.nonroot-pilot-stage-$$"
  mkdir -p "$STAGE_DIR"
  chmod 700 "$STAGE_DIR"
  TOKEN_STAGE="$STAGE_DIR/claude-oat.token"
  install -m 444 "$TOKEN_SRC" "$TOKEN_STAGE"
  say "credential"
  sub "route          : read-only mount → CLAUDE_CODE_OAUTH_TOKEN in the dispatcher env"
  sub "source         : $TOKEN_SRC (value never read by this script)"
else
  say "credential"
  sub "none available at $TOKEN_SRC — the dispatches will refuse at the gate"
fi

# A container name derived from the run, never a fixed one: a fixed name is
# a shared resource exactly like a shared file, and one has already
# destroyed a live mission this week.
NAME="cosmon-nonroot-pilot-$$-$(date +%s)"
say "container $NAME"
RUN_ARGS=(-dit --init --name "$NAME")
if [ -n "$TOKEN_STAGE" ]; then
  RUN_ARGS+=(-v "$TOKEN_STAGE:/run/secrets/claude-oat.token:ro")
fi
docker --context "$CTX" run "${RUN_ARGS[@]}" \
  --entrypoint sleep "$IMAGE" infinity >/dev/null || exit 2
cleanup() {
  docker --context "$CTX" rm -f "$NAME" >/dev/null 2>&1
  if [ "${COSMON_KEEP_IMAGE:-0}" != "1" ]; then
    docker --context "$CTX" rmi "$IMAGE" >/dev/null 2>&1
  fi
  [ -n "${STAGE_DIR:-}" ] && rm -rf "$STAGE_DIR"
}
trap cleanup EXIT

# ── Preflight: did the mount actually arrive? ──────────────────────────
# `-s` is a stat, never a read. A mount the VM could not see presents an
# empty file rather than failing, and discovering that after two dispatches
# and two commit deadlines costs half an hour. Checked as the uid that will
# read it, because readability is the question.
if [ -n "$TOKEN_STAGE" ]; then
  if docker --context "$CTX" exec -u 10001:10001 "$NAME" \
       test -s /run/secrets/claude-oat.token 2>/dev/null; then
    sub "mount check   : the token is present and readable by uid 10001 (stat only)"
  else
    printf '\n\033[1;33mVERDICT INCONCLUSIVE — the token mount arrived empty or unreadable by uid 10001.\033[0m\n'
    printf '  colima shares only $HOME and /tmp/colima into the VM; stage the token under $HOME.\n'
    exit 2
  fi
fi

# ── The run, through the full non-root invocation ──────────────────────
say "replay — docker exec -u 10001:10001 with an explicit HOME"
docker --context "$CTX" exec \
  -u 10001:10001 \
  -e HOME=/home/cosmon-worker \
  -e LC_ALL=C.UTF-8 \
  -e OUT_DIR=/tmp/nonroot-out \
  -w /home/cosmon-worker \
  "$NAME" /usr/local/bin/container-nonroot-pilot
RC=$?

docker --context "$CTX" cp "$NAME:/tmp/nonroot-out/." "$OUT_DIR/" >/dev/null 2>&1
printf '%s\n' "$FINGERPRINT" >"$OUT_DIR/engine-fingerprint.txt"

# ── Scrub gate — the capture must not leak the key it used ─────────────
# Read the value into a variable ONLY to search for it, never to print it.
# A hit here is fatal: the records are destroyed rather than left on disk
# for someone to commit later.
if [ -n "$TOKEN_STAGE" ]; then
  say "scrub"
  TOKEN_VALUE="$(cat "$TOKEN_STAGE")"
  if [ -n "$TOKEN_VALUE" ] && grep -rqF -- "$TOKEN_VALUE" "$OUT_DIR" 2>/dev/null; then
    LEAKED="$(grep -rlF -- "$TOKEN_VALUE" "$OUT_DIR" 2>/dev/null | tr '\n' ' ')"
    unset TOKEN_VALUE
    rm -rf "$OUT_DIR"
    printf '\n\033[1;31mVERDICT LEAK — the token value appears in the captured records (%s). The records were destroyed and nothing is committed.\033[0m\n' "$LEAKED"
    exit 1
  fi
  unset TOKEN_VALUE
  sub "no occurrence of the token value in $OUT_DIR"
fi

# ── Empty evidence is not evidence ─────────────────────────────────────
# A zero-byte file is indistinguishable from a missing one, and committing
# one implies a measurement that was never taken. They are removed and the
# removal is reported rather than silent.
PRUNED="$(find "$OUT_DIR" -type f -empty -print -delete 2>/dev/null | tr '\n' ' ')"
if [ -n "$PRUNED" ]; then
  sub "pruned zero-byte evidence files: $PRUNED"
fi

say "records"
sub "$OUT_DIR"
ls -1 "$OUT_DIR" 2>/dev/null | sed 's/^/    /'

# The verdict is read back from the record the harness wrote, so this line
# cannot say something the machine-readable field does not.
VERDICT="$(jq -r '.verdict // "NO-RECORD"' "$OUT_DIR/nonroot-pilot-record.json" 2>/dev/null || echo NO-RECORD)"
case "$RC" in
  0) printf '\n\033[1;32mVERDICT %s\033[0m\n' "$VERDICT" ;;
  2) printf '\n\033[1;33mVERDICT INCONCLUSIVE (%s)\033[0m\n' "$VERDICT" ;;
  *) printf '\n\033[1;31mVERDICT %s (rc=%s)\033[0m\n' "$VERDICT" "$RC" ;;
esac
exit "$RC"
