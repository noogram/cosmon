# Notary — operator guide

**Who reads this.** Two audiences, one guide. A researcher at
Institut Louis Bachelier wants to understand what the signed
Certificate printed at the end of a run *means*. tenant_auditor Doe
wants to verify that the cryptographic claims hold. Both need the same
thing: a clean path from key generation to Certificate verification,
without marketing vocabulary.

**What this is not.** A blockchain guide. Notarization here is **a
trace, not an enforcement point** — more like a notarized photograph
than a smart contract. No consensus, no token, no ledger.

## 1. Why notarize

Two honest primitives live under one verb:

- **Proof-of-Sealed-Presence (PoSP).** A signed statement that a
  specific content hash existed at a specific moment, under a specific
  key. Nothing about what the content *means*; everything about the
  fact that it *existed*.
- **Proof-of-LLM-Expenditure (PoLE).** A statement that tokens were
  burned producing the content. Not implemented in v0 (the schema
  reserves the `energy_receipt` slot for phase 1).

The combination is what we call **notarization**. A BLAKE3 hash by
itself is a fingerprint — anybody can compute it from the content. An
Ed25519 signature on that fingerprint is what binds the fingerprint
to *you* at *that time*. The marketing copy may say
*"proof-of-cognition"*; the code, the types, and this guide say
*notarization*.

The distinction matters most for tenant_auditor's audience: *cognition* is
unprovable from the outside and over-claims what the protocol
delivers. *Notarization* names what is actually happening — an
operator attests, a reader verifies — without reaching for
metaphysics.

## 2. Generating a key

Ed25519 pure (RFC 8032), 32-byte secret, 32-byte public. `cs notarize`
accepts the secret either as 32 raw bytes or as 64 lowercase hex
characters. The ILB demo container uses the hex form for diffability;
any other form must be converted first.

```bash
# Preferred — 32 bytes of OS randomness, hex-encoded:
python3 -c 'import os,sys; sys.stdout.write(os.urandom(32).hex())' \
    > ~/.config/cosmon/operator.key
chmod 600 ~/.config/cosmon/operator.key

# Alternative — OpenSSL hex generation:
openssl rand -hex 32 | tr -d '\n' > ~/.config/cosmon/operator.key
chmod 600 ~/.config/cosmon/operator.key

# NOT accepted (PEM-wrapped Ed25519 key):
openssl genpkey -algorithm Ed25519 -out ~/.config/cosmon/operator.key   # ❌
```

**Storage.** Mode `0600`, in your home directory. Do **not** bundle
the key into a Docker image — every container instance must generate
its own. The ILB demo's `ilb-demo-setup.sh` does this automatically.

**Rotation.** There is no in-band rotation ceremony in v0. The
operator convention is: generate a new key, notarize future molecules
under it, keep the old key only long enough to re-sign anything the
new key owes re-attestation to. Certificates under a retired key
remain verifiable — this is the point.

**Public key disclosure.** The public key comes out in the JSON
Certificate at `.operator_pubkey` (32 bytes, hex-encoded). That is the
value a verifier needs. The secret key never leaves the operator.

## 3. `cs notarize` — flags and output

```text
cs notarize <MOLECULE_ID> [--key <PATH>] [--dry-run] [--json]
                         [--cosmon-version <VERSION>]
```

Flags:

| Flag | Purpose |
|------|---------|
| `--key <PATH>`        | Path to a 32-byte raw or 64-hex Ed25519 secret. If omitted, behaves as `--dry-run`. |
| `--dry-run`           | Compute the commitment + seal, print them; do NOT sign. Default when `--key` is absent. |
| `--json`              | Emit one line of JSON — the Certificate (or the dry-run payload). |
| `--cosmon-version`    | Override the crate version embedded in the commitment. Testing / reproducibility. |

### Output (`--json`)

```jsonc
{
  "molecule_id": "task-20260422-abcd",
  "kind": "task",
  "commitment": {
    "molecule_id": "task-20260422-abcd",
    "kind": "task",
    "prompt_content_hash": "b3:0x…",
    "briefing_seals_root":  "b3:0x…",
    "parent_commitments": [],
    "formula_id": "task-work",
    "formula_version_hash": "b3:0x…",
    "cosmon_version": "0.2.0",
    "operator_pubkey": "ed25519:0x…",
    "validator_set_epoch": 0,
    "nucleated_at_unix_ms": 1745311500123,
    "nonce": "0x…",
    "canonical_version": 1
  },
  "seal_hex": "b3:0x…",          // BLAKE3(canonical(Commitment))
  "signature_hex": "ed25519:0x…",// present only when --key is provided
  "signed_at_unix_ms": 1745311500456
}
```

The Certificate is the **whole JSON object**. Share it as a file;
never paste only the signature.

## 4. Verifying

A verifier re-runs the canonicalization on the `commitment` field,
hashes it, confirms the result equals `seal_hex`, then verifies the
signature over `seal_hex` under `operator_pubkey`.

### Quick sanity (deterministic seal)

```bash
# What the seal SHOULD be, given the commitment:
cs notarize <mol_id> --dry-run --json | jq -r .seal_hex

# What the Certificate claims:
jq -r .seal_hex /workspace/notarizations/<mol_id>.json

# These two MUST match, byte-for-byte.
```

### Full cryptographic verification

A future `cs notarize verify <certificate.json>` command will package
the check. For v0 the verifier can use any Ed25519 library:

```python
# Python example (pyca/cryptography):
import json
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
cert = json.load(open("notarizations/task-20260422-abcd.json"))
pub  = Ed25519PublicKey.from_public_bytes(bytes.fromhex(cert["commitment"]["operator_pubkey"]))
pub.verify(bytes.fromhex(cert["signature_hex"]), bytes.fromhex(cert["seal_hex"]))
# raises InvalidSignature if the Certificate is tampered with.
```

### Exit-code convention (planned for `cs notarize verify`)

- `0` — Certificate signature and seal match.
- `1` — Signature does not verify (tampering or wrong public key).
- `2` — No Certificate present at the path (inconclusive).

## 5. Exporting and sharing a Certificate

The Certificate is a single JSON file. Share it via any ordinary
channel — email, secure file transfer, git. It contains no secrets.

For deliverables: commit the Certificate to the same git repo as the
article it notarizes. The two travel together. A reader holding the
article and the Certificate can verify the claim without network
access, without Anthropic, without contacting the operator.

## 6. Invariants (ADR-056)

| # | Claim | How it is preserved |
|---|-------|---------------------|
| **I1** | Seal is deterministic for a given canonical Commitment. | Canonical serialization v1 is spelled out in ADR-056 §"Canonical form v1". Tested by `semantically_identical_variants_same_content_hash`. |
| **I2** | An operator can verify a Notarization without the validator's cooperation. | Phase-1 autonomy. Verification is a pure Ed25519 check — no network, no third party. |
| **I3** | Validator revocation is **forbidden**. | The protocol has no `revoke` verb. Disagreement is expressed as a new signed "this notarization is contested" record — never as erasure. |
| **I4** | `witnesses` is always `Vec<Witness>` (plural). | Type-level. `Option<Witness>` would lie about the cardinality in phase 2+. |
| **I5** | Agents cannot notarize on their own account. | `cs notarize` refuses when `COSMON_PARENT_MOL_ID` is set in the environment. The operator is the root of trust, not the agent. |
| **I6** | Individual notarizations are **not tradable**. | Protocol omission — no transfer verb. The asset is the DAG of notarizations, not any one. |
| **I7** | `canonical_version: u8` starts at `1`. Legacy artifacts carry `0` (readable but not verifiable). | Enforced by `SealVerifyError::UnsupportedCanonicalVersion`. |

## 7. Future phases

- **Phase 1** — BLS12-381 aggregation (stub today at `BlsStub`),
  Merkle parent_commitments activated.
- **Phase 2** — Remote Noogram validator. `cs notarize` becomes a
  two-signature ceremony: operator + validator. The validator signs
  over `Notarization || validator_time || validator_nonce` to produce
  a full `NotarizationCertificate`.
- **Phase 3** — Validator set epochs, dis-attestation rules,
  Certificate Transparency-style public log. **Consensus-free** unless
  molecule tradability is in scope (which it is not, per I6).

## References

- [ADR-056](../adr/056-notary-protocol-v0.md) — the governing ADR.
- [ADR-055](../adr/055-cosmon-residence.md) — residence model.
- `crates/cosmon-notary/` — implementation.
- `crates/cosmon-notary/tests/test_vectors.rs` — three knuth-drafted
  vectors.
- `delib-20260420-bae4` — 9-persona convergence (wheeler, shannon,
  niel, godin, hawking, knuth, einstein, turing, feynman).
- `docs/guides/ilb-demo-container.md` — the sibling guide that wraps
  notarization + residence + artifact-map into a single demo.
