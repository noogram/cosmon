#!/usr/bin/env bash
# scripts/ilb-demo-setup.sh — first-run initialization for the ILB demo.
#
# Executed once (by the container operator or the ENTRYPOINT wrapper)
# to stand up the solo-residence galaxy under /workspace/galaxy,
# generate a per-container Ed25519 operator key, and optionally seed
# the artifact-map preset when cs supports it (ADR-057, task-814b).
#
# Idempotent: every step skips its work cleanly on re-run.
#
# Usage (inside the container):
#   bash /opt/cosmon-ilb/scripts/ilb-demo-setup.sh
#
# Environment:
#   COSMON_ILB_GALAXY_DIR   default /workspace/galaxy
#   COSMON_ILB_KEY_PATH     default /home/researcher/.config/cosmon/operator.key
#   COSMON_ILB_ARTIFACT_MAP default bundled preset (see below)

set -euo pipefail

GALAXY_DIR="${COSMON_ILB_GALAXY_DIR:-/workspace/galaxy}"
KEY_PATH="${COSMON_ILB_KEY_PATH:-/home/researcher/.config/cosmon/operator.key}"
ART_MAP_PRESET="${COSMON_ILB_ARTIFACT_MAP:-/opt/cosmon-ilb/artifact-map.ilb.toml}"

narrate() { printf '\n\033[36m# [setup] %s\033[0m\n' "$*"; }
warn()    { printf '\033[33m[setup] warn:\033[0m %s\n' "$*" >&2; }

narrate "cs binary version"
cs --version

narrate "init galaxy at ${GALAXY_DIR}"
if [[ -d "${GALAXY_DIR}/.cosmon" ]]; then
    echo "  .cosmon/ already present — init is a no-op."
else
    cs init "${GALAXY_DIR}"
fi

cd "${GALAXY_DIR}"

# A solo residence is what keeps cosmon internals out of the researcher's
# git history. The migration is a no-op when already in solo residence.
narrate "migrate to solo residence (ADR-055)"
if cs migrate to --help 2>/dev/null | grep -q '^  solo'; then
    # cs init does NOT create a git repo for the galaxy; cs migrate to
    # solo supports --no-git for that case. We stay off git-side here.
    cs migrate to solo --no-git || true
else
    warn "cs migrate to solo not available — residence step skipped"
fi

# --- operator key (Ed25519) --------------------------------------------------
narrate "operator key at ${KEY_PATH}"
mkdir -p "$(dirname "${KEY_PATH}")"
if [[ -s "${KEY_PATH}" ]]; then
    echo "  key already present — re-using."
else
    # cs notarize expects a raw 32-byte Ed25519 secret or its 64-char hex
    # form. We generate 32 bytes of OS randomness and write the hex form:
    # portable, diffable, reviewable. A PEM-wrapped Ed25519 key from
    # openssl is NOT accepted by cs notarize in v0.
    python3 -c 'import os,sys; sys.stdout.write(os.urandom(32).hex())' \
        > "${KEY_PATH}.tmp" 2>/dev/null \
        || openssl rand -hex 32 | tr -d '\n' > "${KEY_PATH}.tmp"
    mv "${KEY_PATH}.tmp" "${KEY_PATH}"
fi
chmod 600 "${KEY_PATH}"

# --- artifact-map preset (optional, ADR-057 / task-814b) --------------------
narrate "artifact-map preset (optional)"
if cs --help 2>&1 | grep -qi 'artifacts '; then
    if [[ -f "${ART_MAP_PRESET}" ]]; then
        cp "${ART_MAP_PRESET}" "${GALAXY_DIR}/.cosmon/artifact-map.toml"
        echo "  preset installed at ${GALAXY_DIR}/.cosmon/artifact-map.toml"
    else
        warn "preset file not found at ${ART_MAP_PRESET} — skipped."
    fi
else
    warn "cs does not support artifact-map yet (task-814b not merged). Skipped."
    warn "Container runs in fallback mode: solo residence + notary only."
fi

# --- summary ----------------------------------------------------------------
pub_line=""
if command -v python3 >/dev/null 2>&1; then
    # We don't derive the Ed25519 pubkey here (would need the dalek math);
    # cs notarize prints operator_pubkey in its JSON output. Point the
    # operator at the scenario for the public-key disclosure step.
    pub_line="(pubkey disclosed after the first cs notarize run)"
fi

cat <<EOF

==== ILB demo setup complete ====
Galaxy directory : ${GALAXY_DIR}
Operator key     : ${KEY_PATH}  (mode 0600, Ed25519 hex)
Residence        : $(cs --config "${GALAXY_DIR}/.cosmon/config.toml" ensemble --json 2>/dev/null | jq -r '.residence // "unknown"' 2>/dev/null || echo "unknown")
Pubkey           : ${pub_line}

Next: place a topic in /workspace/input/topic.txt, or pass --topic via
      /opt/cosmon-ilb/scripts/ilb-demo-scenario.sh "your topic here".
EOF
