# cosmon-rpp-adapter

The Remote Pilot Port (RPP) — cosmon's **central secure-delivery door**:
the §8j HTTPS+OIDC boundary through which a remote pilot reaches a cosmon
instance running on a hardened (hardware-encrypted) VM. It is not a
client-specific feature — it is the capability cosmon ships for *any*
deployment that must be reached remotely under a cyber-secured posture.
Governing ADRs:
[ADR-117](../../docs/adr/117-rpp-central-security-capability.md)
(secure-delivery framing),
[ADR-080](../../docs/adr/080-remote-pilot-port-https-oidc.md)
(architecture).

> **Honesty constraint (ADR-117 §2b).** The RPP *fronts* a tenant's
> hardware-encryption stack — it is the audited, OIDC-authenticated,
> one-way entrance into a cosmon instance that runs inside a
> hardware-encrypted deployment context (e.g. an AWS VM provisioned with
> the tenant's HW-encryption solution). It does **not** itself own
> encryption today (no payload envelope-encryption, no enclave
> attestation, no KMS-sealed audit log); beyond TLS termination and JWT
> signature verification it performs no cryptography. The five-clause
> admission boundary is what makes *access* cyber-secured. This README
> states purpose, never an unbacked encryption feature.

> **Page d'amorce tenant (30 min, registre Feynman + couverture Gödel).**
> Pour un nouvel intégrateur tenant qui doit devenir autonome
> sur la boucle complète *login Claude → nucléer → tackle → récupérer
> artefact → push retour*, lire d'abord :
> [`lumen/docs/onboarding/noogram-tenant.md`](../../../lumen/docs/onboarding/noogram-tenant.md).
> Source : `delib-20260522-a069` (smithy).

The RPP is a **Layer B port adapter** (ADR-023 hexagonal): a long-lived
`axum` server that admits remote pilot requests through a five-clause
admission boundary and shells out to the real `cs` binary for every
admitted request. The adapter holds **no** in-RAM business state.

## V0 surface

One route only:

```
GET /v1/molecules/{id}
  Authorization: Bearer <JWT>
```

Proxies to `cs observe :id --json` under the resolved tenant galaxy.

## Five-clause admission (ADR-080 §3)

| Clause | Implementation |
|--------|----------------|
| (a) identity mapping | `nucleon_map.rs` — `oidc-identity.toml` BLAKE3-sealed |
| (b) causal closure   | `audit.rs` — `<inbox>/api/<request_id>.json` written before `cs` |
| (c) rate limit       | `rate_limit.rs` — per-`sub` leaky bucket on disk |
| (d) one-way topology | V0 forbids POST routes outright |
| (e) subprocess envelope | `subprocess.rs` — `COSMON_API_REQUEST=1` + cwd + timeout |

## Configuration

`~/.config/cosmon/rpp.toml`:

```toml
bind_addr = "127.0.0.1:8443"
posture = "prepared"
state_dir = "~/.cosmon/state"
galaxies_root = "~/galaxies"
subprocess_timeout_sec = 30
```

## Forbidden vocabulary (ADR-080 §15)

The words `daemon`, `cosmon-server`, `microservice`, and `endpoint`
(in isolation) are banned from sources, types, file names, ADR titles,
and chronicle entries. The adapter is **not** a daemon — it is a port
adapter at the §8j slot.

## Tests

| File | Role |
|------|------|
| `tests/api_surface_freeze.rs` | §8p frozen surface (R3 in ADR-080 §12) |
| `tests/admission_test.rs` | §8j five-clause coverage |
| `tests/no_state_read_test.rs` | R1 — adapter never writes `state.json` |

Run all gates:

```sh
cargo check -p cosmon-rpp-adapter
cargo test  -p cosmon-rpp-adapter
cargo clippy -p cosmon-rpp-adapter -- -D warnings
cargo fmt --all -- --check
```

## Operator-only verbs (ADR-080 §5)

The list is **closed**. The RPP refuses to expose, by name or by
accident: `done`, `evolve`, `complete`, `security`, `run`, `kill`,
`purge`, `reconcile`, `verify`, `whisper`, `drop`. Extending it
requires a successor ADR with a `delegate_for` claim model.

## V1+ roadmap pointers

- `POST /v1/molecules` (nucleate) and `POST /v1/molecules/:id/transitions` (tackle).
- Per-`noyau` global rate-limit budget.
- Refresh tokens with `family_id` (RFC 6819).
- `posture = active` with DPoP RFC 9449.

See ADR-080 §10 for the full roadmap.
