# F-benchmark — pre-registered capacity measurement

**Status:** Pre-registered (2026-04-22)
**Measurement date:** 2026-05-19 (W4, hard deadline)
**Scope:** Falsifies the claim *"cosmon capacity is 10× on end-to-end tasks
decomposable by W4"* from `delib-20260421-dac4` §8 Feynman verdict.
**Molecule:** `task-20260421-e81e` (this pre-registration)
**Parent deliberations:**
[`delib-20260421-85a6`](../../.cosmon/state/fleets/default/molecules/delib-20260421-85a6/synthesis.md)
(verdict v9 — the null hypothesis we aim to overturn),
[`delib-20260421-dac4`](../../.cosmon/state/fleets/default/molecules/delib-20260421-dac4/synthesis.md)
(Feynman non-negotiable benchmark).

## Why this exists

The claim *"10–30× by W4 on decomposable work"* is either a measurement or
marketing. Eleven personas converged in `delib-dac4` on one rule: **name X,
N, K tonight, or the claim reverts to 85a6 v9 (5–15× on decomposable,
1–2× on human-gated cycles)**. This file is the *tonight* part. On
2026-05-19 the system runs X, records the wall-clock, counts interventions,
and writes a follow-up chronicle with the verdict.

Feynman's formulation, verbatim from `delib-dac4` §8:

> *"By W4 (2026-05-19), task X — which today takes the operator N hours
> of wall-clock time — will be completed in N/10 hours or less, measured
> end-to-end from nucleation to merged-to-main, with the operator
> intervening less than K times."*

## X — the task

Produce a **dual-register pitch deck (fy8a investor + hardtech
technical)** from a one-page operator brief on a **fresh topic** (not
a reuse of an existing deck — not tenant_auditor, not Tenant-Demo, not an ILB slot).

**Required deliverables** (all must be present):

1. `slides.md` — at least 7 slides, including an *alternatives rejetées*
   section.
2. `dist/slides.html` + `dist/slides.pdf` rendered via `make all` (the
   showroom pipeline used for the tenant_auditor deck is the reference
   toolchain).
3. At least one Mermaid diagram rendered to SVG.
4. `notary/slides.notarization.json` — `verify.sh` exits PASS.
5. Branch merged to `main` via `cs done` (standard transactional-core
   close, not a worktree parked off to the side).

The topic is named by the operator at the moment of the measurement run
— same way a *live* benchmark specifies the input only when the clock
starts. Pre-registering the topic would let the system pre-compute; the
whole point is *fresh cognition under real wall-clock*.

## N — operator-solo baseline

**N = 6 hours wall-clock.**

This is an honest fresh-topic baseline. The tenant_auditor deck produced tonight
(2026-04-21) took ~8 hours of operator time, but that measurement
includes the first pass of the pipeline + script plumbing that now
exists. A bare baseline — operator alone, with the same showroom
pipeline already in place, no cosmon workers — is ~6 hours for a
dual-register deck of comparable scope. The mission target is **N/10 =
36 minutes wall-clock** end-to-end nucleate → merged-to-main.

If the operator prefers a conservative baseline (e.g. N = 8h, target
48 min), note the adjustment in the follow-up chronicle rather than
amending this pre-registration. Pre-registration means *no moving goalposts*.

## K — operator intervention budget

**K = 4 interventions maximum.**

The four authorized intervention slots are:

1. **Initial nucleate** — posing X, pasting the 1-page brief.
2. **Mid-run editorial adjustment** — one correction or clarification
   while the worker is in flight.
3. **Final editorial adjustment** — one polish pass before the deploy
   step.
4. **Go / no-go on `cs done`** — the merge-to-main verdict.

Interventions are counted via `cs peek` message count (pilot → worker
channel events) and operator-authored commits on the worker branch.
Incoming `cs whisper` perturbations count as interventions. Reading
the worker's state (`cs peek`, `cs observe`) does *not* count — it is
observation without coupling.

## Measurement protocol (2026-05-19)

**Start signal:** operator runs `cs nucleate <formula> --var brief="..."`
for the fresh topic. Wall-clock starts at the `nucleated_at` timestamp
of the molecule's `state.json`.

**Stop signal:** `cs done <id>` completes and the merge commit lands on
`main`. Wall-clock stops at the merge commit's `AuthorDate`.

**Success criteria — all three required:**

1. **Wall-clock** ≤ 36 min (N/10 with N = 6h).
2. **Intervention count** ≤ K = 4 (counted via `cs peek`).
3. **All deliverables present and `verify.sh` PASS** (slides.md,
   dist/slides.html, dist/slides.pdf, ≥1 Mermaid SVG,
   notary/slides.notarization.json PASS).

**Outcome mapping:**

- **3/3 criteria held** → claim *10× on decomposable X* is **validated**
  for this task. Chronicle entry written the same day. Immediate
  follow-up: extend the benchmark to a second task X' of a different
  genre (e.g. code refactor, research brief, audit report) to avoid
  mono-task bias. A single validated task is a single data point, not
  a capacity curve.
- **≤ 2/3 criteria held** → claim **falls**. Return to `85a6` verdict
  v9: *5–15× on decomposable, 1–2× on external-human-gated*. Follow-up
  chronicle documents the specific gap (which criterion failed, by how
  much, what the dominant cost center was).

**Honesty clause:** the measurement is run *once* on 2026-05-19. No
warm-up runs, no cherry-picking best-of-three. If the operator needs
to re-run the benchmark for procedural reasons (e.g. infrastructure
outage unrelated to cosmon), both runs are reported in the follow-up
chronicle. We don't quietly discard the slow run.

## What this pre-registration is *not*

- **Not an ADR.** ADR engraving is conditional on operator confirmation
  after reading this file. If confirmed, this content becomes the core
  of a future `ADR-059-capacity-benchmark-preregistered.md` and the
  authoritative reference point. Until then, this is a strategy
  document with the binding force of a pre-registration.
- **Not a capacity curve.** One task, one measurement, one date. The
  capacity curve (decomposable vs. irreducible, Feynman's tenant_auditor-onboarding
  saturation at 1–2×, Hawking's phase transitions) is the *population*
  this benchmark samples. A single sample cannot confirm a curve; it
  can only refute it in one specific regime.
- **Not a competitive benchmark.** We're measuring cosmon against the
  operator's own solo baseline, not against Temporal, Airflow, or any
  other orchestrator. The wedge is identity-and-lifecycle-for-AI-agents,
  not raw throughput.

## Calendar

- **2026-04-22** — pre-registration written (this file), committed to
  `main` via `cs done`.
- **2026-05-19** — measurement run. Operator names fresh X in the
  morning; worker runs; wall-clock, intervention count, and
  verify.sh status recorded in a dated chronicle.
- **2026-05-20** — follow-up chronicle entry in an internal chronicle
  with 3/3 or ≤2/3 verdict and the honest narrative.

A reminder for 2026-05-19 is posted via `sec_watch_status` (or
equivalent mailroom pin) with the slug `f-benchmark-2026-05-19`.

## Predecessors and aliases

- **85a6 v9** — the null hypothesis. 5–15× on decomposable, 1–2× on
  human-gated. If this benchmark fails, 85a6 v9 stands unchanged.
- **dac4 §8** — the Feynman non-negotiable benchmark verdict that
  forced this pre-registration to exist.
- **tenant_auditor deck (2026-04-21 evening)** — the ~8h calibration run that
  seeded the N = 6h baseline. *Not* a valid X for this benchmark
  (topic already known, pipeline already hot).

---

*One task. One date. One verdict. The rest is workbench.*
