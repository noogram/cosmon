# ADR-037 — Lineage conservation and verification architecture

**Status:** Accepted (draft — 2026-04-14)
**Scope:** verification layer, `cs verify`, formula TOML schema,
completion gating in `cs done`, future `trait Verifier`.
**Parent:** deliberation `delib-20260412-2946` (eight-persona panel:
turing, gödel, shannon, feynman, knuth, tolnay, einstein, jobs).
**Binds:** [ADR-027](027-gate-molecules.md) (gate molecules),
[ADR-028](028-cosmon-observability.md) (observability),
[ADR-032](032-p-external-witness-axiom.md) (external witnesses).

## Context

Cosmon molecules emit prose and code that downstream molecules — and
humans — consume as if true. Nothing in the current pipeline
distinguishes a DOI-grounded reference from a confabulated one, a
compiling snippet from a plausible-looking fiction, or an agent's
judgment from a restatement of cited evidence.

The parent deliberation converged on five structural observations:

1. **Truth is undecidable; lineage is not.** The question "is this
   claim true?" is D4/open-domain and cannot be mechanized. The
   question "can this claim be traced to an imported source?" is
   decidable per claim class (Einstein reframe, Turing taxonomy).
2. **Verifiers stratify by decidability tier.** D1 (cheap decidable:
   DOI, path, URL, schema) → D2 (expensive decidable: compiler, Lean)
   → D3 (semi-decidable: entailment, LLM-as-judge) → D4 (undecidable:
   open-domain truth). The verification plane must be explicit about
   which tier a verifier inhabits (Turing).
3. **The recursion bottoms at oracle-free functions.** A verifier that
   calls an LLM adds correlated noise, not error correction. Every
   appeals chain must terminate at a deterministic, LLM-free witness
   (Gödel, Turing, Einstein).
4. **Cognition is a knob on the oracle's noise distribution.** Lower
   temperature and stricter sampling reduce variance but cannot convert
   D4 into D1 (Turing). A per-step *temperament* is a power dial, not a
   verification mechanism.
5. **Completion is the enforcement point.** A molecule with unverified
   claims must not merge unless the escape hatch is taken explicitly
   (Knuth).

v1 ships a monolithic `cs verify` with D1 verifiers and a
`strict = true` flag. This ADR documents the **target v2 architecture**
so the monolithic code can be refactored without re-deliberation.

## Decisions

### 1. Lineage conservation is the organizing principle

A molecule cannot emit more assertion than it imported. Every claim in
a molecule's output falls into exactly one of three classes:

| Class     | Provenance                              | Verifier behavior |
|-----------|-----------------------------------------|-------------------|
| Grounded  | Cited from external source (DOI, URL, path, upstream molecule) | Mechanically checkable at tier D1–D2 |
| Derived   | Produced by a formula step from prior grounded/derived claims | Checkable as a transformation of its inputs |
| Generated | The agent's own judgment or creative proposal ("dark matter") | **Labeled, not verified.** Fraction is an observable, not a defect |

The trust metric is **lineage coverage** —
`grounded + derived / total`. Generated fraction is reported
separately. Unlabeled claims (those the extractor cannot classify) are
the failure mode the system surfaces.

This frames verification as a conservation law: lineage out ≤ lineage
in + labeled generation. The vocabulary is physical — lineage, decay
chain, spectrometer (auditor), dark matter (ungrounded claims).

### 2. Decidability tiers (Turing D1–D4)

Every verifier declares a tier. The tier determines how the verifier
participates in the pipeline:

| Tier | Definition                                   | Examples                              | Pipeline role |
|------|----------------------------------------------|---------------------------------------|---------------|
| D1   | Cheap decidable (O(1) or single RTT)         | DOI-check, path-exists, URL-reachable, JSON-schema | **Step decorator.** Runs inside `cs evolve`. Fail-fast. |
| D2   | Expensive decidable (compilation/proof)      | `cargo check`, Lean proof-check, unit tests | **Blocking gate.** Separate gate molecule (ADR-027). Merge-before-dispatch. |
| D3   | Semi-decidable (entailment, LLM-as-judge)    | Citation-supports-claim, summary-faithfulness | **Evidence with confidence.** Attaches `confidence: f32` and must declare `appeals_to`. |
| D4   | Undecidable (open-domain truth)              | "Is this architectural claim correct?" | **Labeled, not verified.** Surfaces as dark matter. |

Tiers are an ordering, not a ceiling: a D3 verifier may use a D1
verifier as a sub-check (ordering: cheap gates before expensive gates,
Knuth + Tolnay). A D3 verifier MUST declare `appeals_to` terminating at
D1 or D2 (see §5).

### 3. `trait Verifier` — minimal semver-stable surface

Shipped when the third verifier lands (Tolnay rule: two is coincidence,
three is a pattern). Until then, `cs verify` is a single function with
`match`-on-claim-class. The target shape:

```rust
/// A verifier inspects a single claim and returns a verdict.
///
/// Implementations are **oracle-free by default** — they may call out
/// to an LLM only when `VerifierMeta::oracle_free == false`, in which
/// case `appeals_to` MUST terminate at an oracle-free witness.
pub trait Verifier {
    /// Verifier-specific configuration. `#[serde(default)]` on every
    /// field to keep adding fields semver-minor.
    type Config: serde::de::DeserializeOwned + Send + Sync;

    /// Inspect one claim. Synchronous: async in a trait is a semver
    /// poison pill (Send/Sync bounds leak into every caller).
    /// Verifiers that need I/O run inside a blocking scope chosen by
    /// the caller.
    fn verify(&self, claim: &Claim, config: &Self::Config) -> Verdict;

    /// Tier, cost, oracle-free flag, and appeals chain. Static — does
    /// not depend on the claim.
    fn meta(&self) -> VerifierMeta;
}

pub struct VerifierMeta {
    pub tier: Tier,            // D1 | D2 | D3 | D4
    pub cost: Cost,            // O(1) | O(n) | O(compile) | O(exp)
    pub oracle_free: bool,
    pub appeals_to: AppealsChain,
}

pub enum Verdict {
    Grounded { evidence: Evidence },
    Derived { from: Vec<ClaimId>, transform: TransformTag },
    Generated { labeled: bool },
    Unverifiable { reason: String, appeals_to: AppealsChain },
}
```

Semver discipline (Tolnay):

- Adding a field to `Config` / `VerifierMeta` / `Verdict` with
  `#[serde(default)]` is **minor**.
- Removing or renaming a public field is **major**.
- Adding a variant to `Verdict` is **major** (callers must match
  exhaustively); mitigate with `#[non_exhaustive]` from day one.
- No `&mut self` on `verify` — verifiers are logically pure per call;
  caching lives in a wrapper (`CachedVerifier<V>`), not inside `V`.
- No async. If async is needed later, add a sibling
  `trait AsyncVerifier` rather than breaking `Verifier`.

### 4. Cognition temperament — per-step TOML schema

A *temperament* is the cognition profile for a formula step. It is a
power dial on the oracle's noise distribution — **not** a verification
mechanism. The v1 surface is a single boolean; the v2 schema is
designed now so v1's boolean maps to a defined subset.

**v1 surface (ships now):**
```toml
[[steps]]
name = "implement"
strict = true   # lowers temperature, enables all D1 gates
```

**v2 schema (target):**
```toml
[[steps]]
name = "implement"
[steps.temperament]
temperature = 0.2          # 0.0 = greedy, 1.0 = default sampling
sampling    = "greedy"     # "greedy" | "nucleus" | "top_k"
top_p       = 0.9          # only for sampling = "nucleus"
top_k       = 40           # only for sampling = "top_k"
max_tokens  = 4000
gates       = ["doi", "zotero", "path", "url"]  # ordered, cheap first
on_failure  = "block"      # "block" | "warn" | "label_dark"
model_hint  = "claude-opus-4-6"   # advisory, not binding
```

The v1 boolean `strict = true` maps to:
```
temperature = 0.2, sampling = "greedy", gates = <all registered D1>,
on_failure = "block"
```

`strict = false` maps to model defaults with `gates = []` and
`on_failure = "label_dark"` (record, do not block).

Semver: every field carries `#[serde(default)]`. Adding fields is
minor; removing is major.

### 5. Consistency manifest (Gödel)

Every verifier — from the first D1 monolith onward — declares what it
can and cannot decide, and to whom it appeals when it cannot:

```toml
# .cosmon/verifiers/doi-check.manifest.toml
name       = "doi-check"
tier       = "D1"
oracle_free = true

decides = [
  "claim.kind == 'citation' && claim.identifier.schema == 'doi'",
]

cannot_decide = [
  "claim.identifier.schema != 'doi'",
  "claim.kind == 'quotation'",   # we resolve the DOI, not its content
]

appeals_to = [
  { condition = "claim.kind == 'quotation'", witness = "quotation-check" },
  { condition = "_",                         witness = "L_mechanical" },
]
```

**Terminal witnesses** (all appeals chains MUST terminate at one of
these — the recursion bottoms here):

- `L_mechanical` — deterministic, oracle-free functions (regex,
  filesystem, HTTP GET, compiler). Closed under composition.
- `L_formal` — formal proof systems (Lean, Coq). Closed under proof
  replay.
- `L_infty` / `L_human` — human operator judgment. Unbounded but logged
  and signed per ADR-032 (external witness axiom).

**Invariants** (checked by `cs verify --check-manifests`):

1. No cycles in the appeals DAG. (Gödel sentence G₃ — a verifier
   appealing to itself transitively is incoherent.)
2. Every chain terminates at `L_mechanical`, `L_formal`, or `L_infty`.
3. Required for **all D3+ verifiers**; optional for D1/D2 but
   encouraged. v1 D1 verifiers ship with implicit
   `appeals_to = L_mechanical`.
4. `oracle_free = true` ⟹ no witness in the chain may have
   `oracle_free = false`. LLM-based verifiers taint the chain.

The manifest is the mechanical form of the consistency argument: it
documents **what no verifier covers** (the *unverified residue*) so the
system can report dark matter honestly instead of silently.

### 6. Completion gating in `cs done`

`cs done <mol>` refuses a molecule with unlabeled claims or failed
verifiers:

```
$ cs done task-20260412-b185
error: molecule task-20260412-b185 has 3 unverified claims:
  - DOI 10.1234/fake not resolvable  (gate: doi, tier: D1)
  - path `src/missing.rs` does not exist  (gate: path, tier: D1)
  - unlabeled generated claim in synthesis.md:42  (gate: lineage, tier: D1)

pass --allow-unverified to override (creates an audit event).
```

- **Default behavior**: block merge if any registered gate for the
  molecule's formula returns `Verdict::Unverifiable` or a claim is
  `Generated { labeled: false }`.
- `--allow-unverified` is the sanctioned escape hatch. It writes a
  `merge.unverified-override` event to `events.jsonl` with the reason
  the operator supplies. Audit trail, not a warning.
- Shape of the check: `cs done` reads the molecule's
  `verification-report.md` and its `claims.jsonl` sidecar (or runs
  `cs verify` in check mode if missing). **Reads only** — never writes
  state during the gate check (preserves the write-read asymmetry
  invariant from CLAUDE.md §Coherence checklist).
- Interaction with intent+receipt (ADR-036): the verification check is
  part of the pre-merge intent; a crash after override-logging but
  before merge is resumable.

### 7. Strict-mode defaults (v1 shipping defaults)

Until the full temperament schema ships, v1 defaults are:

| Formula kind       | `strict` default | Rationale |
|--------------------|------------------|-----------|
| `deep-think`       | `true`           | Panel synthesis must cite; dark matter labeled |
| `task-work`        | `true`           | Implementation claims (paths, APIs) are D1-checkable |
| `gate`             | `true`           | Gates ARE the verification; strictness is definitional |
| `temp-review`      | `false`          | Curation notes are judgment, naturally dark matter |
| `idea-to-plan`     | `false`          | Ideation is generative; labeling suffices |

Individual molecules may override with `strict = true/false` in the
nucleation variables.

## Consequences

**Positive.**

- Operators get a trust metric (lineage coverage %) that is
  *measurable*, not a subjective grade. Dark matter is surfaced, not
  hidden.
- `trait Verifier` + `VerifierMeta` + manifest form a closed
  architectural contract: a new verifier is a crate, a TOML manifest,
  and a registration — no cosmon-core changes required.
- Appeals chains prevent the regression of LLM-verifying-LLM. Every
  claim's trust bottoms at an oracle-free witness or is labeled D4.
- Completion gating plugs the leakage point — unverified claims cannot
  silently cross molecule boundaries.

**Negative.**

- Manifests are work. v1 avoids most of the cost by shipping monolithic
  D1 verifiers with implicit `L_mechanical` appeals.
- The full temperament schema is speculative (designed, not shipped)
  until a second backend exists. If Claude Code remains the only
  backend, the schema rots.
- Strict-mode defaults will produce false positives on legitimate dark
  matter until the extractor gets good at labeling. Expect operator
  friction in the first weeks; `--allow-unverified` is the relief
  valve.

**Neutral.**

- Claim extraction (the step before verification) is not specified in
  this ADR. It is a sibling concern deferred to implementation. The
  contract is: extraction produces a `claims.jsonl` sidecar; this ADR
  governs what happens next.

## Phasing (coordinates with synthesis §IFBDD roadmap)

| Phase | What ships | What this ADR governs |
|-------|-----------|-----------------------|
| v1 (NOW) | Monolithic `cs verify` + D1 verifiers (DOI, path, URL) + `strict = true` | Vocabulary, completion-gate refusal message shape, `claims.jsonl` schema, manifest *optional* |
| v2 (NEXT) | `trait Verifier` + `VerifierMeta` + third verifier (cargo-check or zotero) + `[step.temperament]` TOML | Full trait surface, manifest *required for D3+*, strict-mode defaults table |
| v3 (LATER) | D2 gate molecules (cargo-check, Lean) + D3 entailment verifier + multi-backend cognition routing | Cross-tier appeals discipline, oracle-free taint propagation, panel correlation metrics |

## Alternatives considered

- **Truth verification (reject).** D4 for open-domain claims. Attempts
  to verify truth via LLM-as-judge add correlated noise. Rejected
  unanimously (Turing, Gödel, Einstein).
- **Claims inline as `{{claim:X|source:Y}}` markup (Knuth, rejected for
  v1).** Authoring friction (Jobs). Resolution: enrichment lives in
  sidecar `claims.jsonl`; synthesis.md stays readable. Inline markup
  may return in v3 for high-stakes artifacts.
- **Cognition profiles as v1 surface (Tolnay, deferred).** Premature
  schema fossilization. v1 ships `strict` boolean; full schema in v2.
- **Verifiers as molecule kinds (rejected).** Would inflate the DAG
  with no information gain. Verifiers are step decorators on the data
  plane; the control plane stays one-bit (done/not-done). Convergent
  across Shannon, Feynman, Jobs, Tolnay.
- **Lean proof-check in v1 (rejected).** Zero formal claims in the
  current corpus (Feynman). Narrow fragment (Turing). Track as
  `temp:cold` until a molecule produces a mathematical claim that
  earns it.

## References

- Parent deliberation: `delib-20260412-2946` (synthesis.md, panel of 8).
- Chronicle (pending): an internal chronicle
  ("a molecule cannot emit more assertion than it imported").
- Turing (1936) — decidability and the halting problem, for D1–D4.
- Gödel (1931) — incompleteness, for the appeals-chain termination
  requirement.
- Shannon (1948) — channel coding, for verifiers-as-checksums.
- ADR-027 — gate molecules (D2 verifier hosting).
- ADR-028 — observability (`claim.emitted` / `claim.verified` events).
- ADR-032 — P_external witness axiom (terminal `L_infty` witness).
- ADR-036 — intent+receipt (crash-safe `cs done` with verification
  gate).
- [THESIS.md Part XX — Two-Axis Proof-of-Work](../../THESIS.md) and
  chronicle `2026-04-14-proof-of-work-two-axes.md`
  — this ADR is the **epistemic axis** of the two-axis doctrine. The
  *process axis* (ADR-036 + per-step commits + `prompt.md` /
  `briefing.md` / `log.md` / `synthesis.md`) answers "how did the
  worker arrive here?"; the *epistemic axis* answers "why believe what
  the worker says?" — and that is exactly what lineage conservation,
  the D1–D4 tier taxonomy, and the appeals chain are **for**. Every
  verifier declared in this ADR is a node in the epistemic chain;
  `provenance.md` (ADR-041, pending) is the sidecar where the claims
  live; `cs verify --claims` is the projection. A Witness Charter
  vetoer (ADR-034) signs `Ratified` iff `cs verify --process &&
  cs verify --claims` both pass.
