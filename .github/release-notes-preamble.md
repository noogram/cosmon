## Signed release

Every binary attached below is **reproducibly built** (`cargo build --release
--locked`, `SOURCE_DATE_EPOCH` pinned), **cosign-signed** (keyless, Sigstore),
and **anchored in the public Rekor transparency log**. An SPDX SBOM is attached
per target.

### Install (Homebrew)

```bash
brew install cosmon/tap/cosmon
cs --version
```

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
