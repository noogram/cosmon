# ADR-140: the `spore` format, `expand()`, the `deterministic` cache, and ASTRA emission

**Status:** Accepted (2026-06-29). Doc-only; refines and extends
[ADR-139](139-spore-shareable-polymer-template.md).
**Date:** 2026-06-29.
**Decider:** Noogram (operator canonisation; brainstorm decisions gravées).
**Scope discipline.** This ADR fixes the **schema, the expansion semantics, the
seal-verification contract, the `deterministic` cache trait, and the ASTRA
emission point**. It lands **no implementation code**. The build is the
follow-up DAG specified in
[`docs/design/spore-impl-dag-manifest.md`](../design/spore-impl-dag-manifest.md),
which this molecule's pilot will foam. Where ADR-139 fixed the *noun* and the
*ontology*, ADR-140 fixes the *contract the code must satisfy*.

**Builds on (realize, do not reinvent):**
- [ADR-139](139-spore-shareable-polymer-template.md): the `spore` primitive,
  the `germinate` verb, the structural reduction
  `Spore = Fleet + [Formula] + ParamSchema + DAG-of-typed-edges + optional .tla seal`,
  and D3 (the seal must gate `expand()`, fail closed).
- [ADR-043 step-hash-validation-modes](043-step-hash-validation-modes.md): the
  `validation_mode` field already on `Step` and the input-hashing machinery.
  The `deterministic` cache below is that machinery promoted from a per-step
  memo to a content-addressable molecule cache; it is **not** new hashing.
- [ADR-038](038-fleet-composability-v0.md) /
  [ADR-039](039-fleet-composability.md): fleet composition and the content-
  addressed `FleetId`. The spore bundle is content-addressed the same way;
  *content-addressing is the registry* (ADR-039 §1).
- ADR-004: `formula` as the
  immutable template of ONE molecule. A spore is the template of the whole DAG.
- [ADR-111](111-mission-convention-existing-primitives.md): a `mission` is the
  live instance (convention, not runtime). A germinated spore is a mission.
- Design note
  [`docs/design/spore-reproducibility.md`](../design/spore-reproducibility.md):
  the trust spectrum (`verify_requires_execution`) this ADR turns into the
  `deterministic` field.
- Knowledge note `2026-06-27-lanusse-astra-lightcone-intersection.md`: ASTRA is
  the descriptive layer a spore composes with at share time.

---

## Context: what ADR-139 left for the format ADR

ADR-139 named the cell and proved a word was owed. It deliberately deferred four
things to "Implémentation (future)": the exact wiring-manifest schema, the
`expand()` semantics, the seal-as-gate wiring, and the sharing mechanism. Since
then a brainstorm (CMB `2026-06-29-0108-from-grace-polymeriser-mission-spore`)
fixed four design decisions that this ADR graves into a contract. They are not
re-litigated here; they are formalized.

The reference prototype
`/srv/cosmon/workshop/spores/grace-business-analysis/` already shows most of the
shape: a `spore.toml` manifest, two formulas, a real TLC-checkable `spore.tla`
seal, a mission template. This ADR's job is to (a) generalize the prototype's
fan-out into a typed **node-kind taxonomy** that distinguishes pre-determined
from emergent topology, (b) specify what the seal quantifies over once topology
can be emergent, (c) add the `deterministic` cache trait, and (d) name the ASTRA
emission point.

---

## Decision

### D1: a `spore` is a mission plan laid over a `fleet`, with three node kinds

A `fleet` is an **org chart of roles**: which personas exist, how they relate,
how feedback loops are wired. Its instantiation count is variable at run time.
A `spore` is a **mission plan laid over a fleet**: it declares, on top of the
crew, the **loops**, the **pre-determined zones** (topology frozen at
germination), and the **emergent zones** (topology decided at run time by the
agents that ran before).

Every `[[spore.node]]` declares exactly one of three kinds. The kind is what
the seal has to reason about, so it is explicit, not inferred:

| node kind | `kind =` | count known at... | bounds source |
|-----------|----------|-------------------|---------------|
| **fixed** | `"fixed"` (default) | germination (always 1) | trivially 1 |
| **pre-determined fan-out** | `"fanout"` | germination (from a param list) | the param list length |
| **emergent zone** | `"emergent"` | run time (an upstream node decides) | a declared `[bounds]` block |

- A **fixed** node germinates one molecule (e.g. `frame`, `synthesize`,
  `graded-verdict` in the prototype). `for_each` is absent.
- A **pre-determined fan-out** node germinates one molecule per entry of a
  *parameter* list. `for_each = "${params.axes}"`. The expander knows the count
  the moment it has the params, so this needs no `[bounds]` block: the param
  list *is* the bound.
- An **emergent zone** germinates one molecule per item of a value produced by
  an *upstream node at run time* (e.g. `for_each = "${nodes.analyse-axis.findings}"`,
  whose cardinality is unknown until `analyse-axis` has run). This is the new,
  load-bearing case. Because the count is not knowable at germination, an
  emergent zone **MUST** declare a `[spore.node.bounds]` block (D2) or the
  spore fails to load. Without bounds the topology is unprovable, and an
  unprovable emergent zone is exactly the unbounded foaming ADR-139 forbids.

> The prototype's `verify-finding` node is, in this taxonomy, an emergent zone:
> its `for_each` ranges over `${nodes.analyse-axis.findings}`. The prototype
> bounded it implicitly via the seal's `N`. ADR-140 makes that bound an explicit,
> declared, per-node object so the format carries it rather than the proof
> author remembering it.

### D2: emergent zones declare bounds; the seal proves safety over the *space* of topologies

The seal does **not** prove a fixed DAG. It proves a property of the
**generator** over the whole space of bodies the emergent zones can grow into.
The brainstorm's exact words: *whatever the growth of the emergent zones, the
mission converges, gates stay fail-closed, and there is no resource collision.*

For that to be model-checkable, each emergent zone declares its bounds:

```toml
[[spore.node]]
id   = "verify-finding"
kind = "emergent"
for_each = "${nodes.analyse-axis.findings}"
formula  = "grace-axis-analysis"

[spore.node.bounds]
output_type    = "finding"   # the type of item the upstream node emits
max_instances  = 64          # hard ceiling on the fan-out; the foaming variant
stop_condition = "all findings from analyse-axis consumed exactly once"
```

The three bound fields map one-to-one onto the three properties the `.tla` seal
must establish over the emergent space:

1. **`max_instances`** feeds **bounded foaming / termination**: the model
   checker quantifies the emergent fan-out as a nondeterministic choice of up
   to `max_instances` children, not a fixed `N`. If termination holds for the
   worst case it holds for every smaller realisation.
2. **`stop_condition`** feeds the **fail-closed gate**: the downstream gate
   opens only when the stop condition is met (every emitted item consumed
   exactly once). On ambiguity the gate denies.
3. **`output_type`** feeds **deterministic parametrization and no resource
   collision**: items of a declared type are accounted for exactly once
   (conservation), and concurrent emergent children of the same type are
   isolated by the fleet's `concurrency_cap` + `isolation = "worktree"` so two
   children never write the same resource.

The seal therefore carries a **fourth named property** beyond ADR-139's three:

4. **`NoResourceCollision`**: at most `concurrency_cap` emergent children of a
   given `output_type` are in flight at once, and worktree isolation guarantees
   their writes do not alias. This is the property that makes emergent growth
   safe rather than merely terminating.

The `.tla` model for an emergent spore replaces the prototype's
`CONSTANTS Axes = {...}` (a fixed set) with a bounded nondeterministic emitter:
`CONSTANTS MaxInstances` and an action that emits between `0` and
`MaxInstances` items. TLC checks the four invariants over that range. The
prototype's fixed-`N` model is the special case `MaxInstances = N` with a
deterministic emitter; both are valid seals, one stronger.

### D3: `expand(spore, params)` is a pure function that replays to `cs nucleate`

`expand` takes a parsed spore and a parameter binding and returns an **ordered
list of `cs nucleate ... --blocked-by ...` invocations**, nothing else. It is
the moral twin of `cs fleet resolve`: a declarative front end over an existing
verb, not a scheduler and not a new molecule type. *"A Makefile that replays as
`cs nucleate`."*

```
expand(spore, params) -> [ NucleateCall { formula, vars, blocked_by, alias } ]
```

Resolution algorithm (deterministic; same spore + same params yields the same
list, byte for byte):

1. **Validate** params against `ParamSchema` (types, required, enum membership).
   A missing required param or a type mismatch is a hard error; expansion does
   not partially proceed.
2. **Resolve fixed nodes** to one `NucleateCall` each, substituting `${params.*}`
   in `[spore.node.vars]`.
3. **Resolve pre-determined fan-out nodes**: for each entry of the referenced
   param list, emit one `NucleateCall`, binding the loop variable
   (e.g. `${axis}`).
4. **Emit emergent-zone placeholders**: an emergent zone cannot be expanded at
   germination time (its `for_each` ranges over a *runtime* value). It expands
   to a single **fan-out controller node** carrying the `[bounds]` block; that
   node, when its upstream completes, performs the run-time fan-out inside the
   declared bounds (this is ordinary dynamic-DAG foaming, ADR-026, now bounded
   and proven). The controller is the static handle the seal quantifies over.
5. **Topologically order** by the typed edges (`feeds` / `produces` / `verifies`
   become `Blocks` / `BlockedBy`), and set each call's `--blocked-by` to its
   predecessors' aliases. A cycle in the edges is a hard error.

`expand` is **pure**: no I/O, no clock, no randomness. It belongs in
`cosmon-core` behind a trait, exactly like the rest of the zero-I/O domain. The
shell (`cs spore run`) executes the returned list against the live state store.

> **Ordering note.** The returned list is ordered so a caller could replay it
> top to bottom and every `--blocked-by` alias is already defined. This is what
> makes a spore a *Makefile*: reading the list top to bottom IS the build plan.

### D4: the seal-verification contract: present / checked / unchecked-honest

Per ADR-139 D3 the seal must gate `expand()` and fail closed. But the machine
running `cs spore run` may not have a JRE / TLC available (the prototype's seal
has never been TLC-checked for exactly this reason). The contract makes the
three states **honest and visible** rather than silently passing:

| seal state | meaning | `expand()` behaviour |
|------------|---------|----------------------|
| **absent** | no `[spore.seal]` block | germinate, print `seal: none` |
| **present + checked** | `.tla` proof verified (this run or a cached BLAKE3 of a prior pass) | germinate, print `seal: verified <hash>` |
| **present + unchecked** | `.tla` present, TLC unavailable or not yet run | germinate ONLY under `--allow-unchecked-seal`, print `seal: present, NOT verified`; default behaviour is to **refuse** |

The honesty rule, stated as an invariant: **`cs spore run` never claims a seal
is verified when it is not.** A present-but-unverifiable seal does not silently
degrade to "good"; it forces the operator to either provide TLC or explicitly
opt into the risk with `--allow-unchecked-seal`. The default is fail-closed: a
sealed spore on a JRE-less machine refuses to germinate rather than germinate
while implying a proof it never ran. This is the difference between the spore
seal (a lock on a generator handed to a stranger) and the molecule seal (a
defensive trace, never on the hot path).

A verified check is cached: `cs verify` records `BLAKE3(spore.tla + spore.cfg)`
against the "TLC passed" verdict, so re-germinating a spore whose seal bytes are
unchanged reuses the prior pass without re-running TLC. The cache key is the
content of the proof, so any edit to the proof invalidates the cached verdict.

### D5: the `deterministic` formula trait and the content-addressable molecule cache

This absorbs OxyMake's content-addressable caching **into cs as one ontology**,
not a second binary. A formula gains a boolean trait:

```toml
# In a .formula.toml:
deterministic = true   # default false
```

Semantics, layered directly on ADR-043's existing input-hashing:

- **`deterministic = true`** declares the formula a **pure function of its
  inputs**: same resolved vars + same upstream input artifacts yields the same
  output bytes (a build, a schema regen, a hash, a deterministic transform).
  Its `verify_requires_execution` bit (the design note's conceptual field) is
  `false`. Such a molecule is **cachable by content**: the runtime computes
  `cache_key = BLAKE3(formula_id || resolved_vars || sorted(input_artifact_hashes))`,
  and if `.cosmon/cache/<cache_key>` already holds an output, it **skips
  execution** and links the cached artifact. Same inputs, skip.
- **`deterministic = false`** (default, every agentic formula today) declares
  the molecule's work an LLM session: **not** byte-reproducible.
  `verify_requires_execution = true`. The molecule is **sealed and re-executed**,
  never content-skipped. `cs verify` certifies the process (artifact present,
  gate passed, chain intact), not a content hash that would replay identically.

The trust spectrum (`docs/design/spore-reproducibility.md`) thus becomes a
single ontology field: `deterministic` on the formula drives both the cache and
the verification mode. A spore spans the spectrum because a polymer mixes both
kinds of node; the cache transparently skips the deterministic ones and the
seal + re-execution covers the agentic ones. **One tool, one ontology, one
cache.** No second engine.

> **Relationship to ADR-043.** ADR-043 already hashes step inputs for
> memoization and drift detection (`validation_mode`). `deterministic = true`
> reuses that exact machinery and adds two things: (a) it promotes the memo from
> per-step-within-one-molecule to a cross-molecule content-addressable store
> keyed by the same hash, and (b) it couples the hash to the verification mode.
> No new hashing primitive is introduced.

### D6: a spore emits an ASTRA at share time; compose, do not reinvent

ASTRA (*Agentic Schema for Transparent Research and Analysis*, Lanusse et al.)
is an **open descriptive specification**: a YAML/RO-Crate-compatible schema that
captures provenance, inputs, and method by construction so a reader can trust a
result **without re-running it**. A spore is the complementary object: a
**seed** that regenerates a mission **by executing it, proof in hand**. They are
two faces of one problem, not competitors.

Therefore: at **share time** (`cs spore export`, and at the completion of a
germinated mission), a spore **emits an ASTRA-compatible descriptive layer**
alongside its content-addressed bundle. cosmon does **not** reinvent the
descriptive schema; it composes with the existing one:

- the spore's `[spore]` header, `ParamSchema`, and node/edge DAG map onto
  ASTRA's method/provenance fields;
- the seal verdict (D4) is attached as the proof artifact ASTRA references;
- the germinated mission's artifact chain (per-node `cs verify` traces) populates
  the run-provenance section.

The **moat is `prouvable`**: ASTRA gives a reader a trustworthy *description*;
the spore seal gives a reader a *proof* that the generator is safe over the
space of bodies it can grow. Openness alone is not the moat; the seal is.
Positioning, stated positively and never by negation:

> **cosmon is the substrate where, at any scale, every step of a mission is
> inspectable and the whole orchestration is provable.**
> Moat word: **provable**.

---

## The schema, section by section

The canonical annotated schema is
[`docs/design/spore-toml-annotated.toml`](../design/spore-toml-annotated.toml)
(deliverable B), a commented `spore.toml` derived from the workshop prototype and
extended with the D1 node-kind taxonomy, the D2 `[bounds]` blocks, the D5
`deterministic` trait, and the D6 ASTRA stanza. The sections, in order:

1. `[spore]`: name, version, description, `verb = "germinate"`.
2. `[spore.seal]`: `module`, `config`, `properties` (now four, including
   `NoResourceCollision`).
3. `[spore.params.*]`: the `ParamSchema` (type, required, default, enum values).
4. `[spore.fleet]`: crew reference + `concurrency_cap` + `isolation` (the bound
   the `NoResourceCollision` invariant quantifies over).
5. `[spore.formulas.*]`: per-node recipe aliases, each with the optional
   `deterministic` trait (D5).
6. `[[spore.node]]`: each with an explicit `kind` (`fixed` / `fanout` /
   `emergent`); emergent nodes carry a `[spore.node.bounds]` block (D2).
7. `[[spore.edge]]`: typed edges (`feeds` / `produces` / `verifies`) that become
   `--blocked-by` links.
8. `[spore.astra]`: the descriptive-emission stanza (D6) naming the ASTRA
   profile and output path.

---

## Consequences

- **The format is fixed; the build is unblocked.** The impl DAG in
  `docs/design/spore-impl-dag-manifest.md` can be foamed against this contract.
- **Emergent topology is now first-class and provable.** The node-kind taxonomy
  (D1) plus declared bounds (D2) close ADR-139's gap between "static wiring you
  can prove" and "dynamic foaming you cannot". An emergent zone is dynamic *and*
  provable because its bounds are declared and the seal quantifies over them.
- **One cache, one ontology.** The `deterministic` trait (D5) absorbs OxyMake's
  content-addressable caching into cs by reusing ADR-043's hashing. Deterministic
  molecules skip; agentic molecules seal and re-execute. No second binary.
- **The seal is honest about its own status.** The present / checked /
  unchecked-honest contract (D4) means a JRE-less machine never lies about a
  proof; it refuses by default or opts in explicitly.
- **Sharing composes, it does not reinvent.** A spore emits an ASTRA descriptive
  layer (D6); the proof is the moat, not the openness.
- **Rollback path:** doc-only. `git revert` of the introducing commit. Every
  cited primitive (`Formula`, `Fleet`, BLAKE3 seals, `cs verify`, ADR-043
  hashing) is untouched.

### Explicit non-goals (v0)

- No `SporeMolecule` super-type, no new typestate, no new scheduler. `expand()`
  replays to existing `cs nucleate`; emergent fan-out is existing dynamic-DAG
  foaming (ADR-026) under declared bounds.
- No new hashing primitive. `deterministic` reuses ADR-043.
- No bespoke descriptive schema. ASTRA is composed with, not rebuilt.
- The bundle-distribution registry (how a `.spore` is published and discovered)
  remains the separate design question ADR-139 flagged; content-addressing
  (ADR-039) is the assumed substrate but the registry surface is out of scope.

---

## Amendment — `task-20260723-d399` (2026-07-24): `bounds.instances_var`, the ceiling that binds

D2 above declares `max_instances` as the ceiling the seal quantifies over. It
left one gap open: the ceiling is a number in the `[bounds]` block, while the
loop an emergent zone actually runs is driven by a **var** handed to that node's
formula. The cosmon-dev `converge` zone is the live instance — `max_instances = 5`
next to `max_rounds = "${params.max_rounds}"`, two numbers with nothing tying
them together. `cs spore validate --var max_rounds=100` printed a clean call list
and `cs spore run` germinated it: a zone bounded on paper, unbounded in the run,
with the seal's Termination argument quietly resting on a number the expansion
had already contradicted.

`[spore.node.bounds]` gains a fourth, optional field:

```toml
[spore.node.bounds]
output_type    = "review-round"
max_instances  = 5
stop_condition = "both engines CLEAN in the same round"
instances_var  = "max_rounds"   # WHICH var carries the run-time count
```

`expand()` (and therefore both `cs spore validate` and `cs spore run`) refuses
when that var resolves above `max_instances`, and refuses equally when a declared
`instances_var` does not resolve to a whole count at all — an uncheckable ceiling
is not a ceiling. The field is optional but not an opt-out: with no explicit
binding, the conventional loop-bound names the shipped formulas already use
(`max_rounds`, `max_iterations`, `rounds`, `iterations`) are checked against the
ceiling, so a spore written before this amendment fails closed without an edit. A
var still holding an unresolved `${nodes.*}` runtime reference carries no static
count and is left to the run-time controller; nothing at expansion can bound it.

A dangling `instances_var` — one naming a var the node does not declare — is a
**parse-time** refusal, not a silently skipped check.

---

## References

- [ADR-139](139-spore-shareable-polymer-template.md): the `spore` primitive and
  the `germinate` verb.
- [ADR-043 step-hash-validation-modes](043-step-hash-validation-modes.md): the
  input-hashing the `deterministic` cache reuses.
- [ADR-038](038-fleet-composability-v0.md),
  [ADR-039](039-fleet-composability.md): content-addressed `FleetId`.
- [ADR-026](026-dynamic-fleet-orchestration.md): dynamic DAG foaming (the
  bounded emergent fan-out runtime).
- [ADR-111](111-mission-convention-existing-primitives.md): `mission` as the
  live instance.
- [`docs/design/spore-reproducibility.md`](../design/spore-reproducibility.md):
  the trust spectrum behind the `deterministic` trait.
- [`docs/design/spore-toml-annotated.toml`](../design/spore-toml-annotated.toml):
  deliverable B, the annotated schema.
- [`docs/design/spore-impl-dag-manifest.md`](../design/spore-impl-dag-manifest.md):
  deliverable C, the impl decomposition.
- Reference prototype: `/srv/cosmon/workshop/spores/grace-business-analysis/`
  (citation-only fixture; lives in workshop).
- Knowledge note `2026-06-27-lanusse-astra-lightcone-intersection.md`: ASTRA vs
  spore, the two faces of one problem.
