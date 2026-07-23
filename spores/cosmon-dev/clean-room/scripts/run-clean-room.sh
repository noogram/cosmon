#!/usr/bin/env bash
# run-clean-room.sh — select a posture, inject the frozen source, enforce the
# network mode, and run a command in the clean-room. blueprint §4.
#
# Usage:
#   run-clean-room.sh <profile> <affected_ref> [--net mechanical|upstream-real] \
#                     [--login-volume <vol>] [--image <digest>] -- <command...>
#
#   <profile>       repro-root | repro-user | judge-user
#   <affected_ref>  the git ref to inject via `git archive` (e.g. v0.2.2)
#   --net           mechanical (default, --network none) | upstream-real (egress
#                   allowlist to the pinned auth/inference endpoints; the release
#                   canary ONLY, never the per-PR gate)
#   --login-volume  a disposable ~/.claude volume from disposable-login.sh
#   --image         the pinned image digest (default: cosmon-dev-cleanroom:pinned)
#
# The Mac's ~/.claude is NEVER mounted; assert-no-host-mounts.sh gates every run.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

profile="${1:?profile: repro-root|repro-user|judge-user}"; shift
affected_ref="${1:?affected_ref (e.g. v0.2.2)}"; shift

net="mechanical"
login_volume=""
image="cosmon-dev-cleanroom:pinned"
cmd=()
while [ $# -gt 0 ]; do
  case "$1" in
    --net) net="$2"; shift 2;;
    --login-volume) login_volume="$2"; shift 2;;
    --image) image="$2"; shift 2;;
    --) shift; cmd=("$@"); break;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
[ "${#cmd[@]}" -gt 0 ] || { echo "no command after --" >&2; exit 2; }

# Posture -> UID + start dir. judge-user starts OUTSIDE the repo so it reads the
# code, never the project self-description (blueprint §6).
case "$profile" in
  repro-root)  uid_arg="-u 0";      workdir="/work/src";;
  repro-user)  uid_arg="-u 10001";  workdir="/work/src";;
  judge-user)  uid_arg="-u 10001";  workdir="/work";;    # outside src/
  *) echo "unknown profile: $profile" >&2; exit 2;;
esac

# Network mode. mechanical = fully closed (deterministic per-PR gate). upstream-real
# = egress allowlist (the release canary). Describing an upstream-real run as
# "isolated" is false; the mode is printed loudly.
case "$net" in
  mechanical)     net_args="--network none";;
  upstream-real)  net_args="";  echo "NOTE: upstream-real mode — network egress is OPEN (release canary, NOT a deterministic gate)." >&2;;
  *) echo "unknown --net: $net" >&2; exit 2;;
esac

# Inject the FROZEN source by `git archive <affected_ref>` into a staging dir,
# never the live checkout. The archive is mounted read-only.
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
git -C "$here/../../../.." archive "$affected_ref" | tar -x -C "$stage"
echo "injected frozen source: $affected_ref -> $stage (read-only)" >&2

# Assemble mounts: RO source, writable /output, optional disposable ~/.claude.
mounts=( -v "$stage:/work/src:ro" -v "$(pwd)/output:/output" )
if [ -n "$login_volume" ]; then
  mounts+=( -v "$login_volume:/home/worker/.claude" )
fi

# Build the full run-arg string and gate it through the no-host-mount assertion.
run_args="$net_args $uid_arg ${mounts[*]}"
bash "$here/assert-no-host-mounts.sh" "$run_args"

mkdir -p "$(pwd)/output"
# shellcheck disable=SC2086
exec docker run --rm -i \
  $net_args $uid_arg \
  -e ANTHROPIC_API_KEY="" \
  -e DISABLE_AUTOUPDATER=1 \
  -e COSMON_DEV_PROFILE="$profile" \
  "${mounts[@]}" \
  -w "$workdir" \
  "$image" \
  /bin/bash -lc "$(printf '%q ' "${cmd[@]}")"
