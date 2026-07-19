#!/bin/sh
# check-install-drift.test.sh — prove the drift checker actually reddens on the
# REAL divergence that shipped, not merely that it passes on a happy path.
#
# A drift detector nobody has seen fail is indistinguishable from a detector
# that cannot fail. Both silent incidents happened under CI that was green, so
# the checker's *red* path is the thing under test here.
#
# Offline, no network, no published release: every case is a local fixture.
# Wired into .github/workflows/install-lint.yml so it runs on every PR.
#
#     scripts/release/check-install-drift.test.sh

set -eu

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH='' cd -- "${SCRIPT_DIR}/../.." && pwd)"
CHECK="${SCRIPT_DIR}/check-install-drift.sh"
CANONICAL="${REPO_ROOT}/infra/install/install.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

pass=0
fail=0

# Run the checker, assert the exit status, and assert stderr mentions a phrase.
# want_status: 0 = must pass, 1 = must red.
expect() {
    _name="$1"; _served="$2"; _want_status="$3"; _want_phrase="${4:-}"
    _out="${tmp}/out.$$"
    set +e
    sh "$CHECK" --served-file "$_served" --canonical "$CANONICAL" \
        > "$_out" 2>&1
    _got=$?
    set -e
    if [ "$_got" -ne "$_want_status" ]; then
        printf '✗ %s\n' "$_name"
        printf '  expected exit %s, got %s. Output:\n' "$_want_status" "$_got"
        sed 's/^/    /' "$_out"
        fail=$((fail + 1))
        return
    fi
    if [ -n "$_want_phrase" ] && ! grep -qF -- "$_want_phrase" "$_out"; then
        printf '✗ %s\n' "$_name"
        printf '  exit status %s was right, but output never said %s. Output:\n' \
            "$_got" "\"$_want_phrase\""
        sed 's/^/    /' "$_out"
        fail=$((fail + 1))
        return
    fi
    printf '✓ %s\n' "$_name"
    pass=$((pass + 1))
}

echo "── check-install-drift.sh ─────────────────────────────────────────────"

# ── 1. THE REAL INCIDENT ─────────────────────────────────────────────────────
# Reconstruct the divergence that is live right now: the served copy predates
# the connector, so it has zero references to cosmon-remote. Everything else
# about it is a faithful installer — it resolves a target, verifies SHA256SUMS,
# unpacks, and installs `cs` successfully. That is precisely why it passed
# unnoticed: it is not broken, it is INCOMPLETE, and the user is never told.
#
# Built by deleting the connector logic from the canonical source rather than
# by hand-writing a stub, so the fixture stays an accurate "canonical minus the
# capability" as install.sh evolves.
stale="${tmp}/served-stale-no-connector.sh"
# Delete the two connector blocks whole (placement, and the next-steps hint),
# then every remaining cosmon-remote mention in comments and docs. Deleting the
# blocks by range first matters: a bare line-delete would strip the `if` and
# leave its orphaned body behind, producing a broken script rather than the
# plausible-looking one that actually shipped.
# shellcheck disable=SC2016  # ${tmp}/${dir}/$HOME are literal text in the
# sed patterns below — they match install.sh's own source, not this shell's vars.
sed -e '/if \[ -f "${tmp}\/cosmon-remote" \]; then/,/^    fi$/d' \
    -e '/if \[ -x "${dir}\/cosmon-remote" \]; then/,/^    fi$/d' \
    -e '/cosmon-remote/d' \
    "$CANONICAL" > "$stale"

# Guard the fixture itself: it must really be connector-free, and it must still
# be a valid shell script (an incomplete installer, not a broken one).
if grep -qF 'cosmon-remote' "$stale"; then
    echo "✗ fixture bug: the stale fixture still mentions cosmon-remote"
    exit 1
fi
if ! sh -n "$stale"; then
    echo "✗ fixture bug: the stale fixture is not valid shell"
    exit 1
fi
# ...and it must still install cs, i.e. be the plausible-looking script that
# slipped through. Proven via the same file:// fixture seam CI's install job uses.
fixture="${tmp}/rel"; stub="${tmp}/stub"; dest="${tmp}/dest"
mkdir -p "$fixture" "$stub" "$dest"
# shellcheck disable=SC2016  # the stub's own $1 must reach the stub unexpanded
printf '#!/bin/sh\n[ "${1:-}" = "--version" ] && { echo "cs 9.9.9"; exit 0; }\nexit 0\n' > "${stub}/cs"
printf '#!/bin/sh\nexit 0\n' > "${stub}/cosmon-remote"
chmod +x "${stub}/cs" "${stub}/cosmon-remote"
target="$(COSMON_UNAME_S=Linux COSMON_UNAME_M=x86_64 sh "$CANONICAL" --print-target)"
tar -czf "${fixture}/cosmon-9.9.9-${target}.tar.gz" -C "$stub" cosmon-remote cs
if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$fixture" && sha256sum cosmon-*.tar.gz > SHA256SUMS )
else
    ( cd "$fixture" && shasum -a 256 cosmon-*.tar.gz > SHA256SUMS )
fi
if COSMON_UNAME_S=Linux COSMON_UNAME_M=x86_64 \
   COSMON_RELEASE_BASE_URL="file://${fixture}" \
   sh "$stale" --dir "$dest" >/dev/null 2>&1 && [ -x "${dest}/cs" ]; then
    if [ -e "${dest}/cosmon-remote" ]; then
        echo "✗ fixture bug: the stale fixture installed the connector after all"
        exit 1
    fi
    echo "✓ fixture is the real thing: installs cs fine, drops cosmon-remote silently"
    pass=$((pass + 1))
else
    echo "✗ fixture bug: the stale fixture failed to install cs (should succeed)"
    exit 1
fi

# THE load-bearing assertion. If this ever goes green, the detector is useless.
expect "reddens on the REAL divergence (served copy has no cosmon-remote)" \
    "$stale" 1 "SERVED INSTALLER IS STALE"
expect "…and names the missing capability by name" \
    "$stale" 1 "cosmon-remote"

# ── 2. the other historical incident: gnu instead of musl ────────────────────
gnu="${tmp}/served-gnu.sh"
sed -e 's/unknown-linux-musl/unknown-linux-gnu/g' "$CANONICAL" > "$gnu"
expect "reddens on the gnu→musl target regression" \
    "$gnu" 1 "SERVED INSTALLER IS STALE"

# ── 3. subtle drift no marker anticipated — identity must still catch it ─────
# Markers cover known capabilities; the byte-identity gate is what covers the
# unknown ones. A one-character change to the install dir is invisible to every
# marker and must still red.
subtle="${tmp}/served-subtle.sh"
# shellcheck disable=SC2016  # $HOME is literal text inside install.sh
sed -e 's|\$HOME/\.local/bin|$HOME/.local/sbin|' "$CANONICAL" > "$subtle"
expect "reddens on drift no marker anticipated (identity gate)" \
    "$subtle" 1 "HAS DRIFTED FROM SOURCE"

# ── 4. an outright wrong body (placeholder / coming-soon shim served as 200) ─
wrong="${tmp}/served-wrong.sh"
printf '#!/bin/sh\necho "coming soon"\nexit 1\n' > "$wrong"
expect "reddens when the endpoint serves something else entirely" \
    "$wrong" 1 "SERVED INSTALLER IS STALE"

empty="${tmp}/served-empty.sh"
: > "$empty"
expect "reddens on an empty served body" "$empty" 1 "empty"

# ── 5. the green path — an actually-current served copy passes ───────────────
current="${tmp}/served-current.sh"
cp "$CANONICAL" "$current"
expect "passes when served == source" "$current" 0 "no drift"

# ── 6. legitimate serving-time additions are NOT drift ───────────────────────
# The endpoint prepends a sanitized `COSMON_VERSION='<tag>'` line for `?version=`.
pinned="${tmp}/served-pinned.sh"
{ printf "COSMON_VERSION='v0.2.0'\n"; cat "$CANONICAL"; } > "$pinned"
expect "passes with the endpoint's version-pin line prepended" \
    "$pinned" 0 "no drift"

# CRLF rewriting by a CDN / object store changes no semantics.
crlf="${tmp}/served-crlf.sh"
awk '{ printf "%s\r\n", $0 }' "$CANONICAL" > "$crlf"
expect "passes when the transport rewrote line endings to CRLF" \
    "$crlf" 0 "no drift"

# But a pin line with an unsanitized value is NOT the endpoint's output, so it
# must be treated as drift rather than quietly stripped.
badpin="${tmp}/served-badpin.sh"
{ printf "COSMON_VERSION='v1.0; rm -rf /'\n"; cat "$CANONICAL"; } > "$badpin"
expect "reddens on an injected pin line the endpoint would never emit" \
    "$badpin" 1 "HAS DRIFTED FROM SOURCE"

# ── 7. the marker list cannot rot into a lie ─────────────────────────────────
# A marker naming something the canonical source no longer says must red, so a
# stale marker can never quietly stop covering anything.
rotten="${tmp}/markers-rotten.txt"
printf '# rotten\nthis-string-is-not-in-install-sh-at-all\n' > "$rotten"
set +e
sh "$CHECK" --served-file "$current" --canonical "$CANONICAL" \
    --markers "$rotten" > "${tmp}/rot.out" 2>&1
rot_status=$?
set -e
if [ "$rot_status" -ne 0 ] && grep -qF 'marker list is stale' "${tmp}/rot.out"; then
    echo "✓ reddens when a conformance marker no longer exists in the source"
    pass=$((pass + 1))
else
    echo "✗ a rotten marker list did not red (exit ${rot_status})"
    sed 's/^/    /' "${tmp}/rot.out"
    fail=$((fail + 1))
fi

echo "───────────────────────────────────────────────────────────────────────"
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
echo "The drift detector reddens on the real incident. It is load-bearing."
