#!/usr/bin/env bash
# docker-strace-test.sh — Tenant-Demo (tenant_auditor Doe) compatibility test.
#
# Builds Dockerfile.strace-test, runs the in-image strace sweep, extracts
# the *.trace files to /tmp/tenant_auditor-cosmon-traces[-v2]/, and produces a
# single tarball ready to send to Tenant-Demo.
#
# Usage (from repo root):
#   bash scripts/docker-strace-test.sh [--rebuild] [--full-lifecycle]
#                                      [--platform linux/amd64]
#
# Flags:
#   --rebuild           Force docker build (ignore cached image).
#   --full-lifecycle    Run the v2 sweep: end-to-end cosmon cycle
#                       (init → nucleate → tackle → observe → peek →
#                       done) on a throwaway fake-target git repo, with
#                       strace -f so forked children are captured. The
#                       output directory and tarball are suffixed -v2.
#   --platform <p>      Override docker platform. Default: linux/arm64
#                       (Apple Silicon native). Use linux/amd64 for the
#                       x86_64 passthrough Tenant-Demo may prefer.
#
# Output:
#   /tmp/tenant_auditor-cosmon-traces[-v2]/        — raw *.trace + SUMMARY.md
#                                            + README.md + binary/cs
#                                            (+ fake-target/ in v2 mode)
#   /tmp/tenant_auditor-cosmon-traces[-v2].tar.gz  — bundle to attach to the email
#
# ANTHROPIC_API_KEY is NOT required. If set in the host shell, it is
# forwarded into the container (useful for a future iteration where
# claude is actually installed). Without it, `cs tackle` still spawns
# tmux + git worktree under strace, which is the representative surface.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="cosmon-strace-test"

# Defaults — aarch64 native on Apple Silicon; operator can override with
# --platform linux/amd64 for x86_64. Tenant-Demo's pipeline may be arch-specific,
# so we make both paths easy.
PLATFORM="${PLATFORM:-linux/arm64}"

REBUILD=0
FULL_LIFECYCLE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rebuild)          REBUILD=1 ;;
        --full-lifecycle)   FULL_LIFECYCLE=1 ;;
        --platform)         shift; PLATFORM="$1" ;;
        --platform=*)       PLATFORM="${1#--platform=}" ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *)
            echo "warning: unknown flag '$1'" >&2
            ;;
    esac
    shift
done

# Suffix output artefacts so v1 and v2 bundles can coexist on disk.
if [[ "${FULL_LIFECYCLE}" == "1" ]]; then
    OUT_DIR="/tmp/tenant_auditor-cosmon-traces-v2"
    BUNDLE="/tmp/tenant_auditor-cosmon-traces-v2.tar.gz"
    MODE_LABEL="full-lifecycle (v2)"
else
    OUT_DIR="/tmp/tenant_auditor-cosmon-traces"
    BUNDLE="/tmp/tenant_auditor-cosmon-traces.tar.gz"
    MODE_LABEL="static-surface (v1)"
fi

cd "${REPO_ROOT}"

if [[ "${REBUILD}" == "1" ]] || ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
    echo "==> Building Docker image '${IMAGE}' for platform '${PLATFORM}' (Dockerfile.strace-test)..."
    DOCKER_BUILDKIT=1 docker build \
        --platform "${PLATFORM}" \
        -t "${IMAGE}" \
        -f Dockerfile.strace-test \
        .
else
    echo "==> Using cached image '${IMAGE}' (pass --rebuild to force)."
fi

echo "==> Preparing trace output directory: ${OUT_DIR}"
rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"

echo "==> Running strace sweep inside container — mode: ${MODE_LABEL}, platform: ${PLATFORM}"

# Forward ANTHROPIC_API_KEY only if it is set in the host shell; otherwise
# the container degrades gracefully (and SUMMARY.md notes the absence).
RUN_ENV=()
if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
    RUN_ENV+=(-e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")
fi
RUN_ENV+=(-e "FULL_LIFECYCLE=${FULL_LIFECYCLE}")

docker run --rm \
    --platform "${PLATFORM}" \
    "${RUN_ENV[@]}" \
    -v "${OUT_DIR}:/out" \
    "${IMAGE}"

echo "==> Trace files produced:"
ls -la "${OUT_DIR}"/*.trace "${OUT_DIR}/SUMMARY.md" "${OUT_DIR}/README.md" 2>/dev/null || true

echo "==> Packaging tarball: ${BUNDLE}"
rm -f "${BUNDLE}"
tar -czf "${BUNDLE}" -C /tmp "$(basename "${OUT_DIR}")"

BUNDLE_SIZE="$(stat -f%z "${BUNDLE}" 2>/dev/null || stat -c%s "${BUNDLE}" 2>/dev/null || echo '?')"

echo
echo "============================================================"
echo "READY TO SEND — ${MODE_LABEL}"
echo "  Bundle:   ${BUNDLE} (${BUNDLE_SIZE} bytes)"
echo "  Traces:   ${OUT_DIR}"
echo "  Platform: ${PLATFORM}"
echo "  Email:    docs/outbound/to-tenant_auditor-strace-response.md"
echo "============================================================"
