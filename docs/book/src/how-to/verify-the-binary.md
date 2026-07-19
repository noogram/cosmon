# Verify the binary's provenance

> **One line.** Every cosmon release binary is reproducibly built, signed with a
> short-lived Sigstore certificate bound to the release workflow, and recorded in
> the public Rekor transparency log. You can check all three from your own
> machine, without having to trust "a download from the internet."

## Why this page exists

[Installing `cs`](../getting-started/install.md) checks that
the tarball you carried home weighs what the stall *said* it would weigh — the
`sha256` from the release's `SHA256SUMS`, served over TLS. That is real, and it
is fail-closed: a mismatch aborts the install. But it only proves the basket
matches the stall's label. It does **not** prove *who filled the basket*.

Cosign and Rekor are the greengrocer's signed, dated, publicly-posted receipt:
*"I, the release workflow, packed this exact basket on this exact day, and here
is the entry in the town ledger anyone can read."* You check the receipt once;
after that you trust the binary, not the download.

> **Be clear about the trust boundary.** The `curl … | sh` installer does the
> **sha256 leg only**. It does not verify signatures, and it does not need
> `cosign` installed. The cryptographic provenance proof below is **opt-in**: it
> is a separate step, it needs `cosign` (and, for Layer 3, `rekor-cli`) on your
> machine, and you run it when you want the stronger claim.

## What a release carries

For each platform target (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) the GitHub Release
carries:

| File | What it is |
|------|-----------|
| `cosmon-<v>-<target>.tar.gz`        | the `cs` binary, tarred (what the installer downloads) |
| `cosmon-<v>-<target>.tar.gz.sig`    | cosign signature over the tarball |
| `cosmon-<v>-<target>.tar.gz.pem`    | the short-lived signing certificate |
| `cosmon-<v>-<target>`               | the raw `cs` binary (so you can verify what's on your `PATH`) |
| `cosmon-<v>-<target>.sig` / `.pem`  | cosign signature + cert over the raw binary |
| `cosmon-<v>-<target>.spdx.json`     | SPDX SBOM (dependency closure) |
| `cosmon-service-<v>-<target>.tar.gz`         | the `cosmon-rpp-adapter` + `cs-oidc-mock` service binaries (see [Run cosmon as a remote service](./deploy-remote-service.md)) |
| `cosmon-service-<v>-<target>.tar.gz.sig` / `.pem` | cosign signature + cert over the service tarball |
| `SHA256SUMS`                        | one digest per shipped artifact |

## Verify — 3 layers, weakest to strongest

### Layer 1 — the sha256 check (you already have it)

The installer fails closed if the downloaded tarball's `sha256` does not match
the digest in the release's `SHA256SUMS`. You get this for free on every
install. It proves byte-integrity of the download against the release's own
manifest — not provenance. Layers 2 and 3 are what close that gap.

### Layer 2 — cosign signature (provenance)

Prove the binary on your `PATH` was signed by the cosmon release workflow:

```bash
v=0.1.0
target=aarch64-apple-darwin          # match your platform
base="https://github.com/noogram/cosmon/releases/download/v${v}"

curl -sSLO "${base}/cosmon-${v}-${target}"        # raw binary we signed
curl -sSLO "${base}/cosmon-${v}-${target}.sig"
curl -sSLO "${base}/cosmon-${v}-${target}.pem"

cosign verify-blob \
  --certificate-identity-regexp 'https://github.com/noogram/cosmon/.github/workflows/release.yml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature  "cosmon-${v}-${target}.sig" \
  --certificate "cosmon-${v}-${target}.pem" \
  "cosmon-${v}-${target}"
# Verified OK
```

`--certificate-identity-regexp` is the load-bearing pin: it asserts the
certificate's subject is the cosmon `release.yml` workflow, not some other
repo's workflow that happened to sign a blob. `--certificate-oidc-issuer` pins
the token issuer to GitHub Actions. Swap the org if you forked.

To verify the binary already sitting on your `PATH` (instead of the raw
download), point the last argument at it — it is byte-identical to the signed
binary inside the tarball:

```bash
cosign verify-blob \
  --certificate-identity-regexp 'https://github.com/noogram/cosmon/.github/workflows/release.yml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature  "cosmon-${v}-${target}.sig" \
  --certificate "cosmon-${v}-${target}.pem" \
  "$(which cs)"
```

### Layer 3 — Rekor transparency log (public, after-the-fact audit)

Keyless cosign uploads every signature to the public Rekor log by default, so
the signing event is permanently auditable even if you weren't watching when it
happened. Find the entry:

```bash
rekor-cli search --artifact "cosmon-${v}-${target}"
# or by the certificate:
rekor-cli search --pki-format=x509 --public-key "cosmon-${v}-${target}.pem"
```

`cosign verify-blob` already checks the Rekor inclusion proof during Layer 2;
Layer 3 is for an independent auditor who wants to read the log directly.

## Reproduce the build yourself (optional, strongest)

Because the build is reproducible (`--locked` + pinned `SOURCE_DATE_EPOCH` +
path remapping), you can rebuild the exact bytes and compare:

```bash
git clone https://github.com/noogram/cosmon && cd cosmon
git checkout v${v}
export SOURCE_DATE_EPOCH="$(git log -1 --pretty=%ct)"
export RUSTFLAGS="-C strip=symbols --remap-path-prefix=${PWD}=/cosmon --remap-path-prefix=${HOME}=/home"
cargo build --release --locked --bin cs --target ${target}
sha256sum target/${target}/release/cs
# compare against cosmon-${v}-${target}.bin.sha256 from the release
```

Identical digests prove the published binary is exactly what this source tree
compiles to — no hidden step between `git tag` and the bytes you run.

## If verification fails

A red `cosign verify-blob` or a sha256 mismatch is a **security signal, not a
nuisance**. Do not bypass it, do not `--insecure`, do not skip the digest. Stop,
and report it as an issue on the repository. A genuine release never fails these
checks; a failure means either a corrupted download or a tampered artifact.

## See also

- [Install cosmon](../getting-started/install.md) — installing `cs` in the first place.
- [Versioning policy](../explanation/versioning.md) — what a version tag promises.
