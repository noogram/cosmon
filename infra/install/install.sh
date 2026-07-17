#!/bin/sh
# install.sh — the public one-liner installer for cosmon's `cs` binary.
#
#     curl -fsSL https://noogram.org/cosmon/install.sh | sh
#
# This is the STRANGER path: a person with zero operator access installs and
# runs `cs` in one command. It is the mechanical projection of the one-gate
# rule (delib-20260711-8d00, Q4=Q5): a `/<tool>/install.sh` endpoint exists
# *iff* the tool ships a public per-platform binary. cosmon does; neurion, a
# private product with no public binary, never will.
#
# WHAT IT DOES, AND ONLY THIS (YAGNI):
#   1. detect the platform from `uname -s` / `uname -m` → one of the four
#      release targets (macos arm64/x64, linux x64/arm64);
#   2. resolve the release (latest, or a pin via $COSMON_VERSION / --version);
#   3. download the matching tarball + the release `SHA256SUMS` from the
#      GitHub Releases of the public repo (default: noogram/cosmon);
#   4. verify the tarball's sha256 against SHA256SUMS (fail closed on mismatch);
#   5. unpack, `chmod +x`, install `cs` into ~/.local/bin (fallback
#      /usr/local/bin), and print the PATH hint + next steps.
#
# It carries NO secret and needs NO privilege beyond writing to the install
# dir. Everything it downloads is a signed, publicly auditable release asset
# (cosign + Rekor; see docs/guides/release-verification.md for the deeper
# trust chain — this script does the sha256 leg, which is the one a stranger
# can check with tools already on the box).
#
# CONFIG (all optional; environment overrides):
#   COSMON_INSTALL_REPO     GitHub owner/repo to install from (default: noogram/cosmon)
#   COSMON_VERSION          pin a release tag, e.g. v0.1.0    (default: latest)
#   COSMON_INSTALL_DIR      where to put `cs`                 (default: ~/.local/bin)
#   COSMON_RELEASE_BASE_URL fetch SHA256SUMS + tarball from this base instead of
#                           github.com/<repo>/releases/... . A private release
#                           mirror, or a local fixture (`file://<dir>`) so CI can
#                           exercise the full resolve→verify→unpack→install path
#                           for every triple with no published release — see the
#                           `fixture` job in .github/workflows/install-lint.yml.
#   COSMON_UNAME_S          override `uname -s` (testing only)
#   COSMON_UNAME_M          override `uname -m` (testing only)
#
# FLAGS:
#   --version <tag>   same as COSMON_VERSION
#   --dir <path>      same as COSMON_INSTALL_DIR
#   --self-test       run the platform-detection table and exit (no network)
#   --print-target    print the resolved release target for THIS host and exit
#   -h | --help       usage
#
# STAGING: the public endpoint stays dark until the operator flips
# `noogram/cosmon` public and cuts the first tagged release. Until then the
# Cloudflare Worker in ./worker serves an honest 503 "coming soon" instead of
# this script. This file itself is always valid to run against any repo that
# already has a matching release (e.g. a private pre-flip smoke test via
# COSMON_INSTALL_REPO).

set -eu

# ── config (env, then overridden by flags below) ─────────────────────────────
REPO="${COSMON_INSTALL_REPO:-noogram/cosmon}"
VERSION="${COSMON_VERSION:-}"        # empty ⇒ latest
INSTALL_DIR="${COSMON_INSTALL_DIR:-}" # empty ⇒ resolved after arg parse
BASE_URL="${COSMON_RELEASE_BASE_URL:-}" # empty ⇒ github.com/<repo>/releases/...

# ── pretty output (fall back to plain if not a tty / no color) ───────────────
if [ -t 2 ] && [ -z "${NO_COLOR:-}" ]; then
    C_INFO='\033[1;36m'; C_OK='\033[1;32m'; C_WARN='\033[1;33m'
    C_ERR='\033[1;31m'; C_OFF='\033[0m'
else
    C_INFO=''; C_OK=''; C_WARN=''; C_ERR=''; C_OFF=''
fi
say()  { printf "${C_INFO}▸${C_OFF} %s\n" "$1" >&2; }
ok()   { printf "${C_OK}✓${C_OFF} %s\n" "$1" >&2; }
warn() { printf "${C_WARN}!${C_OFF} %s\n" "$1" >&2; }
die()  { printf "${C_ERR}✗${C_OFF} %s\n" "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat >&2 <<'USAGE'
cosmon installer — installs the `cs` binary from GitHub Releases.

  curl -fsSL https://noogram.org/cosmon/install.sh | sh

Options:
  --version <tag>   pin a release (e.g. v0.1.0); default: latest
  --dir <path>      install directory; default: ~/.local/bin (fallback /usr/local/bin)
  --self-test       run the platform-detection self-test and exit
  --print-target    print the release target for this host and exit
  -h, --help        show this help

Environment: COSMON_INSTALL_REPO, COSMON_VERSION, COSMON_INSTALL_DIR,
             COSMON_RELEASE_BASE_URL (mirror or file:// fixture base)
USAGE
}

# ── platform detection ───────────────────────────────────────────────────────
# Map (uname -s, uname -m) → the Rust target triple that names the release
# asset (`cosmon-<version>-<target>.tar.gz`). Overridable via COSMON_UNAME_*
# so the self-test can exercise the exact mapping shipped here (the same trick
# as the rpp-adapter installer's --derive-profile-name self-test).
detect_target() {
    _os="${COSMON_UNAME_S:-$(uname -s)}"
    _arch="${COSMON_UNAME_M:-$(uname -m)}"
    case "$_os" in
        Darwin) _os_part="apple-darwin" ;;
        Linux)  _os_part="unknown-linux-musl" ;;
        *) die "unsupported OS: $_os (cosmon ships macOS and Linux binaries)" ;;
    esac
    case "$_arch" in
        arm64|aarch64) _arch_part="aarch64" ;;
        x86_64|amd64)  _arch_part="x86_64" ;;
        *) die "unsupported architecture: $_arch (cosmon ships arm64 and x86_64)" ;;
    esac
    printf '%s-%s' "$_arch_part" "$_os_part"
}

# ── self-test — falsifiable table for the mapping above (no network) ─────────
# Each row: uname-s|uname-m|expected-target. Reverting a mapping line must
# redden this test (fixture-independence: the expected values are literals,
# not re-derived from detect_target).
self_test() {
    _fail=0
    for _row in \
        "Darwin|arm64|aarch64-apple-darwin" \
        "Darwin|aarch64|aarch64-apple-darwin" \
        "Darwin|x86_64|x86_64-apple-darwin" \
        "Linux|x86_64|x86_64-unknown-linux-musl" \
        "Linux|amd64|x86_64-unknown-linux-musl" \
        "Linux|aarch64|aarch64-unknown-linux-musl" \
        "Linux|arm64|aarch64-unknown-linux-musl"
    do
        _s=$(printf '%s' "$_row" | cut -d'|' -f1)
        _m=$(printf '%s' "$_row" | cut -d'|' -f2)
        _want=$(printf '%s' "$_row" | cut -d'|' -f3)
        _got=$(COSMON_UNAME_S="$_s" COSMON_UNAME_M="$_m" detect_target)
        if [ "$_got" = "$_want" ]; then
            ok "$_s/$_m → $_got"
        else
            warn "$_s/$_m → $_got (expected $_want)"
            _fail=1
        fi
    done
    [ "$_fail" -eq 0 ] || die "self-test failed"
    ok "self-test passed"
}

# ── download helper — curl or wget, always fail-closed ───────────────────────
fetch() { # fetch <url> <dest>
    case "$1" in
        # Local fixture / on-disk mirror (COSMON_RELEASE_BASE_URL=file://<dir>).
        # `cp` fails closed if the source is missing, exactly like a 404 would.
        # The `--proto '=https'` hardening below stays intact for every network
        # URL — this branch is only reached for an explicit file:// base.
        file://*)
            _src="${1#file://}"
            cp "$_src" "$2"
            ;;
        *)
            if have curl; then
                curl -fSL --proto '=https' --tlsv1.2 -o "$2" "$1"
            elif have wget; then
                wget -qO "$2" "$1"
            else
                die "need curl or wget to download"
            fi
            ;;
    esac
}

# ── checksum verify — fail closed on any doubt ───────────────────────────────
verify_sha256() { # verify_sha256 <file> <expected-hex>
    _file="$1"; _want="$2"
    if have sha256sum; then
        _got=$(sha256sum "$_file" | awk '{print $1}')
    elif have shasum; then
        _got=$(shasum -a 256 "$_file" | awk '{print $1}')
    else
        die "need sha256sum or shasum to verify the download"
    fi
    [ -n "$_want" ] || die "no checksum for this asset in SHA256SUMS"
    [ "$_got" = "$_want" ] || die "checksum mismatch — refusing to install (want $_want, got $_got)"
}

# ── main install ─────────────────────────────────────────────────────────────
main() {
    target="$(detect_target)"

    # Release base URL. An explicit COSMON_RELEASE_BASE_URL wins (private mirror
    # or a `file://` fixture); otherwise `latest/download/<asset>` resolves the
    # newest release and a pinned version uses the tag path. GitHub redirects
    # both to the asset.
    if [ -n "$BASE_URL" ]; then
        base="$BASE_URL"
        say "Installing cs (${target}) from ${base}"
    elif [ -n "$VERSION" ]; then
        case "$VERSION" in v*) tag="$VERSION" ;; *) tag="v$VERSION" ;; esac
        base="https://github.com/${REPO}/releases/download/${tag}"
        say "Installing cs ${tag} (${target}) from ${REPO}"
    else
        base="https://github.com/${REPO}/releases/latest/download"
        say "Installing cs latest (${target}) from ${REPO}"
    fi

    # The release asset names embed the version. For `latest` we don't know the
    # version string up front, so we fetch SHA256SUMS first and read the exact
    # tarball name for our target out of it — that file is the source of truth
    # for both the name and the hash.
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/cosmon-install.XXXXXX")" \
        || die "cannot create temp dir"
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" EXIT INT TERM

    say "Fetching SHA256SUMS"
    fetch "${base}/SHA256SUMS" "${tmp}/SHA256SUMS" \
        || die "no SHA256SUMS at ${base} — is there a published release yet?"

    # Pick the tarball line for our target (ignore the raw-binary lines).
    line="$(grep -E "  cosmon-[0-9][^ ]*-${target}\.tar\.gz\$" "${tmp}/SHA256SUMS" \
        | head -1 || true)"
    [ -n "$line" ] || die "no ${target} tarball in this release's SHA256SUMS"
    want_sha="$(printf '%s' "$line" | awk '{print $1}')"
    asset="$(printf '%s' "$line" | awk '{print $2}')"

    say "Downloading ${asset}"
    fetch "${base}/${asset}" "${tmp}/${asset}" \
        || die "failed to download ${asset}"

    say "Verifying checksum"
    verify_sha256 "${tmp}/${asset}" "$want_sha"
    ok "checksum ok"

    say "Unpacking"
    ( cd "$tmp" && tar -xzf "$asset" ) || die "failed to unpack ${asset}"
    [ -f "${tmp}/cs" ] || die "tarball did not contain the cs binary"
    chmod +x "${tmp}/cs"

    # Resolve install dir now (after flags/env). Prefer ~/.local/bin; if the
    # user pointed elsewhere, honor it. Create it if missing.
    dir="$INSTALL_DIR"
    if [ -z "$dir" ]; then dir="$HOME/.local/bin"; fi
    mkdir -p "$dir" 2>/dev/null || true
    if [ ! -w "$dir" ]; then
        if [ "$dir" = "$HOME/.local/bin" ] && [ -w /usr/local/bin ]; then
            warn "$dir not writable — falling back to /usr/local/bin"
            dir="/usr/local/bin"
        else
            die "install dir not writable: $dir (set COSMON_INSTALL_DIR or run with more privilege)"
        fi
    fi

    mv "${tmp}/cs" "${dir}/cs" || die "failed to move cs into ${dir}"
    ok "installed cs → ${dir}/cs"

    # PATH hint — only if the dir isn't already on PATH.
    case ":${PATH}:" in
        *":${dir}:"*) : ;;
        *)
            warn "${dir} is not on your PATH."
            # shellcheck disable=SC2016  # $PATH is meant to stay literal in the hint
            printf '  Add it, e.g.:  export PATH="%s:$PATH"\n' "$dir" >&2
            ;;
    esac

    printf '\n' >&2
    ok "Done. Next steps:"
    printf '    cs --version\n' >&2
    printf '    cs help guide      # what cosmon is, in five minutes\n' >&2
    printf '    cs nucleate --help # start your first molecule\n' >&2
}

# ── arg parse ────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        --version)      VERSION="${2:?--version needs a tag}"; shift 2 ;;
        --version=*)    VERSION="${1#*=}"; shift ;;
        --dir)          INSTALL_DIR="${2:?--dir needs a path}"; shift 2 ;;
        --dir=*)        INSTALL_DIR="${1#*=}"; shift ;;
        --self-test)    self_test; exit 0 ;;
        --print-target) detect_target; printf '\n'; exit 0 ;;
        -h|--help)      usage; exit 0 ;;
        *)              usage; die "unknown argument: $1" ;;
    esac
done

main
