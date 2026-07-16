# hello-notarized

**The smallest end-to-end proof of cosmon's notarize-release loop.**

No LLM. No Anthropic key. No research topic. One toy artifact —
`Hello, notarized world!` — bound to an Ed25519 key at a specific
moment, shipped as a signed JSON Certificate next to the text file
itself.

If you want to understand what `cs notarize` *does*, read this before
anything else. The ILB container
([`docs/guides/ilb-demo-container.md`](../../docs/guides/ilb-demo-container.md))
layers residence + genre classification + a real research topic on
top of the same verb; this example strips every one of those layers
away so the notarization primitive stands alone.

## The three-minute story

Imagine you hand someone a text file and tell them *"I wrote this
today"*. They have your word. Now imagine you hand them the text file
**plus a JSON Certificate** containing:

- a BLAKE3 hash of the file's intent (the commitment),
- your Ed25519 public key,
- your signature over that hash.

They no longer need your word. They re-compute the hash, they check
the signature, and the two of those together prove *something shaped
like this intent was attested under this key at this time*. No
network. No blockchain. No trust in cosmon, Anthropic, or the author.

That is the notarize-release loop. The loop is *release* because the
artifact + Certificate pair travels together, forever, by ordinary
means (email, git, tarball).

## What this example is not

- Not a proof the author *wrote* the artifact — that requires provenance
  you would build on top (git blame, signed commits, editor plugins).
- Not a blockchain — there is no ledger, no consensus, no token.
- Not a revocation service — the protocol has no `revoke` verb by
  design (ADR-056 invariant I3).
- Not an anti-plagiarism tool — the Certificate says *existed at t
  under k*; it says nothing about meaning, novelty, or authorship of
  ideas.

## Run it

```bash
cd examples/hello-notarized
./run.sh
```

Wall clock: ~1 second. The script writes to a freshly-minted temp
directory (because `cs init` refuses to nest inside another cosmon
project, which this repo is) and leaves a stable `.output` symlink
in the example folder for inspection:

```
.output -> /tmp/hello-notarized-XXXX/
              ├── galaxy/              — throwaway cosmon project
              ├── operator.key         — teaching key (32 zero bytes, DO NOT REUSE)
              ├── notarize-out.json    — CLI summary of the signing call
              └── release/
                  ├── hello.txt         — the toy artifact
                  ├── certificate.json  — signed Ed25519 Certificate
                  └── MANIFEST.md       — what the bundle contains
```

Override the location by setting `HELLO_NOTARIZED_OUT=/your/path`
before running.

Verify the signature cryptographically:

```bash
./verify.sh
# OK — signature verifies (ed25519).
```

## What `run.sh` actually does

Eight steps, each one named in the script output. In prose:

1. **Reset** — create a fresh temp dir, repoint `.output` at it.
2. **Init** — `cs init <tmp>/galaxy`. A fresh cosmon state store
   in a throwaway directory (outside any enclosing cosmon galaxy so
   walk-up discovery doesn't get confused).
3. **Key** — write a 32-byte Ed25519 secret, hex-encoded, mode 0600.
   *The key used here is 32 zero bytes. Reproducible on purpose; not
   secret, never reuse for anything real.*
4. **Artifact** — write `hello.txt` containing the toy text.
5. **Nucleate** — `cs nucleate hello --no-parent --var artifact="…"`.
   Creates a pending molecule whose commitment (prompt + formula +
   time + key + nonce) is what the Certificate will bind to.
6. **Sign** — `cs notarize <mol> --key operator.key --json`. Emits a
   mint file at `.cosmon/state/…/mint.json` and prints its
   `content_hash` + path.
7. **Bundle & verify** — enrich the mint with an inline
   `content_hash` (so the Certificate is standalone), copy it into
   `release/certificate.json`, then call `./verify.sh` — the
   cryptographic signature must verify or the script exits non-zero.
8. **Manifest** — write `release/MANIFEST.md` so someone receiving
   the bundle by email or tarball knows what they are looking at.

No step 9. The molecule stays in `pending` — we deliberately do
**not** `cs tackle` it. That is the point: *the commitment binds the
molecule's intent, not its outcome*. Notarization is valid whether
the molecule ever runs or not.

## What `verify.sh` does

```
ed25519_verify(
    pub     = certificate.commitment.operator_pubkey.bytes_hex,
    msg     = hex_decode(certificate.content_hash),
    sig     = hex_decode(certificate.signature.bytes_hex),
)
```

Uses `python3` with
[`cryptography`](https://pypi.org/project/cryptography/) if
available, and falls back to `openssl pkeyutl -verify` with a
hand-assembled SubjectPublicKeyInfo (RFC 8410) if not.

Exit codes:
- `0` — signature valid.
- `1` — signature does not verify (tampering or wrong key).
- `2` — input missing or tooling unavailable.

Run it against any Certificate:

```bash
./verify.sh path/to/certificate.json
```

## The principle

> *The commitment binds the molecule's intent, not its outcome.*

Notarization happens **before** or **regardless of** execution. That
is why it survives a worker crash, a rerun, a model change, a
timezone shift. The seal is a function of the canonical commitment
only — and the canonical commitment is a function of the intent, the
formula, the operator key, and the moment. Not the response. Not the
tokens. Not the wall time of the run.

This is Wheeler's *it from bit* at the notary layer: the minimal bit
is *"this intent existed under this key at this time"*. Everything
else is downstream.

## Where to go next

- **The operator guide** —
  [`docs/guides/notary-operator-guide.md`](../../docs/guides/notary-operator-guide.md)
  — the full primitive, with all flags, failure modes, and
  invariants (I1–I7).
- **The ADR** — [`docs/adr/056-notary-protocol-v0.md`](../../docs/adr/056-notary-protocol-v0.md)
  — the governing decision, including the canonical serialization.
- **The Crate** — [`crates/cosmon-notary/`](../../crates/cosmon-notary/)
  — the implementation, with three Knuth-drafted test vectors in
  `tests/test_vectors.rs`.
- **The full demo** — [`docs/guides/ilb-demo-container.md`](../../docs/guides/ilb-demo-container.md)
  — residence + genre + notary composed for a live research topic.

## Files in this example

| File | Role |
|------|------|
| `run.sh` | One-shot demo. Idempotent. No flags. |
| `verify.sh` | Cryptographic signature check over the Certificate. |
| `hello.formula.toml` | Trivial one-step formula the molecule nucleates from. |
| `README.md` | This file. |
| `.gitignore` | Keeps `.output/` out of git. |
