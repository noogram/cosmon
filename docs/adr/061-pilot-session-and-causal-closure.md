# ADR-061 — `pilot-session` molecule kind, `nucleon_id`, and the causal-closure invariant

**Status:** proposed (foundational — implementation deferred to sibling tasks)
**Date:** 2026-04-22
**Parent deliberation:** `delib-20260422-f6d6`
**Authoring task:** `task-20260422-b84d`
**Implementation siblings (not covered by this ADR):**
- `task-20260422-b146` — `cs session start | note | end` CLI (carnet append-only)
- `task-20260422-eef5` — `constellation` molecule-kind (méta-connexion typée)
- `task-20260422-1da5` — `cs peek` zoom-continu (UX)
- `task-20260422-01dc` — Matrix-as-transport sub-stream (parallel, non-blocking)

**Related ADRs:**
- ADR-004 — molecule/formula/bead distinction (the composability substrate)
- [ADR-013](013-particle-convergence.md) — MoleculeKind (🧭 joins 💡/🔧/📐/🐛/⚡/🧠)
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — regimes (Inert/Propelled/Autonomous)
- [ADR-030](030-cosmon-archive-model.md) — selective gitignore (markdown artifacts tracked, `state.json` gitignored)
- [ADR-047](047-event-log-protocol-v0.md) — event-log substrate (`events.jsonl` as proof)

---

## Context

Cosmon already knows how to name **what is worked on**: every unit of work is
a molecule. It knows how to reason **with which lens**: Agent (persona). It
knows how to execute **with which body**: Worker. What it does not yet have
is a first-class name for **the cognition that causes a molecule to exist**
— the continuous pilot-consciousness that looks at three existing molecules,
sees a pattern, and decides the fourth is worth nucleating.

Today that cognition lives entirely inside a Claude Code session. Its
artifacts are the compacted session transcript, the operator's short-term
memory, and the mental thread-of-thought between `cs nucleate` calls. None
of it is on disk in `.cosmon/`. When the session compacts, reboots, or is
replaced by a different tool, the cause-of-nucleation evaporates. The
molecule survives; its reason for being dispatched right-then does not.

The parent deliberation (`delib-20260422-f6d6`) asked seven personas
(Wheeler, Einstein, Jobs, Feynman, JR, Torvalds, Niel) to name the gap.
Seven lenses converged on the same proposition: **pilot-cognition is part
of the domain and must be observable from the same referential as the
molecules it causes**. Two personas formalised the claim:

- **Wheeler — "Nucléon."** The verb `nucleate` existed since day one, but
  without a subject. Who nuclées? Until now, the answer was *"an unnamed
  human in an unnamed process."* Wheeler proposes a **Nucléon**: the
  continuous cognitive field that persists across sessions, crashes, and
  tool switches. Typed orthogonal to Molecule, as Agent is orthogonal to
  Molecule.
- **Einstein — "causal closure."** Any cognition that causes a molecule
  must be observable from the same referential (`.cosmon/`) as the
  molecule itself. If a reboot of the operator between two molecules
  produces a different system trajectory, the system is only stateless
  in appearance; a portion of its causal state lives out of scope. That
  is a violation.

The two lenses appear opposed (Wheeler wants a *type orthogonal to
Molecule*, Einstein wants a *new MoleculeKind*), but the synthesis step
of the deliberation converged: both win. The pilot-cognition is persisted
as a **molecule-kind** (so composability is preserved — "everything is a
molecule or a formula"), and the continuity of the same consciousness
across successive sessions is carried by a **`nucleon_id` field** on that
kind. One logical Nucléon; N successive `pilot-session` molecules that
reference it by ID.

This ADR ratifies the vocabulary and the invariant. It does not ship code.
The code lands in the sibling tasks listed above.

---

## Decision

### (1) New molecule kind: `pilot-session` (🧭)

Introduce `MoleculeKind::PilotSession` as a **seventh** molecule kind,
joining `Idea` (💡), `Task` (🔧), `Decision` (📐), `Issue` (🐛),
`Signal` (⚡), and `Deliberation` (🧠).

**Glyph:** 🧭 (compass — the cockpit, the direction-giver, not a process).

A `pilot-session` represents a **bounded episode of pilot cognition**:
from the moment the operator opens the cockpit to the moment they close
it. It carries:

- `prompt.md` — the operator's intention for the session (*"today I look
  at syzygie coherence"*, *"today I ship the pilot-session primitive"*).
- A journal file — the append-only stream of inter-molecule sparks the
  operator notes during the session (the connections seen between mol-A
  and mol-B *before* mol-C is nucleated). The concrete filename is
  implementation-scope (sibling `task-20260422-b146`); this ADR only
  prescribes that such a file must exist and must be append-only.
- `synthesis.md` — optional closing note, written at session end.
- Standard molecule lifecycle (`cs nucleate`, `cs done`). No worktree
  is required (the session is cognitive, not coding) — but one may be
  created if the operator wants a shell to run CLI commands.

A `pilot-session` is an **ordinary molecule** from the substrate's
point of view: it has a state machine, it emits events, it projects onto
surfaces, it honours the briefing-seal contract (§ADR-058/ADR-016).
The only dimension in which it differs from other kinds is the semantic
one — its *content* is "the cockpit was open and here is what happened
in the pilot's head." Composability is preserved: one primitive
(molecule), one extension surface (formula), zero new subsystems.

### (2) New identity field: `nucleon_id`

A `pilot-session` carries a field:

```rust
pub struct MoleculeData {
    // ... existing fields ...
    /// For PilotSession molecules: the identity of the pilot-consciousness
    /// that this session belongs to. None for non-PilotSession kinds.
    pub nucleon_id: Option<NucleonId>,
}
```

Multiple `pilot-session` molecules that share a `nucleon_id` represent
**the same continuous pilot-consciousness across successive sessions**.
The Nucléon is not a separate record in a table; it is a **reference
identity** that pilot-sessions agree on. One Nucléon, N sessions —
Einstein's molecule-kind gets the type, Wheeler's orthogonal-identity
gets the continuity semantics.

`NucleonId` is a newtype over `String`, subject to the same charset
rules as other IDs (ADR standard practice). Assignment policy is
deliberately left open (see §Open questions) — this ADR ratifies the
field's *existence and meaning*, not its *provenance*.

### (3) New typed DAG link: `SparkedBy`

Introduce `MoleculeLink::SparkedBy` with symmetric counterpart
`MoleculeLink::Sparked`. The link is typed, directed, and carries
the usual 1-bit control-plane semantics:

- `Sparked { target: MoleculeId }` — this `pilot-session` sparked the
  creation of `target` during its lifetime.
- `SparkedBy { source: MoleculeId }` — this molecule was sparked by
  the referenced `pilot-session`.

When a molecule is nucleated from inside an open `pilot-session`, the
two links are added symmetrically, analogous to `Blocks` ↔ `BlockedBy`.
The control channel remains 1 bit per edge per lifetime; the content
channel (the cognitive context) remains on the filesystem (the parent
session's journal file).

`SparkedBy` is NOT a dispatch dependency. It does NOT block the child's
`Pending → Propelled` transition, does NOT appear in the Plan reducer's
frontier, does NOT participate in cascade failure (§7d). It is a
**provenance link**, not a scheduling link. It exists to answer one
question, from any molecule: *"which cognitive episode caused me to
be dispatched, and what else was on that pilot's mind at the time?"*

### (4) The causal-closure invariant

Add the following invariant to `docs/architectural-invariants.md`, as
§8e (next free slot in the 8x series), immediately after the existing
§8d on `events.jsonl` as source-of-truth:

> **§8e. Causal closure of the pilot-cognition.** Any cognition that
> causes a molecule — decides it should exist, decides when it is
> dispatched, decides what its briefing should say — must be observable
> from the same referential as the molecule itself, i.e. the `.cosmon/`
> filesystem. If a reboot of the operator between two molecules produces
> a different system trajectory, the system is only stateless in
> appearance; causal state is leaking into an external process memory
> (a Claude Code session, a tmux buffer, a human's short-term memory).
> The substrate-level remedy is the `pilot-session` molecule kind
> (ADR-061): the cockpit becomes a molecule; the journal becomes
> durable; the `SparkedBy` link binds a dispatched molecule back to
> the cognition that dispatched it.

Einstein's thought experiment is included *verbatim* in the
invariants file as the justification paragraph:

> *Imagine the operator is rebooted between two molecules — the last
> 47 minutes of memory are erased, then the operator is asked to tackle
> the next molecule on the backlog. It is done correctly:
> `cs ensemble --tag temp:hot`, choose, `cs tackle`. The molecules
> survive; their state is on disk. But the **next molecule the operator
> would have nucleated** — the one connecting mol-A to mol-B through an
> intuition formed at 14:03 reading the two syntheses side-by-side —
> will never exist. The spark died with the memory. If cosmon were
> truly stateless + fair + typed, this reboot would be indifferent: a
> different operator would resume from the same referential and produce
> equivalent molecules. The fact that this is not the case proves that
> causal state lives outside `.cosmon/`. Session compaction is not an
> ergonomic inconvenience — it is a partial unspecified reboot, and
> it violates closure.*

### (5) Coherence checklist (§5 of invariants) — no new primitives needed

`pilot-session` is a MoleculeKind variant, `nucleon_id` is a field on
`MoleculeData`, `SparkedBy`/`Sparked` are variants of `MoleculeLink`.
All three extend existing domain types; none introduces a new command,
a new daemon, a new state store, or a new lifecycle. The coherence
checklist is passed by construction:

| # | Question | Answer |
|---|----------|--------|
| 1 | Stateless? | Yes — no new command. |
| 2 | Idempotent? | N/A (no new command). |
| 3 | Regime-aware? | Yes — pilot-session is Inert while pending, Propelled if tackled, otherwise dormant like any molecule. |
| 4 | Single perimeter? | Yes — no new command. |
| 5 | Symmetric undo? | Yes — `cs nucleate` ↔ `cs done`/`cs collapse`. |
| 6 | Runtime-compatible? | Yes — a resident runtime sees pilot-sessions as ordinary molecules (L3 may choose not to dispatch them, but that is a policy concern, not a type concern). |
| 7 | Worker/human boundary? | Respected — sessions are typically opened and closed by humans; workers may read their journal for context. |
| 8 | Write/read asymmetry? | Preserved — no command writes and returns a coupling report. |
| 9 | Merge-before-dispatch? | N/A — `SparkedBy` does not dispatch. |
| 10 | CLI-first for workers? | Yes — any worker-side interaction (if any) uses walk-up discovery. |
| 11 | Scope-bounded? | Yes — a pilot-session's state is its own directory; its `Sparked` edges are enumerable. |
| 12 | Self-similar? | Yes — composes at single session, cross-session Nucléon, fleet-of-sessions. |
| 13 | Alphabet-Closure? | **Yes — the spec edit must land with the type edit**. When sibling `task-20260422-b146` adds `MoleculeKind::PilotSession`, `nucleon_id`, and `SparkedBy`/`Sparked` to the code, the corresponding `vars` / `Next` additions in `docs/specs/CosmonRun.tla` must land in the same commit. |

Nothing in this ADR contradicts an existing invariant.

---

## Consequences

### Positive

- **The pilot is on disk.** A verifier replaying `events.jsonl` can see
  not only which molecules were nucleated, but the cognitive context
  (the prior notes in the same session) that preceded each nucleation.
  Syzygie-style cross-galaxy audits (`docs/guides/syzygie.md`) gain a
  new substrate: *"cite the pilot-session under which this work was
  decided"*.
- **Session compaction stops being invisible.** When Claude Code
  compacts mid-session, the `pilot-session` molecule's journal file
  records the history that the transcript lost. The compaction becomes
  a non-event (the journal is durable).
- **Matrix-as-transport becomes evaluable on its own terms.** Once the
  cockpit is typed, the "is Matrix the right substrate for the
  pilot-session?" question (sibling `task-20260422-01dc`) becomes a
  transport-layer question, not a substrate question. The molecule kind
  is the substrate; Matrix is a candidate *channel*.
- **The `constellation` kind gets a foundation.** Sibling
  `task-20260422-eef5` introduces `constellation` (the red-thread
  molecule-kind that connects N existing molecules by a shared pattern).
  Constellations become natural decay products of `pilot-session` —
  a constellation is a synthesis artifact from a session that noticed
  the thread.

### Neutral / accepted costs

- **Wheeler's preferred taxonomy is softened.** He wanted a fourth
  orthogonal axis alongside Molecule / Agent / Worker. The ADR instead
  carries the continuity-of-consciousness via a field (`nucleon_id`).
  This is the synthesis step's resolution — Wheeler loses type-level
  orthogonality, gains identity-level continuity. Accepted.
- **Pilot-sessions add to molecule count.** Surface projections (e.g.
  `STATUS.md`) must decide whether to show pilot-sessions alongside
  tasks/issues. Recommended default: a new surface `PILOT.md` or a
  dedicated section of `STATUS.md`. Decision deferred to
  `task-20260422-b146` (the implementation sibling).
- **`nucleon_id` is nullable.** Non-`PilotSession` kinds leave it
  `None`. The alternative — widening the field to every kind as a
  *"which pilot-session caused me?"* link — is rejected because that
  edge is already carried by `SparkedBy`. One edge, one role.

### Negative (risks)

- **Pilot-session bloat.** If every `cs` invocation opens an implicit
  session, the backlog fills with thin, uninteresting sessions. The
  countermeasure is the same as for any molecule kind: curation via
  `temp-review`, explicit `cs session start` (not implicit), and a
  hard rule that a session with zero journal entries and zero
  `Sparked` edges auto-collapses on `cs done`.
- **The nucleon-id can be forged.** Nothing prevents an attacker (or a
  confused tool) from asserting a bogus `nucleon_id`. This is the
  briefing-seal model (§ADR-058) — detection via `cs verify`, not
  prevention via PKI. Acceptable for the same reason the seal is
  acceptable: the threat model is silent drift, not a motivated
  adversary.
- **Compaction is still lossy.** The `pilot-session` journal captures
  what the operator *chose to note*, not what Claude Code silently
  compacted away. This is by design (the deliberation synthesis
  rejected auto-LLM summarization of the session) — but it must be
  stated honestly: the journal is a discipline, not a recording.

---

## Alternatives considered

### A. Wheeler's pure-orthogonal Nucléon (rejected in synthesis)

Make Nucléon a fourth typed axis next to Molecule / Agent / Worker,
with its own directory (`.cosmon/nucleon/<id>/log.md`) and its own
lifecycle (no terminal state). The deliberation rejected this in §D1
because it violates the composability principle: *"everything is a
molecule or a formula"*. Adding a fourth parallel type splits the
substrate into two kinds of first-class entities, each needing its
own surface-sync, its own event variants, its own reconcile projection.
The `nucleon_id` field on a MoleculeKind variant gives Wheeler the
continuity semantics at a fraction of the cost.

### B. Einstein's raw pilot-session kind without nucleon_id (rejected)

Keep just `MoleculeKind::PilotSession` without `nucleon_id`. Every
session is its own island; cross-session continuity is expressed only
through user convention (naming, tags). Rejected because Wheeler's
core observation still lands: the pilot-consciousness *does* persist
across sessions, and a substrate that cannot name that fact pushes
the continuity back into the operator's short-term memory — i.e., it
re-opens the causal-closure hole the ADR is trying to close.

### C. Teach `cs tackle` to attach a sparked-by edge implicitly (deferred)

Whenever `cs tackle <mol>` runs inside an open pilot-session (detected
via an env var `COSMON_PILOT_SESSION_ID`, symmetric to the existing
`COSMON_PARENT_MOL_ID`), automatically add `SparkedBy { source:
<session-id> }` to the tackled molecule. This is **attractive** and
probably correct, but it is a **CLI behaviour change** and belongs
in the implementation sibling (`task-20260422-b146`), not in a
foundational ADR. Noted here; decided there.

### D. Implement `pilot-session` as a formula, not a kind (rejected)

Since cosmon's extension principle is "formulas, not infrastructure"
(§3g, ADR-057), could this be a formula over existing kinds (e.g.
an `Idea` with a distinguishing formula)? The deliberation synthesis
says no: the pilot-session is not a *way of executing* an idea/task;
it is a *different semantic category*. Surface projection differs
(PILOT.md vs IDEAS.md), interaction rules differ (a pilot-session
cannot decay into tasks — it *sparks* them), and the `nucleon_id`
has no home on a generic `MoleculeData`. A new kind is the right
shape. The six existing kinds are not sacred.

---

## Scope and non-scope

**In scope (this ADR).**
- Naming the kind (`pilot-session`, 🧭).
- Naming the identity field (`nucleon_id`).
- Naming the DAG link (`SparkedBy` / `Sparked`).
- Formalising the causal-closure invariant for the invariants file.
- Proving the coherence checklist is satisfied.
- Listing the open questions that subsequent implementation ADRs or
  tasks must resolve.

**Out of scope (this ADR).**
- Writing Rust code for `MoleculeKind::PilotSession`. That is
  `task-20260422-b146`, together with the TLA+ spec update.
- Defining `cs session start | note | end` CLI behaviour. That is
  also `task-20260422-b146`.
- The `constellation` molecule kind. That is `task-20260422-eef5`.
- The `cs peek` zoom-continu UX. That is `task-20260422-1da5`.
- The Matrix-as-transport debate. That is `task-20260422-01dc`.
- Any UI or dashboard rendering of pilot-sessions. Deferred to the
  implementation sibling + a follow-up if needed.
- A decision on whether pilot-sessions get their own surface file or
  a section of an existing one. Deferred to `task-20260422-b146`.

---

## Open questions (deferred to implementation)

These questions are **acknowledged, not answered**, by this ADR. Each
is load-bearing for an implementation, but each can be resolved without
disturbing the vocabulary or invariant established here.

1. **How is `nucleon_id` assigned?** Three candidates:
   - (a) Deterministic from `git config user.email` + machine hostname,
     producing a stable-per-(operator, machine) Nucléon. Simple, no
     config file, works out of the box.
   - (b) A UUID stored in `~/.config/cosmon/nucleon`, created on first
     `cs session start`, optionally copied across machines by the
     operator for true cross-device continuity.
   - (c) Operator-chosen via `cs session start --nucleon <name>`,
     defaulting to (a) or (b). Maximum flexibility, minimum automatic
     behaviour.
   Recommended default: **(b)**, with an `--override` flag. Decided
   in the CLI-implementation ADR that ships with `task-20260422-b146`.

2. **Does a worker's molecule get `SparkedBy` automatically when
   nucleated from within a `cs session`?** Proposed: **yes**, by analogy
   with the existing `COSMON_PARENT_MOL_ID` env-var contract. When a
   `cs session` is active, an env var `COSMON_PILOT_SESSION_ID` is set;
   `cs nucleate` reads it and adds the symmetric `SparkedBy`/`Sparked`
   links automatically. Decided in `task-20260422-b146`.

3. **Should the regimes (Inert / Propelled / Autonomous) be extended
   to describe whether a pilot-session is open?** Currently the regimes
   describe a (molecule, observer) relationship. A "cockpit open / cockpit
   closed" binary is arguably a separate axis — a meta-regime. Proposed:
   **no**, for now. A pilot-session is an ordinary molecule and inherits
   the regime of its state machine (Pending → Propelled → Completed).
   The cockpit-open/closed distinction is a *property of a specific
   molecule kind*, not a new regime of the substrate. Revisit if a
   future feature requires cross-regime cockpit semantics.

4. **Does `pilot-session` get its own surface projection?** Two options:
   - A dedicated `PILOT.md` in the repo root, symmetric to `IDEAS.md`
     and `DELIBERATIONS.md`.
   - A section of `STATUS.md`, since pilot-sessions are
     substrate-level observations about the current operating state.
   Deferred. A decision is only needed when `task-20260422-b146`
   lands; the ADR is content with either outcome.

5. **Can a `pilot-session` decay into `constellation`(s)?** Proposed:
   **yes**. A session that notices three red threads can decay into
   three constellations at `cs done`. The `MoleculeKind::can_decay`
   table should include `PilotSession → [Constellation, Task, Idea]`.
   Decided jointly with `task-20260422-eef5`.

6. **Sealing.** `pilot-session`'s `prompt.md` and journal should be
   briefing-sealed (ADR-058) so retroactive edits are detectable. Is
   an append-only file the same briefing seal (hash of the whole file),
   or a chain of per-entry seals? Deferred to the implementation; the
   ADR ratifies that the seals *must exist*, not their precise form.

---

## Invariants file — surgical change

`docs/architectural-invariants.md` gains a cross-reference item in
§1 (the two-layer model) or the document's top matter, pointing to
this ADR, and a new subsection §8e with the full invariant text and
the Einstein thought-experiment paragraph verbatim. The edit is
applied in this same commit; the cross-reference uses a relative
link to keep the document self-contained.

---

## Acceptance

This ADR is **proposed**, not **accepted**. Operator ratification is
the explicit next step (acceptance happens when the operator reviews
this file and either merges it unchanged, requests edits, or rejects
it in favour of a different synthesis). Until ratified:

- Sibling tasks `b146` / `eef5` / `1da5` / `01dc` MAY proceed with
  scoping and briefing, but MUST NOT land code that introduces
  `MoleculeKind::PilotSession`, `nucleon_id`, or the `SparkedBy`
  link until this ADR is marked `accepted`.
- The causal-closure invariant is **drafted** in
  `architectural-invariants.md` but flagged as `(proposed — ADR-061)`
  so no downstream code treats it as a hard rule prematurely.

Wheeler's closing line from the deliberation is this ADR's motto:
*"le verbe nucleate existait depuis le premier jour ; il lui
manquait son sujet."* The ADR gives it the sujet.
