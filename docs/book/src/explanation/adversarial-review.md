# Adversarial review: agents that cross-examine each other

A single agent that reviews its own work grades its own homework. Everything on
this page exists to break that loop: the reviewer is a *different molecule*, run
by a *different worker*, in a *different worktree*, and its verdict is written
down where you can read it.

Nothing here is a new command or a new runtime. Every mechanism below is either
a formula (a TOML recipe over molecules) or a plain dependency edge in the DAG.
That is the point: cross-examination is a *shape of work*, not a feature.

## The panel: one question, several perspectives

The `deep-think` formula turns a question into a structured deliberation instead
of an answer. Its four steps leave four artifacts in the molecule's directory:

| Step | Artifact | What it holds |
|------|----------|---------------|
| Frame | `frame.md` | The question broken into numbered sub-questions `Q1…Qn`, plus the panel roster |
| Dispatch | `responses/<persona>.md` | One file per panelist, written in parallel and independently |
| Synthesize | `synthesis.md` | Convergences, divergences, and *why* they diverge |
| Outcomes | `outcomes.md` | The follow-up work the panel identified |

The panel is a roster of named personas — an architect, a systems pragmatist, a
first-principles skeptic, a product voice — each dispatched as its own subagent
with its own brief. They do not see each other's answers while writing. The
disagreement in `synthesis.md` is therefore real disagreement, not one agent
performing both sides of a debate.

`deep-think-inline` is the same panel run by a single worker: it produces the
synthesis and a recommendation but never nucleates children. Both are in the
[formula catalog](../reference/formula-catalog.md).

### The coverage table stops the panel from dodging

The natural failure of any panel is that it answers an easier question than the
one asked and nobody notices. `deep-think` closes this structurally: the
synthesis step must end with a **frame-question coverage table** that accounts
for every `Qn` declared in the frame, marked exactly one of:

- **Treated** — the panel answered the question as framed.
- **Substituted** — it answered an adjacent, easier question instead (and the
  synthesizer must name what was swapped, and by whom).
- **Declined-with-rationale** — deliberately out of scope, reason stated.
- **Silent** — nobody addressed it and no reason was given.

`Silent` is the alarm. It means the question was forgotten or dodged, and the
synthesizer must flag it and recommend either another round or an explicit
decline from you. `cs evolve` refuses to advance off this step until
`synthesis.md` actually exists in the molecule directory — a hard artifact gate,
not a convention.

## The pre-mortem: an independent molecule that tries to sink the work

A panel is a deliberation *before* the work. A **pre-mortem** is an adversarial
audit *after* it, and it is a separate molecule with a separate worker: it reads
the merged commit and the binding spec, and it is briefed to find the reasons
this will fail in production, not to confirm that it compiles.

It returns a verdict — **NO-GO** or **GO** — with numbered findings, each with a
severity, a concrete file:line, a deterministic counter-example, and a "what
would close this" clause. A NO-GO does not revert anything. It nucleates the
remediation work, which then goes back through another audit round.

This runs for as many rounds as it takes. The realized-model attribution feature
(the `~>` drift glyph you see in `cs peek`) is the worked example, and it took
four:

1. **Round 1 — NO-GO.** The audit found the announced runtime capture existed
   for only one adapter, and only when a human happened to open `cs peek`. The
   journal was a side effect of the UI, not a runtime trace.
2. **Round 2 — NO-GO.** Findings partially closed, new ones opened.
3. **Round 3 — NO-GO.** Two of three conditions closed; the third only
   *partially*, and the auditor named exactly why: the new test inverted the
   critical order, forcing the capture before simulating the crash — so it could
   not fail on the case it claimed to cover.
4. **Round 4 — GO.** All conditions closed, with one operational reservation
   recorded outside the code.

Round 3 is the part worth staring at. The implementation was green — build,
test, clippy, and fmt all passed, and the audit says so explicitly before
returning NO-GO. Gates prove the suite still passes. An adversary asks whether
the suite would notice if the code were wrong.

## The verification molecule: the reviewer is not the author

The general pattern the pre-mortem is an instance of: **the thing that checks
the work is a different molecule from the thing that did it.** cosmon ships this
in several shapes.

- `verify-surface` renders a visual surface and observes it from an independent
  molecule. It is a *built-in* formula for a structural reason: the
  `surface_visual` gate refuses `cs complete` until a sibling `verify-surface`
  has landed green, so a project without the formula could not satisfy the
  refusal.
- `visual-qa` is the gate for deliverables that are seen rather than compiled —
  decks, posters, rendered diagrams. It rasterises the output, reads the pixels,
  runs an adversarial layout checklist, and fails closed. Neither `task-work`
  nor `editorial-work` ever looks at the rendered page.
- `bug-closure` runs after a fix lands and walks the bug's whole semantic
  surface — help text, tests, docs, callers, invariants — returning either
  *closed* or *reopened, naming the surfaces still uncovered*. It exists because
  a verb repaired in one place and left stale in three others reads as fixed.

Each is ordered behind the work it audits with an ordinary `cs nucleate
--blocked-by` edge. There is no reviewer registry and no special molecule kind.

## The judge: a seated panel that cannot be picked after the fact

For changes to the rules themselves — constitutional amendments rather than
ordinary code — cross-examination gets a voting procedure. `cs panel` seats a
fixed core of four personas plus a rotating seat drawn from a pool **by hashing
the diff**. Because the rotating seat is a pure function of the artifact under
review, the person convening the panel cannot look at the change and then choose
a friendly judge. `cs panel decide` refuses ballots from non-panelists, refuses
to rule until every seat has voted, and needs a 4-of-5 supermajority. The full
grammar is in [Tools & introspection commands](../reference/tools.md).

Where a panel votes, `cs witness` seals. A separate agent reads a sealed prior
file, hashes its bytes, and emits a `SealAttested` event — and refuses if the
witness identity matches the tackler's own session. That refusal is a cheap
structural independence check: it makes "I reviewed myself" fail loudly rather
than silently.

## The honest limit: independence is a spectrum

Cross-examination buys you less than it looks like if every seat is the same
model wearing a different name.

A `deep-think` panel fans out through the harness's own subagent mechanism,
which means every persona is one provider's weights under a different brief.
That gives **channel independence** — separate sessions, separate contexts, no
shared scratchpad — and channel independence is genuinely worth having: it stops
one agent from anchoring on its own earlier sentence. What it does *not* give is
**error independence**. A model auditing itself under a different name shares
its own blind spots. If the failure is one the family does not see, five seats
do not see it five times; they miss it once, together.

The axis that actually buys error independence is provider diversity: pinning a
refuter seat to a different model family, so the reviewer's failure modes are
uncorrelated with the author's by construction rather than by label. Adapters
are pinnable per step, which is what makes such a committee expressible as a
formula rather than a new primitive — see [Agent adapters](./adapter.md).

Worth saying plainly, because the distinction is easy to lose: a panel of five
personas over one provider is a real check against sloppiness and a weak check
against a systematic blind spot. Read a unanimous panel accordingly.

## See also

- [Fleets: many agents, one portal](./fleets.md#cross-examination) — where these
  reviewers run.
- [Formula catalog](../reference/formula-catalog.md) — `deep-think`,
  `deep-think-inline`, `verify-surface`, `visual-qa`, `bug-closure`.
- [Formulas: the only extension point](./formulas.md) — why a review pattern
  ships as TOML and never as a command.
- [Agent adapters](./adapter.md) — pinning a reviewer to a different provider.
- [Tools & introspection commands](../reference/tools.md) — `cs panel`.
- [Integrity & audit commands](../reference/integrity.md) — `cs witness`,
  `cs notarize`.
