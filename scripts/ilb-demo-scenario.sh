#!/usr/bin/env bash
# scripts/ilb-demo-scenario.sh — rehearsable end-to-end scenario.
#
# Runs the full pilot cycle (nucleate → tackle → wait → done → notarize)
# on the solo-residence galaxy prepared by ilb-demo-setup.sh, exports
# the researcher-facing artefacts into /workspace/articles and
# /workspace/notarizations, and prints the verify incantation so the
# researcher can satisfy themselves the Certificate matches the article.
#
# Two input channels (mailbox, per delib-20260420-2839):
#   1. CLI arg:                        bash ilb-demo-scenario.sh "topic"
#   2. /workspace/input/topic.txt:     first non-empty line is the topic.
#
# If neither is present, falls back to the default M2 Itô topic.
#
# Environment:
#   COSMON_ILB_GALAXY_DIR   default /workspace/galaxy
#   COSMON_ILB_KEY_PATH     default /home/researcher/.config/cosmon/operator.key
#   COSMON_ILB_TIMEOUT_SEC  default 600
#   ANTHROPIC_API_KEY       REQUIRED for a live run (worker calls Claude)

set -euo pipefail

GALAXY_DIR="${COSMON_ILB_GALAXY_DIR:-/workspace/galaxy}"
KEY_PATH="${COSMON_ILB_KEY_PATH:-/home/researcher/.config/cosmon/operator.key}"
TIMEOUT_SEC="${COSMON_ILB_TIMEOUT_SEC:-600}"
INPUT_FILE="/workspace/input/topic.txt"
OUT_ARTICLES="/workspace/articles"
OUT_NOTARIZATIONS="/workspace/notarizations"
OUT_CHRONICLES="/workspace/chronicles"

DEFAULT_TOPIC="Écrire un article Wikipédia-style sur le lemme d'Itô en calcul stochastique, niveau Master 2 maths appliquées, 300-500 mots, avec formalisme mathématique LaTeX intégré."

narrate() { printf '\n\033[36m# [scenario] %s\033[0m\n' "$*"; }
warn()    { printf '\033[33m[scenario] warn:\033[0m %s\n' "$*" >&2; }
die()     { printf '\033[31m[scenario] error:\033[0m %s\n' "$*" >&2; exit 2; }

# --- 0. Resolve topic -------------------------------------------------------
TOPIC=""
if [[ $# -ge 1 && -n "${1:-}" ]]; then
    TOPIC="$1"
elif [[ -s "${INPUT_FILE}" ]]; then
    TOPIC="$(grep -m1 -v '^[[:space:]]*$' "${INPUT_FILE}" | head -c 2000)"
fi
if [[ -z "${TOPIC}" ]]; then
    TOPIC="${DEFAULT_TOPIC}"
    narrate "no topic provided — using default"
fi
narrate "topic: ${TOPIC}"

# --- 1. Pre-checks ----------------------------------------------------------
[[ -d "${GALAXY_DIR}/.cosmon" ]] || die "galaxy not initialized — run ilb-demo-setup.sh first."
[[ -s "${KEY_PATH}" ]]           || die "operator key missing at ${KEY_PATH} — run ilb-demo-setup.sh first."

mkdir -p "${OUT_ARTICLES}" "${OUT_NOTARIZATIONS}" "${OUT_CHRONICLES}"

cd "${GALAXY_DIR}"

# --- 2. Nucleate ------------------------------------------------------------
narrate "nucleate task-work"
NUC_OUT="$(cs nucleate task-work --var "topic=${TOPIC}" --json)"
MOL_ID="$(printf '%s' "${NUC_OUT}" | jq -r '.id // .molecule_id // empty')"
[[ -n "${MOL_ID}" ]] || die "nucleate returned no molecule_id:\n${NUC_OUT}"
echo "  molecule: ${MOL_ID}"

# --- 3. Tackle (background via tmux under the hood) -------------------------
narrate "tackle ${MOL_ID}"
if [[ -n "${COSMON_ILB_DRY_RUN:-}" ]]; then
    warn "COSMON_ILB_DRY_RUN set — skipping tackle/wait (dry-run mode)."
else
    cs tackle "${MOL_ID}" --no-worktree >/dev/null 2>&1 \
        || cs tackle "${MOL_ID}"
fi

# --- 4. Wait ----------------------------------------------------------------
if [[ -z "${COSMON_ILB_DRY_RUN:-}" ]]; then
    narrate "wait ${MOL_ID} (timeout ${TIMEOUT_SEC}s)"
    cs wait "${MOL_ID}" --timeout "${TIMEOUT_SEC}" --quiet \
        || warn "wait returned non-zero — molecule may have collapsed"
fi

# --- 5. Done (teardown) -----------------------------------------------------
if [[ -z "${COSMON_ILB_DRY_RUN:-}" ]]; then
    narrate "done ${MOL_ID}"
    cs done "${MOL_ID}" --if-completed >/dev/null 2>&1 || true
fi

# --- 6. Export article ------------------------------------------------------
narrate "export article → ${OUT_ARTICLES}/${MOL_ID}.md"
MOL_DIR_CANDIDATES=(
    "${GALAXY_DIR}/.cosmon/state/fleets/default/molecules/${MOL_ID}"
    "${GALAXY_DIR}/.cosmon/state/fleets/default/molecules/active/${MOL_ID}"
    "${GALAXY_DIR}/.cosmon/state/fleets/default/molecules/pending/${MOL_ID}"
)
ARTICLE_SRC=""
for candidate in "${MOL_DIR_CANDIDATES[@]}"; do
    for name in synthesis.md response.md article.md frame.md; do
        if [[ -s "${candidate}/${name}" ]]; then
            ARTICLE_SRC="${candidate}/${name}"
            break 2
        fi
    done
done

if [[ -z "${ARTICLE_SRC}" ]]; then
    # Last resort: archived molecule after cs done.
    ARTICLE_SRC="$(find "${GALAXY_DIR}/.cosmon/state/archive" -type f \
        \( -name 'synthesis.md' -o -name 'response.md' -o -name 'article.md' \) \
        -path "*${MOL_ID}*" 2>/dev/null | head -1 || true)"
fi

if [[ -n "${ARTICLE_SRC}" && -s "${ARTICLE_SRC}" ]]; then
    cp "${ARTICLE_SRC}" "${OUT_ARTICLES}/${MOL_ID}.md"
    echo "  ${ARTICLE_SRC} → ${OUT_ARTICLES}/${MOL_ID}.md"
else
    warn "no synthesis/response/article artefact found — writing stub"
    cat >"${OUT_ARTICLES}/${MOL_ID}.md" <<EOF
# ${MOL_ID}

Topic: ${TOPIC}

(No cognitive artefact was produced. This is expected in dry-run mode
or when the worker timed out. Inspect cs observe ${MOL_ID} for details.)
EOF
fi

# --- 7. Notarize ------------------------------------------------------------
narrate "notarize ${MOL_ID}"
NOT_OUT="${OUT_NOTARIZATIONS}/${MOL_ID}.json"
if cs notarize "${MOL_ID}" --key "${KEY_PATH}" --json >"${NOT_OUT}.tmp" 2>&1; then
    mv "${NOT_OUT}.tmp" "${NOT_OUT}"
    echo "  certificate → ${NOT_OUT}"
else
    warn "cs notarize failed (this is fine in --dry-run mode):"
    cat "${NOT_OUT}.tmp" >&2 || true
    mv "${NOT_OUT}.tmp" "${NOT_OUT}" 2>/dev/null || true
fi

# --- 8. Chronicle (local audit trail, solo residence) ----------------------
CHRON="${OUT_CHRONICLES}/${MOL_ID}.md"
{
    echo "# Chronicle — ${MOL_ID}"
    echo
    echo "Date : $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "Topic: ${TOPIC}"
    echo
    echo "## Artefacts"
    echo
    echo "- Article       : /workspace/articles/${MOL_ID}.md"
    echo "- Notarization  : ${NOT_OUT}"
    echo
    echo "## Observe"
    echo
    cs observe "${MOL_ID}" --json 2>/dev/null | jq '{status,formula,steps,created_at}' 2>/dev/null \
        || echo "(cs observe unavailable — molecule may be archived)"
} >"${CHRON}"
echo "  chronicle → ${CHRON}"

# --- 9. Artifact audit (optional, ADR-057 / task-814b) ---------------------
if cs --help 2>&1 | grep -qi 'artifacts '; then
    narrate "cs artifacts audit"
    cs artifacts audit || warn "artifact-map audit surfaced anomalies"
else
    warn "cs artifacts audit not available (task-814b fallback)."
fi

# --- 10. Summary ------------------------------------------------------------
cat <<EOF

==== ILB demo scenario complete ====
Molecule       : ${MOL_ID}
Article        : ${OUT_ARTICLES}/${MOL_ID}.md ($(wc -c <"${OUT_ARTICLES}/${MOL_ID}.md" 2>/dev/null || echo 0) bytes)
Notarization   : ${NOT_OUT}
Chronicle      : ${CHRON}

Verify the attestation:
  cs notarize ${MOL_ID} --key ${KEY_PATH} --dry-run --json | jq .seal_hex
  jq .seal_hex ${NOT_OUT}
  # — both values MUST match.
EOF
