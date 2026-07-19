# Release verification — proving the `cs` you run is the one we built

> **One line.** Every cosmon release binary is reproducibly built, signed with
> a short-lived Sigstore certificate bound to the release workflow, and recorded
> in the public Rekor transparency log. You can verify all three from your own
> machine, offline of any trust in "a download from the internet."

This closes gap **G3** of the deployment-scenarios panel
(`delib-20260420-a631`): without it, the 30-second `brew install` is a lie —
the trust chain terminates at a tarball on a CDN. With it, the chain terminates
at *"GitHub Actions, running the cosmon `release.yml` workflow at tag `vX.Y.Z`,
produced these exact bytes, and the world can see the receipt in Rekor."*

## The garden image

`brew install` checks that the basket you carried home weighs what the stall
*said* it would weigh (the `sha256` pinned in the formula, served over TLS).
That is good — but it only proves the basket matches the stall's label. It does
**not** prove *who filled the basket*. Cosign + Rekor are the greengrocer's
signed, dated, publicly-posted receipt: *"I, the release workflow, packed this
exact basket on this exact day, and here is the entry in the town ledger anyone
can read."* You verify the receipt once; after that you trust the binary, not
the download.

## What you get on a release

For each platform target (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-musl`) the GitHub Release carries:

| File | What it is |
|------|-----------|
| `cosmon-<v>-<target>.tar.gz`        | the client tarball: the `cs` binary **and** `cosmon-remote` (what brew + install.sh download) |
| `cosmon-<v>-<target>.tar.gz.sig`    | cosign signature over the tarball |
| `cosmon-<v>-<target>.tar.gz.pem`    | the short-lived signing certificate |
| `cosmon-<v>-<target>`               | the raw `cs` binary (so you can verify what's on your PATH) |
| `cosmon-<v>-<target>.sig` / `.pem`  | cosign signature + cert over the raw binary |
| `cosmon-remote-<v>-<target>`        | the raw `cosmon-remote` connector binary (verify the client too) |
| `cosmon-remote-<v>-<target>.sig` / `.pem` | cosign signature + cert over the raw connector binary |
| `cosmon-<v>-<target>.spdx.json`     | SPDX SBOM (dependency closure) |
| `SHA256SUMS`                        | one digest per shipped artifact |

## Verify (3 layers, weakest to strongest)

### Layer 1 — Homebrew already did the sha256 check

`brew install noogram/tap/cosmon` fails the install if the downloaded tarball's
`sha256` does not match the digest pinned in the formula. That digest came from
the signed release. You get this for free — but it only proves byte-integrity
of the download against the formula, not provenance.

### Layer 2 — cosign signature (provenance)

Prove the binary on your PATH was signed by the cosmon release workflow:

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

To verify the binary already installed by brew (instead of the raw download),
point the last argument at it — it is byte-identical to the signed binary
inside the tarball:

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
nuisance**. Do not bypass it, do not `--insecure`, do not delete the formula's
`sha256`. Stop, and report it (an issue on the repo, or to the operator). A
genuine release never fails these checks; a failure means either a corrupted
download or a tampered artifact.

## Relation to the L1 operator-key track

This guide covers the **public, keyless** release track (§9 G3): CI signs with a
short-lived Sigstore certificate, no key custody, trust root = *"the cosmon
`release.yml` workflow at this tag"*. There is a **second, separate** track —
L1 ([`dist/l1-brew-tap/`](../../dist/l1-brew-tap/), [ADR-084](../adr/084-release-signing-cosign-piv.md))
— where the operator signs each release with a YubiKey-bound key (minisign →
cosign-PIV) for tenant_auditor and peer auditors. Different formula (`cs.rb` vs
`cosmon.rb`), different tap, different trust model. Pick the track that matches
who is verifying: the public Rekor-anchored chain for the world, the
operator-key chain for a named auditor who wants a key they can pin.

## See also

- [`docs/book/src/how-to/verify-the-binary.md`](../book/src/how-to/verify-the-binary.md)
  — the reader-facing projection of this guide, published on docs.noogram.org.
  Keep the two in step: this file carries the internal lineage (G3, the L1
  track), the book page carries only what a stranger needs.
- [`deployment-scenarios.md`](deployment-scenarios.md) §9 G3 — the gap this closes.
- [`release-resync.md`](release-resync.md) — projecting the public repo the
  release is cut from.
- [`.github/workflows/release.yml`](../../.github/workflows/release.yml) — the
  pipeline that produces and signs these artifacts.
- [`packaging/homebrew-tap/`](../../packaging/homebrew-tap/) — the tap scaffold.
