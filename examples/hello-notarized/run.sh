#!/usr/bin/env bash
# examples/hello-notarized/run.sh
#
# End-to-end demonstration of the cosmon notarize-release loop on a toy
# artifact. No LLM calls. No Anthropic key. No research topic. Just the
# six verbs: init → nucleate → notarize (sign) → release.
#
# The cryptographic signature check lives in the sibling ./verify.sh
# because it requires python3 or openssl.
#
# Idempotent: each run starts from a fresh temp dir. The seed key is
# deterministic (all-zero bytes) so the pubkey is stable across runs —
# useful for teaching, NEVER use this key for anything real.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# The galaxy MUST live outside any enclosing cosmon project — `cs init`
# refuses to nest. When the user runs this example from inside the
# cosmon repo itself, that rule would trip; we default to a temp dir
# and expose a stable symlink back to HERE/.output for inspection.
if [[ -n "${HELLO_NOTARIZED_OUT:-}" ]]; then
    OUT="${HELLO_NOTARIZED_OUT}"
else
    OUT="$(mktemp -d -t hello-notarized-XXXXXXXX)"
fi
GALAXY="${OUT}/galaxy"
RELEASE="${OUT}/release"
KEY="${OUT}/operator.key"
FORMULA="${HERE}/hello.formula.toml"
SYMLINK="${HERE}/.output"
ARTIFACT_TEXT="${ARTIFACT_TEXT:-Hello, notarized world!}"

blue()  { printf '\033[36m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*" >&2; }
die()   { red "[hello-notarized] error: $*"; exit 2; }

command -v cs >/dev/null 2>&1 || die "cs binary not found on PATH — run 'just install' from the cosmon root, or add the target/release dir to PATH."
command -v jq >/dev/null 2>&1 || die "jq not found — required for reading the Certificate."

blue "# [hello-notarized] step 0 — reset ${OUT}"
rm -rf "${OUT}"
mkdir -p "${OUT}" "${RELEASE}"
# Replace any stale .output/ back-pointer (could be a dir or a symlink
# from a previous run).
rm -rf "${SYMLINK}"
ln -s "${OUT}" "${SYMLINK}"

blue "# [hello-notarized] step 1 — init a throwaway galaxy"
cs init "${GALAXY}" --json >/dev/null
mkdir -p "${GALAXY}/.cosmon/formulas"
cp "${FORMULA}" "${GALAXY}/.cosmon/formulas/hello.formula.toml"

blue "# [hello-notarized] step 2 — teaching key (32 zero bytes, hex-encoded)"
# A deterministic teaching key. The pubkey derived from the all-zero
# secret is a fixed 32-byte value — anyone can reproduce every byte of
# this demo. DO NOT copy this pattern for real operator keys; generate
# fresh randomness from the OS.
printf '0000000000000000000000000000000000000000000000000000000000000000' > "${KEY}"
chmod 600 "${KEY}"

blue "# [hello-notarized] step 3 — write the toy artifact"
ARTIFACT_FILE="${RELEASE}/hello.txt"
printf '%s\n' "${ARTIFACT_TEXT}" > "${ARTIFACT_FILE}"
echo "  wrote ${ARTIFACT_FILE} ($(wc -c <"${ARTIFACT_FILE}" | tr -d ' ') bytes)"

blue "# [hello-notarized] step 4 — nucleate the molecule that wraps the artifact"
# --no-parent so this demo stays hermetic even when the caller is itself
# inside a tackled cosmon worker (COSMON_PARENT_MOL_ID would otherwise
# try to auto-link us to a molecule that does not exist in the fresh
# galaxy).
NUC_OUT="$(cd "${GALAXY}" && cs nucleate hello --no-parent --var "artifact=${ARTIFACT_TEXT}" --json)"
MOL_ID="$(printf '%s' "${NUC_OUT}" | jq -r '.id // .molecule_id // empty')"
[[ -n "${MOL_ID}" ]] || die "nucleate returned no molecule id:\n${NUC_OUT}"
echo "  molecule: ${MOL_ID}"

blue "# [hello-notarized] step 5 — sign the commitment (mint)"
SIGN_OUT="${OUT}/notarize-out.json"
(cd "${GALAXY}" && cs notarize "${MOL_ID}" --key "${KEY}" --json) > "${SIGN_OUT}"
CONTENT_HASH="$(jq -r .content_hash "${SIGN_OUT}")"
MINT_PATH="$(jq -r .mint_path "${SIGN_OUT}")"
echo "  content_hash : ${CONTENT_HASH}"
echo "  mint file    : ${MINT_PATH}"

[[ -s "${MINT_PATH}" ]] || die "mint file missing at ${MINT_PATH}"

PUBKEY="$(jq -r .commitment.operator_pubkey.bytes_hex "${MINT_PATH}")"
SIG="$(jq -r .signature.bytes_hex "${MINT_PATH}")"
SEALED_AT="$(jq -r .sealed_at "${MINT_PATH}")"
echo "  pubkey       : ${PUBKEY}"
echo "  signature    : ${SIG:0:16}… (64 bytes, hex)"
echo "  sealed_at    : ${SEALED_AT}"

blue "# [hello-notarized] step 6 — release bundle"
CERT_FILE="${RELEASE}/certificate.json"
# The raw mint file does not inline `content_hash` (it is derivable
# from `commitment` via canonicalization + BLAKE3). For a bundle that
# a verifier can consume without re-running cosmon, we enrich the
# mint with the hash the CLI reported alongside a small
# `__hello_notarized__` stamp.
jq --arg ch "${CONTENT_HASH}" \
   '. + {content_hash: $ch, __hello_notarized__: {version: 1, note: "content_hash added by hello-notarized/run.sh; signature covers these bytes."}}' \
   "${MINT_PATH}" > "${CERT_FILE}"
echo "  built ${CERT_FILE}"

blue "# [hello-notarized] step 7 — cryptographic signature check"
# The demo is only useful if the release bundle actually verifies. Call
# the sibling verify.sh so the final green line is the cryptographic
# one, not a bash message.
if "${HERE}/verify.sh" "${CERT_FILE}"; then
    :
else
    die "signature check failed — bundle at ${RELEASE} is broken."
fi

blue "# [hello-notarized] step 8 — emit release manifest"
MANIFEST="${RELEASE}/MANIFEST.md"
{
    echo "# hello-notarized — release bundle"
    echo
    echo "- Molecule     : \`${MOL_ID}\`"
    echo "- Artifact     : \`hello.txt\` ($(wc -c <"${ARTIFACT_FILE}" | tr -d ' ') bytes)"
    echo "- Certificate  : \`certificate.json\`"
    echo "- Content hash : \`${CONTENT_HASH}\`"
    echo "- Operator key : \`${PUBKEY}\`"
    echo "- Sealed at    : \`${SEALED_AT}\`"
    echo
    echo "## What a verifier does"
    echo
    echo "1. Read \`certificate.json\`."
    echo "2. Compute \`ed25519_verify(pub=commitment.operator_pubkey.bytes_hex,"
    echo "                         msg=<content_hash_bytes>,"
    echo "                         sig=signature.bytes_hex)\`."
    echo "3. The \`content_hash\` being signed is the BLAKE3 of the canonical"
    echo "   commitment, prefixed with the domain separator"
    echo "   \`cosmon-notary/v1/commitment\\x00\` (see"
    echo "   \`crates/cosmon-notary/src/commitment.rs\`)."
    echo
    echo "The check proves: *this commitment was attested by this key at"
    echo "this moment*. Nothing more, nothing less."
} > "${MANIFEST}"
echo "  manifest → ${MANIFEST}"

cat <<EOF

==== hello-notarized OK ====
Release bundle at:
  ${RELEASE}/
    hello.txt           — the toy artifact
    certificate.json    — signed Ed25519 notarization (mint)
    MANIFEST.md         — what the bundle contains

Convenience symlink (inspection):
  ${SYMLINK}  ->  ${OUT}

Cryptographic signature check:
  ${HERE}/verify.sh

Run again: ./run.sh (idempotent, output dir is recreated each time).
EOF
