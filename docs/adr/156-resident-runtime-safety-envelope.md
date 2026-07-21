# ADR-156 — Resident Runtime Safety Envelope and Re-ignition Gate

**Status:** Proposed (2026-07-13). This ADR is binding only after the C10
cross-provider review has cleared.
**Decision owner:** Noogram
**Origin:** `delib-20260713-92fe`,
C0 — safe-runtime envelope.
**Amends:** [ADR-095](095-resident-runtime-ifbdd-path.md). ADR-095's RR-1
through RR-5 construction constraints remain in force; this ADR adds the
operational safety envelope that controls whether the runtime may be enabled.
**Implemented by:** C1–C5 from `delib-20260713-92fe`; this ADR specifies their
invariant contract, not their mechanisms.

## Context

ADR-095 re-opened the Resident Runtime build path as a constrained client of
the transactional core. The failure dossier considered by
`delib-20260713-92fe` shows that this structural shape alone is insufficient to
permit autonomous advancement:

- a merge collision advanced `main` into a broken combined state (#1);
- an unreviewed security merge and an operator-reserved decision were crossed
  autonomously (#2 and #5);
- a pilot could lose a tackle race to the runtime (#3-preempt), while the
  kill-switch was the only effective control (#6);
- a worker that produced no output for 43 minutes appeared healthy (#4), and
  adapter intent could be silently dropped into a budget leak (#3-budget).

The deliberation's Feynman classification is decisive. Failures #1, #2, and
#5 are **defects**: their controls must be gates that refuse advancement.
Failure #4 is **advisory**: its control must inform the operator, not impose an
automatic kill. Failure #6 is not a defect: the kill-switch worked. It is,
however, evidence that a positive per-molecule ownership control is required.

This ADR does not re-litigate C1–C5's code-level choices. It names the safety
properties those children must establish and makes runtime re-ignition a
binary, inspectable decision.

## Decision

The Resident Runtime SHALL remain disabled until the re-ignition gate in §3 is
satisfied. When enabled, it SHALL preserve all four RR-SAFE invariants below.

### RR-SAFE-1 — Integrity

The runtime never advances `main` beyond a failed combined-state gate.

This is the response to failure #1. C1 provides a two-stage refusing control.
Every merge runs a bounded, pre-stamp, **diff-scoped** combined-target compile
gate under the trunk lock. It maps `git diff --name-only <pre-merge>..HEAD` to
the changed crates, then checks those crates **and their reverse dependants**
with `cargo check -p … --all-targets`; the reverse closure catches a changed
signature in crate A that breaks a test target in caller B. A documentation-only
merge does not run Cargo. Root workspace/dependency changes and `cosmon-core`
fall back to `cargo check --workspace --all-targets`. The gate has a short
five-minute hang bound and catches the combined-state signature/type collision
class without executing tests. A failed **or timed-out** gate rolls the merge
back, releases the trunk lock, and does not mark the molecule merged. A
per-branch success is not evidence that the combined state is safe.

The heavyweight assurance suite is deliberately separate: an operator invokes
`cs validate` at a project milestone to run doctests, the complete workspace
test suite, strict clippy, formatting, and the mutation falsifier. It is never
automatically run by `cs done`; development-cycle speed is part of the
integrity control's operability, while the explicit milestone gesture retains
the deeper validation boundary.

### RR-SAFE-2 — Authority

The runtime never autonomously crosses a human-reserved threshold: a reserved
decision or a security/`needs-review` merge. Reservation markers are monotone:
the normal worker-reachable tag mutation surface cannot remove `hold:human`,
`needs-review`, `security`, or `security:*`; nor can it add the `auto:ok`
exception to a decision. A security/`needs-review` `cs done` additionally
requires `molecules/<id>/review-verdict.md` to contain `verdict: approved`.

This is the response to failures #2 and #5. C2 provides the refusal controls:
reserved work is not automatically tackled, and review-required completed work
is held for a human rather than automatically merged. Human reservation is an
authority boundary, not a scheduling hint.

### RR-SAFE-3 — Ownership

The runtime never preempts a pilot-claimed molecule; it defers to a human mark
unconditionally.

This is the response to #3-preempt and the lesson of #6. C3 supplies the
positive control: a pilot claim excludes the molecule from the runtime
frontier. The current scheduler still has a one-poll residual race; it does
**not** yet implement an atomic compare-and-swap reservation. The global
kill-switch remains an emergency brake; it is not a substitute for
per-molecule ownership.

### RR-SAFE-4 — Legibility

The runtime surfaces output-stall and routing/budget state as witnesses without
imposing an automatic kill.

This is the response to #4 and #3-budget. C4 preserves adapter-routing intent;
C5 distinguishes real output from liveness and exposes an `OutputStalled`
witness to `cs health`/patrol. These signals inform the operator. They do not
authorize the runtime to terminate or re-tackle work automatically.

### RR-SAFE-5 — Harvest routing

The runtime never auto-harvests a molecule whose merge the operator has
reserved or routed. Two per-molecule tags carry the intent, both honored in the
resident loop's first (harvest) sweep alongside `hold:human` and the review
gate:

- **`no-auto-harvest`** — reserve the harvest as an operator gesture. The
  completed molecule is left untouched; a human runs `cs done` (or merges by
  hand) when the park is lifted.
- **`harvest_to:<branch>`** — a routing intent naming a non-trunk merge target.
  The resident loop can only ever merge to the trunk (its `cs done` shell-out
  carries no branch argument), so *any* `harvest_to:` is honored as "not the
  runtime's to harvest": the merge is reserved for the operator gesture that can
  route it.

This is the response to the 2026-07-20 failure: the operator had rewritten
`main` to park the whole math-attack line on `spore/math-attack` pending
validation, yet the runtime auto-harvested completed `task-20260720-90d2` and
merged its branch into `main` (merge `63fc899`), silently undoing the park. The
git-level park was invisible to the runtime, which reasons over molecule
tags/status, not branch topology. A harvest-reserved completed blocker also
never clears its dependents (it never merges, so `merged_at` stays absent),
mirroring the operator's park of the whole line rather than draining past it.

Falsifier: a runtime harvest event merging to the trunk for a molecule carrying
`no-auto-harvest` or any `harvest_to:` tag.

## Re-ignition gate

The gate is intentionally binary. Its wording is copied verbatim from the
parent deliberation:

```
enabled = false   # stays false until ALL of:
  C1 (integration gate)      green in CI
  C2 (reserved-threshold)    green in CI
  C3 (pilot reservation)     green in CI
  C4 (adapter plumbing)      green in CI
  C5 (output witness)        present (surfaced to cs health/patrol)
# C6–C9 do NOT gate re-ignition. C10 (cross-provider review of the ADR) must
# clear before the ADR is inscribed as binding.
```

C1 through C3 demonstrate the refusing gates for Integrity, Authority, and
Ownership. C4 and C5 establish the two Legibility witnesses. C6–C9 remain
valuable later work, but are not conditions for enabling the runtime. C10 is a
process condition on this ADR's binding force, not an additional runtime
mechanism.

## Consequences

- The runtime cannot be re-enabled by an informal judgement that the loop
  "looks safe"; each prerequisite has a named, independently observable
  disposition.
- Defect controls fail closed: a runtime action that would violate Integrity or
  Authority is refused. Legibility stays advisory: it makes a stall or routing
  problem visible without converting an observation into autonomous control.
- ADR-095 remains the architectural build path. This amendment is the envelope
  that governs its operational admission; it neither replaces ADR-095's
  client-of-core/no-state/deletability constraints nor specifies C1–C5's
  implementation.
- Until C1–C5 are green/present and C10 clears, `enabled=false` is the only
  conforming runtime state.

## Verification and binding condition

The C1–C5 implementation molecules supply the evidence named in §3. Before
this ADR is inscribed as binding, C10 MUST run the existing cross-provider
committee against this draft, including the assigned dissent that a permanent
gate may itself harm the drain and that ownership must not be displaced by a
satisfying-looking gate. A confirming C10 review changes this ADR's status from
Proposed to binding; a refutation requires amendment before re-ignition.
