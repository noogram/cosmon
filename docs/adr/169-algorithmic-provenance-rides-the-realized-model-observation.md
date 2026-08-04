# ADR-169 — Algorithmic provenance rides the realized-model observation

**Status:** Accepted (2026-08-04).
**Date:** 2026-08-04.
**Decider:** Noogram.
**Authoring task:** `task-20260729-7dd4`.

**Entry artefact.** A typed escalation from the sporarium crisis-spore bench.
Its `crisis-spore-seal-property-register.md` §2.6 and
`crisis-spore-legal-regulatory-register.md` §3.1 record that pinning a model by
identifier and version is **necessary but not sufficient** for an artefact meant
to be contested, and explicitly refuse to patch around the gap locally. The
bench stated the need and left the form to cosmon. This ADR answers the form.

**Related ADRs:** the realized-model capture (`delib-20260718-c70e`) this
extends, and ADR-151 (monotone provenance for critical-task declarations).

---

## Context

### What a model id settles, and what it does not

`EventV2::ModelObserved` records the concrete id an adapter reported running.
That settles the **identity** of the method — which is real progress over the
pin, and its honesty invariant is structural: the event is emitted only when a
real id was observed, so there is no value meaning "ran but unknown".

It settles nothing about the method's **reliability** or **reproducibility**.
A hosted model's version label is a string its vendor asserts; the vendor can
re-point that label at different weights without the label changing. Nothing in
the id says at what temperature the model decoded, under what quantization, or
over what prompt — and in an incident response the prompt is partly written by
the adversary, so the prompt *is* part of the algorithm. A chain of custody that
names only the model has pinned the signature and left the function body
unstated.

### The inversion the bench asked not to lose

From the algorithmic-integrity angle alone, a **self-hosted fallback model — the
path a safety review treats as the risky one — is more verifiable than a
frontier hosted model.** Its weights are hashable, its decoding pinnable, its
run replayable. The model one trusts most for alignment is the one one can prove
least about for admissibility.

That is counter-intuitive enough to be lost in a paraphrase, so it is not left
as prose: `AlgorithmicProvenance::is_algorithm_replayable` computes it, and
`the_self_hosted_fallback_outranks_the_hosted_frontier_model` is the executable
statement of it.

## Decision

**The algorithmic-provenance subset is an optional typed field on
`EventV2::ModelObserved`, not a separate record.**

The record itself
(`cosmon_core::algorithmic_provenance::AlgorithmicProvenance`) carries:

| Field | Shape |
|---|---|
| `weights` | `Pinned { algorithm, digest }` **or** `HostedUnverifiable { asserted_by }` — no third arm |
| `quantization` | `Disclosure<String>` |
| `decoding` | temperature / top-p / seed, each a `Disclosure` |
| `prompt_context` | `Disclosure<ContextDigest>` — a digest, never the bytes |
| `reproducibility` | `Replayable { procedure }` / `NotReplayable { reason }` / `Undetermined` |

### Why on `ModelObserved` rather than beside it

The bench's open question was: widen the observation event, or attach a distinct
provenance record to the node? Three properties decide it.

1. **`ModelObserved` is already node-scoped, not exchange-scoped.** It carries
   `(mol_id, worker_id, adapter_name)` — the molecule, the attempt, the adapter.
   The bench's worry that the capture is "attached to a provider exchange rather
   than to a DAG node producing a conclusion" describes where the *emit site*
   sits, not what the event is keyed on. The scoping already exists; only the
   payload was thin.
2. **A separate record would need its own scoping, and would drift.** The
   realized id is subject to a fail-closed per-attempt guard (an observation
   from a dead worker never contaminates a re-tackle). A sibling record would
   have to reimplement that guard, and the first divergence between the two
   implementations produces a provenance attesting to a method that did not
   produce the conclusion — the exact failure the primitive exists to prevent.
   Riding the same event makes the guard shared by construction:
   `AdapterAttribution::fold_with_provenance` is one pass.
3. **Universality is a property of the carrier.** The subset must hold on *every*
   node producing a conclusion, not only on fallback events. `ModelObserved` is
   emitted by every dispatch that observes a model at all; a new event would
   have to be wired at every seam separately, and would be absent wherever
   someone forgot. Fallback-specific facts (trigger, transition kind, attempt
   number) stay on the fallback events, where they belong.

### Why `Disclosure`, not `Option`

Every field is either `Observed(v)` or `Undisclosed(reason)`. `Option` spells
the negative case as *nothing*, and nothing is exactly what an opposable record
may not contain: a reader cannot distinguish a field the writer never had from a
field nobody filled in. The reasons are load-bearing and not interchangeable —
`NotSetByCosmon` is an operator-fixable gap (pin the parameter),
`HostedProvider` and `AdapterSilent` are not (change provider, change adapter).
Collapsing them would tell an auditor to go fix something unfixable.

`weights` has no undisclosed arm at all: a node either pins a digest or declares
`hosted_unverifiable`. Both are statements. Not answering is not among the
options — this is the runtime's half of the doctrine the bench applied to
itself.

### What the emitters disclose today

- **anthropic / openai (in-process providers)** — `hosted_unverifiable` asserted
  by `base_url`, plus the **digest of the request body actually sent**. That last
  leg is real: cosmon holds those bytes. Decoding is `NotSetByCosmon`, which is
  literally true — neither adapter sets a temperature, top-p or seed today, so
  the provider's unstated default is in force. Recording that as `NotSetByCosmon`
  rather than as a guessed value is what makes the gap actionable.
- **claude / codex (session-log adapters)** — `adapter_silent`: the transcript
  reports a model id and nothing else about the decode.
- **Nobody emits a fabricated `Pinned`.** A self-hosted adapter that hashes its
  weights builds the record with `with_weights`; until one does, the field says
  so.

### The claim and the verdict are separate axes

`reproducibility` is what the *emitter claimed*. `is_algorithm_replayable()` is
computed from the three legs (pinned weights, replayable decode, observed prompt
digest). An audit compares them, and a `Replayable` claim over a record that
fails the computation is precisely the discrepancy worth surfacing — so the
computed verdict deliberately does not inherit the claim.

## Consequences

- `cs observe <id>` prints one line under `Model:` naming the verdict and the
  gaps that produced it. The line is **omitted entirely** when no in-scope
  observation carried a record: an absence is not a disclosure, and printing one
  would suggest a producer looked.
- **The machine surface is the journal itself.** The record is serialized on the
  `model_observed` line of `events.jsonl`, append-only and readable with `jq`
  alone. For an artefact meant to be contested that is the right surface: a
  derived view can be recomputed differently later, whereas the line that was
  written at the moment of the conclusion cannot. `cs peek --json` is
  deliberately **not** extended. That schema publishes a
  fleet slice under a stated rule — a field ships only when it is settled and
  not reconstructible — and provenance is neither fleet-scoped nor settled. The
  per-molecule read is `cosmon_state::ops::realized_provenance`.
- `emit_model_observed` / `emit_new_model_observations` now take the record as a
  **value, not an `Option`**: every emitter answers the question, and the honest
  floor for an adapter that can disclose nothing is itself a statement.
- `None` survives on the wire for lines written before this field existed. It is
  an absence, not a disclosure, and the fold reports it as such.

### What this does not do

It does not make any current cosmon dispatch replayable. Every adapter shipping
today returns `false` from `is_algorithm_replayable()`, and says why. That is
the point: the runtime now grows the field, and where it has nothing to put in
it, it is said — in writing, on the journal, per node.
