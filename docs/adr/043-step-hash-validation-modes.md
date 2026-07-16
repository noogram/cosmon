# ADR-043 — Step hash validation modes

*Status:* Accepted (2026-04-15) · *Provenance:* delib-20260415-6b9d
(IDÉE-1, inspired by Dust core/src/app.rs:27-32,250-255 per-block blake3
chain) · *Related:* ADR-011 content-identity-principle, ADR-042-provenance
sidecar, ADR-034 witness-charter.

## Context

Cosmon steps today have no content-addressed identity. `cs verify` (plumbing
v2) only checks the event-log hash chain. This leaves two gaps:

1. **Drift.** If a formula's step inputs change silently (prompt rewrite,
   dependency bump, edited briefing), the worker still runs — no signal
   tells the overseer the step no longer represents the original intent.
2. **Memoization.** A re-run on unchanged inputs cannot be skipped; we
   have no cache key.

Dust (Apple's recent local agent runtime) wires BLAKE3 per-block in its
app core. The pilot arbitrage (2026-04-15) noted: *« Oui! On peut faire
comme dans oxymake : avoir différents modes de validation (mtime = plus
rapide, différents types de hash selon le contexte et le besoin). »*

OxyMake ships three validation modes — mtime, blake3, sha256 — because
dev loops value speed while release pipelines value cryptographic
integrity. Cosmon should copy the pattern.

## Decision

Introduce an extensible **ValidationMode** for formula steps, exposed via
the `cosmon-hash` crate and the formula TOML schema.

### Modes

| Mode           | Algorithm    | Use case                                |
|----------------|--------------|-----------------------------------------|
| `mtime`        | timestamp    | Default. Dev/iterative loops. No digest.|
| `blake3`       | BLAKE3-256   | Release default. Fast cryptographic.    |
| `sha256`       | SHA-256      | SLSA/Sigstore/git-object interop.       |
| `keyed_blake3` | keyed BLAKE3 | **Reserved** — project-scoped MAC,      |
|                |              | wired when `cosmon-sign` lands.         |

### Domain surface

```rust
pub enum ValidationMode { MTime, Blake3, Sha256, KeyedBlake3 }

pub trait StepValidator {
    fn mode(&self) -> ValidationMode;
    fn validate(&self, inputs: &[InputRef], prev: Option<&StepHash>)
        -> Result<Validation, ValidationError>;
}

pub struct StepHash { mode: ValidationMode, digest: Hash }
```

`InputRef` is `(name, Option<bytes>, Option<mtime>)` — the core remains
**zero-I/O**. Resolution from disk belongs to callers.

### Formula schema

```toml
[[steps]]
id = "build"
title = "Build"

[steps.validation]
mode = "blake3"     # optional; defaults to project-level default
```

Absence of `[steps.validation]` leaves `step.validation_mode = None`.
The orchestrator falls back to a project default (initially `MTime`,
the "dev is fast" choice).

### Event surface

`EventV2::MoleculeStepCompleted` gains an optional
`step_hash: Option<StepHash>`. Backward compatible: `None` is the legacy
meaning, skipped by `skip_serializing_if` on old readers.

### cs verify extension (future)

`cs verify` today walks the event-log hash chain. A future `--strict`
flag re-computes step hashes from live inputs and compares against
`step_hash` on each `MoleculeStepCompleted`. Out of scope for this ADR;
the plumbing enables it.

### Git commit subjects (future)

`cs evolve`'s per-step commit subject can grow a truncated hash:
`evolve(mol-id): step N/M — name (hash: abcd1234)`. The `StepHash::short()`
helper is pre-built. Wiring is a follow-up; does not affect backward
compatibility of existing commits.

## Consequences

**Positive.**
- Memoization becomes a local, opt-in decision per step.
- `cs verify --strict` is unblocked.
- External attestation (SLSA, sigstore) has a canonical digest surface.
- Single `StepHash` type — no `ContentHash`/`InputHash`/`FileHash` sprawl.

**Negative.**
- One more knob in formulas. Mitigation: optional with sensible default.
- Two crypto deps (`blake3`, `sha2`) — both already in the workspace and
  `cargo deny`-allowlisted.

**Neutral / deferred.**
- Keyed-BLAKE3 is accepted as a shape in the enum but errors at runtime
  until `cosmon-sign` wires a project key.
- Project-level default mode (dev vs release) is not yet a config knob;
  defaulting to `MTime` is the pragmatic starting point.

## Alternatives considered

- **Always-on BLAKE3, no mode selector.** Rejected: dev loops pay a
  crypto cost for no benefit; oxymake's experience shows the mode
  selector is worth the one-line config.
- **No hashing at all.** Rejected: breaks `cs verify --strict`, blocks
  sigstore integration, contradicts ADR-011 (content identity principle).
- **Per-project single mode.** Rejected: the dev/release boundary is
  per-step in practice (a `build` step benefits from BLAKE3 even in dev).

## References

- Dust — `core/src/app.rs:27-32,250-255` (BLAKE3 block chain).
- OxyMake — multi-mode validation (mtime/blake3/sha256).
- ADR-011 — content-identity-principle.
- delib-20260415-6b9d arbitrage (2026-04-15).
