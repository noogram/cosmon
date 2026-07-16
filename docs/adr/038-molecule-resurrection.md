# ADR-038 — Molecule Resurrection — `cs resurrect`

**Status:** Accepted (via `delib-20260414-8643`, 2026-04-14)
**Scope:** lifecycle verbs, `cs resurrect` command, prompt composition,
`EventV2::Resurrected` variant, `MoleculeLink` non-exhaustive prep,
relation to `cs recover`, `cs tackle`, resident runtime (ADR-016).
**Parent deliberation:** `delib-20260414-8643` (five-persona panel:
wheeler, torvalds, shannon, tolnay, hawking).
**Binds:** [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
(autonomy regimes), [ADR-030](030-cosmon-archive-model.md) (artifact
tracking / gitignored `state.json`), `docs/architectural-invariants.md`.
**Implementation:** `task-20260414-21b7` (v1 implementation).

## Context

A pilot fear articulated a structural constraint: a worker can die —
tmux session killed, host crashed, context exhausted — while its
molecule remains mid-formula, its DAG position intact, and its artifacts
on disk unchanged. The pilot's whisper was "will a message fall to the
bottom of the sea if the worker is gone?" That fear named a latent
primitive.

The realization: **the molecule never died — only the observer did.**
Artifacts on disk (`prompt.md`, `briefing.md`, per-step git commits,
`log.md`, `synthesis.md`, `responses/`) are already an
information-theoretic bottleneck that the worker crosses by design
every time it evolves a step. The filesystem is the compressed proof-
of-work record. What cosmon lacked was a verb to **re-attach an
observer** to a molecule whose session had been lost — to compose those
artifacts into a bootstrap prompt for a fresh worker that resumes from
the last committed step.

This is ontologically distinct from `/compact`: artifacts are a
gated, causally-ordered, deliverable-preserving code, whereas
`/compact` is an ungated, chronological, LLM-readability-preserving
summary. The new verb operates on a different channel.

See the companion internal chronicles
for the peur-driven discovery narrative.

## Decision — the settled points (panel convergence)

The five-persona panel reached unanimous agreement on ten points.
These are to be treated as closed.

1. **Ship it.** Small, real, needed today. Not an anti-feature.
2. **Option A identity.** The resurrected molecule keeps its
   `mol_id`. Reject Option B (clone with a `Reincarnated-From`
   link) and reject A⊕B duality. The DAG does not care about worker
   context windows; forcing link re-pointing encodes a distinction
   the DAG cannot see.
3. **Separate verb, not a flag on `cs recover`.** `cs recover`
   detects and marks (N wrecks, cardinality N); the new verb is
   single-target surgery (cardinality 1). Flag-composition grows
   polynomially; conflating the two roles breaks the single-perimeter
   rule.
4. **No new `MoleculeStatus`.** Resurrection is an **event**
   (`EventV2::Resurrected { … }`), not a state. The molecule returns
   to `Running` / `Propelled` as if `cs tackle` had just run. Adding
   a status would force every downstream surface (peek, ensemble,
   patrol, done) to handle it for zero capability gain.
5. **Same branch, same worktree.** Git is already the snapshot;
   the branch lineage already expresses DAG-aligned content flow.
   No parallel `.cosmon/wrecks/` as primary store — at most a
   minimal metadata breadcrumb.
6. **Human-only verb.** A worker cannot reliably resurrect itself
   (its context is gone by definition); a living worker spawning
   a competing worker on its own worktree is a race. Same perimeter
   row as `cs done`.
7. **Artifacts ≠ `/compact`, ontologically.** Different distortion
   measure (deliverable preservation vs LLM readability), different
   source (proof-of-work vs conversation), different commitment
   (gated bits vs ungated), different directionality (causal DAG
   order vs chronological). Do not market the feature as "a better
   /compact."
8. **Bottom-turtle preserved.** `cs resurrect` is a stateless CLI
   verb. When the resident runtime (ADR-016, Phase 3+) arrives, it
   **calls** `cs resurrect` — it does not replace it. Recursion
   bottoms at the filesystem.
9. **Prompt composition is the entire novelty.** Everything else
   reuses existing infrastructure (`tackle`'s launch path,
   `events.jsonl`, worktrees, `recover`'s preconditions). The new
   code is one `compose_resurrection_prompt(mol_dir) -> String`
   pure function plus CLI wiring.
10. **Reject `cs fear`.** Do not ritualize the spontaneous. The
    fear-driven discovery pattern works because it is unritualized;
    formalizing it destroys the mechanism.

## Decision — divergences resolved

### Verb: `cs resurrect`

The panel split on `cs resume` (wheeler — ontologically accurate:
the molecule never died) vs `cs resurrect` (torvalds, tolnay,
hawking — unambiguous, memorable, zero collision). The open question
was whether `cs resume` was already claimed.

**Verified:** `crates/cosmon-cli/src/cmd/resume.rs` exists with the
meaning "Nudge idle workers" (re-attach / re-prompt flow, distinct
from respawning a dead session). `cs resume` is therefore already
the name of a different operation.

**Resolution:** **`cs resurrect`** wins by disambiguation. Wheeler's
ontological framing is preserved in documentation prose ("the
molecule never died; an observer did") but the CLI verb uses the
unambiguous term operators will type at 2am. `salvage` and the
submarine metaphor live in chronicles, not in the CLI.

### Guardrail scope: v1 minimal, v1.1 data-driven

Torvalds argued for minimal shipping (4 protocol steps, ~200 lines
Rust). Hawking argued for six edge-case guardrails from day one.
Shannon argued for instrumentation from day one regardless.

**Resolution:** torvalds and hawking disagree on **when**, not
whether. Ship v1 as torvalds-minimal **plus shannon's measurement
event** (the sensor). Defer hawking's guardrails to v1.1, to be
added only where data proves them necessary.

- **v1 (ship now):** minimal protocol + `ρ̂ = 1 − T_resume_to_done /
  T_baseline` and `κ = A_bytes / H(S_t)` logged in every
  `Resurrected` event; `resurrection_count: u32` counter logged per
  resurrection (free — it's `grep Resurrected events.jsonl | wc -l`).
- **v1.1 (after ~2 weeks of wild data):** guardrails justified by
  the distribution — authority ordering (`events > git > log.md >
  synthesis.md`), consistency_scan, horizon rule (`k ≤ 3`), forward-
  progress check — added only where ρ̂ distributions reveal the need.
- **v2 (resident runtime, ADR-016 Phase 3):** runtime subsumes
  detection and scheduling but calls `cs resurrect`; the CLI verb
  stays as the bottom-turtle.

### Wreck storage: git + minimal breadcrumb, no artifact duplication

Torvalds: the worktree's last commit *is* the snapshot. Tolnay and
hawking: a small metadata file is useful for forensics and for the
branch-deletion case.

**Resolution:** ship the breadcrumb but not the artifact copy.
Per resurrection, write
`.cosmon/state/fleets/<f>/molecules/<id>/wrecks/<timestamp>.json`
containing `{ tip_sha, prior_count, composed_prompt_hash,
t_orig_tokens }`. Duplication of markdown artifacts is rejected —
git already carries them.

## Consequences

**Positive**

- Stateless CLI bottom-turtle preserved: `cs resurrect` is a
  one-shot verb, composable with any scheduler (including the
  future resident runtime per ADR-016).
- Prompt composition is a pure function — trivially testable,
  reusable by runtime and CLI alike.
- Measurement from day one: ρ̂ and κ accumulate from the first
  resurrection, enabling data-driven v1.1 guardrails instead of
  speculative pre-building.
- Option B remains reachable as a semver-minor bump: `MoleculeLink`
  gains `#[non_exhaustive]` in a prep commit so a future
  `Reincarnated-From` variant is additive.
- Architectural invariants (all ten in `docs/architectural-
  invariants.md`) preserved: stateless, idempotent, regime-aware,
  single perimeter, human-only, no write-read asymmetry, no
  merge-before-dispatch violation.

**Negative**

- **Wedged-worker case not handled in v1.** A worker whose tmux is
  alive but whose context is exhausted does not satisfy the
  `!tmux_alive` precondition. Documented as a known gap. Follow-up
  molecule (parking-lot) for `cs recover --wedged` detection.
- **Horizon rule not enforced in v1.** A molecule can in principle
  be resurrected indefinitely. The count is logged (free); the
  enforcement is deferred to v1.1 pending data.
- **No consistency_scan arbitration in v1.** If `events.jsonl`,
  git history, `log.md`, and `synthesis.md` disagree, the composed
  prompt presents whatever is on disk. Authority ordering lands in
  v1.1 if divergences are observed.

**Follow-ups (parking-lot, `temp:warm`)**

- Wedged-worker detection (`cs recover --wedged` + kill-before-
  resurrect).
- Measurement dashboard: surface ρ̂ distribution per formula in
  `cs peek` once N resurrections accumulate.
- Artifact-sufficiency doc entry: add the property
  `I(output_{s+1} ; hidden_cognition_s | A_s) = 0` to
  `docs/founding/founding-thesis-ubiquitous-language.md` as a
  named aspiration for workers.

## Alternatives considered

- **Option B — clone with `Reincarnated-From` link.** Rejected
  unanimously. Forces link re-pointing for a distinction the DAG
  cannot observe; the molecule is its filesystem bits plus its
  DAG position, both of which persist.
- **Flag on `cs recover` (`cs recover --respawn <id>`).** Rejected.
  Flag-composition grows polynomially with features;
  single-perimeter rule requires separate verbs for detection (N
  targets) vs surgery (1 target).
- **Verb `cs resume`.** Rejected by disambiguation. `cs resume`
  exists in `crates/cosmon-cli/src/cmd/resume.rs` with a different
  meaning (nudge idle workers). Wheeler's ontological framing is
  elegant but the collision would be a footgun under stress.
- **Full hawking guardrails from v1 (consistency_scan, authority
  ordering, horizon rule, forward-progress check).** Deferred, not
  rejected. Data-driven timing: add only where ρ̂ distributions
  prove necessity.
- **`cs fear` formula.** Rejected. Formalizing the spontaneous
  pattern destroys the mechanism that made it valuable. The
  pattern recurs or it doesn't; `cs fear` as a ritual provides no
  leverage over what the operator already does.
- **New `MoleculeStatus::Resurrected`.** Rejected. Resurrection
  is an event, not a state — a status entry forces every surface
  to handle it for no capability gain.
- **Primary archive at `.cosmon/wrecks/<id>/<timestamp>/`
  containing artifact copies.** Rejected. Git is the snapshot;
  copying markdown files duplicates with no gain and adds a
  consistency burden. The minimal metadata breadcrumb is kept.

## References

- `.cosmon/state/fleets/default/molecules/delib-20260414-8643/synthesis.md`
- `.cosmon/state/fleets/default/molecules/delib-20260414-8643/responses/*.md`
  (wheeler, torvalds, shannon, tolnay, hawking)
- [ADR-016 — Autonomy regimes and resident runtime](016-autonomy-regimes-and-resident-runtime.md)
- [ADR-030 — Cosmon archive model](030-cosmon-archive-model.md)
- [docs/architectural-invariants.md](../architectural-invariants.md)
- the companion internal chronicles (Anthropic channels; resurrection and fear-driven discovery)
- `crates/cosmon-cli/src/cmd/resume.rs` (verb-collision evidence)

## Implementation

v1 implementation lands in molecule `task-20260414-21b7`. Code
perimeter:

- `cosmon-core::prompt::compose_resurrection_prompt(mol_dir) ->
  String` — pure function; prompt body per the panel's ordered
  authority (intent → plan → git log → log.md tail →
  draft-marked synthesis → responses listing).
- `cosmon-cli::cmd::resurrect` — precondition `state == Stuck`,
  human-only, refuses `tmux_alive`, writes breadcrumb, emits
  `Resurrected` event, reuses `tackle`'s launch path via
  `LaunchMode::Resurrect(String)`.
- `EventV2::Resurrected { at, from_session, composed_prompt_bytes,
  t_orig_tokens, prior_count, rho_hat, kappa }`.
- `ResurrectError` (`thiserror`, `#[non_exhaustive]`) — v1 variants
  `NotAWreck`, `ArtifactsMissing`, `FlockContended`,
  `DoubleResurrect`, `Transport`, `Io`. v1.1 adds `BranchDiverged`,
  `ArtifactsCorrupt`.
- `MoleculeLink` gains `#[non_exhaustive]` (prep commit) so Option
  B remains a semver-minor door.

Prose only in this ADR; all code lands in `task-20260414-21b7`.
