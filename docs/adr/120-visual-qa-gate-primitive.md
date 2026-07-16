# ADR-120 — `G_visual` — Adversarial Visual-QA Gate as a Cosmon Primitive

**Status:** proposed
**Date:** 2026-06-05
**Decider:** Noogram
**Authoring task:** `task-20260605-6a02`
**Source finding:** `echo` galaxy, cosmon-ward feedback flow —
`/srv/cosmon/echo/presentation/LAYOUT-QA.md` §"Signal cosmon-ward",
corrective molecule `echo task-20260605-fd35`.

**Binds:**
[ADR-027](027-gate-molecules.md) (gate molecules — the precedent for a
molecule that exists to *block* another until a condition holds),
[ADR-009](009-governance-tiers.md) (testing scales with tier; the
visual gate is the design-deliverable analogue of a cargo gate). Sits
beside the `editorial-work` formula (the prose-deliverable analogue of
`task-work`).

**Architectural invariants:** Composability Principle (CLAUDE.md —
molecules + formulas are the only extension points); `docs/architectural-
invariants.md` §8b (*propose mechanisms of verification, do not impose
them* — the gate is fail-closed at the formula level, not chmod-enforced).

---

## Context

Cosmon's verification surface is bifurcated by deliverable type:

- **Code molecules** run `task-work` → `cargo check / test / clippy /
  fmt`. The gates *look at the artifact* (the compiler reads the code).
- **Prose molecules** run `editorial-work` → reading-based coherence
  review. The gate *reads the artifact* (the worker reads the text).
- **Visual molecules** — HTML decks, PDF slides, posters, rendered
  diagrams, printable documents — have **no gate that looks at the
  rendered page.** A worker validates that the deck *compiles* and
  *prints*, and stops there.

The blind spot is structural, not incidental. A deck can compile
cleanly, print to a valid PDF, pass every prose check, and still ship a
**broken layout**: columns whose tops and bottoms do not align, a block
overflowing the footer separator rule, a large empty zone under a title,
cards off the grid, text clipped at a margin. None of these is a
compile error, a prose error, or a missing file. Nothing in the generic
pipeline stops them.

This is not hypothetical. In the `echo` galaxy a 7-slide deck
shipped **3 slides with misaligned column tops/bottoms plus a block
overflowing the footer separator**. It compiled, it printed, it passed.
The defect was caught only by a human eyeball, and fixed by a corrective
pass (`echo task-20260605-fd35`) the operator had to request **by
hand, a posteriori**. The deck-local note
`echo/presentation/LAYOUT-QA.md` diagnosed the root cause correctly
and routed it cosmon-ward rather than patching it silently — exactly the
feedback-flow discipline CLAUDE.md mandates.

The missing primitive is a **reusable visual-QA gate**: render →
rasterise → *read the pixels* → adversarial checklist → fail-closed →
iterate until the page is right. Re-deriving it inline in every galaxy
that ships a deck is the anti-pattern (the echo note would become
N divergent copies).

## Decision

Ship `G_visual` as a cosmon-level **formula**, `visual-qa`, that any
molecule with a visual deliverable composes before completion.

### Why a formula (and not a command, a daemon, or a config gate)

The Composability Principle is load-bearing here. *Molecules + formulas
are the only extension points.* The gate is a **discipline** — a fixed
sequence (render → read → checklist → correct → re-render, fail-closed)
— and a discipline is exactly what a formula encodes. The alternatives
were rejected:

- **A new `cs visual-audit` verb** — would couple cosmon's
  transactional core to a rasteriser and a vision model, violating the
  zero-I/O-core / stateless-CLI invariants, and duplicate lumen's
  engine.
- **A `[gates]` entry in `config.toml`** — gates are *universal* shell
  commands injected into *every* worker prompt. Visual QA is
  *conditional* (only visual deliverables) and *iterative* (a loop, not
  a one-shot exit code). It does not fit the gate slot.
- **A daemon** — forbidden in the transactional core, and pointless: the
  gate is one-shot per render.

### Two layers — lumen is the floor, the vision checklist is the ceiling

`lumen` already ships `lumen visual-audit` — the `G_visual_inspection`
gate: pixel-level, fail-closed, exit `2` on failure, emitting an RFC
6902 JSON Patch report and an annotated `overlap.png`. The instinct
"just wire lumen and done" is **half the primitive**. lumen detects
**cross-block bbox overlap and pixel-cluster drift** — *collisions*. The
echo failure was **not a collision**; it was an *empty zone* and a
*column-imbalance* — a layout that is wrong precisely because two regions
do **not** touch. A pure pixel-overlap detector is blind to it.

So `G_visual` is two composed layers:

1. **Objective floor — `lumen visual-audit`.** Called when `lumen` is
   on PATH. Catches collisions and drift deterministically. This is the
   primitive cosmon *wires* rather than reimplements (the mission's
   "généraliser/câbler" instruction, honoured precisely: the engine
   stays in lumen; cosmon invokes it).
2. **Semantic ceiling — the vision checklist.** A worker READS each
   rasterised page and answers a binary adversarial checklist covering
   balance/fill, alignment (the echo class), overlap/overflow, and
   legibility. This catches what pixel-overlap cannot: empty zones,
   imbalance, off-grid card edges, footer-rule crossings.

Either layer can fail the gate; both must pass. lumen absent degrades
to checklist-only (logged in the verdict), never to skip.

### Fail-closed, iterate-in-place

The default verdict is FAIL until proven PASS. Any NO on any checklist
box triggers: diagnose source → correct → re-render → re-read, looping
until all pages pass. This mirrors a cargo gate looping until green — it
does **not** fan out into child molecules (the gate is tier-0, no
nucleation). The iteration trail (`visual-iterations.md`) and the
verdict (`visual-verdict.md`) are the proof-of-work, written to the
molecule state directory.

### Composition — two modes

- **Standalone:** `cs nucleate visual-qa --var artifact=<path>` blocked
  by the producing molecule; the gate runs as its own molecule before
  `cs done`.
- **Inline:** the producing molecule's worker runs the `visual-qa` steps
  before `cs complete`. Recommended wiring: `editorial-work` and
  `task-work` descriptions point a visual-deliverable worker at this
  formula. The formula text is the checklist of record in both modes.

See [`docs/guides/visual-qa-gate.md`](../guides/visual-qa-gate.md).

## Consequences

- A visual deliverable can no longer pass the pipeline with a broken
  layout that "compiles and prints." The empty-zone / column-imbalance
  failure class the echo deck exhibited is now caught *before*
  completion, by the producing worker, without a human-requested
  corrective pass.
- The echo-local `LAYOUT-QA.md` checklist is generalised once, in
  cosmon, and inherited by every galaxy via the formula — not copied N
  times.
- lumen's `visual-audit` engine is reused, not forked. cosmon stays
  free of a rasteriser/vision dependency in its core; the formula calls
  the binary when present.
- The gate is a **mechanism proposed, not imposed** (invariants §8b):
  it is fail-closed inside the formula's discipline, but there is no
  filesystem lock — a motivated worker can still skip it, exactly as a
  worker can `git commit --no-verify`. The verdict artifact makes the
  skip *observable*.

### Secondary pathology — filed separately, not solved here

The mission flagged a *related* pathology: **worktree isolation means a
molecule already branched does not see inputs committed to its
predecessor after it departed** (a v2 content molecule missed a
manipulation journal committed in the interval). This is a real
DAG-branching / merge-before-dispatch question, orthogonal to the visual
gate, and is left to its own molecule per the mission's "à examiner
séparément." It is **named here so it is not lost**, not addressed: it
touches `cs tackle`'s branch-from-blocker semantics and the
merge-before-dispatch invariant, which is ADR-grade in its own right.

## References

- `.cosmon/formulas/visual-qa.formula.toml` — the primitive (this ADR's
  implementation).
- `docs/guides/visual-qa-gate.md` — operator/worker guide + the full
  binary checklist.
- `/srv/cosmon/echo/presentation/LAYOUT-QA.md` — the source note and
  cosmon-ward signal that motivated this ADR.
- `/srv/cosmon/lumen/crates/lumen-cli/src/commands/visual_audit.rs` —
  the `lumen visual-audit` engine (`G_visual_inspection`) wired as the
  objective floor.
- [ADR-027](027-gate-molecules.md) — gate-molecule precedent.
