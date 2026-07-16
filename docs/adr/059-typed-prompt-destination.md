# ADR-059 — Typed Prompt Destination

**Status:** Proposed (2026-04-21)
**Scope:** replace the free-form `PathBuf` that locates a notarized prompt's
target with a structured triple `(residence, branch, genre)`.
Defines the `PromptDestination` struct in `cosmon-core`, states the
invariant it preserves, and enumerates the rejected alternatives.

**Parent deliberation:**
`delib-20260420-bae4`
— the three-axis nomenclature (residence + genre + notary) converged
the nine-persona panel on what the *target* of an attestation is.

**Binds:**
- [ADR-055](055-cosmon-residence.md) — defines the residence axis
  (`Solo | Team | Encrypted | Remote`). `PromptDestination::residence`
  is the adapted mint-time subset of that enum.
- [ADR-056](056-notary-protocol-v0.md) — defines the notary protocol
  that today signs a `prompt_content_hash` opaque to the destination.
  This ADR tells the notary *where the bytes were headed* at signing
  time, so a verifier can reject a Seal whose prompt has been moved to
  an incompatible audience after the fact.
- [ADR-057](057-genre-and-artifact-map.md) — defines the genre axis
  (`chronicle | adr | addl | github-surface | deliberation | code`).
  `PromptDestination::genre` is the same vocabulary, not a parallel one.

**Blocks (follow-ups, not in scope here):**
- Re-hash migration of existing BriefingSeals against the typed triple
  (replayable from canonical form v2; v1 seals read but do not verify
  against the new invariant).
- `cs notarize` wiring: populate `PromptDestination` from the
  molecule's declared state instead of the current path-derivation.
- TLA+ invariant `I_DestinationWellTyped` added to the spec once
  `cosmon-spec` lands.

## 1 · Context

Today the notary (ADR-056 `Commitment`) carries a `prompt_content_hash`
but no typed statement of **where that prompt is meant to live**. The
target is inferred by the caller from a free-form path: `cs notarize`
walks up from the molecule directory and attests a content hash, with
no claim about the residence, branch, or genre of the artifact.

**Wheeler's detachment functor.** The operator's mental model for
notarization is `U(M ⊕ σ) = M`: *the content M is what it is, the
signature σ is layered on top and can be detached without changing M*.
That functor is preserved today **only contingently** — because the
target is an opaque path. Rename the file, move it across branches, or
publish it under a different genre, and the Seal still "verifies"
against the same hash but no longer attests anything meaningful about
what the operator committed to. The notary answers *"this bytes
existed"* but not *"these bytes were on their way to this audience"*.

A typed destination makes `U(M ⊕ σ) = M` hold **structurally** rather
than contingently. The Seal binds to the triple the operator declared
at mint time; later moves that violate the triple either break
verification (desirable — the operator wants to know) or force the
operator to re-notarize under the new triple (honest about the change).

The three axes are not a new decomposition: they are the same three
that `delib-20260420-bae4` converged on (*residence + genre + notary*)
and that ADR-055 / ADR-057 already named. This ADR merely lifts them
from operator-facing nouns into a Rust struct the notary can sign over.

## 2 · Decision

### 2.1 The triple

```rust
pub struct PromptDestination {
    /// Where the bytes live at mint time (ADR-055).
    pub residence: Residence,
    /// The git branch the prompt is being committed on.
    pub branch: BranchName,
    /// What kind of thing this prompt is (ADR-057).
    pub genre: Genre,
}
```

The three fields are the **proper name** of the three axes the panel
identified. Each axis is orthogonal to the others:

- `residence` answers *"which social contract governs these bytes?"* —
  changes when the operator runs `cs mode`.
- `branch` answers *"which git history holds them right now?"* —
  changes when the operator checks out a different branch.
- `genre` answers *"what kind of artifact is this, and who is its
  audience?"* — changes when the operator renames or moves the file
  across the `artifact-map.toml` globs.

A notarization that commits to a triple therefore promises *"at mint
time, these bytes were at this residence, on this branch, as this
genre"*. Any one of the three changing invalidates the triple, and
verification must surface that fact.

### 2.2 Rust surface — `cosmon-core`

The type lives in `crates/cosmon-core/src/prompt_destination.rs`, is
`#[derive(Serialize, Deserialize)]` for canonical encoding, and has no
dependency on the notary crate (the notary will carry a
`PromptDestination` into its `Commitment` in a future follow-up — the
direction of the import edge is notary → core, never the other way).

Newtypes and enums introduced in this ADR:

```rust
/// Adapted subset of ADR-055 `Residence` used at mint time.
/// Data-carrying variants (`Team { repo }`, `Encrypted { repo, recipients }`,
/// `Remote { url }`) are intentionally flattened for v1: the residence
/// axis at mint time is a classifier, not a redeclaration of the full
/// residence configuration. The full ADR-055 enum remains the source
/// of truth for the galaxy's residence; `PromptDestination::residence`
/// is the canonical snapshot of which variant the galaxy was in.
pub enum Residence { Solo, Team, Encrypted, Remote }

/// Git branch name. Validated non-empty; no deeper git-ref-format check
/// at the type level (deferred — the git command itself is the ground
/// truth if it ever comes to that).
pub struct BranchName(String);

/// The six v0 genres declared by ADR-057 §2.2.
pub enum Genre {
    Chronicle,
    Adr,
    Addl(PartnerName),
    GithubSurface,
    Deliberation,
    Code,
}

pub struct PartnerName(String);
```

The type is intentionally small. Anything richer (the full ADR-055
`Residence::Team { repo }` or a ref-format-validated `BranchName`)
would force the struct to encode a configuration rather than a
classification — the wrong role for a mint-time attestation.

### 2.3 Canonical encoding

`PromptDestination` serializes to a JSON object with three fields, in
the sorted key order `branch`, `genre`, `residence`. Genre and
residence encode as `snake_case` strings; the `Addl` variant encodes
as `{"addl": "<partner>"}`. `BranchName` and `PartnerName` serialize
as strings. No nested objects, no floats, no optional fields — the
whole struct must hash deterministically under the same rules as
ADR-056 canonical form v1.

The notary, when it adopts this type, will bump its canonical form to
v2: the `Commitment` schema gains a `prompt_destination` field, and
the domain separator changes from `cosmon-notary/v1/commitment\x00`
to `cosmon-notary/v2/commitment\x00`. Existing v1 notarizations
remain verifiable against their own domain separator; new ones
produced against the typed destination carry v2.

## 3 · Alternatives considered

### 3.1 Single-field path (status quo) — *rejected*

Keep the `PathBuf` (or `String`) that points to `prompt.md` and let
the verifier infer residence/branch/genre from the filesystem at
verification time.

**Why rejected.** This is the contingent preservation of `U(M ⊕ σ) = M`
discussed above: a rename silently moves the prompt to a different
genre, the Seal still "verifies" against the hash, and the audience
reading the notarized output sees a contract that was never signed.
The whole point of a typed destination is to convert this into a
structural verification failure.

### 3.2 URL scheme `attest://residence/branch/genre` — *rejected (too early)*

Encode the triple as a URI: `attest://team/main/chronicle/<path>`.

**Why rejected.** Attractive for grep-ability and cross-galaxy
citation, but it puts a protocol rename on the critical path: ADR-055
deliberately chose `Residence` as the internal noun and `mode` as the
CLI verb. Dropping a third name (`attest://`) into the canonical form
before those two have settled adds a word without adding a concept.
Revisit once the notary has a remote-residence story (ADR-056 phase
2).

### 3.3 `PathBuf`-alias (`type PromptDestination = PathBuf`) — *rejected*

Provide a `PathBuf` alias and call it a typed destination.

**Why rejected.** A type alias is not a type: the compiler still
accepts any `PathBuf` everywhere a `PromptDestination` is expected,
and the invariant `I_DestinationWellTyped` reduces to "the string
parsed as a path". This is exactly the *faux typage* pattern the
panel explicitly warned against — the name changes, the contract does
not. A genuine typed destination must require construction through
validated newtypes and enums, as §2.2 does.

### 3.4 Overload `Commitment::prompt_content_hash` — *rejected*

Keep the single `prompt_content_hash` and add the destination as
unsigned metadata in a sidecar file.

**Why rejected.** The whole purpose of signing is to bind the
operator to a claim. Anything unsigned is a comment, not an
attestation. If the destination matters for verification it must
enter the signed payload; if it does not matter, there is no reason
to record it at all.

## 4 · Consequences

**Gained**

- `U(M ⊕ σ) = M` holds structurally: any rename/move that lands the
  prompt in a different `(residence, branch, genre)` triple triggers a
  verification failure, surfacing to the operator exactly what the
  Seal was meant to detect.
- One canonical name for the three axes. Future deliberations citing
  *"the residence of the prompt"* or *"the genre of the prompt"* point
  at the same Rust field, not at a reconstructed path slice.
- A `cargo-audit`-style lint is now well-defined: a call site that
  passes a raw `PathBuf` to the future notary API is a type error at
  the function boundary, not a silent regression.
- TLA+ invariant `I_DestinationWellTyped` is expressible:
  `∀ p ∈ SealedPrompts : p.destination.residence ∈
  {solo, team, encrypted, remote} ∧ p.destination.branch ≠ "" ∧
  p.destination.genre ∈ {chronicle, adr, addl, github-surface,
  deliberation, code}`. Enforceable at the schema level once
  `cosmon-spec` lands.

**Lost / constrained**

- Canonical form bump: existing v1 notarizations stay readable but
  gain no retroactive destination. The cost of the migration is one
  re-hash per molecule when the 3-field triple is re-derived from
  state; the notary's `verify` command must learn to accept both
  forms during the grace window.
- The adapted `Residence` subset duplicates the ADR-055 enum name. The
  duplication is intentional (a *classification snapshot* vs a
  *configuration carrier*) but real — a future unification pass
  will collapse them once the ADR-055 Rust enum lands in
  `cosmon-core`.
- `BranchName` deliberately does not validate the git-ref-format
  grammar at construction time; that decision is deferred and may
  force a `BranchName::parse` that returns `Result` later.

**Open (deferred)**

- Integration with `cs notarize`: no wiring in this ADR. The struct
  lands in `cosmon-core` compile-only; the notary-side adoption and
  the canonical-form v2 bump are follow-up tasks, grouped with the
  post-S4 batch (alongside `rotate-key` and the remote-residence
  work).
- Dis-attestation: ADR-056 I3 already forbids revocation. Once the
  typed destination lands, a dis-attestation that says *"this
  destination has moved"* becomes a first-class signed record, not a
  verbal note.
- Re-hash migration tooling: write-once, run against every molecule
  with a v1 seal to produce a v2-compatible replay. Deferred to the
  migration task after `cs notarize` adopts the new canonical form.

## 5 · Implementation sketch (this ADR)

Scope shipped with this ADR:

1. New module `crates/cosmon-core/src/prompt_destination.rs` with the
   types in §2.2.
2. `#[derive(Serialize, Deserialize)]` + `parse`/`as_str` helpers for
   the newtypes.
3. Unit-test-free: this is a shape declaration, not an implementation.
   `cargo check --workspace` passes; no new tests, no new CI gates.
4. Export via `pub mod prompt_destination;` in `cosmon-core/src/lib.rs`.

Deferred (post-S4):

- Notary integration (`Commitment.prompt_destination`, domain
  separator bump to v2, `cs notarize` populates the triple from state).
- Migration tooling for v1 seals.
- TLA+ invariant in `cosmon-spec`.
- `cargo-audit` lint for raw-path call sites.

## 6 · References

- Panel synthesis:
  `delib-20260420-bae4`
- Residence axis: [ADR-055](055-cosmon-residence.md)
- Notary protocol: [ADR-056](056-notary-protocol-v0.md)
- Genre axis: [ADR-057](057-genre-and-artifact-map.md)
- tenant_auditor deck context (roadmap item): `task-20260421-88cc`

## The one-sentence destination

*The destination of a notarized prompt is the triple (residence,
branch, genre) it was headed for at mint time — anything else is a
path, and a path is a contract the compiler cannot verify.*
