# ADR-129 — `G_latex` — LaTeX Quality-Convergence Gate as a Cosmon Primitive

**Status:** proposed
**Date:** 2026-06-23
**Decider:** Noogram
**Authoring task:** `task-20260622-5055`
**Source finding:** `qfa` galaxy, cosmon-ward feedback flow — the
`q4beam-poc` paper pipeline (re-dispatch of collapsed
`spark-20260621-3c04`).

**Binds:**
[ADR-120](120-visual-qa-gate-primitive.md) (`G_visual`, the slide/deck
gate — this ADR is its paper sibling, same two-layer discipline applied
to the LaTeX surface),
[ADR-027](027-gate-molecules.md) (gate molecules — a molecule that exists
to *block* another until a condition holds),
[ADR-009](009-governance-tiers.md) (testing scales with tier; this gate
is the paper-deliverable analogue of a cargo gate). Sits beside the
`editorial-work` formula (prose) and `visual-qa` (decks).

**Architectural invariants:** Composability Principle (CLAUDE.md —
molecules + formulas are the only extension points); `docs/architectural-invariants.md`
§8b (*propose mechanisms of verification, do not impose them* — the gate
is fail-closed at the formula level, not chmod-enforced).

---

## Context

Cosmon's verification surface is bifurcated by deliverable type:

- **Code molecules** run `task-work` → `cargo check / test / clippy /
  fmt`. The gates *look at the artifact* (the compiler reads the code).
- **Prose molecules** run `editorial-work` → reading-based coherence
  review. The gate *reads the artifact*.
- **Deck molecules** run `visual-qa` (ADR-120) → render → read pixels →
  layout checklist.
- **Compiled-paper molecules** — LaTeX/PDF reports, articles, preprints —
  have **no gate that looks at the typeset page.** A worker validates that
  the paper *compiles*, and stops there.

The blind spot is structural, not incidental. A paper can build cleanly,
produce a valid PDF, pass every prose check, and still ship a **badly set
document**: ~30 `Overfull \hbox` (lines bleeding past the right margin),
every figure dumped in the appendix instead of placed beside the sentence
that discusses it (and none appearing early), undefined `\ref`/`\cite`
printing as `??`, and no `microtype`. None of these is a compile error, a
prose error, or a missing file. Nothing in the generic pipeline stops
them.

This is not hypothetical. It came cosmon-ward from the `qfa` galaxy:
paper-generation fleets there ship PDFs with exactly these defects. A
`latex` skill already exists (`~/.claude/skills/latex` — a
content-generation skill that prevents the common LaTeX errors), **but it
was never wired into a feedback loop.** The worker invokes the skill once,
generates LaTeX, and ships whatever falls out — there is no `build → see
the defects → fix → rebuild` cycle. The paper has no equivalent of the
slide-gate.

The missing primitive is a **reusable LaTeX quality-convergence gate**:
build → audit the log + source for the mechanical defect classes → read
the rendered page → fail-closed → iterate until the typography converges.
Re-deriving it inline in every galaxy that ships a paper is the
anti-pattern (the `latex` skill's checklist would become N divergent
copies, none of them a *loop*).

## Decision

Ship `G_latex` as a cosmon-level **formula**, `latex-convergence`, that
any molecule with a compiled-paper deliverable composes before completion,
plus an in-tree objective oracle `scripts/latex-audit.sh`.

### Why a formula (and not a command, a daemon, or a config gate)

The Composability Principle is load-bearing — the *exact same reasoning*
that made `visual-qa` a formula rather than a `cs visual-audit` verb
(ADR-120 §"Why a formula"). *Molecules + formulas are the only extension
points.* The gate is a **discipline** (build → audit → correct →
re-build, fail-closed), and a discipline is what a formula encodes. The
alternatives were rejected identically:

- **A new `cs latex-audit` verb** — would couple cosmon's transactional
  core to a TeX-log parser and rasteriser, violating the
  zero-I/O-core / stateless-CLI invariants. The *parser* belongs in a
  `scripts/` helper (like `architecture-audit.sh`), not in the core verb
  set.
- **A `[gates]` entry in `config.toml`** — gates are *universal*
  one-shot shell commands injected into *every* worker prompt. The LaTeX
  gate is *conditional* (only paper deliverables) and *iterative* (a loop,
  not a one-shot exit code). It does not fit the gate slot.
- **A daemon** — forbidden in the transactional core, and pointless: the
  gate is one-shot per build.

### Two layers — the parser is the floor, reading the page is the ceiling

This mirrors ADR-120's lumen-floor / vision-ceiling split exactly:

1. **Objective floor — `scripts/latex-audit.sh`.** A pure text parser over
   the build `.log` and the `.tex` source. It reports `Overfull
   \hbox`/`\vbox`, undefined `\ref`/`\cite`, figures stranded after the
   appendix/bibliography, rigid `[H]`/`[h]` float placement, and missing
   microtype. Exit `2` on a FAIL-class defect (mirroring `lumen
   visual-audit`'s exit 2). It is cosmon's **in-tree** analogue of lumen:
   because the defect classes are mechanically present in the log the
   compiler already wrote, no rasteriser and no TeX install are needed —
   the parser stays out of the core (`scripts/` helper) just as the
   rasteriser stays out of the core for `visual-qa`.
2. **Semantic ceiling — the worker reads the rendered PDF.** The floor
   counts overfull boxes; it cannot see whether figure 3 belongs beside
   paragraph 2, or whether the first figure appears early enough. The
   worker rasterises and READS: figure-next-to-its-discussion,
   first-figure-early, tables fitting, math set not raw.

Either layer can fail the gate; both must pass. For full visual-layout QA
(column balance, empty zones) the producer composes `visual-qa` on the
*same* PDF — the gates stack: `G_latex` owns typography + float placement,
`G_visual` owns layout geometry.

### The `latex` skill is the tool; the formula is the loop

The pre-existing `latex` skill is a **content generator** — it produces
correct LaTeX and knows the avoid-overflow rules. What was missing is the
**loop** that forces the worker to keep applying it until the page
converges. `latex-convergence` is that loop; inside its `audit` correct
step the worker reaches for the skill. This is the precise diagnosis from
the source finding: *the skill exists but is not wired into a feedback
loop.*

### Fail-closed, iterate-in-place

The default verdict is FAIL until proven PASS. Any floor FAIL or any
ceiling NO triggers: diagnose source → correct (real fixes, not `\sloppy`
band-aids) → re-build → re-audit, looping until clean. This mirrors a
cargo gate looping until green — it does **not** fan out into child
molecules (tier-0, no nucleation). The iteration trail
(`latex-iterations.md`) and the verdict (`latex-verdict.md`) are the
proof-of-work, written to the molecule state directory.

### Composition — two modes

- **Standalone:** `cs nucleate latex-convergence --var tex_main=<path>`
  blocked by the producing molecule; the gate runs as its own molecule
  before `cs done`.
- **Inline:** the producing molecule's worker runs the steps before
  `cs complete`. The formula text is the checklist of record in both
  modes.

See [`docs/guides/latex-convergence-gate.md`](../guides/latex-convergence-gate.md).

## Consequences

- A compiled-paper deliverable can no longer pass the pipeline with ~30
  overfull boxes and every figure in the appendix "because it compiled."
  The mechanical defect classes are caught *before* completion, by the
  producing worker, without a human-requested corrective pass.
- The `latex` skill is finally wired into a *loop*. The skill stays the
  content generator; the formula supplies the convergence discipline it
  lacked.
- The objective floor (`latex-audit.sh`) is a **portable, toolchain-free**
  parser. Its self-test (`scripts/latex-audit.test.sh`) demonstrates the
  before→after convergence on a fixture carrying the exact pathology — no
  TeX install required, so it runs in CI.
- cosmon stays free of a TeX/rasteriser dependency in its core; the parser
  is a `scripts/` helper, the rendering-read is the worker's job.
- The gate is a **mechanism proposed, not imposed** (invariants §8b):
  fail-closed inside the formula's discipline, but no filesystem lock — a
  motivated worker can still skip it, exactly as a worker can `git commit
  --no-verify`. The verdict artifact makes the skip *observable*.

## References

- `.cosmon/formulas/latex-convergence.formula.toml` — the primitive (this
  ADR's implementation).
- `scripts/latex-audit.sh` — the objective floor (TeX-log + source parser).
- `scripts/latex-audit.test.sh` — the before→after convergence
  demonstration / regression guard.
- `docs/guides/latex-convergence-gate.md` — operator/worker guide + full
  checklist.
- `~/.claude/skills/latex/SKILL.md` — the content-generation skill the
  loop drives.
- [ADR-120](120-visual-qa-gate-primitive.md) — `G_visual`, the deck
  sibling this gate is modeled on.
- [ADR-027](027-gate-molecules.md) — gate-molecule precedent.
