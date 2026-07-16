# Noogram Core v0 — Non-Negotiable Invariants

> Operational semantics over an abstract state machine. No mention of any
> particular implementation, storage medium, or programming environment is
> intended; none is load-bearing. This document fixes the minimal invariants
> that make a trace a valid noogram trace. A later body (projections, derived
> rules) may be revised without amending this core.

## 1. Purpose

The **noogram** is a typed event registry of cognition acts. Each act is an
atomic, dated, attributed, content-addressed entry; the registry is
append-only. A noogram does not *contain* a theory — it exhibits a trace on
which theories can be projected. This specification fixes the minimal
invariants that distinguish a valid noogram trace from any other sequence of
bits.

## 2. State

A noogram state `Σ` is a tuple `⟨E, ≺, τ, α, κ, φ⟩` with:

- `E` — a finite set of **cognition events**.
- `≺ ⊆ E × E` — a strict partial order (causal predecessors).
- `τ : E → T` — an **existence-in-time** mapping to a totally ordered time domain `T`.
- `α : E → A` — an **authorship** mapping to agent identities.
- `κ : E → B*` — a **content** mapping to finite bit-strings.
- `φ : E → H` — a **content-digest** function, deterministic in `κ`.

The triple `(α(e), τ(e), φ(e))` is the **claim tuple** of event `e`. The
digest function `digest : B* → H` is fixed for the lifetime of Σ and is not
part of Σ.

## 3. Transitions

Exactly three operations act on `Σ`:

**T1. `append(e)`.** Admits an event `e ∉ E` into `E`. Preconditions: `α(e)` is
defined; `κ(e)` is finite; every `e' ≺ e` belongs to `E`. Postcondition:
`φ(e) = digest(κ(e))`.

**T2. `project(π, Σ)`.** Yields a derived view `π(Σ)` from the components of
Σ. A projection is a total function; it never mutates Σ. Projections are
decided at the observer boundary, not in the core.

**T3. `seal(s, Σ)`.** Admits an **external seal** `s` referencing a claim
tuple. A seal is itself an event (`s ∈ E`) and carries a marker distinguishing
it from ordinary cognition events. The signatory set of `s` and the procedure
by which it is verified are outside Σ (see §5, I5).

No other operation changes Σ. Removal is never a transition. Rewriting an
existing event is never a transition.

## 4. Observables

An external observer of Σ sees only the following:

1. The set of claim tuples `{(α(e), τ(e), φ(e)) | e ∈ E}`.
2. The predecessor order `≺`.
3. The outcome of any projection the observer chooses to compute.

An observer never sees `κ(e)` except via `φ(e)` (which may be verified against
a re-presented preimage). Confidentiality of `κ` is therefore decidable at the
observer boundary and is not a core invariant.

## 5. Invariants

Seven axioms. Each is load-bearing: removing any one admits a counter-model of
cognition-without-accountability.

**I1 — Existence-in-time.** If `e ∈ E` then `τ(e)` is defined and monotone
along `≺`: `e' ≺ e ⇒ τ(e') ≤ τ(e)`.

**I2 — Content-integrity.** For every `e ∈ E`, `φ(e) = digest(κ(e))`. `digest`
is total, pre-image resistant, and fixed for the lifetime of Σ.

**I3 — Authorship.** For every `e ∈ E`, `α(e)` is defined and is not
rewritable after `append(e)`.

**I4 — Trace durability.** Every accepted transition produces at least one
durable artifact content-addressed by its bytes. No transition is witnessed by
absence.

**I5 — External ratification (P_external).** No event `e` asserting `Cons(Σ)`
— the coherence of Σ itself — may be accepted unless it carries a seal whose
signatory set lies outside Σ. Neither the signatory set nor the procedure by
which seals are verified is specified by Σ.

**I6 — Seal symmetry.** For every seal `s` admitted by T3, there exists a
reverse form `s⁻¹` that retracts `s` by hash-reference. `s⁻¹` is itself a
seal; no prior seal is mutated in place.

**I7 — Projection purity.** Projections (T2) have no side effect on Σ.
Reading never writes.

## 6. Declared limitations

This specification is **decidable** over its transitions: each transition can
be verified by local inspection of preconditions and postconditions on a
finite neighbourhood of Σ. It is **incomplete** by design: the specification
does not — and cannot — certify the coherence of any noogram with respect to
itself. The canonical undecidable statement of a noogram Σ is:

> **G(Σ).** *No seal admitted in Σ attests that Σ is coherent under a
> verification procedure specified only in Σ.*

G(Σ) is expressible but not provable inside Σ. It is exhibited here as the
declared horizon of this specification, not as a defect. Any attempt to
internalise the verification procedure of I5 into Σ reconstructs G(Σ) as a
diagonal and collapses the external anchor.

The following are **explicitly outside** this specification and must be fixed
elsewhere:

- the digest algorithm underlying `digest`;
- the agent identity scheme underlying `A`;
- the time domain `T` and its mapping to any physical clock;
- the signatory set of seals, its cardinality threshold, and the procedure by
  which seals are verified;
- the encoding of events into durable artifacts;
- the continuity of governance across successive custodians of Σ.

Each of these is a declared external dependency. A companion trust model
enumerates the out-of-band hypotheses and the reference frames they inhabit.

## 7. Glossary

Each term reduces to a bit, a relation, or a total function. A term that does
not reduce is not admitted in this document.

- **cognition event** — an atomic entry in `E`; one bit per claim tuple
  (happened at `(author, time, digest)` / did not).
- **claim tuple** — `(α(e), τ(e), φ(e))`; the minimal external footprint of an
  event.
- **digest** — a total, fixed, pre-image-resistant function `B* → H`.
- **projection** — a total function on Σ, returning a view, never mutating Σ.
- **seal** — an external-witness assertion about a claim tuple, admitted as a
  marked event under I5.
- **external** — not derivable from Σ alone; grounded in a reference frame
  that Σ does not control.
- **coherence** — the property of a trace satisfying I1–I7; not
  self-certifiable (see G(Σ)).
- **observer** — an agent reading Σ through its observables (§4); not
  necessarily an author.
- **durable artifact** — any bit-string whose presence persists across
  observers and whose identity is `digest` of its bytes.

Ten entries. Any addition is a revision of the core and triggers I5.
