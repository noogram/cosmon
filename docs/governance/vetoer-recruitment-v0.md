# Vetoer Recruitment — CONSTITUTION v0 Gate

**Status:** OPEN — deadline 2026-04-27 (14 days from 2026-04-13).
**Gates:** P2 (CONSTITUTION v0). If no vetoer is named by the deadline, P2
collapses with reason `no-adversary-no-constitution`.
**Parent deliberation:** [delib-20260413-fb7b](../../.cosmon/state/fleets/default/molecules/delib-20260413-fb7b/synthesis.md) — D1, T1, adversary §5.
**Related chronicle:** [2-of-3 vetoers as social P_external](../lore/CHRONICLES.md).

## Why this gate exists

The cosmon system cannot be its own external witness (adversary panel, §5;
knuth axiom E). A CONSTITUTION that ships without at least one adversarial
reader outside the operator's head is indistinguishable from ship-theater:
the axioms will be tuned to pass the operator's own blind spots. Recruiting
a vetoer is the minimum social implementation of P_external — the only
reason to ship CONSTITUTION v0 at all.

**If this gate cannot be cleared, shipping CONSTITUTION v0 is worse than
not shipping it.** Collapse is the honest move, not the failure mode.

## Criteria for an acceptable vetoer

An acceptable vetoer satisfies **all** of:

1. **External to the operator's daily work.** Not a co-author of any cosmon
   commit, not a member of Noogram, not in the operator's default panel
   (wheeler/torvalds/popper/…).
2. **Technically capable of reading 8 axioms + 160 LOC of CI.** Can judge
   whether an axiom is decidable, whether its CI check actually enforces it,
   whether a back-tested PR was a real hit.
3. **Willing to say "no".** Prior evidence of rejecting a proposal from
   someone they respect. Absence of this is disqualifying; politeness is a
   failure mode here.
4. **Time budget ≥ 2 hours per amendment.** Reading, back-testing the
   proposed axiom against ≥3 historical PRs/molecules, writing a veto memo.
5. **No financial stake in Noogram.** Eliminates the "polite investor"
   failure mode.

Nice-to-have (not required):

- Background in formal methods, type systems, or distributed systems.
- Familiarity with Rust or at least willingness to read `cargo test` output.
- Track record of public technical writing (lowers the cost of written vetoes).

## Candidate shortlist (operator to fill)

This list is intentionally empty of names — the pilot must populate it from
their own network. The slots below are the typology to aim for.

| Slot | Archetype | Why this archetype | Candidate (operator) |
|------|-----------|--------------------|-----------------------|
| V1 | Senior Rust/systems engineer outside Noogram | Judges decidability + CI quality | _TBD_ |
| V2 | Formal-methods / PL researcher | Judges axiom independence, catches knuth-tier errors | _TBD_ |
| V3 | Quant or ops practitioner who has shipped governance docs | Judges whether axioms fire in real workflows | _TBD_ |

**One named V1-or-V2 is sufficient** to clear the gate. V3 is a bonus that
strengthens the 2-of-3 pattern referenced in the chronicle.

## Outreach template

Subject: _Would you veto 8 axioms for me?_

> Hi {name},
>
> I'm about to commit to a written constitution for a project of mine
> (`cosmon`, a multi-agent orchestration CLI). 8 axioms, each enforced by a
> CI check. Total reading: ~500 lines of markdown + 160 lines of CI.
>
> I need exactly one thing from an outside reader: the willingness to say
> "no, that axiom is not decidable" or "no, that CI check doesn't enforce
> what the axiom claims" — before it ships, and again on any future
> amendment.
>
> Estimated cost: 2 hours at v0, then ~2 hours per amendment (I expect
> <1 amendment per month). Compensation: {…}. You can quit at any time
> with no obligation.
>
> If you're in, I'll send the draft + the back-test cases this week. If
> not, a one-line "no" is a perfectly good answer.
>
> — Noogram

## Timeline

| Day | Milestone |
|-----|-----------|
| D+0 (2026-04-13) | This document lands. Operator begins outreach. |
| D+7 (2026-04-20) | At least one candidate has replied (yes / no / negotiating). If all replied "no", trigger the fallback round. |
| D+14 (2026-04-27) | **Deadline.** Vetoer named in this doc OR P2 collapses. |

## Collapse path (if the deadline is missed)

1. `cs collapse <P2-molecule-id> --reason no-adversary-no-constitution`.
2. Promote the 8 axioms' content into `docs/invariants.md` with CI checks
   kept (the CI value is independent of the constitution framing).
3. Chronicle the collapse as a positive event — it confirms that
   `P_external` is load-bearing, not decorative.
4. Do **not** re-nucleate P2 until a vetoer is named out-of-band. A second
   attempt without a vetoer would be ship-theater by construction.

## What "named" means

A vetoer is named when **all** of:

- Their name is written into the "Candidate shortlist" table above, and
- They have acknowledged (in writing, even one line) that they accept the
  role and the 2h/amendment budget, and
- The CONSTITUTION v0 PR cannot merge without their written sign-off
  (mechanical: added as a required reviewer on the PR, or equivalent
  out-of-band signature committed alongside the constitution).

A vetoer who cannot block the merge is not a vetoer.
