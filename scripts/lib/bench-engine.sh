#!/usr/bin/env bash
# bench-engine.sh — the one place the container benches decide WHICH docker
# engine they run on, and refuse to run when it is not there.
#
# WHY THIS FILE EXISTS
# ────────────────────
# Until 2026-07-27 the three container benches each pinned
# `--context desktop-linux` and each carried a header sentence asserting that
# Docker Desktop's LinuxKit VM was "the tester's engine" and that a colima
# context was "NOT faithful". On 2026-07-27 the external tester corrected his
# own earlier description, unprompted, on GitHub issue #20: his bed is
# **Colima (Lima-based), Ubuntu 24.04.4 LTS, aarch64** — not Docker Desktop.
# He had said Docker Desktop in the original repro recipe and in several
# comments; the correction is his.
#
# So the benches had been pinned AWAY from his real engine by a comment
# written to keep them faithful to it. This file is the fix, and it is a
# single file precisely so the next correction is one edit rather than three.
#
# WHAT IS AND IS NOT CLAIMED
# ──────────────────────────
# The old header's factual half is not discarded: a colima VM really does run
# a different kernel from LinuxKit, and its user-namespace posture really is
# different. What was wrong was the CONCLUSION drawn from that difference.
# The measured comparison of both engines — kernel, OS, arch, the sysctls read
# rather than inferred, a functional `unshare -Ur` probe as a non-root uid, and
# a virtiofs bind-mount `chown` probe — is in
# `docs/benches/engine-fidelity-2026-07-27.md`, produced by
# `scripts/container-engine-posture.sh`. Read that before changing the default
# here; do not replace one unverified fidelity claim with another.
#
# NO SILENT FALLBACK
# ──────────────────
# If the bench engine is unreachable these helpers print the exact command the
# operator should run and exit **2 = INCONCLUSIVE**, which is the verdict
# `bench/README.md` already defines for "the discriminating step could not run
# here". They never fall through to whatever context happens to be current. A
# bench that quietly ran on a second engine is exactly the drift this file
# exists to close, and it would be invisible in the log.
#
# DEDICATED PROFILE
# ─────────────────
# The default profile is `cosmon-bench`, which belongs to the benches and to
# nothing else. Five other colima profiles exist on this workshop's machine —
# `forgeron-build`, `maqi-admin-vm`, `maqi-instance-vm`, `radience-optix-vm`,
# `default` — and each belongs to some real piece of work. A bench that shares
# a VM with real work is a bench that will one day break it, so these scripts
# will not start, stop, prune, or build on any of them.
#
# USAGE
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/bench-engine.sh"
#   CONTEXT="$(bench_engine_context)"
#   bench_engine_require "$CONTEXT"
#   bench_engine_fingerprint "$CONTEXT"
#
# ENVIRONMENT
#   COSMON_BENCH_COLIMA_PROFILE  colima profile to use (default: cosmon-bench)
#   COSMON_DOCKER_CONTEXT        explicit docker context override. Honoured as
#                                the operator's deliberate choice — and still
#                                required to be reachable. An override that
#                                cannot be reached is INCONCLUSIVE too.

# The colima profile these benches own. Nothing else may be named here.
COSMON_BENCH_COLIMA_PROFILE="${COSMON_BENCH_COLIMA_PROFILE:-cosmon-bench}"

# The VM shape the fidelity capture was taken on. `vz` + `virtiofs` is what
# reproduces the tester's two standing findings (see the doc named above);
# qemu with sshfs mounts is a different bed and would silently change them.
COSMON_BENCH_COLIMA_ARGS=(
  --cpu 4 --memory 8 --disk 60
  --vm-type vz --mount-type virtiofs
  --runtime docker
)

# bench_engine_context — the docker context name for the bench profile.
#
# colima names the context for its default profile plainly `colima`, and every
# other profile `colima-<profile>`. Getting this wrong produces a context that
# does not exist, which the reachability check below then reports honestly —
# but the message is nicer if we name it correctly in the first place.
bench_engine_context() {
  if [ -n "${COSMON_DOCKER_CONTEXT:-}" ]; then
    printf '%s\n' "$COSMON_DOCKER_CONTEXT"
  elif [ "$COSMON_BENCH_COLIMA_PROFILE" = "default" ]; then
    printf 'colima\n'
  else
    printf 'colima-%s\n' "$COSMON_BENCH_COLIMA_PROFILE"
  fi
}

# bench_engine_start_command — the exact line the operator should run.
bench_engine_start_command() {
  printf 'colima start --profile %s %s\n' \
    "$COSMON_BENCH_COLIMA_PROFILE" "${COSMON_BENCH_COLIMA_ARGS[*]}"
}

# bench_engine_require <context> — refuse, loudly and specifically, unless the
# named engine answers. Exit 2 (INCONCLUSIVE), never 0, never a fallback.
bench_engine_require() {
  local ctx="$1"
  command -v docker >/dev/null 2>&1 || {
    printf '\n\033[1;33mVERDICT INCONCLUSIVE — docker is not on PATH; the container bench cannot run here.\033[0m\n' >&2
    exit 2
  }
  docker --context "$ctx" info >/dev/null 2>&1 && return 0

  {
    printf '\n\033[1;33mVERDICT INCONCLUSIVE — the bench engine is not reachable.\033[0m\n'
    printf '\n  docker context : %s\n' "$ctx"
    if [ -n "${COSMON_DOCKER_CONTEXT:-}" ]; then
      printf '  source         : COSMON_DOCKER_CONTEXT (your override)\n'
      printf '\n  You pinned this context yourself. Start it, or unset the variable to\n'
      printf '  fall back to the bench profile — this script will not pick another\n'
      printf '  engine for you, because a bench that silently ran somewhere else is\n'
      printf '  the drift this harness exists to close.\n'
    else
      printf '  source         : colima profile %s (the benches own this profile)\n' "$COSMON_BENCH_COLIMA_PROFILE"
      printf '\n  Start it with:\n\n      %s\n' "$(bench_engine_start_command)"
      printf '\n  Do NOT point this at colima-forgeron-build, colima-maqi-*, or any\n'
      printf '  other profile: those belong to real work, and a bench sharing a VM\n'
      printf '  with real work is a bench that will one day break it.\n'
    fi
    printf '\n  This is INCONCLUSIVE, not a pass and not a failure: the engine that\n'
    printf '  discriminates was unavailable, so nothing was measured.\n\n'
  } >&2
  exit 2
}

# bench_engine_fingerprint <context> — one line naming the engine actually
# used. Every capture must carry it: an evidence file whose engine cannot be
# read off it is the exact artefact this molecule had to go back and relabel.
bench_engine_fingerprint() {
  local ctx="$1"
  docker --context "$ctx" info \
    --format 'server={{.ServerVersion}} os={{.OperatingSystem}} arch={{.Architecture}} kernel={{.KernelVersion}}'
}
