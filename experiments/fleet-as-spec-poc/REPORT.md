# Fleet-as-Spec — POC report (wiki-core)

**Date:** 2026-04-20 · **Task:** task-20260420-ac88 · **Scope:** one fleet, one spec, one TLC run, honest report.

## 1. What we modelled

The wiki-core fleet (`frank/.cosmon/fleets/wiki-core.fleet.toml`, 40 lines, 3 STUB agents + 4 gates) together with its master constitution (`frank/.cosmon/fleet.toml`, 21 lines, 5 pillars) and ADR-0002's kill-switch, rewritten as a 215-line TLA+ module `WikiCore.tla` checked by TLC on a small bounded world.

The **5 master pillars** became safety invariants on produced articles:

| Pillar (prose) | Invariant (TLA+) |
|---|---|
| Named reader required | `I_NamedReaderRequired` — `Len(articles) > 0 => active_reader # NULL` |
| Primary paper cited | `I_PrimaryPaperCited` — every article cites every primary source |
| No neologism | `I_NoNeologism` — `neologisms = {}` on every article |
| Gate or not exist | `I_GateOrNotExist` — `promotion # NULL => gates_passed = AllGates` |
| Author ≠ scorer | `I_AuthorNotScorer` — `author # scorer` on every article |

The **4 sub-fleet gates** became state predicates: `G_LedgerClosure` (every cite is in the ledger), `G_SingleWriter` (writer-lock shape, I7-echo), `G_PromotionSigned` (signed verdict is non-null), `G_PromotionImpliesReader` (signer = attested reader).

The **kill-switch** (ADR-0002 §5) became a boolean latch gating every productive action; its stickiness is a temporal property `[] (kill_switch => []kill_switch)`.

The pipeline is driven by seven actions: `AttestReader`, `AcquireLedgerLock` / `ReleaseLedgerLock`, `AppendToLedger`, `ProduceArticle`, `PassGate`, `SignPromotion`, `FireKillSwitch`.

## 2. What we did NOT model (honest boundary)

A spec is a picture; these parts of the world do not fit inside the frame.

- **Article content.** Whether a draft string is *truthful*, *cites a real paper*, or *contains hidden neologisms* is undecidable in the Rice-theorem sense. The spec tracks a `neologisms` set as metadata — but whether an agent would correctly populate it is an oracle question, not a mechanical one.
- **LLM agent semantics.** The three STUB agents (`scope`, `source`, `draft`) are modelled as nondeterministic actions that produce well-typed records. Their *cognitive behaviour* — why they picked those citations, whether they hallucinated — is opaque to TLA+.
- **Filesystem races outside the ledger.** Tmux lifecycle, worker pid death, branch merges — ADR-052's I3–I9 hazards — are **not** re-modelled here. They live in `cosmon/docs/specs/CosmonRun.tla`, which already checked them. The POC stays focused on the sub-fleet's own invariants.
- **Attestation-file frontmatter parsing.** The seven required fields of `.cosmon/readers/<name>.md` (ADR-0002 §3) are checked by a bash hook (§4), not by the spec. The spec abstracts the attestation down to a single state transition `AttestReader`.
- **Gödel-incompleteness caveat.** Even the invariants we *did* encode are checked only on the actions we *did* write. If an agent finds a state transition we did not enumerate — e.g. a filesystem side-effect that mutates `ledger` without taking `writer_lock` — the spec's "no counter-example" silence is not a proof; it is evidence only about the moves we bothered to name. Consistency of the spec ≠ completeness of the model.

## 3. TLC result

Invocation: `run-tlc.sh` (uses `/opt/homebrew/opt/openjdk@17/bin/java` and `cosmon/docs/specs/tla2tools.jar`).

Bounded model (from `WikiCore.cfg`):

- `PrimarySources = {p1, p2, p3}` (3)
- `Readers = {misha}` (1)
- `Authors = {a1, a2}`, `Scorers = {s1, s2}` (disjoint, 4 author/scorer pairs with `a # s`)
- `MaxArticles = 2`, `NeologismPool = {"H-PID-2"}` (the wiki3 fossil from ADR-0002 §1)
- `AllGates` — the 4 gates

Full log: [`tlc-out.log`](tlc-out.log).

**Headline: `Model checking completed. No error has been found.`**

| Metric | Value |
|---|---|
| states generated | 27,001,057 |
| distinct states | 7,835,008 |
| search depth | 18 |
| wall-clock | 3 min 38 s (16 workers, 27 GB heap) |
| invariants checked | `TypeOK`, `MasterPillars` (5 pillars), `SubFleetGates` (4 gates) |
| temporal properties checked | `L_KillSwitchSticky` |

Interpretation: **partial formal validation on a bounded space.** All five pillars, all four gates, and the kill-switch stickiness hold across every reachable state in the model's finite world. No counter-example trace — neither a Gödel sentence inside the frame we drew, nor a permutation of actions that lets an article ship without a reader / without the primary sources / with neologisms / before the gates pass / with author = scorer.

We discovered two genuine bugs *along the way* (honest progress report, not sanded over):

1. The first ASSUME (`Authors \cap Scorers # {}` instead of `\E a \in Authors, s \in Scorers : a # s`) stated the wrong thing — TLC rejected it immediately. That is exactly the value of TLC: the assumption you meant ≠ the assumption you wrote.
2. Representing `NULL` as the string `"NULL"` made TLC's `TypeOK` try to compare a record to a string, which throws. The fix was to declare `NULL` as a TLA+ model value (atom identified only with itself). Design lesson: in a spec, distinguished absence wants a distinguished *type*, not a sentinel string.

Both bugs were in the spec, not in the fleet. We mention them because a POC that hides its mistakes is not honest.

## 4. Lesson for cosmon (the 6 missing primitives)

The POC is a partial answer to delib-38cf §7 and idea-20260420-2d2b (six cosmon primitives missing to make composition safe). Three observations:

- **Attestation as a typed action.** Replacing the bash tripwire with a first-class cosmon primitive (an `attest` molecule kind, typed against a reader-schema) would make the `AttestReader` action in the spec match the implementation one-for-one, not merely echo it. A primitive that a spec can bind to mechanically is more useful than a prose contract.
- **Ledger with writer-lock.** `AppendToLedger` + `writer_lock` are exactly the I7 shape from ADR-052. A generic cosmon `ledger` primitive (append-only, single-writer, BLAKE3-sealed per entry) would let every sub-fleet inherit the property for free rather than reinvent it in bash.
- **Gate set as first-class type.** In the spec `gates_passed` is a `SUBSET AllGates` and `promotion` is nullable record guarded by `gates_passed = AllGates`. A cosmon `gate_set` primitive with the same type would let formulas declare their gates and let `cs reconcile` project the set — as opposed to today's `[[gates]]` being an opaque array of shell commands.

TLC cannot validate what is not typed. Every primitive the operator adds to cosmon is a new axis on which spec-checking becomes possible.

## 5. What to do after

- **If this POC counts as a positive signal** (it does — a 7.8M-state proof that five pillars + four gates + kill-switch compose without hidden interaction): write `cosmon/docs/patterns/fleet-as-spec.md` distilling the recipe (what to model, what to leave off, how big to bound, how to present the honest-boundary section).
- **Extending to EFS** (sandbox): EFS has multiple interacting sub-fleets and an actual scheduler; the spec would need channels and much more state. Possible but a full day, not a POC.
- **Coupling the spec back to the implementation**: the seven actions here (AttestReader, AcquireLedgerLock, …) are ~50% in `cs` today and ~50% in ad-hoc bash. A refactor that brings the remaining 50% under `cs` primitives would make the spec auditable against the code, not against the prose description.
- **Un-modelled hazards worth their own spec.** `ProduceArticle` treats content as a record — a future POC could model *citation churn* (agent A adds cite, agent B removes it) as a concurrent protocol, which is a genuinely falsifiable property the current single-writer design may or may not satisfy.

---

**Bottom line.** We drew the wiki-core fleet as a small mathematical game, pointed 16 CPU cores at it for three minutes, and watched TLC fail to break any of the rules the constitution names. The rules are internally consistent — the pipeline, as typed, cannot ship an article that violates any pillar. The rules are *not* complete: what the agents do inside the allowed moves remains an oracle question Rice and Gödel saw coming.
