#!/usr/bin/env bash
# container-engine-posture.sh — measure, on every docker engine you can
# reach, the things the container benches' fidelity claim is made of.
#
# WHY THIS EXISTS
# ───────────────
# The container benches used to carry a sentence asserting that a colima
# context "runs an Ubuntu kernel with a DIFFERENT user-namespace posture and
# is NOT faithful" to the external tester's engine. Nobody had measured it.
# The factual half turned out to be roughly right and the conclusion inverted:
# on 2026-07-27 the tester corrected his own earlier description and named his
# bed as Colima (Lima-based), Ubuntu 24.04.4 LTS, aarch64.
#
# The lesson is not "the default was wrong". It is that a fidelity claim which
# nobody probed was load-bearing for a year. So this script probes, and every
# line it prints is either something it ran or something it read — never
# something it inferred. Where a probe cannot say WHY, it prints
# `cause undetermined`, in the same discipline as
# `cosmon_core::egress::NetnsBlocker::Undetermined`: our earlier advisory named
# two sysctls as a cause without reading them and cost the tester a week.
#
# WHAT IT MEASURES, per engine
#   engine identity     server version, OS, arch, kernel  (docker info)
#   userns sysctls      /proc/sys/user/max_user_namespaces and
#                       kernel.unprivileged_userns_clone, READ inside a
#                       container rather than assumed
#   userns functional   `setpriv --reuid 10001 … unshare -Ur true` under the
#                       DEFAULT seccomp profile — the thing the sysctls were
#                       being treated as a proxy for
#   seccomp attribution the same probe again with `seccomp=unconfined`. If it
#                       flips, the seccomp profile is the cause and we say so;
#                       if it does not, we say the cause is undetermined
#   virtiofs chown      a host directory bind-mounted in, `chown`ed to uid
#                       10001 from inside, then `stat`ed. The tester's standing
#                       finding is that this silently no-ops on virtiofs, which
#                       is why the project must live on container-local storage
#
# Usage:
#   scripts/container-engine-posture.sh                       # every reachable context
#   scripts/container-engine-posture.sh colima-cosmon-bench desktop-linux
#   COSMON_POSTURE_OUT=/path/report.txt scripts/container-engine-posture.sh
#
# SECRETS: none, ever. Nothing is read from the host beyond a scratch
# directory this script creates and removes; no credential is mounted, minted,
# printed, or logged.
#
# CONTAINER HYGIENE: every container is named `cosmon-posture-<pid>-*` and is
# `--rm`'d. The base image (ubuntu:24.04) is PULLED but never removed: it may
# have been on your engine before this ran, and deleting somebody else's image
# to tidy up after ourselves is precisely the accident this discipline forbids.
set -uo pipefail

OUT="${COSMON_POSTURE_OUT:-}"
PROBE_IMAGE="${COSMON_POSTURE_IMAGE:-ubuntu:24.04}"
MOUNT_PROBE_DIR="$HOME/.cosmon-engine-posture-$$"

say() { printf '\n\033[1;34m▶ %s\033[0m\n' "$*"; }

cleanup() { rm -rf "$MOUNT_PROBE_DIR"; }
trap cleanup EXIT

command -v docker >/dev/null 2>&1 || {
  printf 'docker is not on PATH; nothing can be measured here.\n' >&2
  exit 2
}

# Contexts to probe: the ones named on the command line, else every context
# docker knows about. Unreachable ones are REPORTED as unreachable, not
# skipped in silence — "we did not measure it" is itself a finding.
if [ "$#" -gt 0 ]; then
  CONTEXTS=("$@")
else
  mapfile -t CONTEXTS < <(docker context ls --format '{{.Name}}' | sort)
fi

# The in-container probe. Kept as one heredoc so that the SAME bytes run on
# every engine: a posture comparison whose two halves ran different scripts
# compares the scripts, not the engines.
read -r -d '' PROBE_SCRIPT <<'PROBE' || true
set -u
echo "uname               $(uname -srm)"
echo "os_release          $(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME")"
printf 'max_user_namespaces %s\n' "$(cat /proc/sys/user/max_user_namespaces 2>&1)"
if v="$(cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null)"; then
  echo "unprivileged_userns_clone $v"
else
  # An unknown key is NOT a zero. Saying so is the whole point.
  echo "unprivileged_userns_clone <key absent on this kernel — absent is not 0>"
fi
echo "seccomp_status      $(grep -E '^Seccomp' /proc/self/status | tr '\n' ' ')"
if setpriv --reuid 10001 --regid 10001 --clear-groups unshare -Ur true 2>/tmp/e; then
  echo "unshare_as_uid10001 OK"
else
  echo "unshare_as_uid10001 BLOCKED rc=$? err=$(tr -d '\n' </tmp/e)"
fi
# The bind mount, if one was given: chown it and read back what stuck.
if [ -d /probe-mount ]; then
  : >/probe-mount/chown-target 2>/dev/null || echo "mount_writable      NO"
  if [ -f /probe-mount/chown-target ]; then
    before="$(stat -c '%u:%g' /probe-mount/chown-target)"
    chown_rc=0
    chown 10001:10001 /probe-mount/chown-target 2>/tmp/ce || chown_rc=$?
    after="$(stat -c '%u:%g' /probe-mount/chown-target)"
    echo "mount_chown_rc      $chown_rc $( [ $chown_rc -ne 0 ] && tr -d '\n' </tmp/ce )"
    echo "mount_owner_before  $before"
    echo "mount_owner_after   $after"
    if [ "$chown_rc" -eq 0 ] && [ "$after" = "$before" ]; then
      echo "mount_chown_verdict SILENTLY-IGNORED (chown returned 0 and ownership did not change)"
    elif [ "$after" != "$before" ]; then
      echo "mount_chown_verdict HONOURED"
    else
      echo "mount_chown_verdict REFUSED (chown reported an error; at least it is not silent)"
    fi
  fi
else
  echo "mount_chown_verdict NOT-PROBED (no bind mount supplied)"
fi
PROBE

mkdir -p "$MOUNT_PROBE_DIR"

emit() {
  printf '%s\n' "$*"
  [ -n "$OUT" ] && printf '%s\n' "$*" >>"$OUT"
  return 0
}

if [ -n "$OUT" ]; then
  mkdir -p "$(dirname "$OUT")"
  : >"$OUT"
fi

emit "=== container engine posture ==="
emit "date_utc            $(date -u +%Y-%m-%dT%H:%M:%SZ)"
emit "host_uname          $(uname -a)"
emit "probe_image         $PROBE_IMAGE"
emit "mount_probe_dir     $MOUNT_PROBE_DIR (host side; created and removed by this script)"

for ctx in "${CONTEXTS[@]}"; do
  emit ""
  emit "--- context: $ctx ---"
  if ! docker --context "$ctx" info >/dev/null 2>&1; then
    emit "reachable           NO — not measured (this is a gap in the comparison, not a pass)"
    continue
  fi
  emit "reachable           YES"
  emit "$(docker --context "$ctx" info --format 'engine_server       {{.ServerVersion}}
engine_os           {{.OperatingSystem}}
engine_arch         {{.Architecture}}
engine_kernel       {{.KernelVersion}}')"

  if ! docker --context "$ctx" image inspect "$PROBE_IMAGE" >/dev/null 2>&1; then
    say "pulling $PROBE_IMAGE on $ctx (left in place afterwards — it may not be ours)"
    docker --context "$ctx" pull -q "$PROBE_IMAGE" >/dev/null 2>&1 \
      || { emit "probe_image_pull    FAILED — posture not measured on this engine"; continue; }
  fi

  emit "[default seccomp profile]"
  emit "$(docker --context "$ctx" run --rm --name "cosmon-posture-$$-def-${ctx//[^a-zA-Z0-9]/_}" \
      -v "$MOUNT_PROBE_DIR:/probe-mount" \
      "$PROBE_IMAGE" bash -c "$PROBE_SCRIPT" 2>&1)"

  # Second pass with seccomp off. This is the ATTRIBUTION step: if unshare
  # flips from BLOCKED to OK, the default seccomp profile is the cause and we
  # can say so because we changed exactly one thing. If it does not flip, we
  # decline to name a cause.
  emit "[seccomp=unconfined — attribution pass]"
  unconf="$(docker --context "$ctx" run --rm --security-opt seccomp=unconfined \
      --name "cosmon-posture-$$-unc-${ctx//[^a-zA-Z0-9]/_}" \
      "$PROBE_IMAGE" bash -c "$PROBE_SCRIPT" 2>&1)"
  emit "$unconf"
done

emit ""
emit "NOTE: every line above was run or read on the engine it is filed under."
emit "Where a probe could not attribute a cause it says so; no cause is named"
emit "from a setting that was merely read. (cosmon: NetnsBlocker::Undetermined.)"

[ -n "$OUT" ] && say "written to $OUT"
exit 0
