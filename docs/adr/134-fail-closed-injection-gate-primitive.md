# ADR-134 — `G_inject` — Fail-Closed Zero-Hit Injection Gate as a Cosmon Primitive

**Status:** proposed
**Date:** 2026-06-25
**Decider:** Noogram
**Authoring task:** `task-20260625-5687`
**Parent idea:** `idea-20260625-0e54`
(capture → feasibility → plan; this ADR is its Child 1).
**Source finding:** migrated from dave `idea-20260508-80de`, surfaced by
torvalds in the dave deliberation `delib-20260508-da7d` — cosmon-ward
feedback flow. Concrete instance: `wiki-site/build.sh` stage 4.5 (a
paper-bundle galaxy) prints `favicons injected: 0` / `nav bars injected: 0`
**and then `exit 0`**.

**Binds:**
[ADR-128](128-d7-attribution-vacuum-and-publish-gate.md) (the D7 publish
gate / ban-list — this ADR is its **sign-flipped twin**, same fail-closed
spine, opposite polarity),
[ADR-027](027-gate-molecules.md) (gate molecules — a molecule that exists
to *block* until a condition holds),
[ADR-120](120-visual-qa-gate-primitive.md) (`G_visual`, the visual-render
gate),
[ADR-129](129-latex-convergence-gate-primitive.md) (`G_latex`, the
LaTeX-convergence gate — this ADR adopts its **in-tree-analogue** precedent
and its **exit 2** FAIL convention). Together these four ADRs constitute
the cosmon **fail-closed gate-primitive family**.

**Architectural invariants:** Composability Principle (CLAUDE.md —
molecules + formulas are the only extension points); `docs/architectural-invariants.md`
§8b (*propose mechanisms of verification, do not impose them* — the gate is
fail-closed at the convention level, not chmod-enforced).

---

## Context

A build pipeline routinely runs **post-processing injection/transform
steps**: stamping a favicon into every rendered page, splicing a nav bar
into the HTML, appending a footer attribution, rewriting canonical URLs,
shimming MathJax, injecting an analytics snippet. Each such step has a
**known-nonzero expected hit count** — it is dispatched precisely because
there are N>0 targets it is supposed to touch.

The pathology is structural, not incidental. The step is written to *print*
its hit count and *return success*:

```
favicons injected: 0
nav bars injected: 0
```

…followed by **`exit 0`**. The print line is informational; the exit code is
the only thing the calling pipeline — and the operator's attention — actually
reads. A zero-hit injection is, in every realistic case, a **broken selector
/ moved template / renamed marker** — i.e. a real failure — yet the script
reports green. The site ships without favicons or nav, and nobody is told.

This is the **silent-fail-open** shape: *the absence of work is reported as
the successful completion of work.* It is the build-pipeline cousin of the
leak and quality gates cosmon already ships, and it **reproduces
structurally** — *any* paper-bundle / static-site galaxy that does a
post-render injection pass has the same shape: a transform that *should* hit
N>0 targets, with no machinery asserting that it did.

Per the cosmon-ward feedback flow (CLAUDE.md), the test for a cosmon-ward
escalation is *"is an invariant broken or is a primitive missing?"* — not
*"did something rub?"*. This is a **missing primitive**: silent-ignore is
forbidden cosmon-ward (*"Silent-ignore is forbidden cosmon-ward too"*), and
the absence of a fail-closed assertion on a must-hit transform is precisely
silent-ignore at the build layer.

The missing primitive is a **fail-closed zero-hit injection gate**: a
post-processing injection step with an expected nonzero hit count must
**assert `hits > 0` (or `hits >= expected`) and exit non-zero otherwise**.
The print-then-`exit 0` shape is banned. Re-deriving this inline in every
galaxy that ships a build pass is the anti-pattern.

## Decision

Ship `G_inject` as a small, self-contained **in-tree bash helper**
(`scripts/assert-hits.sh`) plus a **named convention**: any post-processing
injection/transform step with a known-nonzero expected hit count MUST route
its hit count through the helper, which echoes the count on success and
**fails closed (exit 2)** when the count is below the expected minimum
(default 1).

### The sign-flip insight — `G_inject` is the must-hit twin of the D7 ban-list

ADR-128's D7 publish gate (and its ban-list machinery
`check_git_remote_blocklist` / `collect_*_violations` in
`crates/cosmon-cli/src/cmd/done.rs`) is cosmon's house pattern for a
fail-closed gate. Its 4th property — *the gate **blocks**, it does not merely
warn* — is exactly what the print-then-`exit 0` step is missing.

`G_inject` generalises that gate's **polarity**:

| | trigger to abort | example |
|---|---|---|
| **Ban-list** (D7, ADR-128) | a forbidden thing is **present** — `count > 0` is bad | confidential name in a footer; blocklisted git remote |
| **Must-hit** (`G_inject`, this ADR) | a required thing is **absent** — `count == 0` is bad | favicon injected zero times |

Same fail-closed spine, **opposite sign**. A ban-list aborts when it *finds*
something; a must-hit assertion aborts when it *finds nothing*. Both obey
the 4th property: the gate is hard-blocking (a non-zero exit at a boundary
the caller reads), never advisory, never a scrolling print line the operator
can miss. This is the precise structural relationship that places `G_inject`
in the gate-primitive family beside ADR-027, ADR-120, ADR-128, ADR-129.

### Why an in-tree bash helper (not lumen, not cosmon-core Rust)

Three policy decisions, each carried from the parent idea's `feasibility.md`:

1. **Not a lumen code change.** `lumen` is the external paper/document-build
   substrate. ADR-129 already faced exactly this fork ("put the gate in lumen
   vs. in cosmon") and settled it: cosmon does **not** push build-gate
   primitives *into* lumen. It ships a small self-contained **in-tree
   analogue** (ADR-129: *"It is cosmon's **in-tree** analogue of lumen"*)
   that lumen and every other galaxy can call. Adoption is a one-line
   vendor/source, not a cross-repo dependency.

2. **Not a cosmon-core Rust surface.** The bug lives in *bash build
   pipelines*, not in the `cs` state machine. Forcing it into cosmon-core
   would couple the transactional core to a build-pipeline concern and
   violate *"resist single-step formulas / abstractions that exist only to
   satisfy the system."* `scripts/` is already full of exactly this shape —
   `cs-paste-nudge.sh` + `.test.sh`, `curate-classify.sh` + `.test.sh`,
   `architecture-audit.sh`, `latex-audit.sh` — so a new `scripts/assert-hits.sh`
   + `scripts/assert-hits.test.sh` is **idiomatic cosmon**, not a new
   infrastructure category. It honours the Composability Principle (no new
   command, no daemon, no plugin interface — a script + a convention).

3. **Exit 2 on FAIL.** The helper exits `2` on a zero-hit (FAIL-class)
   result, mirroring `lumen visual-audit` and ADR-129's `latex-audit.sh`
   convention. Exit 2 distinguishes a *gate FAIL* from a generic exit-1 error
   and from exit-0 success, so a calling pipeline can branch on it.

### Author opt-in per step — no name heuristic

The expected-nonzero contract is declared by the **author, per step** —
never inferred from a step name. A heuristic ("any step named `inject*` must
hit >0") manufactures **false red cords**: a legitimately empty pass would
abort the build, re-introducing an over-blocking failure mode to fix an
under-blocking one. Consistency with the gate-primitive family settles it:
the D7, `G_visual`, and `G_latex` gates are all **author-declared
boundaries**, not inferred. The author opts the step in:

```bash
# stage 4.5 — inject favicons
hits=$(inject_favicons ...)             # the step prints/returns its hit count
assert_hits "favicons" "$hits"          # echoes hits on success; exit 2 if hits == 0
```

or, wrapping a command whose stdout is grep-counted:

```bash
inject_navbars ... | assert_hits_stream "nav bars" --min 1
```

The trigger is specifically *injection/transform steps with a known-nonzero
expected hit count*. A grep that legitimately finds nothing is **not** this
bug; the author declares intent by opting the step in. Turning every bash
command into a gate is explicitly out of scope.

### Convention-level fail-closed (§8b — propose, do not impose)

The gate is fail-closed *at the convention level*, not enforced by the
filesystem. A galaxy that never sources the helper is not chmod-forced to —
exactly as `G_latex` is fail-closed inside its formula's discipline but a
worker can still `git commit --no-verify`. This honours
`docs/architectural-invariants.md` §8b: *propose mechanisms of verification,
do not impose them.* The convention guide + the helper make the correct
behaviour the one a worker reaches for by reflex; the verdict (a non-zero
exit) makes any skip observable.

### The operator-attention thesis

The whole point is **which channel the operator actually reads.** A
scrolling print line (`injected: 0`) is not that channel — it scrolls past,
and the operator was not watching the conveyor belt at that exact second.
The **exit code** is the channel the calling pipeline branches on and the
operator's CI surface reports. `G_inject` moves the signal from the
informational print line to the exit code. This is the same
operator-attention discipline that motivates every gate in the family: a
degenerate output must pull the red cord, not wave the artifact through.

## Consequences

- A post-processing injection step can no longer pass the pipeline with
  `injected: 0` "because it printed and exited 0." The zero-hit case is
  caught *at the step boundary*, by a non-zero exit the caller reads, without
  a human noticing the scrolled print line after the fact.
- The fail-closed gate-primitive family becomes **coherent across the
  pipeline depth**: publish-boundary (D7 / ADR-128), visual-render
  (`G_visual` / ADR-120), typeset-page (`G_latex` / ADR-129), and now
  build-pipeline injection (`G_inject` / this ADR). Degenerate output ⇒ red
  cord, everywhere.
- The sign-flip is now **named doctrine**: ban-list (count>0 bad) and
  must-hit (count==0 bad) are the two polarities of one fail-closed spine.
  Future gates can be classified by polarity rather than re-derived.
- The helper is **portable and toolchain-free** — a pure function of
  `(label, count, [--min N])`. Its self-test (`scripts/assert-hits.test.sh`)
  demonstrates zero→exit-2 / N→pass / `--min` boundary / garbage-args /
  stream-mode with no external dependency, so it runs in CI.
- cosmon-core stays free of a build-pipeline concern; the helper is a
  `scripts/` companion, the assertion is the author's opt-in.
- The gate is a **mechanism proposed, not imposed** (§8b): fail-closed inside
  the convention, but no filesystem lock — a galaxy that never sources it is
  not forced to, exactly as a worker can `--no-verify`.

### Deliberately out of scope (follow-ups, not this ADR)

- **Implementing the helper** — `scripts/assert-hits.sh` + `.test.sh` + the
  convention guide `docs/guides/fail-closed-injection.md` + chronicle entry
  is **Child 2** of the parent idea (🔧 task, blocked by this ADR).
- **Adopting at the known instance** — patching `wiki-site/build.sh` stage
  4.5 to wrap its favicon/nav injection in `assert_hits` is a **cross-galaxy**
  edit, surfaced via the cosmon-ward / syzygie feedback flow once the
  primitive ships — not a cosmon-local task.
- **A lint formula** grepping build scripts for `inject`-shaped steps lacking
  an assertion is a possible later hardening, explicitly out of this minimal
  V0 (over-blocking / over-engineering risk; convention + helper first).

## References

- `idea-20260625-0e54`
  — parent idea (capture / feasibility / plan), this ADR's full rationale.
- dave `idea-20260508-80de`, `delib-20260508-da7d` (torvalds) — the
  cosmon-ward origin finding.
- [ADR-128](128-d7-attribution-vacuum-and-publish-gate.md) — the D7 publish
  gate / ban-list, the sign-flipped twin of this gate.
- [ADR-027](027-gate-molecules.md) — gate-molecule precedent.
- [ADR-120](120-visual-qa-gate-primitive.md) — `G_visual`, the visual-render
  gate sibling.
- [ADR-129](129-latex-convergence-gate-primitive.md) — `G_latex`, the source
  of the in-tree-analogue and exit-2 precedents.
- `docs/architectural-invariants.md` §8b — *propose mechanisms of
  verification, do not impose them.*
- `scripts/assert-hits.sh` + `scripts/assert-hits.test.sh`,
  `docs/guides/fail-closed-injection.md` — the implementation (Child 2,
  forthcoming).
