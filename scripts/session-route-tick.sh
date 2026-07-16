#!/usr/bin/env bash
# session-route-tick.sh — fire the Tier-1 regex router over every session.
#
# Sibling of `session-to-spark-tick.sh`. Where the older script parses
# the session markdown in awk and decides what to spark, this one
# delegates **all** logic to the Rust implementation in
# `crates/cosmon-cli/src/cmd/route.rs` via `cs session route`. The
# shell script only exists to satisfy the LaunchAgent contract and
# provide an install/uninstall hook point.
#
# See ADR-072 (`docs/adr/072-session-route-formula-and-sidecar-invariants.md`)
# for the full routing protocol — Tier-1 regex, body_hash keying,
# `temp:proposed` staging, verdict-door.
#
# Flags:
#   --cosmon-root <DIR>     explicit project root containing `.cosmon/`.
#                           defaults to walk-up from cwd.
#   --session <ID|PATH>     process only a single session file.
#   --all                   process every session file (default when
#                           running under LaunchAgent).
#   --dry-run               classify without writing sidecars or
#                           nucleating molecules.
#   --no-stage              write sidecars only; skip `temp:proposed`
#                           molecule nucleation.
#   --json                  emit NDJSON on stdout (one line per note
#                           plus a `tick_complete` summary).
#   --help                  show this help.
#
# Exit codes:
#   0 — success (even zero notes)
#   1 — operator error (bad flags, no `.cosmon/`, missing session)
#   2 — transient failure (LaunchAgent re-fires next tick)
#
# Invariants (ADR-072):
#   I1 body-primacy — every sidecar keyed by `blake3(body)`.
#   I2 idempotent — same body + same router_version → no new write.
#   I3 append-only — a router bump writes a new sidecar beside the old.
#   I4 carnet untouched — `session-*.md` is never opened for write.

set -euo pipefail

SELF_NAME="session-route"

usage() {
    sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "${SELF_NAME}: $*" >&2
    exit 1
}

COSMON_ROOT=""
FORWARD_ARGS=()
DRY_RUN=0
ALL_SESSIONS=1  # default when invoked from LaunchAgent
SESSION_ARG=""

# Walk up from $1 until a directory containing `.cosmon/state/sessions/` is found.
discover_root() {
    local dir="$1"
    dir="$(cd "$dir" && pwd -P)"
    while [[ "$dir" != "/" && -n "$dir" ]]; do
        if [[ -d "$dir/.cosmon/state/sessions" ]]; then
            printf '%s\n' "$dir"
            return 0
        fi
        dir="$(dirname "$dir")"
    done
    printf ''
}

# Parse CLI flags. Anything we recognise is forwarded to `cs session route`;
# anything else errors.
while (($#)); do
    case "$1" in
        --cosmon-root)
            COSMON_ROOT="$2"; shift 2 ;;
        --session)
            SESSION_ARG="$2"
            ALL_SESSIONS=0
            shift 2 ;;
        --all)
            ALL_SESSIONS=1; shift ;;
        --dry-run)
            DRY_RUN=1
            FORWARD_ARGS+=("--dry-run")
            shift ;;
        --no-stage)
            FORWARD_ARGS+=("--no-stage")
            shift ;;
        --json)
            FORWARD_ARGS+=("--json")
            shift ;;
        -h|--help|help)
            usage 0 ;;
        *)
            echo "${SELF_NAME}: unknown flag: $1" >&2
            usage 1
            ;;
    esac
done

if [[ -z "$COSMON_ROOT" ]]; then
    COSMON_ROOT="$(discover_root "$(pwd)")"
    [[ -n "$COSMON_ROOT" ]] || die "no .cosmon/state/sessions/ found above $(pwd) — pass --cosmon-root <DIR>"
fi

[[ -d "$COSMON_ROOT/.cosmon/state/sessions" ]] || die "$COSMON_ROOT/.cosmon/state/sessions does not exist"

command -v cs >/dev/null 2>&1 || die "'cs' not on PATH"

# Build the final argv for `cs session route`.
CS_ARGS=("session" "route")
if [[ -n "$SESSION_ARG" ]]; then
    CS_ARGS+=("$SESSION_ARG")
elif (( ALL_SESSIONS )); then
    CS_ARGS+=("--all")
fi
CS_ARGS+=("${FORWARD_ARGS[@]:-}")

# --json on the outer cs invocation forces NDJSON. We respect whatever
# the caller passed.
cd "$COSMON_ROOT"

# Capture + forward stdout. We deliberately do NOT swallow stderr — a
# panic in `cs session route` must surface in LaunchAgent's `.err` log.
exec cs "${CS_ARGS[@]}"
