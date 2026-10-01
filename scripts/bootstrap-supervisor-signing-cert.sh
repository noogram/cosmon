#!/usr/bin/env bash
# Create the local code-signing certificate used by the daemon supervisor.
# Run once on the operator's Mac; this script never installs or restarts it.

set -euo pipefail

IDENTITY="${COSMON_SUPERVISOR_SIGNING_IDENTITY:-Cosmon Local Signing}"
KEYCHAIN="${HOME}/Library/Keychains/login.keychain-db"

if security find-identity -p codesigning 2>/dev/null | grep -qF "\"${IDENTITY}\""; then
    echo "bootstrap-supervisor-signing-cert: $IDENTITY already exists"
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
password="$(openssl rand -hex 12)"

openssl req -x509 -newkey rsa:2048 -keyout "$work/key.pem" -out "$work/cert.pem" \
    -days 3650 -nodes -subj "/CN=${IDENTITY}" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null
openssl pkcs12 -export -legacy -inkey "$work/key.pem" -in "$work/cert.pem" \
    -out "$work/cert.p12" -name "$IDENTITY" -passout "pass:${password}" 2>/dev/null
security import "$work/cert.p12" -k "$KEYCHAIN" -P "$password" -T /usr/bin/codesign

echo "bootstrap-supervisor-signing-cert: created $IDENTITY; reinstall the supervisor before granting Full Disk Access"
