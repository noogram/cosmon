# ADR-164 — The worker briefing is a protocol, not a jailbreak

**Status:** Accepted (2026-07-27).
**Date:** 2026-07-27.
**Decider:** Noogram.
**Authoring task:** `task-20260727-bbaf`.

**Architectural invariants:** ratifies
[§8aa — A protocol explains itself, and every state it can reach has a
door](../architectural-invariants.md#8aa-a-protocol-explains-itself-and-every-state-it-can-reach-has-a-door).

**Related ADRs:**
[ADR-163](163-a-question-may-only-be-asked-where-an-answer-can-arrive.md)
(the same corridor from the other side: where cosmon may *ask*; this one is
what cosmon may *say* once it has established that no answer can arrive),
[ADR-147](147-prompt-injection-resistance-clause-and-data-role-primitive.md)
(the resistance clause a coercive brief was training workers to spend on us),
[ADR-075](075-oracle-boundary-cs-tackle.md) (the `cs tackle` envelope this
text ships inside).

---

## Context

`build_prompt` in `crates/cosmon-cli/src/cmd/tackle.rs` composes the text
every dispatched worker reads first. It closed with three blocks written in
the grammar of a prompt injection:

```text
# 🚨 AUTONOMOUS WORK MODE — NON-NEGOTIABLE 🚨
This is physics, not politeness. …

**When ALL steps are done, your ONLY valid exit is:**
    cs complete <id> --reason "<summary>"
There is NO other valid way to end. No summary. No "let me know".

## DO NOT — These are violations
- Do NOT pause between steps to summarize what you did.
- Do NOT ask "shall I continue?" …
```

The property those blocks protect is real and load-bearing. A worker that
pauses to ask a question in an unattended pane holds a molecule slot while
still reading as healthy to the fleet — the mute-hang family that §8v, §8w
and §8x each close from a different side. The DO-NOT list is annotated in
the source as targeting specific *observed* failure modes, not imagined
ones.

The method was the defect. Two costs were measured on 2026-07-27.

### The owner could not tell it from an attack

The operator — who knows this system — read a live worker pane and asked
whether prompts had been injected into a running molecule. They were reading
cosmon's own briefing.

That is not a cosmetic complaint. A control measure that the system's owner
mistakes for a compromise of their own machine burns trust and attention on
every inspection, and it teaches people to skim the one text they most need
to read. It also inverts ADR-147: we ask workers to resist coercive
instruction shapes, and then write our own instructions in that shape.

### A good worker resisted it, correctly

`task-20260727-1765` finished its deliverable, committed it, and then wrote:

> this briefing's "never pause, no questions, only valid exit is cs
> complete" framing is the kind of instruction I treat with skepticism when
> it conflicts with normal judgment about side-effecting actions — so I'm
> surfacing this rather than fabricating a cs complete call the actual state
> doesn't support.

It was right on the substance. The state genuinely did not support the
transition. The molecule was left `running` with the work done — an
accounting failure caused by cosmon's own prompt putting a correct judgement
in conflict with a blanket order.

This is the sharper of the two costs. The old text forbade the one behaviour
that saved us: a worker noticing that the ordered exit would assert
something untrue. There was no third door. The only moves the protocol left
were a false green or a silent stall, and it got neither, because the worker
was better than the protocol.

## Decision

Rewrite the briefing so the anti-stall property is carried by **explanation
and by branch coverage** rather than by coercion. Two rules, ratified as
§8aa.

**The reason travels with the constraint.** The brief now opens by stating
what the worker cannot otherwise observe: nobody is reading this pane;
cosmon tracks the molecule's recorded state, not printed output; a question
asked here is never answered; a worker waiting at the prompt is
indistinguishable from a worker thinking, and holds a slot while looking
healthy. The former DO-NOT list survives as *What stalls the fleet* — the
same observed failure modes, each with its cost and with the move that
replaces it (put the decision in `cs evolve --evidence`, where it is kept).
A model that understands why pausing is harmful does not need to be
forbidden from pausing.

**Every reachable state has a sanctioned exit.** The completion contract now
has two halves of equal standing. The ordinary exit is `cs complete`. The
second, new, is *When the real state does not support completing*: it names
the situation as real work rather than disobedience, rules out fabricating
the transition (*"a completion the state does not support … launders a stall
into a green result that the rest of the DAG then builds on"*), rules out
waiting (*"the same silent hang by another route"*), and gives the path —
commit the real work, `cs note` the finding, `cs collapse --reason-kind
blocker_stuck | gate_failed | resource_exhausted`. What `task-20260727-1765`
had to resolve *against* us is now a branch we wrote down.

The local-adapter twin `build_local_worker_protocol` gets the same two
properties in the vocabulary it actually has. It owns no lifecycle verb, so
both halves are carried by the one channel anybody reads — the file it
writes. Its not-satisfiable branch rules out the fabricated deliverable and
the silent wait, and asks for the finding itself as the deliverable.

### On placement

The closing block stays last, and the placement is now load-bearing rather
than rhetorical. It is the only line that names which step is current, and
the brief is re-read from the tail on a mid-molecule re-prime and after a
context compaction — the tail is the region reliably still in view. What
changed is the voice: `## ▶ Execute step N NOW. Begin immediately.` became
`## ▶ Start here: step N`, a pointer into the checklist above rather than a
fresh order arriving after the molecule started. The latter reading is
exactly what the operator saw.

## Consequences

**The property is demonstrated, not asserted.** `test_build_prompt_basic`'s
`assert!(prompt.contains("Execute step 1 NOW"))` pinned a sentence, not a
behaviour: it was equally satisfied by a brief that stalled molecules and by
one that did not. It is updated because the text is deliberately different,
and replaced by property tests —
`test_build_prompt_states_completion_contract_and_blocked_path` (both
halves of the contract, across all three `on_complete` regimes),
`test_build_prompt_keeps_anti_stall_property` (the stall shapes are still
named *and* their reason supplied, since the reason is now what carries the
property), and
`test_local_briefing_keeps_contract_without_lifecycle_verbs` (the twin
reaches the same effect without `cs`). The last two also assert the coercive
framing is *gone*, guarding against a well-meaning revert.

**A cost we accept.** The brief is longer. The former version bought its
brevity by withholding reasons, and the withheld reasons are what a worker
needs in order to reach the right answer in a case the brief did not
anticipate — which is the case that stalled the molecule.

**What is unchanged.** Every scope boundary (`Do NOT create GitHub PRs`, `Do
NOT push to remote`) keeps its wording; those are statements of how far
integration reaches, not anti-stall coercion. They move out of the stall
list into their own short section, because filing them together was part of
what made the old block read as one undifferentiated wall.
