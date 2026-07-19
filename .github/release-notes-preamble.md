## Signed release

Every binary attached below is **reproducibly built** (`cargo build --release
--locked`, `SOURCE_DATE_EPOCH` pinned), **cosign-signed** (keyless, Sigstore),
and **anchored in the public Rekor transparency log**. An SPDX SBOM is attached
per target.

### Install

All three routes below deliver the **same signed bytes** — the tarballs
attached to this release. They cover macOS and Linux alike, on arm64 and
x86_64 (Linux builds are statically linked against musl, so no glibc version
to match).

**Native install script** (recommended, macOS + Linux):

```bash
curl -fsSL https://noogram.org/cosmon/install.sh | sh
cs --version
```

It detects your platform, downloads the matching tarball, and **fails closed**
if the sha256 does not match the published `SHA256SUMS`.

**Homebrew** (macOS + Linuxbrew):

```bash
brew install noogram/tap/cosmon
cs --version
```

The tap formula points at these very tarballs; `brew` checks the same digests.

**From source** (any platform with a Rust toolchain, MSRV 1.88):

```bash
cargo install --git https://github.com/noogram/cosmon.git --locked cosmon-cli
```

This compiles locally, so it produces *your* binary rather than the signed one
attached here — the verification steps below apply to the first two routes.

### Verify the binary you installed

```bash
cosign verify-blob \
  --certificate-identity-regexp 'https://github.com/.*/cosmon/.github/workflows/release.yml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature  cosmon-<version>-<target>.sig \
  --certificate cosmon-<version>-<target>.pem \
  "$(which cs)"
# exits 0 on a green signature
```

### Self-host the remote service

Each target also ships a `cosmon-service-<version>-<target>.tar.gz` bundle
containing the `cosmon-rpp-adapter` (HTTP fente) and `cs-oidc-mock` (demo IdP)
binaries — signed and Rekor-anchored like the `cs` tarball. The
[Run cosmon as a remote service](https://github.com/noogram/cosmon/blob/main/docs/book/src/how-to/deploy-remote-service.md)
how-to deploys directly from this bundle; no Rust toolchain required.

Full operator instructions:
[`docs/guides/release-verification.md`](https://github.com/noogram/cosmon/blob/main/docs/guides/release-verification.md).

---
