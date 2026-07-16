## ADR-143 — CMB diagnosis-verify gate: a relayed diagnosis is a hypothesis, not a fact (companion to ADR-091)

**Status:** proposed
**Date:** 2026-07-05
**Decider:** Noogram
**Authoring task:** `task-20260705-0e3f` (🔧 task, topic `adr-cmb-diagnosis-verify-gate`)
**Source finding:** the 2026-07-05 showroom → cosmon CMB
(from-showroom, model-routing flag ignored)
— the first CMB in the field to carry a **causal diagnosis** of a cosmon-side
defect (the `cs tackle --model` pin being ignored), authored by a sibling
galaxy's session that does not hold cosmon's source as ground truth.

**Binds / extends (touches no load-bearing decision of either):**
ADR-091 (CMB handoff substrate — this ADR adds
one epistemic class to its content taxonomy),
ADR-094 (multi-file CMB companion).
**Kin doctrine:** the *trace-not-a-lock* philosophy of
`docs/architectural-invariants.md` §8b (propose mechanisms of verification, do
not impose them) and the **cosmon-ward feedback flow** in `CLAUDE.md` (a
sibling galaxy that finds a cosmon-level pathology surfaces it back — this ADR
governs what the *receiver* does with that surfaced claim).

## Context

ADR-091 inscribed the CMB (Cosmic Microwave
Background) substrate: a markdown note deposed by a closing session into a
sibling galaxy's CMB inbox, read by the next session as its first
action to recover the *living frame*. ADR-091 named the content it carries as
**frame / patch / trace** — "what to read first, what is in flight, what the
operator prefers." Every one of those classes is **descriptive** and
**advisory**: if the reader mis-reads it, the frame is simply re-derived from
disk the slow way. The blast radius of a wrong frame note is a few wasted
minutes, and it is self-correcting — nothing is committed on its authority.

On 2026-07-05 a CMB of a **new epistemic class** landed. The showroom session
did not relay a frame; it relayed a **diagnosis** — a causal claim about a
defect in *cosmon's own code*:

> "`cs tackle --model claude-fable-5` **n'épingle pas** le worker … la seule
> origine restante : cosmon lit le **modèle effectif de la session parente** et
> l'injecte, en **écrasant le flag**. `resolve_worker_model` renvoie (ou est
> court-circuité par) le modèle parent au lieu de la valeur `--model`."

The note is careful, honest, and even flags its own uncertainty
(*"Hypothèse à confirmer"*). But it names a **probable locus**
(`crates/cosmon-cli/src/cmd/tackle.rs`, `tackle_env`, `resolve_worker_model`)
and a **stated mechanism** (parent model actively injected, overriding the
flag). A diagnosis is not a frame: acting on it **writes a commit**. That is the
qualitative jump ADR-091 did not anticipate, and the reason this class needs
its own intake discipline.

## The load-bearing distinction: symptom vs. mechanism

A cross-galaxy diagnosis is authored from **outside** the receiving galaxy's
source tree. The sender can observe only what its own boundary exposes:

- **The symptom is trusted evidence.** The sender ran the command and read the
  worker's env with `ps eww`; it *saw* `ANTHROPIC_MODEL=claude-opus-4-8` where
  it asked for `claude-fable-5`. That observation is real, reproducible, and the
  sender is the authority on it. The symptom is the load-bearing gift of the CMB.
- **The stated mechanism is an unverified hypothesis.** "Cosmon injects the
  parent model, overriding the flag" is the sender's *inference* about code it
  does not own and cannot run under a debugger. It carries no more authority
  than any first guess — its confident, detailed prose notwithstanding. In fact
  detail *raises* the persuasive weight without raising the truth-probability;
  that asymmetry is the hazard (kahneman: a fluent, specific story is believed
  past its evidence).

The receiver's temptation is to conflate the two: to treat the whole note as
one trusted artifact and "go fix the locus it named." That conflation is the
defect this ADR closes.

## Founding witness — the mechanism did not survive a code read

The strongest possible evidence that the mechanism must be verified separately
from the symptom is that, for this very CMB, **it was wrong**. A static read of
`crates/cosmon-cli/src/cmd/tackle.rs` at HEAD shows the `--model` flag *is*
threaded with the highest precedence, and there is **no parent-model source** in
the resolution chain:

```
args.model                                   (the --model flag)
  → resolve_model_selection(args.model, …)   (flag is arg #1, top precedence)   [tackle.rs ~560]
  → preferred_model
  → resolve_worker_model(preferred_model, …) (probes, then returns the pin)     [tackle.rs ~3340]
  → effective_model
  → .env("ANTHROPIC_MODEL", effective_model) (the value injected into the worker) [tackle.rs ~3358]
```

Nothing in that path reads "the effective model of the parent session." The
CMB's stated mechanism — *cosmon reads the parent model and overrides the flag*
— describes a code path that **does not exist**. A pilot who took the diagnosis
verbatim and "made the flag win over the inherited parent model" would have
edited code that already behaves as specified, shipped a non-fix, and closed the
molecule green while the field symptom persisted.

The **symptom is still real**: the worker did get `opus-4-8`. The true cause
therefore lies where the CMB did *not* look — most plausibly the probe /
fallback inside `resolve_worker_model` → `cosmon-core::model_chain`
(`decide_worker_model`), where a pinned model that is unreachable under the
worker's Claude account falls back down a chain that can resolve to a different
model. Pinning *that* down requires **reproduction**, not a code read and not
trust in the relayed guess. Which is exactly the point.

(This ADR does **not** fix the model-routing defect. The CMB itself scopes the
fix as an operator-piloted system-state gesture — rebuild + install `cs` — to be
walked through `bug-closure`. This ADR extracts only the *doctrine* the incident
illuminated. The fix is a separate molecule.)

## Decision — the diagnosis-verify gate

When a CMB (or any relayed handoff artifact) carries a **diagnosis** — a causal
claim that some defect in the *receiving* galaxy is caused by X at locus Y — the
receiving galaxy MUST, before committing any change that acts on it:

1. **Trust the symptom; reproduce it if cheap.** The observed behaviour is the
   sender's authoritative contribution. Where reproduction is inexpensive,
   reproduce it first — a symptom you cannot reproduce is the first finding, and
   may itself be environmental (cf. the *mobilité / coupures réseau* rule: a
   one-off symptom is not yet a structural bug).
2. **Treat the stated mechanism and locus as an unverified hypothesis.** Grant
   them no authority beyond "a good place to start looking." Detail and
   confidence in the prose do not raise their truth-probability.
3. **Verify the mechanism against your own code before writing a fix.** Read the
   named path; confirm (or refute) that the causal chain the CMB describes
   actually exists; reproduce the defect down to the line. Only a confirmed
   causal path licenses a commit.
4. **When verification contradicts the CMB, follow the code, not the note — and
   record the divergence cosmon-ward.** Write back (a return CMB, or a note on
   the source molecule) so the sender learns its inference was off. Silent
   divergence is a bug in both directions (same rule as silent-ignore).

The gate is a **cognitive checkpoint in CMB intake**, not an automated code
gate. This is deliberate and load-bearing: a causal claim about arbitrary source
**cannot be machine-verified** — the only verifier is reproduction plus a human
(or worker) reading the code. Per §8b, the gate *proposes* verification; it does
not pretend to *enforce* it. It catches the lazy "trust the confident note and
patch the named locus," not a determined self-deception.

It is the natural **front half of `bug-closure`**: the CMB's own companion
suggestion was to walk the fix through `bug-closure` (help/man, resolver, env
injection, isolation tests, the invariant, the `--help` resolution order). The
verify gate is the step *before* that walk — confirm the bug is where the CMB
says before you walk the surface, or you will walk the wrong surface thoroughly.

### Scope — what the gate does NOT touch

- It does **not** distrust the sender, and it does **not** tax the descriptive
  classes. Frame / patch / trace content (ADR-091 §3–§4) stays advisory-as-is;
  its blast radius is minutes and it is self-correcting. Only the **diagnostic**
  class carries the gate, because only it drives a commit on a claim the sender
  could not verify.
- It does **not** slow the CMB channel's core value (fast frame recovery). The
  gate fires on exactly one content shape, recognisable by a single test: *does
  acting on this note write a commit against a causal claim?* If no, no gate.
- It adds **no new frontmatter field, no new directory, no blocking hook** —
  honouring ADR-091 §6's seven forbidden anti-roles. The net normative content
  is the four-step Decision above.

## Options considered

**Option A (chosen) — intake discipline: verify the mechanism before acting.**
Trust the symptom, verify the cause, follow the code on divergence, write back.
Cheap (four sentences of doctrine), fires on one recognisable shape, leaves the
descriptive channel untouched.

**Option B (rejected) — trust relayed diagnoses as authoritative.** "The note is
detailed and field-observed; act on the locus it names." Rejected by the
founding witness itself: the stated mechanism described a nonexistent code path;
acting on it verbatim ships a non-fix and, worse, closes the molecule green
while the symptom lives. Trusting the mechanism erodes the channel — the first
wrong-and-acted-on diagnosis teaches everyone to distrust *all* CMB, killing the
frame-recovery value too.

**Option C (rejected) — distrust all CMB content, re-derive everything.**
Over-broad. Frame / patch / trace content is low-stakes and self-correcting; a
blanket verify-tax on every relayed note would negate ADR-091's whole reason to
exist (one-read frame recovery). The gate must fire on the diagnostic class
*only*.

**Option D (rejected) — build an automated diagnosis-verifier gate in code.** A
tempting symmetry with the other gate-primitive ADRs (128 / 129 / 134). But a
causal claim about arbitrary source is not machine-checkable; the only verifier
is reproduction. A script that "checks a diagnosis" would be a **false lock** — a
green light with no evidence behind it, the exact anti-pattern §8b warns against.
The gate is honestly cognitive, not dishonestly automated.

**Decision Outcome:** Option A. Options B, C, D rejected by name above; rationale
recorded for `INV-ADR-OPTIONS-CONSIDERED` conformance (ADR-082).

## Consequences

- The CMB content taxonomy gains a fourth, epistemically-distinct class:
  **diagnosis** (causal, commit-driving, verify-gated) alongside frame / patch /
  trace (descriptive, advisory). A CMB author who relays a diagnosis should
  separate the **symptom** (evidence) from the **mechanism** (hypothesis) in the
  note itself, so the receiver can trust the former and gate the latter — the
  2026-07-05 showroom note already does this well (a dedicated "Le diagnostic"
  section distinct from "Repro exact"), and is the template.
- Cross-galaxy bug reports become first-class but **non-binding**: a sibling
  galaxy can surface a cosmon defect (cosmon-ward flow) without its inference
  becoming load-bearing on the receiver. The receiver owns the cause.
- The write-back on divergence (step 4) closes an epistemic loop: senders learn
  which of their diagnoses held, calibrating the confidence of future CMB.

## Tattoo

**A relayed diagnosis is a bug report, not a patch.** The CMB names *where to
look*, never *what is true* — reproduce the symptom, re-derive the cause.
