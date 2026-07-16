# Appendix — Physics Inspiration (Non-Normative)

This appendix collects constructs that were useful during cosmon's
design phase as metaphorical scaffolding but that **do not pass the
Feynman test** from THESIS Part XII (line 1133): they make no
prediction that would fail if the quantity were wrong. THESIS
Part XIX — The Unification Principle — demotes them here.

Content in this file is **non-normative**. It explains where certain
names and intuitions came from. It does not constrain the code.
The normative content — `EnergyBudget`, `worker_status_entropy`,
`free_energy_ratio`, the five Part XII entropy sources, and the
Part XVIII coupling channel capacity — remains in THESIS.md.

## Helmholtz Free Energy

The formula `F = U − T·S` was proposed as a global efficiency
measure for a fleet, with `U` in tokens, `T` as LLM sampling
temperature, and `S` as worker-status Shannon entropy in bits.

**Why it was demoted.** The units do not reconcile: tokens minus a
dimensionless-times-bits product is not a meaningful quantity. No
code path reads the result. Shannon and Feynman panels flagged this
independently in `delib-20260411-066c`.

**What survives.** The two inputs are real on their own:
`EnergyBudget` (tokens) is a genuine resource tracker, and
`worker_status_entropy()` is a genuine Shannon `H(X)`. Their ratio,
`free_energy_ratio = useful_tokens / total_tokens`, is a genuine
efficiency metric and remains in the thesis.

## The Carnot Cycle Mapping

Early design notes mapped the molecule lifecycle onto a Carnot
cycle: nucleation = isothermal expansion, evolution = adiabatic
compression, completion = isothermal compression, collapse = heat
rejection. The analogy produced no prediction and informed no
decision. It is preserved here only because the ordering-of-stages
intuition was useful during formula design.

## The Three Laws of Cosmon Thermodynamics

- **First Law** (conservation of tokens across a molecule): the
  useful version is `EnergyBudget`. The "law" framing added no
  constraint the type system was not already enforcing.
- **Second Law** (entropy non-decrease in a closed fleet): the
  useful version is the observation that worker-status entropy
  tends to rise under load. Framing it as a law implied a
  prohibition the code never checks.
- **Third Law** (entropy → 0 as temperature → 0): decorative. No
  code path uses this.

## Temperature

LLM sampling temperature is a real knob on a real API. It is not
thermodynamic temperature. The two share a symbol and no physics.
The codebase calls it `Temperature` for the LLM meaning only; any
thesis passage that treats it as `T` in a thermodynamic formula is
borrowing the symbol, not the physics.

## Why keep any of this?

Because the metaphors were load-bearing during exploration. They
gave the design team a shared vocabulary when the actual
information-theoretic content had not yet crystallised. Stripping
them entirely would erase the provenance of concepts that are now
stated correctly elsewhere. Keeping them here, clearly marked as
non-normative, is the honest record of how the thesis arrived at
its current quantitative commitments.

## References

- `delib-20260411-066c/synthesis.md` — C2 (thermodynamic formulas
  are cargo cult): Feynman, Shannon, Einstein converge
  independently.
- `delib-20260411-066c/responses/feynman.md` — cargo cult detection
  audit, 40/40/20 breakdown.
- `delib-20260411-066c/responses/shannon.md` — information-theoretic
  audit: only `worker_status_entropy` is a genuine Shannon
  computation.
- THESIS Part XI — Energy Principle (normative content that
  survives).
- THESIS Part XII — Entropy as Computable Observable (the
  four-criterion Feynman test at line 1133).
- THESIS Part XIX — The Unification Principle (the demotion
  decision).
