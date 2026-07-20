#!/usr/bin/env bash
# Shared helpers for the cosmon regression bench.
#
# This library is the *producer core*: it knows how to obtain a pristine
# source tree at a chosen ref (the unit-under-test), where to write evidence,
# and how to emit one machine-readable probe record. Every probe sources this
# file so the report schema is defined in exactly one place.
#
# Unit under test (COSMON_TAG): defaults to HEAD — the FIXED tree — so the
# bench measures the post-fix code. Set COSMON_TAG=v0.2.1 to re-measure the
# original released baseline (the tree the external tester reported against),
# which is how the before/after delta is produced.
#
# Design invariant: the bench NEVER modifies cosmon source. It tests the tree
# at COSMON_TAG as-is. `checkout_uut` materialises that ref into a throw-away
# directory via `git archive`; nothing is ever written back.

set -euo pipefail

# Absolute path to the bench/ directory, regardless of caller CWD.
BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Repo root (bench lives at <repo>/bench).
REPO_ROOT="$(cd "$BENCH_DIR/.." && pwd)"
# Output roots.
OUT_DIR="${BENCH_OUT_DIR:-$BENCH_DIR/out}"
PROBES_OUT="$OUT_DIR/probes"
EVIDENCE_OUT="$OUT_DIR/evidence"

# The ref we measure. Defaults to HEAD (the FIXED tree). Set COSMON_TAG=v0.2.1
# to re-measure the original baseline the external tester (MIT) reported against.
COSMON_TAG="${COSMON_TAG:-HEAD}"

# Path to a prebuilt `cs` binary for the runtime-decisive probes (#1/#3/#4).
# Defaults to the release binary built from the fixed tree; overridable so the
# bench can point at any `cs` (e.g. the v0.2.1 baseline binary) for A/B runs.
CS_BIN="${CS_BIN:-$REPO_ROOT/target/release/cs}"

mkdir -p "$PROBES_OUT" "$EVIDENCE_OUT"

log()  { printf '[bench] %s\n' "$*" >&2; }
warn() { printf '[bench][warn] %s\n' "$*" >&2; }
die()  { printf '[bench][fatal] %s\n' "$*" >&2; exit 1; }

# has CMD -> 0 if an executable is on PATH.
has() { command -v "$1" >/dev/null 2>&1; }

# checkout_uut DEST
# Materialise a pristine copy of the COSMON_TAG source tree into DEST (created
# if absent). Uses `git archive` so no worktree, index, or ref is touched — the
# development repo is never rewritten in place (a CLAUDE.md hard rule).
checkout_uut() {
  local dest="$1"
  mkdir -p "$dest"
  if ! git -C "$REPO_ROOT" rev-parse "$COSMON_TAG" >/dev/null 2>&1; then
    die "ref $COSMON_TAG not found in $REPO_ROOT — cannot obtain unit-under-test"
  fi
  git -C "$REPO_ROOT" archive "$COSMON_TAG" | tar -x -C "$dest"
  log "materialised $COSMON_TAG -> $dest"
}

# Backward-compatible alias: earlier probes called checkout_v021.
checkout_v021() { checkout_uut "$@"; }

# stage_docker_context CONTEXT_DIR
# Prepare a self-contained Docker build context: the pristine v0.2.1 tree under
# CONTEXT_DIR/uut plus the bench Dockerfiles. Both Dockerfiles COPY ./uut, so
# the container compiles the released tree as-is (never main, never mutated).
stage_docker_context() {
  local ctx="$1"
  rm -rf "$ctx"
  mkdir -p "$ctx"
  checkout_v021 "$ctx/uut"
  cp "$BENCH_DIR/Dockerfile" "$BENCH_DIR/Dockerfile.nodeps" "$ctx/"
  log "staged docker context -> $ctx"
}

# emit_probe writes one probe record as JSON to $PROBES_OUT/<id>.json.
# The record schema is the contract consumed by aggregate.sh:
#   { id, name, adapter, verdict, captured_signature, evidence_path,
#     judge_verdict, note }
# verdict       : RED | GREEN | INCONCLUSIVE
#   RED          = the reported defect reproduced (bad behaviour observed)
#   GREEN        = the reported defect did NOT reproduce on this tree
#   INCONCLUSIVE = could not run the discriminating step headless; NOT a pass
# judge_verdict : filled later by the LLM-as-judge harness; "PENDING" until then.
#
# Usage: emit_probe ID NAME ADAPTER VERDICT SIGNATURE EVIDENCE_PATH [NOTE]
emit_probe() {
  local id="$1" name="$2" adapter="$3" verdict="$4" sig="$5" evidence="$6" note="${7:-}"
  local rel_evidence="$evidence"
  # Store evidence path relative to the bench dir when it lives underneath it,
  # so the report is portable across machines.
  case "$evidence" in
    "$BENCH_DIR"/*) rel_evidence="${evidence#"$BENCH_DIR"/}" ;;
  esac
  jq -n \
    --arg id "$id" \
    --arg name "$name" \
    --arg adapter "$adapter" \
    --arg verdict "$verdict" \
    --arg sig "$sig" \
    --arg evidence "$rel_evidence" \
    --arg note "$note" \
    '{
       id: $id,
       name: $name,
       adapter: $adapter,
       verdict: $verdict,
       captured_signature: $sig,
       evidence_path: $evidence,
       judge_verdict: "PENDING",
       note: $note
     }' > "$PROBES_OUT/$id.json"
  log "probe $id -> $verdict"
}
