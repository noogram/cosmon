# ADR-107 — Typed provenance for prompt context (§8r causal-attribution + §8s prompt-context provenance)

**Status:** proposed (blocked-by `delib-20260521-955f`; sibling of
`task-20260522-1f6b`, precondition for VISAGE worker-read path)
**Date:** 2026-05-22
**Parent deliberation:** `delib-20260521-955f` — *Architect cosmon-incarné v0*
(see `.cosmon/state/fleets/default/molecules/delib-20260521-955f/synthesis.md`)
**Authoring task:** `task-20260522-069b`
**Panelists whose verdicts converged on it:** wheeler (§5 — causal attribution),
godel (§4 — shadow causal graph / typed prompt provenance)

**Ratifies** two structural invariants that the deliberation synthesis (§5.4)
named load-bearing for cosmon-incarné and that MUST land **before** the
VISAGE worker-read path ships:

- **§8r — Causal attribution on `events.jsonl` rows.** Every row carries an
  `initiator: Operator | Tick | Ingest | Worker | Runtime` field; `cs verify
  --since <Δ> --by <variant>` becomes a one-liner read against the typed
  narration. (wheeler.)
- **§8s — Typed provenance of prompt context.** Every durable file read into
  a worker's prompt that did not originate as a typed `events.jsonl`
  projection MUST carry an explicit `SparkedBy` or `InformedBy`
  `MoleculeLink` in the receiving molecule. (godel.)

The two invariants are **complementary**, not in tension: §8r answers
*who initiated this row?*; §8s answers *what durable file leaked into the
worker's cognition without leaving a typed edge?*. One without the other
still leaks the bit — the ADR ratifies them as a unit.

**Implementation siblings (separate molecules — not covered by this ADR):**
- `task-20260522-1f6b` — cosmon-incarné v0 baseline (the sibling ADR; cites
  this ADR as a precondition).
- `task-20260522-8bcd`, `task-20260522-34e4`, `task-20260522-55a8` — the
  three implementation tasks downstream of the v0 baseline; each respects
  §8r and §8s by construction.
- Future implementation beads file separately for: `initiator` field on
  `EventRow`; `cs verify --since/--by` flag plumbing; `cs tackle` /
  `cs evolve` gate that refuses to read a non-typed durable into the
  prompt unless `SparkedBy`/`InformedBy` is registered; CI linter for the
  cosmon-managed organs (`peau-morning-digest`, `curate-patrol`).

**Related ADRs:**
- [ADR-061](061-pilot-session-and-causal-closure.md) — §8e causal closure
  of the pilot-cognition. §8s is the *prompt-context* projection of §8e:
  §8e closes the operator-side bit (cockpit becomes a molecule); §8s
  closes the worker-side bit (briefing inputs become typed links). Without
  §8s, ADR-061's closure invariant is violated by every organ that reads
  SOUL.md / hippocampus notes / voix permits.
- [ADR-047](047-event-log-protocol-v0.md) — `events.jsonl` schema and
  seal discipline. §8r extends the row schema with one optional enum
  field; legacy rows deserialise as `Unknown` via serde default.
- [ADR-066](066-ux-v2-substrate.md) — §8k' wheat-paste and §8t bounded-Δ.
  §8s sits in the same `§8…` family of structural invariants that govern
  the two-plane model (control / data) and is added in the same
  prime-notation style — invariants ratified before code lands.
- ADR-064 — §8j ingress bindings.
  §8s does **not** create a new port class; it tightens the existing
  worker-read path by requiring a typed link for non-events sources.

---

## Context

### The cosmon-incarné v0 baseline and the durable file conventions

The parent deliberation `delib-20260521-955f` ratified three durable file
conventions that organs of the cosmon-incarné v0 substrate read into worker
prompts:

- **`SOUL.md`** — the operator's living self-portrait (PEAU layer).
- **Hippocampus notes** — `.cosmon/state/hippocampe/notes/*.md` (HIPPOCAMPE
  layer summaries of WhatsApp / mail / meeting evidence).
- **Voix permits** — per-channel ingest permits granting an external
  feed (LinkedIn, Goodreads, Strava, …) the right to surface signals to
  the operator's daily digest.

None of these three are typed `events.jsonl` projections. They are files
on disk that organs read at prompt-construction time. The `cs tackle` and
`cs evolve` paths today have no contract about *what may be read into the
briefing*; in particular they do not require the receiving molecule to
record the read as a typed link.

### The shadow causal graph (godel.md §4)

Without §8s, the system grows two causal graphs side by side:

- **G(cosmon)** — the typed DAG cosmon exposes: `SparkedBy`, `InformedBy`,
  `DecayProduct`, `Blocks`, `Refines`, `Entangled`. Walkable, sealable,
  auditable.
- **G★(incarné)** — the implicit causal graph induced by *what files
  workers actually read*. Includes every SOUL.md edit, every hippocampus
  summary, every voix permit. Not walkable from `events.jsonl`. Not
  auditable from `.cosmon/state/`.

The two graphs are individually internally consistent. **Jointly** they
violate ADR-061 §8e causal-closure: a cognition that *caused* a molecule
to be dispatched (e.g. an operator edit to SOUL.md following a WhatsApp
exchange) leaves no typed edge in `events.jsonl` — the worker's
behaviour depends on the SOUL.md content, but the worker's molecule does
not declare the dependency.

The concrete chain godel called out:

```
cause:       WhatsApp message lands in inbox
summarised:  HIPPOCAMPE folds the exchange into a note
folded:      VISAGE updates SOUL.md to reflect the new commitment
acted:       worker tackles `peau-morning-digest`, reads SOUL.md, drafts the digest
missing:     no events.jsonl row records the causal step from WhatsApp → SOUL.md edit
```

A verifier reading `.cosmon/state/` can see the worker's `Sparked` event
and the resulting digest molecule. The verifier *cannot* see that the
SOUL.md edit (which materially shaped the digest) was caused by a
WhatsApp signal HIPPOCAMPE summarised three hours earlier. The bit is
real and the bit is lost.

### Observer-locked attribution (wheeler.md §5)

The complementary failure mode wheeler named: today, the §1 *liveness bit*
(*is the system alive without a human?*) is **inferred**, not **queryable**.

Reading the system means scrolling `events.jsonl` and guessing whether a
row originated from a typed keystroke (operator), a scheduled tick
(runtime), an ingest webhook (matrix-bridge, voix permit, peau morning
sweep), a worker subprocess (`cs evolve`), or a resident runtime
(`cs run`). The narration carries the rows; it does not carry the
*authorship* of the rows.

Kahneman's sticky-autonomy obituary (the falsifiable counter-claim that
cosmon is *not* drifting toward observer-required operation) becomes
structurally unfalsifiable in this regime — there is no machine-queryable
proof of which initiator class produced which row over a given window.

### Joint impact on VISAGE worker-read path

The cosmon-incarné v0 baseline (sibling ADR `task-20260522-1f6b`) is on
the verge of shipping `peau-morning-digest` and `curate-patrol` — two
organs that read SOUL.md / hippocampus notes / voix permits into worker
prompts as a routine matter. Shipping the worker-read path **without**
§8r + §8s in place would:

1. Bake the shadow-graph hazard into the substrate at the moment the
   substrate ships; refactoring after-the-fact is expensive.
2. Produce a corpus of `events.jsonl` rows with no `initiator` field —
   the legacy-tail godel warned about, growing daily.
3. Render the very first audit of cosmon-incarné inconclusive: the
   verifier cannot tell whether the operator drove the morning digest or
   whether a runtime tick + ingest pair drove it autonomously.

Hence the ADR lands **before** the VISAGE worker-read path. The two
invariants are gating, not aspirational.

---

## Decision

### (1) §8r — Causal attribution on `events.jsonl` rows

Add to `docs/architectural-invariants.md` as a new subsection in the
§8-series (next free slot after §8q):

> **§8r. Causal attribution.** Every row in any
> `.cosmon/state/.../molecules/<m>/events.jsonl` carries an `initiator`
> field whose value is exactly one of:
>
> - `Operator` — a human keystroke on a `cs` CLI, a UI tap on a wheat-paste
>   viewport, or an apfel-mediated session-note that resolves to the
>   operator's Nucléon (per ADR-066 §8j-apfel).
> - `Tick` — a scheduled patrol or autopilot tick (LaunchAgent, future
>   `cs autopilot tick`).
> - `Ingest` — a non-CLI spark admitted through a §8j ingress port
>   (matrix-bridge, voix permits, peau morning sweep, future webhooks).
> - `Worker` — a subprocess of a tackled molecule (`cs evolve`,
>   `cs complete` invoked from inside a worktree).
> - `Runtime` — the resident runtime (`cs run`, ADR-095 Phase 3+)
>   dispatching from a DAG policy.
>
> Legacy rows (pre-§8r) deserialise as `Unknown` via serde default; the
> `Unknown` variant is **read-only** and MUST NOT be written by new code.
> `cs verify --since <Δ> --by <variant>` becomes a one-liner read against
> the typed narration — the §1 liveness bit is queryable, not inferred.

**Migration.** The `initiator` field is added to the `EventRow` newtype in
`cosmon-state::events::EventRow` as `Option<Initiator>` with
`#[serde(default)]`; legacy rows read clean. New writes by every command
in the perimeter table (ADR-016 §3) MUST populate the field. The seal
chain is unaffected — `initiator` is part of the sealed bytes for new
rows, absent from the sealed bytes for legacy rows; verifiers project
both correctly because the seal covers *the bytes that exist*.

**Why one enum, five variants.** The variants partition the universe of
row authors under the current command perimeter (Inert / Propelled /
Autonomous regimes × CLI / runtime / ingress entry classes). A sixth
variant is admissible only at the cost of a new ADR; silent extension is
a structural breach. The five variants are stable across the Layer A
(Transactional Core) → Layer B (Resident Runtime) transition: `Runtime`
already names the future state.

### (2) §8s — Typed provenance of prompt context

Add to `docs/architectural-invariants.md` as the next subsection after §8r:

> **§8s. Typed provenance of prompt context.** Every durable file read
> into a worker's prompt by `cs tackle` (during briefing construction)
> or by `cs evolve` (during step rendering) that **did not originate as
> a typed `events.jsonl` projection** of the receiving molecule MUST
> carry an explicit `SparkedBy` or `InformedBy` `MoleculeLink` on the
> receiving molecule. The link points at the molecule (or fleet-level
> molecule-of-record) that owns the durable file's edit history.
>
> **Scope.** §8s applies to durable, cosmon-managed files: `SOUL.md`,
> hippocampus notes under `.cosmon/state/hippocampe/notes/`, voix
> permits under `.cosmon/state/voix/permits/`, and any future
> read-only briefing source the substrate adds. The list is closed at
> the moment of writing — a new durable category requires either a
> successor ADR or an existing §8s-compliant molecule of record.
>
> **Out of scope (explicitly).** `CLAUDE.md` (per-repo conventions read
> by every session), per-session memory under
> `~/.claude/projects/.../memory/`, and the agent harness's transient
> prompt scaffolding (formula briefings, system-reminder blocks).
> These are not durable cosmon-managed state; admitting them would
> stretch §8s beyond what the v0 audit can enforce. The shadow-graph
> hazard godel named lives *inside* `.cosmon/state/`; §8s closes it
> there.
>
> **Test of legitimacy.** A verifier walking `events.jsonl` for a
> molecule `m` MUST be able to enumerate every durable file the worker
> read during `m`'s lifetime by reading the `SparkedBy`/`InformedBy`
> links and following them to molecules of record. If a durable file
> was read and no link names it, §8s is breached — file a bead, do
> not patch the worker.

**Distinction between `SparkedBy` and `InformedBy`.** §8s reuses the
existing typed-link vocabulary (no new edge type):

- **`SparkedBy`** — the durable file *caused* the molecule to exist (the
  morning digest was nucleated *because* SOUL.md said so). Strong
  causal claim.
- **`InformedBy`** — the durable file *shaped* the molecule's briefing
  but did not cause its existence (the worker read SOUL.md as context;
  the molecule would have existed without it). Weaker dependency.

The receiving molecule chooses the edge that honours the cognitive
relationship; the verifier accepts either as §8s-compliant.

### (3) Coherence checklist (§5 of invariants) — passed by construction

| # | Question | Answer |
|---|----------|--------|
| 1 | Stateless? | Yes — neither invariant introduces a new command or daemon. §8r adds one optional enum to an existing serde record. §8s adds a precondition to an existing prompt-read path. |
| 2 | Idempotent? | N/A — invariants, not commands. The downstream `cs verify` extension is a pure read (idempotent by construction). |
| 3 | Regime-aware? | Yes. §8r names a variant per regime origin (`Operator` Inert, `Tick`/`Ingest` Propelled-watchdog, `Worker` Propelled-subprocess, `Runtime` Autonomous). §8s applies in all three regimes uniformly. |
| 4 | Single perimeter? | Yes. §8r tightens the narration row schema (one site: the event writer). §8s tightens the prompt-construction path (two sites: `cs tackle` briefing, `cs evolve` step rendering). |
| 5 | Symmetric undo? | N/A — invariants, not state-changing commands. The implementation siblings inherit existing `cs nucleate` ↔ `cs done` symmetry. |
| 6 | Runtime-compatible? | Yes. §8r is *forward-compatible* with the Resident Runtime (the `Runtime` variant already names the future state). §8s does not change the prompt-construction *mechanism*, only its precondition. |
| 7 | Worker/human boundary? | Respected. §8r distinguishes `Worker` from `Operator` at the row level — the boundary becomes queryable rather than implicit. §8s applies symmetrically to both kinds of prompt construction. |
| 8 | Write/read asymmetry? | Preserved. The `cs verify --since --by` extension is a pure read against `events.jsonl`. §8s admits new write contracts only at the molecule-creation seam (where `SparkedBy`/`InformedBy` are already authoritative). |
| 9 | Merge-before-dispatch? | N/A — no dispatch path change. The implementation siblings respect merge-before-dispatch by construction. |
| 10 | CLI-first for workers? | Yes — the prompt-construction path is the CLI (`cs tackle`, `cs evolve`); §8s tightens the CLI's contract. No MCP dependency. |
| 11 | Scope-bounded? | Yes. §8r enumerates five initiator variants, closed-set. §8s enumerates the in-scope durables (SOUL.md, hippocampus notes, voix permits + future explicit additions) and the out-of-scope durables (CLAUDE.md, per-session memory, transient prompt scaffolding). |
| 12 | Self-similar? | Yes. §8r composes with the seal chain (the field is part of the sealed bytes for new rows). §8s composes with the typed DAG (reuses `SparkedBy`/`InformedBy`, no new edge class). |
| 13 | Alphabet-Closure? | **Mandatory.** When sibling tasks land the Rust edits, the §8r + §8s text edits in `docs/architectural-invariants.md` MUST land in the same commit. |

No existing invariant is contradicted. §8e (ADR-061) is *completed* on
the worker-side by §8s. §8d (`events.jsonl` is source-of-truth) is
*tightened* by §8r.

---

## Rationale

The two invariants close two halves of the same gap.

**§8r closes the row-level gap.** Without it, `events.jsonl` is a typed
narration that records *what happened* but elides *who decided it should
happen*. The five-variant enum is the minimum sufficient typing: any
finer partition is premature, any coarser partition collapses two
auditing questions into one. The `Operator` / `Tick` / `Ingest` /
`Worker` / `Runtime` partition mirrors the existing command perimeter
table (ADR-016 §3) — it is not a new ontology, it is the *naming* of
the partition the perimeter table already implies.

**§8s closes the molecule-level gap.** Without it, the typed DAG cosmon
exposes is a strict sub-graph of the actual causal graph workers reason
over. The PEAU+HIPPOCAMPE+VISAGE chain is the concrete instance the
deliberation surfaced; future organs will produce equivalent shadow
edges unless the prompt-construction path *requires* the typed link at
the point of read.

The two invariants are individually necessary and jointly sufficient
for the cosmon-incarné v0 audit:

- §8r alone closes attribution but leaves shadow durables unrecorded —
  the audit knows *who* ran every command but not *what files* their
  cognition depended on.
- §8s alone closes durable provenance but leaves row authorship
  inferred — the audit knows *what* was read but not *who* initiated
  the surrounding row.

Without §8r, kahneman's sticky-autonomy falsification clause becomes
structurally inert: there is no way to count *Tick*-initiated rows over
24h, hence no way to falsify the claim *"cosmon is observer-locked"*.

Without §8s, ADR-061 §8e *causal closure of the pilot-cognition* is
violated whenever a worker reads SOUL.md or a hippocampus note — the
worker's behaviour depends on durable state that does not appear as a
typed link on the worker's molecule. The shadow-graph hazard is real
and observable today (the PEAU+HIPPOCAMPE+VISAGE chain is a working
example, not a hypothetical).

The ADR ratifies them together because separating them would invite
the partial-fix anti-pattern: shipping §8r in v0 with §8s deferred to
v1 would produce a system that *records* the initiator of every row
without *recording* the durable files those rows depend on — a more
auditable shadow-graph, not the absence of one.

---

## Consequences

### Positive

- **Liveness bit becomes queryable.** `cs verify --since 24h --by Tick`
  is a one-liner against the narration; the §1 sticky-autonomy obituary
  becomes falsifiable. (wheeler.)
- **Shadow-graph hazard is structurally closed.** A verifier walking
  `events.jsonl` for any molecule under cosmon-incarné can enumerate
  every durable file the worker read. (godel.)
- **VISAGE worker-read path ships on a sealed contract.** The three
  implementation tasks downstream of the v0 baseline (`-8bcd`, `-34e4`,
  `-55a8`) inherit §8s as a precondition; no organ can add a new
  durable read without registering a typed link.
- **Audit cost drops to grep-level.** Both invariants are addressable
  by grep against `.cosmon/state/`: the `initiator` field is one column
  in `events.jsonl`; the `SparkedBy`/`InformedBy` links are typed
  edges in `state.json`. No external index, no daemon, no resident
  process required.
- **Forward-compatible with the Resident Runtime.** The `Runtime`
  variant of `initiator` already names the future state; §8s applies
  to runtime-dispatched workers identically.

### Neutral / accepted costs

- **One enum field per row.** A handful of bytes per `events.jsonl`
  row, BLAKE3-sealed alongside the rest of the row. Legacy rows
  read clean via serde default.
- **Two new typed links per worker that reads a durable.** Existing
  code that constructs molecules from typed links (`cosmon-cli::cmd::nucleate`,
  `cosmon-cli::cmd::tackle`) already accepts arbitrary `MoleculeLink`
  vectors; the cost is the discipline of registering the link, not the
  cost of changing the data model.
- **One CI linter for cosmon-managed organs.** A shell-level grep over
  the formula files that ship as part of cosmon (`peau-morning-digest`,
  `curate-patrol`, and any future organ in `.cosmon/formulas/`) asserts
  that prompt-construction sites that read SOUL.md / hippocampus notes /
  voix permits also nucleate with a `SparkedBy` or `InformedBy`
  link. The linter is out-of-scope for this ADR — file as a follow-up.

### Negative (risks)

- **Legacy-tail growth.** Every `events.jsonl` row written before §8r
  ships deserialises as `Unknown`. The `cs verify --since --by` query
  must treat `Unknown` as *"pre-§8r legacy, inconclusive"* — not as
  *"unknown initiator now"*. Countermeasure: the implementation bead
  ships a one-shot reconciliation that annotates pre-§8r rows by
  reading the molecule's other artefacts (formula, fleet ledger);
  rows that cannot be back-attributed remain `Unknown` and are surfaced
  as such by `cs verify`.
- **Drift between formula-declared and worker-read durables.** A
  formula author may declare `peau-morning-digest` reads SOUL.md and
  the worker may also read a hippocampus note opportunistically. The
  linter checks declared reads; opportunistic reads break §8s
  silently. Countermeasure: the implementation bead ships an
  audit-mode flag on `cs evolve` that records every durable file
  opened during a step into `events.jsonl` and cross-checks against
  the registered links; mismatches are loud-by-design.
- **Out-of-scope durables become a future scope-creep vector.** The
  `CLAUDE.md` / per-session memory carve-out is deliberate; future
  contributors may try to widen §8s to cover them. Countermeasure: the
  scope clause is verbatim in the invariant text — widening requires
  a successor ADR.

### Open

- Whether `cs verify --since --by` should be part of the default
  `cs doctor` sweep. Tentative yes, deferred to the implementation
  bead.
- Whether the `Initiator::Ingest` variant should carry a sub-tag
  identifying the §8j port that admitted the spark. Deferred to the
  implementation bead; the v0 schema is the five-variant enum, with
  the ingress port already nameable via the existing `via:` field on
  the row (ADR-066 §3c).
- Whether voix permits should be modelled as *molecules of record* in
  their own right (each permit a `MoleculeKind::VoixPermit`) so that
  `InformedBy` always points at a real molecule rather than at a raw
  filesystem path. Deferred to the sibling v0 baseline ADR
  (`task-20260522-1f6b`); the answer affects §8s only as a
  type-system convenience, not as a structural change.

---

## Alternatives considered

### A. Do nothing (rejected)

godel's verdict explicitly named *"shadow causal graph forms within
weeks"*. The PEAU+HIPPOCAMPE+VISAGE chain is a working example; the
worker-read path is on the verge of shipping. *Doing nothing* is the
choice to ship the shadow graph as the substrate. Rejected by both
panelists, and by the deliberation synthesis §5.4.

### B. Wider invariant — *everything* read into the prompt is typed (rejected)

A tempting maximal claim: every file the worker reads (CLAUDE.md,
per-session memory, the agent harness's transient scaffolding) must
carry a typed provenance link. Rejected for v0:

- `CLAUDE.md` is conventions, not state; modelling it as a molecule of
  record would explode the typed DAG with conventional cargo.
- Per-session memory under `~/.claude/projects/.../memory/` is the
  agent's own private state, not cosmon-managed state.
- The harness's transient prompt scaffolding (formula briefings,
  system-reminder blocks) is constructed at prompt time and is not
  durable in the §8d sense.

The narrower invariant — *durable, cosmon-managed files only* — closes
the structural gap godel named without admitting non-state into the
typed DAG. The wider claim is structurally pure but operationally
unenforceable; the narrower claim is enforceable today.

### C. Two separate ADRs — one per invariant (rejected)

A tempting split: §8r in one ADR (wheeler), §8s in another (godel).
Rejected because the two invariants are tightly coupled — shipping one
without the other still leaks the bit (see *Rationale* above). The
deliberation synthesis §5.4 explicitly recommended unifying them, and
the panelists' verdicts are complementary (not in tension), so a single
ADR is the correct unit of ratification.

### D. Implement enforcement in this molecule (rejected)

The ADR ratifies the decision; the implementation lands in sibling
tasks (`task-20260522-1f6b` and its downstream `-8bcd` / `-34e4` /
`-55a8`). Implementing in the same molecule would couple the structural
ratification to the v0 code drop — if the code drop slips, the
invariant slips with it, and the VISAGE worker-read path ships on no
contract. Decoupling preserves the *propose mechanisms of verification,
do not impose them* discipline (ADR-058) one level up.

### E. New typed-link edge `ReadAsContext` (rejected)

A natural alternative to reusing `SparkedBy`/`InformedBy`: introduce a
new edge type `ReadAsContext` that names "this file was read into my
prompt." Rejected because:

- The existing `SparkedBy` / `InformedBy` distinction already captures
  the causal-strength axis (strong cause vs weaker dependency).
- A new edge type widens the typed-link vocabulary without earning
  semantic ground — every `ReadAsContext` instance is reclassifiable
  as either `SparkedBy` (file caused the molecule) or `InformedBy`
  (file shaped the briefing). Tolnay's API-minimalism counsel
  (ADR-066 §IV applied transitively) argues against the surface
  expansion.
- The verifier's walk is identical either way — it follows typed
  edges, regardless of which type.

The existing vocabulary is sufficient; do not extend it.

---

## Scope and non-scope

**In scope.** Ratifying §8r + §8s in `docs/architectural-invariants.md`;
naming the five `Initiator` variants and their semantics; naming the
in-scope durables for §8s (SOUL.md, hippocampus notes, voix permits);
naming the out-of-scope durables (CLAUDE.md, per-session memory,
transient prompt scaffolding); declaring `SparkedBy` / `InformedBy` as
the §8s-compliant edges; declaring `cs verify --since --by` as the §8r
query path; listing Followups.

**Out of scope** — each a sibling/downstream molecule:
- `Initiator` enum + `EventRow.initiator: Option<Initiator>` field in
  `cosmon-state` (implementation bead, separate molecule).
- `cs verify --since <Δ> --by <variant>` flag plumbing (implementation
  bead, separate molecule).
- `cs tackle` / `cs evolve` gate that refuses to read a non-typed
  durable into the prompt without a registered `SparkedBy` /
  `InformedBy` (implementation bead, separate molecule).
- CI linter for cosmon-managed organs that read SOUL.md / hippocampus
  notes / voix permits (implementation bead, separate molecule).
- One-shot reconciliation that annotates pre-§8r rows with best-effort
  initiator attribution (implementation bead, separate molecule).
- Voix permits as molecules of record (deferred to sibling v0 baseline
  ADR `task-20260522-1f6b`).

---

## Gate (load-bearing)

**No organ beyond `peau-morning-digest` and `curate-patrol` may read a
durable file into its prompt until §8s is enforced in `cs tackle` /
`cs evolve`.** The two existing organs are grandfathered for the
duration of the v0 ship window only; they MUST be brought into §8s
compliance by the time the implementation siblings (`-8bcd`, `-34e4`,
`-55a8`) land.

A new formula that reads a durable file MUST either:

1. register the typed `SparkedBy` / `InformedBy` link on every
   nucleation step that materialises a worker molecule, OR
2. cite this ADR explicitly in the formula's `briefing.md` and file a
   follow-up bead to bring itself into compliance before next ratification.

Silent extension of the worker-read surface without §8s registration is
a structural breach (file a bead, do not patch the formula).

---

## Followups

Tracked as sibling or downstream molecules; none block this ADR.

1. **`task-20260522-1f6b`** — cosmon-incarné v0 baseline ADR; cites this
   ADR as a precondition.
2. **`task-20260522-8bcd`**, **`task-20260522-34e4`**,
   **`task-20260522-55a8`** — the three v0 implementation tasks; each
   respects §8r and §8s by construction.
3. **`Initiator` enum + `EventRow.initiator` field** — implementation
   bead, separate molecule. Adds the optional enum field to
   `cosmon-state::events::EventRow` and threads it through every
   row-writing site (CLI, runtime, ingress, worker).
4. **`cs verify --since <Δ> --by <variant>` plumbing** — implementation
   bead, separate molecule. Extends `cs verify` to filter rows by
   `initiator` and to assert the `initiator` field is populated on
   post-§8r rows.
5. **`cs tackle` / `cs evolve` §8s gate** — implementation bead,
   separate molecule. Refuses to construct a prompt that reads a
   listed-in-scope durable without a registered `SparkedBy` /
   `InformedBy` on the receiving molecule.
6. **CI linter for cosmon-managed organs** — implementation bead,
   separate molecule. Greps `.cosmon/formulas/*.formula.toml` for organs
   that name SOUL.md / hippocampus notes / voix permits and asserts the
   formula's nucleation steps register the corresponding typed links.
7. **One-shot reconciliation for legacy `events.jsonl`** —
   implementation bead, separate molecule. Reads each pre-§8r row,
   attributes its `initiator` by best-effort heuristic against the
   surrounding molecule's formula and fleet ledger, and annotates as
   `Unknown` whatever it cannot back-attribute.
8. **Voix permits as molecules of record** — deferred to sibling v0
   baseline ADR.

---

## Acceptance

This ADR is **proposed**, not **accepted**. Operator ratification is the
explicit next step. Until ratified:

- The sibling cosmon-incarné v0 baseline ADR (`task-20260522-1f6b`) MAY
  proceed with briefing and scoping.
- The three v0 implementation tasks (`-8bcd`, `-34e4`, `-55a8`) MUST
  NOT land code that ships a new organ reading a durable file without
  a registered `SparkedBy` / `InformedBy` link.
- §8r and §8s are drafted in `docs/architectural-invariants.md` but
  flagged `(proposed — ADR-107)` so no downstream code treats them as
  hard rules prematurely.

godel's closing line from `responses/godel.md` §4 is this ADR's motto:
*"the typed DAG the system reasons over must equal the typed DAG cosmon
exposes."* The ADR gives the rule its surface.
