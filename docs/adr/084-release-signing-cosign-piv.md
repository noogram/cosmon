# ADR-084 — Release signing: minisign-on-disk → cosign + PIV YubiKey

**Status:** Proposed (2026-05-04)
**Decider:** Noogram
**Origin:** a pre-release signing review that identified the disk-bound key as
the single weakest link in the release-signing chain.
**Supersedes (partially):** the *"Why minisign and not cosign"*
section of [`dist/l1-brew-tap/README.md`](../../dist/l1-brew-tap/README.md)
(L1 V0 rationale is preserved as historical record; this ADR governs
L2 onward).
**Adjacent:** [ADR-076](076-cs-security-binary-posture.md),
[ADR-077](077-worker-pilot-signing-regime.md),
[ADR-080](080-remote-pilot-port-https-oidc.md).

---

## 1. Context

The L1 brew-tap distribution of `cs` (`dist/l1-brew-tap/`) ships
binaries signed with **minisign** using a passphrase-protected secret
key on the operator's disk (`~/.config/cosmon/minisign.key`). The
brew formula `Formula/cs.rb` runs `minisign -V` at install time
against the operator's pubkey embedded verbatim in the formula.

The V0 threat model assumes:

- The signing key file is readable only by the operator (Unix `0600`).
- The passphrase is entered on a trusted machine.
- Distribution is private (restricted tap, authorized tenant auditors).

This is acceptable for the V0 example-tenant deliverable (a single-laptop
install at one site). It has two structural weaknesses:

1. **Disk + passphrase compromise = key compromise.** If an attacker
   obtains both the encrypted key file (e.g. by exfiltrating
   `~/.config/cosmon/`) and the passphrase (keylogger, shoulder-surf,
   compromised dev machine), they can sign arbitrary binaries that
   pass `brew install cs` verify.
2. **No physical-presence requirement.** A signature can be produced
   by any background process on the operator's laptop that has read
   access to the key file and stdin to type the passphrase. There is
   no "operator was physically present at sign time" claim attached
   to a release.

These match exactly the *"un seul maillon faible suffit"* class
identified by the security review for worker-side signing and re-flagged by
the L1 distribution findings sweep. The trigger for migration was observed.

## 2. Decision

Migrate release signing from **minisign-on-disk** to **cosign +
PIV YubiKey**, in three phased regimes implemented and gated by
[`dist/l1-brew-tap/scripts/release-l1.sh`](../../dist/l1-brew-tap/scripts/release-l1.sh):

| Regime | Sign-side                              | Verify-side (`Formula/cs.rb`)              | When |
|--------|----------------------------------------|--------------------------------------------|------|
| **L1** (V0)  | `minisign -S` with key on disk        | `minisign -V` with embedded `MINISIGN_PUBKEY`         | Until L1.5 lands. |
| **L1.5** | Both: `minisign -S` **and** `cosign sign-blob --key pkcs11:...` | Try `cosign verify-blob` first, fall back to `minisign -V` | 1-2 release window for transition. |
| **L2**   | `cosign sign-blob --key pkcs11:...` only | `cosign verify-blob` only                     | After L1.5 closes and a tenant auditor confirms the cosign install path. |

Concrete instantiation:

1. **Operator-side bootstrap** (one-time per YubiKey) is documented
   in [`COSIGN-PIV-BOOTSTRAP.md`](../../dist/l1-brew-tap/COSIGN-PIV-BOOTSTRAP.md):
   `ykman piv keys generate` → `ykman piv certificates generate` →
   export pubkey to `~/.config/cosmon/cosign.pub` → reference via
   PKCS#11 URI `pkcs11:object=cosmon-release-signing?module-path=/opt/homebrew/lib/libykcs11.dylib`.
2. **PIN policy ONCE, touch policy ALWAYS.** Each `cosign sign-blob`
   call requires a physical touch on the YubiKey button; the PIN is
   prompted once per session by the PKCS#11 module.
3. **The brew formula embeds the cosign PEM pubkey body** (single-line
   base64 between `BEGIN PUBLIC KEY` / `END PUBLIC KEY`). At install
   time, the formula reconstructs the PEM in a tempfile and calls
   `cosign verify-blob --key <tempfile> --signature <sidecar.sig> <binary>`.
4. **No Sigstore transparency log (Rekor) and no Fulcio CA.** We use
   cosign's *raw-key* verify mode (`--key cosign.pub`), explicitly
   not its OIDC-keyless mode (`--certificate-identity ...`). The L2
   distribution is not public-internet-facing; pinning a public
   transparency log would add a vendor dependency and a privacy leak
   without strengthening the model under our threat surface.
5. **`Formula/cs.rb` carries a `REGIME` constant** (`L1` / `L1.5` /
   `L2`) that switches the install-step verifier. The same template
   formula serves all three regimes; `render-formula.sh` substitutes
   the value at release time.
6. **`release-l1.sh` auto-detects the regime** from the available
   keys (minisign secret key on disk → L1; cosign pubkey present + no
   minisign key → L2; both → L1.5). `FORCE_REGIME=...` overrides.

### 2.1 What changes on the operator's machine

| Concern | L1 (V0) | L2 |
|---------|---------|-----|
| Signing key location | `~/.config/cosmon/minisign.key` (encrypted file) | YubiKey PIV slot 9c |
| Sign gesture per release | `minisign -S` × N + passphrase 1× | `cosign sign-blob` × N + PIN 1× + touch × N |
| Compromised disk → forgery | Possible (disk + passphrase observable) | Impossible (private key never leaves YubiKey) |
| Rotation | Regenerate keypair on disk, commit pubkey diff | Provision new YubiKey, commit pubkey diff |
| CI signing automation | Possible (not implemented V0) | **Refused** — physical presence required |

### 2.2 What this regime does **not** do

- **Does not enable Sigstore keyless / OIDC.** We refuse the public
  Rekor transparency log and Fulcio CA. cosign is used as a raw-key
  verifier. (See §3 alternative *cosign keyless / sigstore-as-a-service*.)
- **Does not enable CI release automation.** The release pipeline
  becomes operator-attended by construction. CI publishing of an
  *unsigned* artifact, followed by an offline re-sign + replace, is
  acceptable. CI signing itself is not.
- **Does not change the worker→pilot signing regime** (ADR-077).
  Workers still produce unsigned commits on worktree branches; the
  pilot still signs the merge to `main`. ADR-084 is about *binary
  release signing*, not about *git commit signing*.
- **Does not require the tenant auditor to do anything new.** The brew formula
  changes (`depends_on "cosign"` instead of `"minisign"`); `brew
  install cs` Just Works during the L1.5 window because the formula
  still understands `.minisig` as a fallback.
- **Does not impose a single-vendor lock-in to YubiKey.** Any
  PIV-compatible smartcard accessible via `libykcs11.dylib` (or
  another PKCS#11 module) works. We document the YubiKey 5C nano
  path because that is what the operator owns.

## 3. Alternatives rejected

### (a) Stay on minisign-on-disk indefinitely

**Rejected.** The V0 review criteria listed three triggers, any of which would
cause migration:
1. Phase 2 example-tenant scale-up (multiple auditors in regular use,
   with a real tenant blast radius if compromised).
2. External party explicitly requests hardware-attestation for release
   signature.
3. First near-miss on the on-disk key.

The pre-release finding sweep flagged the on-disk key as the
single weakest link in the chain — that is functionally trigger (3)
("first near-miss surfaced via audit, not via post-incident"). The
window for "stay on V0" closed.

### (b) cosign keyless (Sigstore OIDC + Rekor)

cosign supports a keyless flow where the operator authenticates with
OIDC (GitHub, Google, …), Fulcio issues a short-lived cert, and the
signing event is anchored in the Rekor public transparency log.

**Rejected** because:
- Adds Fulcio + Rekor as runtime dependencies of `brew install cs`.
  The verify step on the auditor's machine would need to talk to
  `rekor.sigstore.dev` to confirm the signature is in the log,
  introducing an internet round-trip and a privacy leak (every
  `brew install cs` becomes an example-tenant telemetry event visible
  to Sigstore).
- Couples the cosmon release lifetime to Sigstore's uptime / policy
  decisions. We do not want a third-party log to be a single point
  of failure for the L2 distribution.
- The threat model L2 needs to close (offline private-key forgery)
  is solved by hardware key, not by transparency log.

The two are independent dimensions: hardware key + raw-key verify
gives us the property we want without buying into the public-log
architecture. If a future use-case demands public attestation (open
source distribution, supply-chain SBOM), Sigstore-keyless is the
right tool *added on top of* the PIV signing, not replacing it.

### (c) Pure GPG smart-card (OpenPGP applet on YubiKey)

The YubiKey OpenPGP applet supports `gpg --sign` with the private key
hardware-resident.

**Rejected** because:
- GPG's signature format has no clean integration with brew formulas
  (`gpg --verify` works but the signature format is verbose and
  doesn't map to a sidecar workflow as cleanly as cosign's `.sig`).
- GPG keyrings + trustdb introduce complexity and surprise surfaces
  on tenant auditor's machine. cosign's `verify-blob --key <pem>` is one
  call against a single PEM file.
- The operator already runs Ed25519 keys for everything else; ECCP256
  via PIV slots aligns better with the cosign + brew formula vocabulary.

### (d) `signify` (OpenBSD Ed25519 signer)

Equivalent to minisign in audit surface and ergonomics; different
sigfile format.

**Rejected** because hardware-bound is not part of `signify`'s
threat model. `signify` would replace minisign in L1 V0 without
changing the disk-bound key property; that is not the migration we
need.

### (e) Hardware token via `age-plugin-yubikey` for signing

`age-plugin-yubikey` provides hardware-bound *encryption* (X25519
ECDH), not *signing*. It does not solve this problem.

### (f) Build-time SBOM + reproducible builds + checksum-only verify

Drop signature entirely; rely on reproducible builds and SBOM
attestation for binary integrity.

**Rejected** for L2: reproducible builds are an L3 goal (see
[`dist/l1-brew-tap/README.md`](../../dist/l1-brew-tap/README.md)
*"Open questions for L2"*) and orthogonal to signing. They prove
*"this binary matches this source"*, not *"this binary was authorized
by the operator"*. The L2 threat model requires the latter.

## 4. Consequences

### 4.1 Operational

- **Each release is operator-attended.** No more `release-l1.sh`
  unattended in the background. Roughly 3–6 touches per release
  (one per binary in the build matrix). Adds ~10 seconds of operator
  attention per release; conservatively 1 release/week → 10 min/year
  of cumulative attention. Acceptable.
- **CI release publishing decoupled from signing.** A future CI
  pipeline can build and publish *unsigned* binaries to a staging
  area; the operator pulls them down, signs locally, replaces them in
  the release. This preserves the separation between "build correctness
  is automatable" and "signing requires presence".
- **Recovery cost.** Losing the YubiKey loses the signing capability.
  Mitigation: provision two YubiKeys (primary + backup), each with its
  own slot 9c; embed both pubkeys in the formula in L2.1 (out of scope
  for this ADR; tracked in `dist/l1-brew-tap/COSIGN-PIV-BOOTSTRAP.md`
  §10).

### 4.2 Auditor-facing

- tenant auditor's `brew install cs` flow is **unchanged in semantics**: she
  taps `brew install cs`, the formula verifies, the binary lands.
  Underlying tool changes from `minisign` to `cosign`; she may be
  prompted by brew to install `cosign` (one-shot dependency).
- The `verify-cs-binary.sh` auditor script accepts both signature
  types. During L1.5, an auditor with only `minisign` installed
  still gets the verify path.

### 4.3 Composability

- The signing surface respects the cosmon "control plane / data
  plane" separation: the YubiKey gesture is operator-side
  (out-of-band of cosmon's filesystem state), the resulting `.sig`
  sidecar is data-plane (filesystem alongside the binary). Cosmon
  itself does not need a signing primitive — it consumes the artifacts.
- This ADR does not touch `cs notarize` (ADR-056/059/060 family),
  which uses its own signing scheme for cognitive-artifact seals.
  Release-binary signing (this ADR) and notary signing (ADR-056)
  are orthogonal channels with separate keys. The operator may use
  the same YubiKey for both with separate PIV slots.

## 5. Open questions

- **Slot collision with age-plugin-yubikey.** If the operator already
  uses slot 9c for `age-plugin-yubikey` (encryption of `.cosmon/`
  config), the cosign signing key must live in another slot (9a, 9d,
  or 9e). Documented in
  [`COSIGN-PIV-BOOTSTRAP.md`](../../dist/l1-brew-tap/COSIGN-PIV-BOOTSTRAP.md)
  §1.
- **Multi-YubiKey rotation.** L2.1 (out of scope here): embed two
  pubkeys in the formula and have `verify-blob` accept either. Tracked
  in the bootstrap doc §9 / §10.
- **Pubkey publication channel.** The pubkey lives in the brew
  formula (verbatim, reviewable in the diff). Should it also be
  cross-published in the cosmon repo README, or does that introduce a
  divergence risk? Currently: tap formula is canonical;
  cross-publication is best-effort.

## 6. References

- This ADR and the linked scripts form the public implementation record.
- [`dist/l1-brew-tap/COSIGN-PIV-BOOTSTRAP.md`](../../dist/l1-brew-tap/COSIGN-PIV-BOOTSTRAP.md)
  — operator-side bootstrap procedure.
- [`dist/l1-brew-tap/scripts/release-l1.sh`](../../dist/l1-brew-tap/scripts/release-l1.sh)
  — release-cutting script (regime auto-detect).
- [`dist/l1-brew-tap/homebrew-tap/Formula/cs.rb`](../../dist/l1-brew-tap/homebrew-tap/Formula/cs.rb)
  — brew formula (regime-aware verify).
- [ADR-077](077-worker-pilot-signing-regime.md) — git commit signing
  regime (orthogonal channel).
- [Sigstore cosign documentation](https://docs.sigstore.dev/cosign/) — `verify-blob`, PKCS#11 hardware keys.
- [RFC 7512](https://datatracker.ietf.org/doc/html/rfc7512) — PKCS#11
  URI scheme.
