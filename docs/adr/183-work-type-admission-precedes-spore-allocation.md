<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# ADR-183 — Work-type admission precedes spore allocation

**Status:** Accepted (2026-10-01)
**Date:** 2026-10-01
**Decider:** the operator (approved the review recommendation on 2026-10-01
and asked to start with admission before allocation)
**Authoring molecule:** `task-20261001-099c`
**Source review:** `task-20261001-301b`, `report.md` in its molecule
directory (baseline `b75c2a8f`)
**Amends:** [ADR-139](139-spore-shareable-polymer-template.md),
[ADR-140](140-spore-format-expand-deterministic-cache-astra.md),
[ADR-147](147-provider-family-diversity-witness-invariant.md),
[ADR-160](160-spore-export-ex-post-manifest.md),
[ADR-161](161-spore-run-scoped-output-home.md) — each by a short dated note;
none of their decisions is rewritten.

## Context

The facts below were verified by the source review in sources, dry runs and
retained run records. A recorded verdict is evidence that its producer
reported it; the review did not rerun historical experiments.

- `cosmon-dev` expands to 14 initial molecules (13 fixed nodes and one
  emergent `converge` controller) in both `auto` and `full` lanes, before
  committee, repair and retry children. A forced fast lane is refused. Its
  formulas total 32 top-level steps.
- `cs spore run` type-checks `issue`, `affected_ref` and `upstream_version` as
  arbitrary strings, then creates the whole topology. Nothing before
  allocation decides whether the input is a reproducible defect.
- The five `germ-20260930-*` runs (issues #143–#147, feature requests)
  allocated 70 initial molecules. One intake node completed with a valid
  BLOCKED 0/7 report after 17.98 minutes of lifecycle span; the other 69 were
  collapsed, 56 of them never started. The intake refusal was correct; the
  workflow selection was wrong.
- Issue #85 consumed 42 initial molecules across three germinations before any
  review child, and none produced an accepted spore release.
- Across the retained runs, implementation and CI measurements reached PASS
  while route, rehearsal, convergence or release measurements were BLOCKED.
  The release checks (published-route audit, four packaged cells, a human
  dissent-overrule identity) are attached to every defect run although they
  describe a candidate, not a change.
- The seal model (`spore.tla`, `spore.cfg`) describes an older 13-role
  topology with a round bound of 3; the manifest declares 14 nodes and a bound
  of 2. The seal check verifies declared property names and runs the model; it
  does not check correspondence with the manifest.
- The generic scheduler orders on lifecycle completion and does not read a
  spore verdict file. A node that completes with BLOCKED unblocks its
  dependents.

## Decision

### D1 — Select the work type before germination

The pilot states the desired result and its work type before choosing a
vehicle:

| Work | Vehicle |
|---|---|
| One bounded writer can implement and verify the whole result | one `task-work`, with the contract, RED/GREEN or document checks and the required gates in the brief |
| Interfaces, ordering or write-set partition still need design | a plan, then explicitly scoped `task-work` units |
| An established multi-role protocol with distinct artefacts and genuine independent verification | a spore, after admission (D2) |
| Read-only audit | an evidence-only formula, not an implementation formula |

A capability request is never sent through defect intake. Missing
reproduction evidence is never relaxed into a defect PASS. Stakes and work
type are separate axes: a security change widens assurance in any vehicle.

### D2 — Admission is complete before any allocation

No spore molecule is created until admission has produced, for this run: the
chosen vehicle and work type, the pinned baseline, the intended integration
base, the declared paths and the risk derived from them, the applicable gates,
the reviewer capability (executable seats meeting the ADR-147 floor), the
required execution substrates, and the expected molecule count. A defect
requires reporter evidence and an immutable affected baseline; a feature
requires an accepted behavioural contract and non-goals.

Any missing input refuses before allocation. It never selects a cheaper lane
or a lower review floor. Admission is a step taken before `expand()`; it is
not a conditional inside the expanded DAG, and it adds no scheduler.

Acceptance for this step: the #143–#147 feature requests are refused by defect
admission without germinating 70 nodes.

### D3 — Target recipes

These are **proposed budgets**, excluding explicit repair and review children.
No wall-clock or cost reduction is claimed until measured on comparable work.

| Recipe | Initial nodes | Shape |
|---|---:|---|
| `cosmon-defect` | 6 | contract → frozen reproduction → implement → verify → independent review → accept |
| `cosmon-feature` | 6 | accepted contract → frozen acceptance RED → implement → verify → independent review → accept |
| candidate release validation | 3 | independent route audit and packaged matrix in parallel → release manifest |
| external confirmation | 0 by default | a persisted obligation; a confirmation task is dispatched when replay evidence arrives |

The defect and feature recipes share implementation, verification, review and
acceptance formulas and differ in admission and RED contract. A feature RED
asserts desired behaviour absent on the pinned baseline and fails for that
reason, not because the harness does not build; it invents no reporter
symptom; contract, fixture, command, timeout and baseline are frozen before
implementation; reverting only the feature restores RED.

Release validation is a mission over a candidate, run once per candidate and
reusable across the changes it contains. Its evidence is reused only when the
measured candidate, artefact, substrate and gate-definition identities match;
an ancestor `base_sha` alone does not suffice.

Until contract-first feature admission is stable, plan + tasks remains the
preferred route for features. A feature spore is not permission to germinate
every idea.

### D4 — `cosmon-dev` is a legacy recipe

`cosmon-dev` stays available, explicitly scoped as the robust defect-and-release
protocol for an evidence-backed released defect whose full release contract is
wanted and feasible. It is no longer the default development route. The
`spore/issue-85` branch is a stale integration of it, not a second supported
recipe.

### D5 — Keep, merge, split, retire

- **Keep:** spore as pure topology expansion; fail-closed seal admission;
  run-scoped output homes; frozen tests; measured negative controls;
  distinct-provider and distinct-surface review; explicit missing-evidence
  outcomes (PASS, BLOCKED, INCOMPLETE, VOID); explicit publication authority;
  the harvest refusal on a wrong base checkout.
- **Merge:** intake classification into admission; deterministic lane
  evaluation into admission plus a realized-diff check; green replay and CI
  into one verification node with distinct results; semantic-surface
  enumeration into one reusable artefact shared by release and confirmation.
- **Split:** defect from feature admission; development acceptance from
  release validation; candidate acceptance from external confirmation.
- **Retire as defaults:** the 14-node all-purpose route, a dedicated agent for
  transport tracing, per-run generation of deterministic gate scripts, and idle
  confirmation workers. Historical manifests, branches and run evidence are
  preserved pending audit; nothing is deleted by this decision.

### D6 — Defaults decided, revisable

The source review's §5 questions are settled with its recommended defaults.
The operator may revise any of them by amending this section.

| Question | Decided default |
|---|---|
| Is `cosmon-dev` a defect protocol or the universal path? | Legacy robust-defect/release protocol; universal routing removed (D4) |
| Separate feature spore or a feature flag? | Separate entry point, shared formulas; plan + tasks until admission is stable (D3) |
| Does every local change owe packaged release validation? | No; one candidate-level release mission, reused only under matching identities (D3) |
| How does a fast lane handle INITIAL findings? | Escalate to full confirmation; a changed tree is never certified by the review of the old one |
| May the pilot lower review requirements when a family is unavailable? | No; record the missing capability and stop, or use an explicitly authorized lower-stakes workflow chosen before dispatch |
| Does the current seal prove the current topology? | No; bind and check manifest-to-model correspondence first, keeping the honest model-check result |
| Who owns delayed external confirmation? | A persisted operator obligation; a worker is created only once usable replay evidence exists |
| What happens to the two spore branches? | Preserved pending branch audit; useful template work recovered selectively; no automatic deletion or bulk merge |
| Should routine schema and trace checks use premium models? | No model where a stable script suffices; reasoning capacity is reserved for contracts, implementation and independent judgement |
| What authorizes a release dissent overrule? | An explicit operator decision attached to the candidate; never a synthesized identity or approval inferred from node completion |

### D7 — Sequence

Step 1, authorized now, is admission (D1, D2): a documented work-type catalog
beside the spore READMEs and a versioned admission formula or checker that
emits the record of D2. The later steps of the review's §4.D (shared evidence
contract with consumer-side predecessor validation, the recipe split, seal
correspondence, dispatch and integration-base persistence, the research
template, measured comparison) each need their own authorization.

## Consequences

- A request whose type does not match a recipe costs one admission record, not
  a germinated DAG.
- Each target development run allocates six initial molecules instead of 14
  (proposed, not measured); release cost moves to once per candidate.
- A single bounded change no longer waits on release cells that a local change
  cannot satisfy by retrying.
- Security work keeps the full independent floor and its substrates whatever
  the node count.
- Admission records and verdicts bind to an exact candidate. After a history
  rewrite, old evidence keeps its old identity; nothing is re-hashed to make
  ancestry pass.
- `cosmon-dev` README and catalog text change to state its legacy scope.
  Command behaviour changes only if a later step adds a CLI surface, which then
  carries the CLI/UI parity and generated-help updates.

## Rejected alternatives

- **Keep `cosmon-dev` as the universal path and tune its lanes.** The lanes
  share one admission, so a feature still enters defect intake and the release
  checks still attach to every change.
- **One recipe with a feature flag.** Admission and RED contracts differ; one
  manifest would carry both in conditional prose that expansion cannot check.
- **Let the intake node classify after germination.** That is today's
  behaviour; the 70-node batch is its measured cost.
- **A conditional scheduler that prunes the DAG at run time.** Contradicts
  ADR-139/140's pure expansion; staged admission by composition achieves the
  same without a new scheduler.
- **Lower the review floor when a provider family is unavailable.** Converts a
  missing capability into a weaker verdict that reads as a stronger one.
- **Delete the stale branches and historical runs.** They hold evidence that
  has not been audited.

## References

- Source review: `task-20261001-301b` `report.md`, §3 (problems A–I), §4
  (proposal A–E), §5 (questions).
- `spores/cosmon-dev/spore.toml`, `spores/cosmon-dev/spore.tla`,
  `spores/cosmon-dev/spore.cfg`.
- `crates/cosmon-cli/src/cmd/spore.rs` (`run_run`, expansion).
- ADR-139, ADR-140, ADR-147, ADR-160, ADR-161 (amendment notes dated
  2026-10-01).
