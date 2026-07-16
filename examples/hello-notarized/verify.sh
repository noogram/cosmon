#!/usr/bin/env bash
# examples/hello-notarized/verify.sh
#
# Cryptographic verification of the hello-notarized release bundle.
# Reads certificate.json (a cosmon mint file) and checks the Ed25519
# signature against the signed content_hash.
#
# The signed bytes are the BLAKE3 of the canonical commitment, prefixed
# with the domain separator `cosmon-notary/v1/commitment\x00`. In the
# certificate emitted by `cs notarize`, that hash is inlined as
# `commitment.<canonical-json-then-hashed>` — but we do NOT recompute
# it here (canonical form is cosmon-specific). Instead we use the
# `content_hash` the binary reported, cross-checking that the bundle
# is internally consistent with the mint path.
#
# Tries python3 + cryptography first; falls back to openssl + a
# SubjectPublicKeyInfo synthesized from the raw pubkey.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CERT="${1:-${HERE}/.output/release/certificate.json}"

red()   { printf '\033[31m%s\033[0m\n' "$*" >&2; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }

[[ -s "${CERT}" ]] || { red "certificate not found: ${CERT}"; exit 2; }

# The cosmon mint stores the signature over the BLAKE3 of the canonical
# commitment. We do NOT re-run canonicalization here — that would
# duplicate cosmon-notary. Instead we pull `content_hash` from the
# Certificate itself (run.sh enriches the raw mint with this field).

CONTENT_HASH="$(jq -r '.content_hash // empty' "${CERT}")"
if [[ -z "${CONTENT_HASH}" ]]; then
    red "certificate has no \`content_hash\` field — was this built by hello-notarized/run.sh?"
    exit 2
fi

if python3 - "${CERT}" "${CONTENT_HASH}" <<'PY' 2>/dev/null
import json, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from cryptography.exceptions import InvalidSignature

cert_path, content_hash_hex = sys.argv[1], sys.argv[2]
cert = json.load(open(cert_path))

pub_hex = cert["commitment"]["operator_pubkey"]["bytes_hex"]
sig_hex = cert["signature"]["bytes_hex"]

pk = Ed25519PublicKey.from_public_bytes(bytes.fromhex(pub_hex))
try:
    pk.verify(bytes.fromhex(sig_hex), bytes.fromhex(content_hash_hex))
except InvalidSignature:
    print("INVALID — signature does NOT match content_hash under pubkey", file=sys.stderr)
    sys.exit(1)

print(f"OK — ed25519 signature verifies ({len(bytes.fromhex(sig_hex))}-byte sig over {len(bytes.fromhex(content_hash_hex))}-byte hash).")
PY
then
    exit 0
fi

red "python3/cryptography unavailable — trying openssl fallback."

command -v openssl >/dev/null 2>&1 || { red "openssl not found; cannot verify."; exit 2; }
command -v python3 >/dev/null 2>&1 || { red "python3 needed to assemble SPKI; cannot verify."; exit 2; }

python3 - "${CERT}" "${CONTENT_HASH}" <<'PY'
import json, sys
cert = json.load(open(sys.argv[1]))
content_hash_hex = sys.argv[2]
pub_hex = cert["commitment"]["operator_pubkey"]["bytes_hex"]
sig_hex = cert["signature"]["bytes_hex"]

pub_bytes = bytes.fromhex(pub_hex)
# RFC 8410 SubjectPublicKeyInfo for Ed25519 (hand-assembled DER):
spki = bytes([0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,
              0x03, 0x21, 0x00]) + pub_bytes
open("/tmp/hello_notarized_pub.der", "wb").write(spki)
open("/tmp/hello_notarized_msg.bin", "wb").write(bytes.fromhex(content_hash_hex))
open("/tmp/hello_notarized_sig.bin", "wb").write(bytes.fromhex(sig_hex))
PY

if openssl pkeyutl -verify \
        -pubin -inkey /tmp/hello_notarized_pub.der -keyform DER \
        -rawin -in /tmp/hello_notarized_msg.bin \
        -sigfile /tmp/hello_notarized_sig.bin 2>/dev/null
then
    green "OK — signature verifies (openssl ed25519)."
    exit 0
fi

red "INVALID — openssl rejected the signature."
exit 1
