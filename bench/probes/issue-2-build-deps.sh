#!/usr/bin/env bash
# Probe #2 — BUILD DEPS: from-source Linux/glibc build needs pkg-config +
# libdbus-1-dev (keyring v3 -> secret-service -> libdbus).
#
# This probe asserts:
#   WITHOUT pkg-config + libdbus-1-dev  -> the from-source CARGO build FAILS
#   WITH    pkg-config + libdbus-1-dev  -> the from-source CARGO build SUCCEEDS
#
# v1-bench BUG (fixed here): the old probe keyed each outcome on the DOCKER
# command exit code. But `docker build` fails non-zero for reasons that have
# NOTHING to do with the cargo compile — most notably image EXPORT errors like
# `no space left on device` at the final layer. That mislabels a full,
# successful cargo build as "build failed" (or vice-versa). We now key the
# verdict on the CARGO BUILD RESULT parsed from the build transcript, and treat
# docker-infrastructure failures (disk/export/daemon) as INCONCLUSIVE-infra —
# never as a cargo build verdict.
#
# Static portion (real, headless): confirm the native-dbus dependency chain
# exists in the pristine lockfile (keyring / secret-service / dbus / libdbus-sys
# / zbus), which is what pulls in the native libdbus requirement.

source "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

ID="issue-2-build-deps"
NAME="From-source Linux build requires pkg-config + libdbus-1-dev"
ADAPTER="build"

SRC="${1:-}"
CLEANUP=0
if [[ -z "$SRC" ]]; then
  SRC="$(mktemp -d)"; CLEANUP=1
  checkout_uut "$SRC"
fi

EVIDENCE="$EVIDENCE_OUT/$ID.txt"
: > "$EVIDENCE"
{
  echo "# Probe #2 build deps"
  echo "# unit-under-test: $COSMON_TAG"
  echo
} >> "$EVIDENCE"

# --- Static: dependency chain in the lockfile -------------------------------
echo "## static: keyring/secret-service/dbus chain in Cargo.lock" >> "$EVIDENCE"
CHAIN="$(rg -n "^name = \"(keyring|secret-service|dbus|libdbus-sys|zbus)\"" "$SRC/Cargo.lock" 2>/dev/null || true)"
if [[ -n "$CHAIN" ]]; then
  printf '%s\n' "$CHAIN" >> "$EVIDENCE"
  STATIC_CHAIN=1
else
  echo "  (native-dbus dependency crates not found in lockfile)" >> "$EVIDENCE"
  STATIC_CHAIN=0
fi
echo >> "$EVIDENCE"

# classify_build LOGFILE
# Inspect a docker/cargo build transcript and echo one of:
#   cargo-ok            cargo reached "Finished ... profile" (compile succeeded)
#   cargo-fail-deps     cargo failed on the native-dep signature (pkg-config/dbus)
#   cargo-fail-other    cargo failed for some other compile reason
#   infra-fail          non-cargo failure (docker export / no space / daemon)
#   unknown             no decisive marker
# The KEY point: the cargo result is read from the transcript, decoupled from
# the docker process exit code.
classify_build() {
  local logf="$1"
  if rg -q "no space left on device|failed to export image|failed to solve|cannot connect to the Docker daemon|failed to register layer" "$logf" 2>/dev/null; then
    # An infra failure may still sit *after* a successful cargo compile.
    if rg -q "Finished .*(release|dev).*profile|Compiling cosmon-cli" "$logf" 2>/dev/null \
       && ! rg -q "error: failed to run custom build command|error: linking with|pkg-config|libdbus" "$logf" 2>/dev/null; then
      echo "cargo-ok"; return
    fi
    echo "infra-fail"; return
  fi
  if rg -q "Finished .*(release|dev).*profile" "$logf" 2>/dev/null; then
    echo "cargo-ok"; return
  fi
  if rg -q "pkg-config|libdbus-1|Could not run \`pkg-config\`|The system library .* required|PKG_CONFIG" "$logf" 2>/dev/null; then
    echo "cargo-fail-deps"; return
  fi
  if rg -q "error: failed to run custom build command|error\[E[0-9]+\]|error: could not compile" "$logf" 2>/dev/null; then
    echo "cargo-fail-other"; return
  fi
  echo "unknown"
}

# --- Runtime: two Docker builds, verdict keyed on the CARGO result ----------
echo "## runtime: docker build WITHOUT vs WITH pkg-config + libdbus-1-dev" >> "$EVIDENCE"
NODEPS_CARGO="skipped"
DEPS_CARGO="skipped"
NODEPS_LOG="$OUT_DIR/build-nodeps.log"
DEPS_LOG="$OUT_DIR/build-deps.log"

if [[ "${BENCH_SKIP_DOCKER:-0}" == "1" ]]; then
  echo "  BENCH_SKIP_DOCKER=1 — docker build discrimination intentionally skipped (--static)." >> "$EVIDENCE"
elif has docker && docker info >/dev/null 2>&1; then
  CTX="$OUT_DIR/docker-context"
  stage_docker_context "$CTX"
  echo "### build WITHOUT deps (Dockerfile.nodeps) — cargo expected to FAIL on native deps" >> "$EVIDENCE"
  docker build -f "$CTX/Dockerfile.nodeps" -t cosmon-bench-nodeps "$CTX" > "$NODEPS_LOG" 2>&1 || true
  NODEPS_CARGO="$(classify_build "$NODEPS_LOG")"
  tail -30 "$NODEPS_LOG" >> "$EVIDENCE"
  echo "  => nodeps cargo result: $NODEPS_CARGO" >> "$EVIDENCE"

  echo "### build WITH deps (Dockerfile) — cargo expected to SUCCEED" >> "$EVIDENCE"
  docker build -f "$CTX/Dockerfile" -t cosmon-bench "$CTX" > "$DEPS_LOG" 2>&1 || true
  DEPS_CARGO="$(classify_build "$DEPS_LOG")"
  tail -30 "$DEPS_LOG" >> "$EVIDENCE"
  echo "  => deps cargo result: $DEPS_CARGO" >> "$EVIDENCE"
else
  echo "  docker unavailable — runtime build discrimination not run here." >> "$EVIDENCE"
fi
echo >> "$EVIDENCE"

SIG="static_chain=$STATIC_CHAIN nodeps_cargo=$NODEPS_CARGO deps_cargo=$DEPS_CARGO"

# Verdict keyed on the CARGO results, not docker exit codes.
if [[ "$NODEPS_CARGO" == "cargo-fail-deps" && "$DEPS_CARGO" == "cargo-ok" ]]; then
  VERDICT="RED"
  NOTE="Reproduced (keyed on cargo result): cargo build FAILS on the native-dep signature without pkg-config+libdbus-1-dev and SUCCEEDS with them. Lockfile chain present=$STATIC_CHAIN."
elif [[ "$NODEPS_CARGO" == "cargo-ok" && "$DEPS_CARGO" == "cargo-ok" ]]; then
  VERDICT="GREEN"
  NOTE="Defect does NOT reproduce: cargo build SUCCEEDS even WITHOUT pkg-config+libdbus — the native-dbus dependency appears to have been dropped/made-optional on this tree (lockfile chain present=$STATIC_CHAIN)."
elif [[ "$NODEPS_CARGO" == "infra-fail" || "$DEPS_CARGO" == "infra-fail" ]]; then
  VERDICT="INCONCLUSIVE"
  NOTE="Docker infrastructure failure (disk/export/daemon), NOT a cargo verdict (nodeps=$NODEPS_CARGO deps=$DEPS_CARGO). Free space (docker system prune) or run on a host with build capacity. Lockfile chain present=$STATIC_CHAIN."
elif [[ "$NODEPS_CARGO" == "skipped" ]]; then
  VERDICT="INCONCLUSIVE"
  if [[ "$STATIC_CHAIN" -eq 1 ]]; then
    NOTE="Native-dbus dependency chain confirmed in lockfile, but docker build discrimination not run here. Container path required to key the cargo build outcome."
  else
    NOTE="Neither runtime cargo builds nor the lockfile chain confirm the defect; re-map dependencies."
  fi
else
  VERDICT="INCONCLUSIVE"
  NOTE="Ambiguous cargo build outcomes (nodeps=$NODEPS_CARGO deps=$DEPS_CARGO); see evidence — the dependency story may have shifted."
fi

emit_probe "$ID" "$NAME" "$ADAPTER" "$VERDICT" "$SIG" "$EVIDENCE" "$NOTE"

[[ "$CLEANUP" -eq 1 ]] && rm -rf "$SRC"
exit 0
