# ADR-105: I9' Federation Provenance — Naming the Second Gödel Sentence of Cosmon

**Status:** Accepted
**Date:** 2026-05-19
**Parent molecule:** `idea-20260518-d632`
**Governing deliberation:** `delib-20260518-9608` (six-persona panel:
wheeler, torvalds, shannon, godel, godin, knuth) — synthesis at
`.cosmon/state/fleets/default/molecules/delib-20260518-9608/synthesis.md`,
per-persona responses at `responses/{wheeler,torvalds,shannon,godel,godin,knuth}.md`.
**Audit:** an internal audit (provenance drift, 2026-05-17)
(three SHA drift on the provenance gate, all three legitimate
cross-galaxy or back-merge work).
**Extends:** [ADR-052](052-one-ledger-one-writer-one-witness.md) §I9 —
this ADR is a *strict superset* of I9, not an amendment. When
`TrustedFederation = {cosmon}`, I9' collapses back to I9 verbatim.
**Related:**
[ADR-032](032-p-external-witness-axiom.md) (`P_external` — the
constitutional ground of "no system certifies itself"),
[ADR-046](046-p-legibility-axiom.md) (`P_legibility` — every state
decision is human-legible),
[ADR-047](047-event-log-protocol-v0.md) (syzygie protocol — the
`inherit / adapt / refuse` discipline),
[ADR-049](049-cosmon-ward-feedback-flow.md) (cosmon-ward feedback —
this ADR is a binding instance for the federation primitive).

## Context

On 2026-05-17, three merge commits on `cosmon-main` failed the
provenance gate `scripts/check-provenance.sh` (ADR-052 §I9). The
internal provenance-drift audit (2026-05-17)
established a critical fact: **all three SHA are legitimate work.**
Two of them (merges 1 & 2 — `195ff5aa`, `23ce90c1`) are cosmon-ward
deliveries from the sister galaxy `smithy`. The third
(`8a00a8ad`) is a back-merge of `main` into a feature branch during
conflict resolution. Neither pattern is a fraud — they are *real
work* that the gate, as currently written, cannot see.

The audit's diagnosis split the drift into two distinct patterns:

- **α (smithy-cross-galaxy)** — merges 1 & 2. The `mol_id` exists,
  but it exists in `/srv/cosmon/smithy/.cosmon/state/events.jsonl`,
  not in cosmon's ledger. Even if the regex accepted the slug suffix
  and the parenthetical narrative, the ledger-lookup step would still
  fail. **This is a semantic question**: what does *"completed
  somewhere"* mean when *"somewhere"* extends beyond the local
  state machine?

- **β (back-merge)** — merge 3. `cs done` never emits the
  back-merge direction (`Merge branch 'main' into feat/X`), but git
  produces it by default when a feature branch re-syncs with `main`
  during conflict resolution. **This is a syntactic gap**: the
  regex set is incomplete by omission.

The deliberation `delib-20260518-9608` opened a six-persona panel
to answer the load-bearing structural question: *"Is α a second
Gödel sentence of cosmon, or is the gate simply broken?"*

The panel converged unanimously on the answer **α is a second Gödel
sentence** — structurally isomorphic to I9 (ADR-052 §I9), one level
higher in the Tarski hierarchy of theory extensions. Six independent
arguments arrived at the same conclusion:

- **wheeler** named the misdiagnosis: *"un juge de Marseille
  refusant un certificat de naissance de Lyon parce que sa propre
  mairie n'a pas l'entrée"*. The gate is not broken; it is reading
  the wrong registry.
- **godel** constructed the formal Tarski-extension argument
  (responses/godel.md §1–§2): `T_cosmon` cannot prove
  `LegitMerge_cross(m)` because `mol_id` for smithy merges is
  not in its language. Adding a federation axiom produces a strictly
  stronger theory `T_cf`, but `T_cf` admits its own Gödel sentence.
  The sequence is infinite.
- **shannon** quantified the channel: the cross-galaxy merge needs
  ~33 bits per commit (the 30-bit canonical mol_id + ~3 bits of
  federation address space for ≤ 16 galaxies). The gate today
  carries 30 syntactic bits + 1 ledger-witness bit; the federation
  bits are *unallocated*, not absent in principle.
- **godin** identified the cultural inversion: the gate's current
  FAIL message leads with `COSMON_PROVENANCE_SINCE=1` (the escape
  hatch as the easy remedy). This trains workers to bypass rather
  than to *name what their work is*. The remarkable sentence: *"Every
  merge commit must name what it is — a `cs done` of a tracked
  molecule, a cross-galaxy delegation with its sister-ledger trace,
  or an honest back-merge in conflict resolution — and the gate
  refuses only the unnamed."*
- **knuth** wrote BNF for each candidate grammar and proved
  Case 1 (slug-cover with phantom mol_id) is caught by ledger lookup
  under Q2(b) but **Case 2 (slug-hijack on real mol_id)** survives
  every option short of tree-hash binding in `merge_dispatched` —
  flagged as a separate ADR pass.
- **torvalds** insisted on the engineering form: federation cannot
  be designed in bash before it exists as an abstraction. Ship B+B'
  today, name the doctrinal frontier in an ADR, but do not build
  the cross-tree federation lookup until a second cross-galaxy
  producer (beyond smithy) actually exists.

The six arguments converge into one statement that this ADR formalizes.
The cosmon discipline imported from ADR-052 stands: *cosmon detects, does
not prevent*. The next move is to name the frontier the cosmon
state-machine cannot prove from inside.

## Decision

### D1. The vision sentence (verbatim from godin's synthesis)

> **Every merge commit must name what it is — a `cs done` of a
> tracked molecule, a cross-galaxy delegation with its sister-ledger
> trace, or an honest back-merge in conflict resolution — and the
> gate refuses only the unnamed.**

Three legitimate shapes. One refusal. The sentence is carvable
verbatim across cosmon / smithy / mailroom and replaces every
prior framing of the provenance gate's role.

### D2. The named invariant — I9'

**I9' — BranchMergedOnlyIfCompletedSomewhereTrusted.**

**TLA-ready statement:**

```
∀m ∈ Mol_main : branch_merged[m]
              ⇒ ∃ G ∈ TrustedFederation .
                 mol_status_G[m] ∈ {Completed, Collapsed}
```

where:

- `Mol_main` is the set of mol-ids appearing in the canonical
  position of merge-commit subjects on the cosmon `main` branch.
- `TrustedFederation ⊆ {cosmon, smithy, mailroom, …}` is a
  **finite, explicit, versioned set** of cosmon-isomorphic state
  machines whose ledgers the cosmon gate accepts as authoritative
  for completion claims.
- `mol_status_G[m]` is the value of `mol_status[m]` in the ledger of
  galaxy `G`, as read at gate time from `/srv/cosmon/G/.cosmon/state/events.jsonl`
  (or its successor representation).

**Child analogy** (Feynman test, per godel's framing):

> *Le juge de Marseille appelle la mairie de Lyon avant de refuser le
> certificat. Le bébé existe — il est juste né ailleurs. Mais le
> juge ne consultera que les mairies dont il a l'adresse dans son
> carnet, et ce carnet est fini.*

**Empirical ghost blocked:** the α pattern (merges `195ff5aa` and
`23ce90c1` — smithy-cross-galaxy work the cosmon gate refused
because the mol_id was not in its own ledger).

**Enforceability classification:** **Out-of-band — Gödel territory.**
`T_cosmon` cannot prove I9' from inside because `mol_id_smithy`
is not in the language of `T_cosmon` until an axiom names the
sister ledger. The proof is constructive (responses/godel.md §1–§2):

1. `T_cosmon` = (state machine + ledger) is a recursively
   axiomatizable theory expressing arithmetic.
2. `LegitMerge_cross(m)` requires quantification over ledgers in
   another galaxy, which `T_cosmon`'s Gödel numbering does not
   enumerate.
3. Adding `Axiom Federation_smithy : ledger_smithy ⊆ extended_language`
   produces `T_cf` strictly stronger than `T_cosmon`.
4. By Gödel II, `T_cf` admits its own Gödel sentence — the next
   cross-galaxy producer (e.g. `accord`).
5. The sequence `T_cosmon ⊂ T_cf ⊂ T_cfp ⊂ T_cfps ⊂ …` is
   infinite. Each extension resolves one case and opens the next.

**I9' is therefore not enforceable in-band by extension of any
finite federation axiom.** What we can do is *make it detectable*
(the in-band half) and *delegate trust to named oracles* (the
out-of-band half). This is the same discipline as I9 in ADR-052;
the cardinal point is that the frontier has *moved out by one
level*, not disappeared.

### D3. Out-of-band oracles — four candidates, two adopted, one deferred, one refused

ADR-052 §D5 / §I9 enumerates four oracles for I9 (git pre-merge
hook, CI provenance gate, pilot refusal register, TLA+ check). For
I9' the corresponding question is *who tells the gate that
`mol_id_smithy` is `Completed`?* Four candidates were evaluated;
the panel converged on the following hierarchy.

#### Oracle B — Subject-mark (adopted, immediate)

The merge-commit subject carries an explicit federation mark:

```
Merge branch 'feat/<mol_id>-<slug>'
  (<galaxy>/<mol_id> — <narrative>)
```

The slug-suffix (`-<slug>`) and the parenthetical
`(<galaxy>/<mol_id> — …)` are the two channels by which a worker
*names* a cross-galaxy delegation. The gate parses both, accepts
ledger-miss on the canonical `mol_id` when at least one mark is
present, and logs the merge as `[external federation provenance —
see syzygie chronicle]`.

**Why adopted:** zero new infrastructure. The gate cesse d'échouer
sur les α-style merges *si et seulement si* le worker a nommé sa
delegation. The c1cb-class (canonical pattern with no mark + no
ledger entry) remains hard-FAIL.

**Cost:** one regex extension on `scripts/check-provenance.sh` +
the FAIL message rewrite (per godin). Tracked under
`task-20260518-1870`
(gate B+B' patch).

#### Oracle B' — Extended syzygie chronicle (adopted, immediate)

Per ADR-047 (syzygie protocol), shared vocabulary across cosmon
and its peers is answered with `inherit / adapt / refuse`. The
pilot refusal register
(an internal note)
is extended with a new section *"cross-galaxy delegations"* listing
every commit on `cosmon-main` whose canonical mol_id is not in
cosmon's own ledger, together with the sister-galaxy citation
(galaxy id + mol_id + sister-ledger SHA + cosmon-side SHA).

**Why adopted:** the syzygie protocol already provides the cultural
substrate (ADR-052 §D6). Extending it costs marginal prose per
delegation. Detectability is preserved (a refusal-register entry is
the operator's record that the gate logged a federation acceptance).
Both galaxies (cosmon + smithy initially, mailroom next) must
maintain their copy in sync.

**Cost:** one chronicle entry per cross-galaxy delegation. Tracked
under `task-20260518-4e9e`
(chronicle entry seeding the register with the three audit SHA).

#### Oracle B'' — `cs delegate` verb (adopted, structural)

A typed CLI verb that writes a `delegation_dispatched` event in
*cosmon's own ledger* when sister-galaxy work is imported. The
move inverts the federation problem: instead of cosmon reading
smithy's ledger (federation *across*), cosmon writes its own event
when importing (federation *at the boundary*). Once `cs delegate`
runs, the gate's canonical check passes without any cross-tree
lookup because the cosmon ledger now contains the trace.

```
cs delegate smithy/task-20260513-3a9e \
  --branch feat/task-20260513-3a9e-rpp-binding-scopes \
  --narrative "T23 — admin nucleon binding granted scopes union jwt.scopes"
```

emits a typed event:

```json
{"event": "delegation_dispatched",
 "mol_id": "smithy/task-20260513-3a9e",
 "sister_galaxy": "smithy",
 "sister_mol_id": "task-20260513-3a9e",
 "sister_ledger_seq": 4184,
 "imported_at": "2026-05-19T..."}
```

**Why adopted (structural):** this is godin's *smallest viable
audience* move. The federation lives at the boundary, locally
written, with no cross-tree trust required at gate-time. Sister
galaxies remain masters of their state; cosmon acknowledges its
imports as a local primitive.

**Cost:** new `cs delegate` verb, CLI doc update, new typed event
in cosmon-core. Tracked under
`task-20260518-e541`.

**Operational ordering:** Oracle B (subject-mark) is the immediate
gate patch; Oracle B'' (`cs delegate`) is the structural move that
removes the *need* for cross-tree consultation. They layer: B keeps
the gate green while B'' lands; B'' eventually subsumes B for
worker-driven imports because the cosmon ledger entry is sufficient.

#### Oracle C — Federation MCP service (deferred, planned evolution)

A typed service exposing
`merge_dispatched(galaxy, mol_id) → {Completed, Collapsed, MergeDispatched, NotFound}`.
The gate consults the service; the service consults each ledger.
Centralizes the cross-tree read without merging the ledgers.

**Why deferred (not refused):** building a federation MCP for three
active galaxies is over-engineering at the current scale. The
oracle-B prose channel saturates around 5+ active galaxies (shannon
§3 in the synthesis). **Trigger criterion for promotion B → C:**

- ≥ 5 active galaxies producing cross-galaxy deliveries to cosmon-main, **or**
- ≥ 20 delegation entries / month on the pilot refusal register, **or**
- a documented attack vector (Case 2 slug-hijack — see §D7) that
  Oracle B + B'' cannot detect.

When any of the three criteria fires, nucleate the federation MCP
implementation. Until then, Oracle C is **planned, not built**.

#### Oracle A — PKI federated (refused, threat model mismatch)

Each `merge_dispatched` event signed by a per-galaxy private key;
gate verifies signatures, not content. *Mechanically strong but
introduces PKI rotation, revocation, bootstrap of trust* — costs
unjustified at the current threat model.

**Why refused:** the adversary is not a motivated attacker against
the cosmon ledger. The adversary is the *distracted pilot* and the
*half-named delegation*. The prose channel (Oracle B + B') and the
typed local-event channel (Oracle B'') discipline the distracted
pilot better than a signing key — and at zero PKI cost. The threat
model that would justify Oracle A (cross-organization, multi-tenant,
adversarial review) is not the cosmon threat model
(single-operator, single-laptop, federated-prose).

**Refused at every horizon** — not deferred. The day the threat
model changes, this refusal is the artefact to reopen.

### D4. Relation to ADR-052

I9' is a **strict superset** of I9. When `TrustedFederation = {cosmon}`,
I9' collapses to I9 verbatim:

```
I9'(TrustedFederation = {cosmon})
  ≡ ∀m ∈ Mol_main : branch_merged[m]
                  ⇒ ∃ G ∈ {cosmon} . mol_status_G[m] ∈ {Completed, Collapsed}
  ≡ ∀m ∈ Mol_main : branch_merged[m]
                  ⇒ mol_status_cosmon[m] ∈ {Completed, Collapsed}
  ≡ I9
```

**ADR-052 remains valid and unaltered.** This ADR *extends* I9 with
the federation axiom; it does not amend §I9. The Gödel-fingerprint
argument of ADR-052 §I9 (*"I9 is true in every model where
`BypassMerge ∉ Next`, and false in every model where
`BypassMerge ∈ Next`"*) still holds verbatim for the single-galaxy
case. The new fingerprint for I9' is the same shape, one level up:
*"I9' is true in every model where the cross-galaxy producer is in
`TrustedFederation`, and false in every model where it is not."*

The detection-not-prevention discipline of ADR-052 §I9 carries
forward unchanged. The gate's job remains to *log distinctly* what
it accepts, not to prevent what it cannot prove.

### D5. Tarski hierarchy — the frontier cannot be closed by axiom

The naïve move is to try to close the federation by a universal:

> *"Let `TrustedFederation` = every cosmon-isomorphic state machine
> under `/srv/cosmon/` at push time."*

This is mathematically invalid. The arborescence changes
unpredictably (new galaxies are born, old ones move, the operator
clones a third-party galaxy locally). The would-be axiom is not a
recursively enumerable schema — it depends on a runtime filesystem
walk that is not part of the cosmon state machine's language. **The
closure of the universe costs récursive-énumérabilité**: we leave
the domain in which the gate is an algorithm and enter the domain
in which the gate is a heuristic that walks an unknown tree.

The honest move (responses/godel.md §2) is therefore:

> **`TrustedFederation` MUST be a *finite, explicit, versioned*
> list — a meta-axiom maintained at the cosmon configuration
> boundary, not derived at gate time.**

Concretely, the list lives in `.cosmon/config.toml`:

```toml
[provenance_federation]
trusted_galaxies = ["cosmon", "smithy"]  # versioned, finite, explicit
# Adding a galaxy is a deliberate configuration change.
# Removing a galaxy is a deliberate refusal of inheritance.
```

Each extension `T_cosmon ⊂ T_cf ⊂ T_cfp ⊂ …` opens a new Gödel
sentence at the next level (the next galaxy that will deliver to
cosmon-main without being in the list). **This is the Tarski
hierarchy of axiomatic extensions made operational:** each ADR
governs one level; each new active galaxy may require an ADR
amendment naming it as a trusted peer (or refusing it as a peer
and routing its deliveries through Oracle B'' / Oracle C instead).

The chronicle entry inscribing this discipline is also where the
*explicit list of trusted peers as of today* should be quotable
verbatim — see `task-20260518-4e9e`.

### D6. Mechanical validation — the TLA+ cross-galaxy counterexample

ADR-052 §I9 carries a 3-state TLC counterexample at
`docs/specs/CosmonRun_I9Counterexample.cfg`:

```
Init → Nucleate(m1) → BypassMerge(m1)
```

which produces `branch_merged[m1] = TRUE ∧ mol_status[m1] = Pending`
in one step. This is the *cosmon-only* Gödel counterexample.

For I9' the corresponding counterexample is a 4-state trace across
two galaxies:

```
Init → Nucleate_G2(m1) → MergeDispatched_G2(m1) → BypassMerge_G1(m1)
```

where:

- `Init` — both galaxies empty.
- `Nucleate_G2(m1)` — galaxy G2 (e.g. smithy) nucleates `m1` in
  its own ledger.
- `MergeDispatched_G2(m1)` — G2 emits `merge_dispatched` for `m1`
  in `ledger_G2`. From G2's point of view, the molecule is on the
  legitimate path to completion.
- `BypassMerge_G1(m1)` — galaxy G1 (cosmon) merges a commit
  bearing `m1` in its subject *without* `m1` ever appearing in
  `ledger_G1`. From G1's point of view this is a c1cb-style ghost.
  From G2's point of view it is a legitimate cross-galaxy
  delegation.

The counterexample lives at
`docs/specs/CosmonRunXGalaxy.tla` (already in-tree, encoding I11–I15
from the prior cross-galaxy deliberation `delib-20260419-29f9`) and
must be extended with a dedicated `.cfg` companion:

**`docs/specs/CosmonRun_I9PrimeCounterexample.cfg`** (to nucleate
as part of the implementation):

```
SPECIFICATION Spec
CONSTANTS
    Galaxies = {gA, gB}        \* gA = cosmon, gB = smithy
    Mol = {m1}
    MaxSeqno = 2
    MaxCrossEdges = 1
    AdversarialPeerForge = FALSE
    AsyncCrashesEnabled = FALSE
    OutOfBandEnabled = TRUE
    T_STALL = 99
    MaxClock = 2
INVARIANTS
    I9Prime_BranchMergedOnlyIfCompletedSomewhereTrusted
```

with the matching action defined in `CosmonRunXGalaxy.tla` as the
cross-galaxy analogue of `BypassMerge`. TLC's expected output: a
4-step trace producing
`branch_merged[gA][m1] = TRUE ∧ mol_status_G[gA][m1] = Pending ∧ mol_status_G[gB][m1] = MergeDispatched`,
which the gate today reports as a violation but I9' classifies as
*out-of-band* iff `gB ∈ TrustedFederation`.

This extension lands with the `cs delegate` implementation
(`task-20260518-e541`)
so that the typed event is observable in the model as well as in
the cosmon ledger.

### D7. Known gap — Case 2 (slug-hijack on real mol_id)

Per knuth's adversarial analysis (responses/knuth.md §III.2),
**Case 2 survives every option enumerated in this ADR**:

- *Setup.* An attacker (or accident) creates a branch
  `feat/<real-existing-mol_id>-<slug>` whose diff has nothing to do
  with `<real-existing-mol_id>`. Both the regex and the ledger
  lookup pass — the mol_id exists, the canonical pattern matches,
  the slug is well-formed, and the diff is never inspected.

I9' does not close this gap. Neither does Oracle B, Oracle B'',
nor Oracle C. The fix requires **tree-hash binding in
`merge_dispatched`** — the typed event must carry a hash of the
diff scope, and the gate must verify that the merged tree matches
the recorded hash within an admissible epsilon (refactor budget).

This is tracked as a separate ADR pass under
`task-20260518-c4b6`
and as a typed-event upgrade idea under
`idea-20260518-f24d`
(`back_merge_dispatched` typed event, which is the symmetric upgrade
on the β side).

**Naming Case 2 in this ADR is deliberate.** Per the cosmon
discipline (`P_external` — no system certifies itself; ADR-032),
the right move is to *make the gap visible* rather than pretend it
is closed. I9' formalizes the federation level; I9'' will
formalize tree-hash binding when the tooling is ready.

### D8. Syzygie inscription

Per [ADR-047](047-event-log-protocol-v0.md), I9' is shared
vocabulary across cosmon and its peers. The required answers are:

| Galaxy | Answer | Rationale |
|---|---|---|
| **cosmon** | `inherit` (self) | Originating galaxy; ADR is this file. |
| **smithy** | `inherit` | First and current α-source; smithy is a `TrustedFederation` peer as of 2026-05-19. Per syzygie protocol smithy MUST chronicle the inheritance in its own internal chronicles with verbatim citation of this ADR. |
| **mailroom** | `inherit` | Per cosmon-ward feedback flow (ADR-049), mailroom is structurally a `TrustedFederation` peer when it delivers cosmon-side patches. Same inscription requirement. |
| **other galaxies (accord, knowledge, lumen, …)** | TBD | Become `TrustedFederation` peers only by explicit configuration change in `.cosmon/config.toml [provenance_federation]` + a syzygie chronicle entry. Until then, their cross-galaxy work flows through Oracle B (subject-mark + log) only, not through ledger trust. |

The syzygie answer for each peer is itself a typed gesture: a
chronicle line in the internal chronicles of the peer galaxy
naming the inheritance and the cosmon ADR citation. The
chronicle entry seeding both galaxies is tracked under
`task-20260518-4e9e`.

## Consequences

### Decomposition — five implementation children

This ADR is the doctrinal anchor for the B+B' patch on
`scripts/check-provenance.sh` and for the structural `cs delegate`
verb. The synthesis enumerated ~10 candidate children; after dedup
and prioritization the final five are:

| # | Molecule | Role | Order |
|---|---|---|---|
| 1 | `task-20260518-1870` | Gate B+B' patch on `scripts/check-provenance.sh` — two-level regex (canonical mol_id + free suffix), back-merge pattern, federation acceptance with `[external federation provenance]` log on subject-mark, FAIL message rewrite per godin. | **First.** Unblocks the 3 audit SHA. Doctrine inherited from this ADR. |
| 2 | `task-20260518-e541` | `cs delegate` verb implementation in `cosmon-cli` + `cosmon-state` + `cosmon-core` typed event. | **Second.** Structural; removes the *need* for cross-tree consultation. Lands B'' for worker imports. |
| 3 | `task-20260518-4e9e` | Chronicle entry seeding the pilot refusal register (cross-galaxy delegations section) with the three audit SHA; syzygie inscription in cosmon + smithy + mailroom. | Parallel to #1. Closes the cultural half. |
| 4 | `task-20260518-c4b6` | Case 2 (slug-hijack on real mol_id) gap analysis + tree-hash binding proposal for a future ADR pass. | Parallel research, no implementation blocker. |
| 5 | `idea-20260518-f24d` | `back_merge_dispatched` typed event in cosmon-core — symmetric upgrade on β side; closes the *"merge_dispatched is too permissive as a completion proxy"* gap (knuth I7). | Follow-up after #1; β is fixable by typed event, no doctrine change. |

The decision spine of the synthesis (§6) layers as follows:

1. **Tactical layer (#1)** — ships in hours. Gate green for both
   α-with-mark and β.
2. **Structural layer (#2)** — ships within the week. `cs delegate`
   for worker-driven imports.
3. **Doctrinal layer (this ADR)** — lands now.
4. **Mechanical evolution (Oracle C)** — deferred to 5+ galaxies
   trigger. Not built today.
5. **Adversarial hardening (#4)** — parallel research path.

### Positive

- **Names the second Gödel sentence of cosmon.** I9' is a typed,
  formal, TLA-checkable extension of I9. The federation frontier
  is now a citable doctrinal object rather than a tacit assumption.
- **Closes the α-class structurally.** Smithy-cross-galaxy work
  is recognized either by Oracle B (subject-mark at gate time) or
  Oracle B'' (`cs delegate` writing the cosmon-side event).
- **Detection preserved.** The gate's log distinctly tags federation
  acceptances. The pilot refusal register accumulates the chronicled
  trace. The c1cb-class (canonical pattern with no mark + no
  ledger entry) remains hard-FAIL — no false negatives introduced.
- **Tarski hierarchy made operational.** Adding a galaxy to
  `TrustedFederation` is a versioned, explicit, ADR-grade gesture.
  The federation cannot be silently expanded.
- **Symmetric β fix.** The back-merge pattern is recognized as a
  *syntactic gap*, not an *I9 violation*. Two regex lines close
  the immediate need; a typed `back_merge_dispatched` event
  (idea #5) closes the cleanest version.

### Negative

- **TrustedFederation is a configuration burden.** Adding or
  removing a peer requires an explicit `.cosmon/config.toml`
  change plus a syzygie chronicle line. Mitigation: this is the
  *intended* friction. Silent federation expansion is exactly the
  Tarski-hierarchy trap §D5 names.
- **Oracle B + B'' do not catch Case 2.** Slug-hijack on a real
  mol_id with attacker-controlled diff content is not detected at
  the gate. Mitigation: the gap is explicitly named (§D7) and
  tracked under task-20260518-c4b6. The fix is tree-hash binding
  in a future ADR pass; this ADR honestly defers.
- **Subject-mark is a convention, not a contract.** A worker who
  forgets the slug or the parenthetical produces a c1cb-class
  rejection even on legitimate cross-galaxy work. Mitigation: the
  rewritten FAIL message (per godin) leads with *"what kind of
  artefact would be accepted instead"*, not with the escape hatch.
  Worker-side documentation in `docs/handbook.md §Federation` lands
  with task-20260518-1870.
- **Federation MCP (Oracle C) is named but not built.** When the
  trigger criterion fires, an additional ADR pass and implementation
  cycle are required. Mitigation: the trigger criterion is explicit
  and observable. Until 5+ active galaxies, building Oracle C is
  over-engineering.

### Neutral

- **No code change in `cosmon-core` for this ADR alone.** I9' is a
  doctrinal statement; the regex + event-emission code lands in
  child molecules.
- **No state-store schema migration for this ADR alone.** The
  `delegation_dispatched` event lands with task-20260518-e541; its
  typed addition is additive, not breaking.
- **Backwards compatibility on the gate.** `COSMON_PROVENANCE_SINCE`
  remains the bridge for the three audit SHA per the synthesis §3
  (C3 — accept history, no force-push). History remains
  re-readable; the lesson is applied forward.

## Non-goals

- **PKI federated trust.** Refused (§D3 Oracle A). Threat model
  mismatch. The day the threat model changes, this refusal is the
  artefact to reopen.
- **Federation MCP service today.** Deferred (§D3 Oracle C). Build
  when the trigger criterion fires.
- **Closing the Tarski hierarchy by universal axiom.** Forbidden
  (§D5) — costs récursive-énumérabilité; leaves the domain where
  the gate is an algorithm.
- **In-band proof of cross-galaxy completion.** Mechanically
  impossible per Gödel II (§D2). Detection + delegation is the
  achievable goal, same as I9.
- **Tree-hash binding in `merge_dispatched` (Case 2 fix).** Out of
  scope; tracked under task-20260518-c4b6 for a future ADR pass.
- **Force-push to rewrite the three audit SHA.** Refused at the
  delib level (synthesis §3 C3); accepted history is the evidence
  trail of the gate's evolution.

## Mechanical validation

The TLA+ model `docs/specs/CosmonRunXGalaxy.tla` already encodes the
cross-galaxy ledger structure (variables `ledger_by_g`,
`peer_receipt`, `mol_alias_epoch`, `cross_edges`) and four of the
five invariants from the prior cross-galaxy deliberation
`delib-20260419-29f9` (I11–I15).

I9' as stated in §D2 is the *cross-galaxy projection* of I9. The
mechanical validation procedure is:

1. Add a state-function `I9Prime_BranchMergedOnlyIfCompletedSomewhereTrusted`
   to `CosmonRunXGalaxy.tla` — the TLA+ encoding of §D2's formula
   over `Galaxies`.
2. Add the configuration file
   `docs/specs/CosmonRun_I9PrimeCounterexample.cfg` matching the
   skeleton in §D6 above.
3. Run TLC against the configuration. Expected output: a 4-step
   trace producing the cross-galaxy bypass.
4. The trace confirms I9' is **stateable** but **not provable
   in-band** when `MergeDispatched_G2 ∧ ¬(G2 ∈ TrustedFederation)`.
5. TLC re-validates I9' as a stepwise safety property when
   `TrustedFederation = {gA, gB}` and `peer_receipt[gA][gB][m1] = Completed`.

This extension lands as part of task-20260518-e541 (alongside the
`cs delegate` event emission) so that the typed event is observable
in the model checker as well as in the cosmon ledger.

The validation does not "fail" on I9'; it *correctly reports* that
I9' is contingent on a meta-axiom (`TrustedFederation` membership)
which the spec cannot discharge from inside. That last clause is
the formal fingerprint of a second-level Gödel sentence — the
twin of ADR-052 §I9 one level up the Tarski hierarchy.

## References

- **Audit.** An internal audit (provenance drift, 2026-05-17)
  (three SHA drift on the provenance gate, all legitimate, two
  patterns α and β identified).
- **Governing deliberation.** `delib-20260518-9608` synthesis at
  `.cosmon/state/fleets/default/molecules/delib-20260518-9608/synthesis.md`
  — six personas (wheeler, torvalds, shannon, godel, godin, knuth);
  per-persona responses at `responses/{wheeler,torvalds,shannon,godel,godin,knuth}.md`.
  The formal Tarski argument lives at `responses/godel.md` §1–§2
  and the ADR structure proposal at §5.
- **Predecessor ADR.** [ADR-052](052-one-ledger-one-writer-one-witness.md)
  §I9 — the first Gödel sentence of cosmon (single-galaxy case)
  and the discipline of detection-not-prevention this ADR inherits.
- **Cross-galaxy substrate.**
  [ADR-035](035-cross-galaxy-edges.md) (cross-galaxy edges — the
  filesystem channel),
  [ADR-047](047-event-log-protocol-v0.md) (syzygie protocol — the
  `inherit / adapt / refuse` discipline this ADR inscribes).
- **Constitutional axioms invoked.**
  [ADR-032](032-p-external-witness-axiom.md) (`P_external` — no
  system certifies itself; the ground of §D5 and §D7),
  [ADR-046](046-p-legibility-axiom.md) (`P_legibility` — every
  state decision is human-legible; the ground of §D1's vision
  sentence and the Feynman child analogy in §D2).
- **Cosmon-ward feedback discipline.**
  [ADR-049](049-cosmon-ward-feedback-flow.md) — this ADR is a
  binding instance: smithy and mailroom surface the federation
  question back to cosmon, cosmon names it as a typed doctrinal
  object, the answer flows back to the peers via syzygie.
- **Mechanical validation substrate.**
  `docs/specs/CosmonRun.tla` + `CosmonRun_I9Counterexample.cfg`
  (ADR-052 §I9 counterexample, single-galaxy);
  `docs/specs/CosmonRunXGalaxy.tla` + companion `.cfg` files
  (cross-galaxy extension encoding I11–I15 from
  `delib-20260419-29f9`); future
  `CosmonRun_I9PrimeCounterexample.cfg` to land with
  task-20260518-e541.
- **Implementation children.**
  `task-20260518-1870`
  (gate B+B' patch),
  `task-20260518-e541`
  (`cs delegate` verb),
  `task-20260518-4e9e`
  (chronicle + syzygie inscription),
  `task-20260518-c4b6`
  (Case 2 slug-hijack gap analysis),
  `idea-20260518-f24d`
  (`back_merge_dispatched` typed event).
