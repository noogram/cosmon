# Curate-patrol surfacing — galaxy {{GALAXY}} — night {{DATE_UTC}}

<!--
  Template source: ops/templates/curate-surfaced.md
  Rendered into:   .cosmon/state/fleets/default/molecules/<curate-mol-id>/surfaced.md
  Governing delib: delib-20260521-c3cd (synthesis §Operator-surface channel)
  Discipline:      global CLAUDE.md "One question, one decision"
                   + kahneman D3 (avalanche) + carnot bandwidth ceiling

  RULES THAT MAY NEVER BE BENT:
    1. Cap = 20 entries per night, cross-galaxy total.
       The patrol may surface 20 per galaxy individually only if total
       across galaxies stays <= 20 (operator wakes once, reads in sequence).
    2. One question = one decision. Never bundle.
       "tackle X and collapse Y at the same time?" is TWO entries.
    3. Verdict door enumerated. Always 1 / 2 / 3 / later. Never free text.
    4. Same wording each night. Operator must recognise the shape at
       pattern, not by re-reading.
    5. Atomic only. No prose paragraphs. If a question needs a paragraph
       of context, the patrol's vantage is too low — kick to next pass.
    6. ASCII-only inside questions. No bold/italics/links inside the
       atomic question line — they break recognition.
-->

**Cap.** 20 atomic questions max ; overflow rolls to next-night counter.
**Reading discipline.** Each question is one decision. Reply **1 / 2 / 3 / later**
inline (write your choice on the `Your call:` line). Patrol applies your
verdicts on next pass.

**Tonight's count.** {{COUNT}} surfaced of {{CAP}} cap ({{OVERFLOW}} deferred to
next night).
**Counter.** {{COUNTER_TODAY}} surfaced + deferred today ; rolling 3-night sum =
{{COUNTER_3D}} (self-throttle threshold: 50).

---

## Questions

<!--
  RENDER ONE BLOCK PER SURFACED MOLECULE, IN BLAST-RADIUS ORDER.
  Block template — copy verbatim, fill placeholders, drop nothing:

### {{N}}. `{{MOL_ID}}` — `{{TOPIC_<=80_CHARS}}`
**Patrol verdict:** Surface (`{{RATIONALE_<=100_CHARS}}`)
**Your call:**
  1. tackle — promote to temp:hot, schedule next dispatch
  2. collapse — abandon (24h soak, recoverable via `cs revive`)
  3. keep — leave pending, retag temp:warm
  later — defer, re-surface in 7 days
**Reply:**

  (operator writes 1 / 2 / 3 / later on the line above, nothing else)
-->

### 1. `<mol_id>` — `<topic, <=80 chars>`
**Patrol verdict:** Surface (`<one-line rationale, <=100 chars>`)
**Your call:**
  1. tackle — promote to temp:hot, schedule next dispatch
  2. collapse — abandon (24h soak, recoverable via `cs revive`)
  3. keep — leave pending, retag temp:warm
  later — defer, re-surface in 7 days
**Reply:**

### 2. `<mol_id>` — `<topic, <=80 chars>`
**Patrol verdict:** Surface (`<one-line rationale, <=100 chars>`)
**Your call:**
  1. tackle — promote to temp:hot, schedule next dispatch
  2. collapse — abandon (24h soak, recoverable via `cs revive`)
  3. keep — leave pending, retag temp:warm
  later — defer, re-surface in 7 days
**Reply:**

<!-- ... up to 20 entries, then STOP. Overflow rolls to next night. -->

---

## Overflow

<!--
  This block appears only when more than {{CAP}} molecules wanted to be
  surfaced this night. Each overflowed molecule is tagged temp:patrol-surfaced
  in cosmon state ; the counter file at
  ~/.cosmon/curate-surface-counter.json bumps for today.

  If empty (no overflow), the entire "## Overflow" section is omitted by
  the renderer.
-->

**Deferred to next night** ({{OVERFLOW}} entries) — tagged `temp:patrol-surfaced`
in `.cosmon/state/`. They will re-compete for the 20-slot budget tomorrow.

- `<mol_id>` — `<topic>` — deferred from night YYYY-MM-DD
- ...

---

## Self-throttle trace

<!--
  Bookkeeping for the kahneman D3 self-throttle. The patrol reads
  ~/.cosmon/curate-surface-counter.json (rolling 3-night window) and
  reports here.

    rolling_3night_sum <= 50  → status: HEALTHY, no action
    rolling_3night_sum >  50  → status: AVALANCHE
        ⇒ patrol AUTONOMOUSLY nucleates a `deep-think` deliberation with
          topic "curate-patrol is surfacing more than the operator can
          absorb — review the matrix / budgets / autonomy rung"
        ⇒ patrol STOPS surfacing (only Collapse + Revise verdicts fire)
          until the operator closes the review deliberation
        ⇒ surfaced.md still rendered, but the body says
          "self-throttle engaged ; see delib-YYYYMMDD-xxxx"
-->

**Status.** {{THROTTLE_STATUS}}        <!-- HEALTHY | AVALANCHE -->
**3-night rolling sum.** {{COUNTER_3D}} (threshold 50)
**If AVALANCHE.** Self-throttle engaged ; see `{{REVIEW_DELIB_ID}}`. Patrol will
not surface further questions until the operator closes that deliberation.

---

## How to read this file (Feynman, for tired-at-breakfast operator)

1. Open `surfaced.md` for each galaxy in turn (the morning-review script does
   this automatically). Each file is ≤ 20 atomic questions.
2. For each question, write `1` / `2` / `3` / `later` on the **Reply:** line.
   No prose. The patrol parses your choice and applies it on next pass.
3. `1 = tackle` is the ONLY operator-authorised path to dispatch — patrol
   never auto-tackles in v0. Even after you write `1`, patrol still does not
   call `cs done` automatically ; it surfaces "ready to merge" instead.
4. `2 = collapse` triggers `cs collapse` on next pass ; molecule lives in the
   graveyard for 24h (soakable via `cs revive`) before `cs purge`.
5. `3 = keep` retags the molecule `temp:warm` and removes any
   `temp:patrol-surfaced` overflow tag.
6. `later` defers by 7 days. The molecule stays as-is ; the patrol will not
   re-surface it before that horizon.
7. If you leave a question blank, the patrol treats it as `later` (silent
   defer) and re-surfaces on next pass.
8. The whole file is designed to be read in **under 60 seconds**. If it takes
   longer, that's a structural bug in the patrol — file a bead.
