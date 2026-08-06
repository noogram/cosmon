# ADR-171 — A cockpit is a command surface, not a sixth viewport

**Status:** Accepted (2026-08-06).
**Date:** 2026-08-06.
**Decider:** Noogram.
**Authoring molecule:** `task-20260804-222e` — deliberation, doc-only.
**Idea of origin:** `idea-20260803-313f` (cockpit de pilotage), steps
`idea.md` / `feasibility.md` / `plan.md`.
**Unblocked by:** `task-20260731-bd92` (M7 dogfood shadow). The demarcation
below rests on M7's **measured** friction list (its §8, eleven items), not on
a guessed one — the 2026-08-03 deliberation named guessing that list as the
failure mode to avoid.

**Scope:** doc-only. **This ADR changes no CLI surface, no flag, no output
byte, and writes no UI.** It answers three questions that block the surface
canon and the cockpit project itself, and it files what each answer requires.

**Binds:** ADR-023 (cockpit = hexagonal read surface, subprocess write path),
ADR-066 §8k′ (cross-surface wheat-paste), ADR-068 §8l/§8m (UX ↔ CLI parity and
bidirectional revelation), ADR-080 §8p (API ⊊ CLI), ADR-168 (co-pilot inherits
the session substrate).

---

## 0 · The three questions, and why they are answered separately

The mission asks three atomic questions and forbids bundling them. They are
kept separate below because they fail separately: §1 could be wrong while §2
is right, and §3 is a naming decision that binds whatever §1 and §2 decide.

1. **What class of surface is a cockpit** under §8k′?
2. **What becomes of §8l** when the peer surface lives in another repository?
3. **What are the two things called `session`** actually called?

Each section states the measured facts, the decision, what the decision
costs, and the falsifier that would refute it.

---

## 1 · Question 1 — the cockpit's class under §8k′

### 1.1 · Measured

- §8k′ is **proposed**, not ratified
  (`docs/architectural-invariants.md:1479`, *Status: Proposed (ADR-066)*).
  Its own text says: *"Until ratified, the rule applies to new surfaces only;
  existing surfaces are grandfathered."*
- The existing SwiftUI apps are **in structural breach**, by §8k′'s own
  admission — `mac-pilot/PilotView.swift` and `ios-pilot/ContentView.swift`
  use `TabView`, `Label(systemImage:)`, `RoundedRectangle`. A remediation bead
  exists; it has not been done.
- §8k′'s decidable test is superposition: *"a screenshot of surface A and a
  screenshot of the same molecule on surface B overlay glyph-for-glyph, modulo
  crop and tint."* That test is the **only** mechanical thing in §8k′; the rest
  is a prohibition list.
- Axis 2 of the operator vision asks literally for *"interactive graphical
  equivalents of `cs peek` / `cs ensemble`"* and an *"execution DAG"*. §8k′
  names `force-directed graphs` in its MUST NOT list. The collision is frontal.

### 1.2 · Decision — (b), narrowed

**A cockpit is a command surface: a class distinct from an observation
surface, with its own invariant (§8ac below). §8k′ is not amended, weakened,
or grandfathered further. It keeps its full force wherever a canonical raster
exists.**

The narrowing is what makes (b) safe, and it is the actual content of this
decision. (b) as posed in the idea document reads *"§8k′ is amended: it governs
state-observation surfaces, and the pilot cockpit is declared another class"*.
Declared wholesale, that answer hands the cockpit a blanket exemption and §8k′
dies by attrition — falsifier F4 of the idea document, *the canon has diverged
in silence*. So the class boundary is drawn per **view**, not per application:

> **A view of the cockpit that renders state for which `cs` emits a canonical
> raster is a §8k′ viewport, without exception.** `peek`, `ensemble`,
> `sensorium`, `pulse`, `health` — these are wheat-pasted, glyph-for-glyph, and
> the superposition test applies to them unchanged.
>
> **Only a view that renders state for which no canonical raster exists** —
> the interactive DAG, the pilot-session timeline, the instruction-to-molecule
> lineage — is of the command-surface class, and it is governed by §8ac.
>
> **The boundary moves in one direction only.** If `cs` later emits a canonical
> raster for a view held by §8ac, §8k′ reclaims that view at the next release.
> §8ac never reclaims from §8k′.

Answer (a) — *the DAG enters the canon as a deterministic ASCII render, and we
lose interactivity* — is rejected on a measured ground and not on taste:
`cs peek` **already is** an interactive ASCII surface (three zoom scales, `j/k`,
`+/-/=`, `A` cycles unfinished→all), so "ASCII" and "interactive" are not the
opposed pair the question implies. What (a) actually costs is different and
worse: it puts the DAG layout algorithm inside the byte-deterministic canon,
which means every layout change is a canon change, gated by a golden-snapshot
test, shipped in the `cs` release train. A view whose whole value is *fast
iteration on what to show* is the worst possible thing to nail into a
byte-determinism contract.

Answer (c) — *out of canon, does not show peek* — is rejected because it
empties axis 2, which the question itself states.

### 1.3 · What (b) costs, and what replaces it

The cost named in the mission is real and is not waved away: **the cockpit's
§8ac views lose the superposition test, which is the only decidable test the
canon has.** A class without a decidable test is a preference.

So §8ac ships with one, and it is decidable from a bare clone of the cockpit
repository plus a `cs` binary. It is not a pixel test — it is a **vocabulary
containment** test, which is the property superposition was actually
protecting (§8k′ forbids *"re-rendering the same state in a different visual
vocabulary"*; the underlying hazard is the surface acquiring meaning of its
own — falsifier F3, *the UI has learned the domain*).

> **§8ac. A command surface adds no vocabulary.** *(proposed — ADR-171)*
>
> A **command surface** is a cosmon-facing surface that renders state for which
> `cs` emits no canonical raster, and that translates operator gestures into
> `cs` invocations. It is bound by four clauses:
>
> 1. **Containment.** Every name a command surface displays as domain
>    vocabulary — every field label, status word, enum value, edge kind, tag,
>    role, phase — is an element of the set emitted by `cs … --json` and
>    `cs peek --snapshot`. The surface MAY drop names, reorder them, and
>    translate them into a declared human language via a checked-in
>    dictionary keyed by the canonical token. It MUST NOT introduce a name of
>    its own, and MUST NOT compute a domain value from two others.
> 2. **Derivation.** Every state-bearing region of a command-surface view is
>    attributable to a `cs` invocation and a byte range of its output, and the
>    surface can emit that attribution on demand (the §8m *Reveal CLI*
>    affordance, applied to a region rather than to an action).
> 3. **No write path.** A command surface never writes under
>    `.cosmon/state/`. Its only write path is a `cs` subprocess or a
>    cosmon-served endpoint that shells out to one — ADR-023 §2, unchanged.
> 4. **Reversion.** Where a canonical raster exists for the state a view
>    renders, §8k′ governs that view and §8ac does not apply.
>
> **Test of legitimacy.** Extract the domain-vocabulary token set of the
> surface (its view-model field names and enum values, its label dictionary
> keys) and the token set emitted by cosmon's JSON schemas and snapshot
> alphabet. The first must be a **subset** of the second. A token in the
> surface and not in cosmon is a breach: the surface has learned something
> `cs` does not know. This is decidable, mechanical, and — unlike
> superposition — computable in CI without a screenshot, a simulator, or a
> retina.

Clause 1's *"MUST NOT compute a domain value from two others"* is the clause
that does the work. A cockpit that displays `stalled` because it noticed a
worker heartbeat is old has invented a domain predicate; `cs health` owns that
predicate, and the cockpit must call it. This is the same boundary ADR-023
drew for writes, applied to reads.

### 1.4 · Falsifier

**F1-171.** If, six months out, a `§8ac` view exists whose state *does* have a
canonical raster and no bead records the reversion, clause 4 is decorative and
(b) has become the blanket exemption it was narrowed to avoid. Measurement:
enumerate §8ac views, diff against the `cs` raster inventory.

### 1.5 · Consequence — §8k′ must be ratified or dropped

This decision makes §8k′ load-bearing for a set of views that will actually be
built, so its *proposed* status stops being harmless. Either it is ratified —
which forces the `mac-pilot` / `ios-pilot` remediation bead that ADR-066 has
been carrying since 2026-04 — or it is withdrawn and §8ac stands alone. A
canon whose only two consumers are in acknowledged breach is not a canon.
**Filed as a child molecule; not decided here** (this deliberation was not
asked to ratify ADR-066).

---

## 2 · Question 2 — §8l when the peer lives in another repository

### 2.1 · Measured, 2026-08-06

This section is the file the mission asked to be entered into the record.

- `docs/guides/ux-cli-parity-audit.md` **is not in the repository, and
  `git log --all -- '*ux-cli-parity-audit.md'` returns nothing** — it is absent
  from the working tree *and* from every ref of this repository's history. It
  lives in the knowledge galaxy at
  `~/galaxies/knowledge/cosmon/guides/ux-cli-parity-audit.md`, added there by
  `e1f9efa relocate(cosmon): internal docs/guides` (2026-07-14) and last
  amended by `f8d1682`. Two sibling guides relocated in that same commit
  (`adapter-parity-bar.md`, `cluster-views.md`, `blink-sideload.md`) show the
  identical signature — absent from the tree, zero commits in cosmon — while
  guides that stayed (`api-cli-coverage.md`) carry normal history. So this is
  the relocation's footprint, not a hole in the log.

  *This is sharper than the brief stated, and it changes what §8l is.* The
  invariant does not cite a file that drifted out of reach; it cites a file
  this repository has no record of ever holding, while ADR-068's own scope
  section lists *"producing the audit guide `docs/guides/ux-cli-parity-audit.md`"*
  as a deliverable. Whatever the sequence, the state today is: the deliverable
  is not here, and §8l's rule text links to where it is not.
- **Six live references still point at the missing path**, three of them from
  the invariants file itself, which is where §8l names its own instrument:
  - `docs/architectural-invariants.md:1873` — inside the quoted rule of §8l,
    as a Markdown link: *"The mapping is enumerated in
    `docs/guides/ux-cli-parity-audit.md`"*. **Broken link.**
  - `docs/architectural-invariants.md:1983` (§8m), `:2185` (§8p).
  - `docs/adr/068-ux-cli-equivalence.md` (×4), `docs/adr/078` (×1),
    `docs/guides/README.md` (×1), `docs/guides/api-cli-coverage.md` (×2 —
    including a Markdown link that resolves to nothing).
- The moved file, read at its current location: **32 CLI verb rows, all 32
  carrying ❌ in at least one UI column**; 10 rows carrying `TBD` in the API
  column. *(The mission's brief said "35 red/TBD lines"; the measured count of
  verb rows is 32, all red. The correction does not change the argument — it
  strengthens it: coverage is not partial, it is nil.)*
- The rows for exactly the views the cockpit wants: `cs peek` (l. 45),
  `cs peek --all` (46), `cs health` (47), `cs pulse` (48), `cs sensorium` (50),
  `cs ensemble` (51) — every one of them ❌ ❌ ❌.
- Header of the moved file: *"Status: v0 (snapshot of 2026-04-23, API column
  added 2026-04-27)"*. Last modified 2026-07-19.

Three facts follow, and they are what decides the question:

**Fact one — §8l is currently unenforceable and also unreadable.** Its rule
text cites an instrument that a reader of this repository cannot open. An
invariant whose instrument is a 404 is not a weak invariant; it is a sentence.

**Fact two — §8l names the wrong peer.** Its text is written against *"the
native pilot apps (mac-pilot, ios-pilot)"*, which `idea-20260803-313f` §3.1
measured as inert: `apps/` untouched since 2026-07-29 (a release chore) and
before that 2026-07-17. The bijection is stated against a peer nobody edits.

**Fact three — the bijection was never a bijection.** 32 of 32 rows red means
one half of §8l — *"every user-facing CLI verb has at least one UI
counterpart"* — has held for zero verbs since the audit was written. Only the
other half (*"every UI control has a CLI counterpart"*) has ever been true, and
it is true trivially, because the apps are shells over `cs`.

### 2.2 · Decision — amend §8l, and repatriate the registry, not the narrative

Neither "repatriate the audit" nor "replace it with a build-gated surface
canon" alone survives the facts. The decision is the third option, **amend**,
with a specific amendment that makes the first option's useful half mandatory:

**(i) §8l stops naming a peer and starts naming a *declared* peer.** Replace
*"the native pilot apps (mac-pilot, ios-pilot)"* with *"every surface declared
a parity peer in the registry"*. A repository outside cosmon can be a declared
peer; so can an app inside `apps/`; so can none, in which case §8l holds
vacuously on the UI side and says so out loud instead of pretending.

**(ii) The instrument splits in two, and only one half comes home.**

| | Lives | Content | Gated by |
|---|---|---|---|
| **Registry** | **in this repo**, `docs/guides/ux-cli-parity-registry.md` | one row per user-facing `cs` verb; per declared peer, a coverage cell; the bead closing each gap | cosmon CI |
| **Narrative** | stays in `~/galaxies/knowledge/cosmon/guides/ux-cli-parity-audit.md` | the prose, the design rationale, the priorisation | nothing — it is a document |

The registry is repatriated because **§8l's rule text links to it**, and a
repository must be able to resolve its own invariants from a bare clone — the
same argument CLAUDE.md makes for why `publish.sh --check` covers only what is
decidable from a fresh clone with git and python3. The narrative is not
repatriated, because the 2026-07-14 relocation put it there for a reason this
ADR does not reopen — internal guides are internal. What comes home is only
the part an invariant of this repository is obliged to be able to resolve.

**(iii) The bijection becomes two half-gates with a named seam.** This is the
part that answers *"a cosmon CI cannot gate an external repository"* honestly
instead of wishing it away:

- **Half-gate A — enumeration completeness. Gated in cosmon CI, hard.**
  Enumerate the user-facing verbs of the clap tree; assert every one has a row
  in the registry. Decidable from a bare clone with `cs --help` and nothing
  else. This catches the actual recurring breach (a verb ships and no row is
  filed) and it catches it in the repository where the verb was added.
- **Half-gate B — implementation coverage. Attested by the peer, referenced
  by cosmon, never gated by cosmon.** The peer repository publishes a coverage
  attestation — verb list, coverage state, commit — and the registry records
  its content hash and date. Cosmon CI checks the attestation is *present and
  parses*, never that it is *true*. A stale attestation is visible as a date;
  a lying one is the peer's breach, on the peer's side of the seam.

**(iv) The six broken references are repaired in the same change.** Under
§8l's own Alphabet-Closure clause this is not optional.

### 2.3 · What this costs

§8l stops being a bijection cosmon can prove and becomes a bijection cosmon
can *enumerate*, with the other half delegated across a seam that is written
down. That is a genuine loss of strength. It is preferred to the current state,
where §8l is nominally a proven bijection and is in fact 0/32, cited through a
dead link, against a peer nobody has edited in a month. **An invariant that
states less and is checked is worth more than one that states everything and is
checked nowhere.**

### 2.4 · Falsifier

**F2-171.** If, after this lands, a new user-facing `cs` verb ships without a
registry row, half-gate A did not fire and the amendment bought nothing —
cosmon CI was the one place this was supposed to be mechanical.

---

## 3 · Question 3 — `cs session` versus `cs sessions`

### 3.1 · Measured

Both are top-level commands in the same clap enum, four lines apart
(`crates/cosmon-cli/src/main.rs:224–231`):

```rust
/// Session — operator carnet (start/note/end), append-only, BLAKE3-sealed
Session(cmd::session::Args),

/// Sessions — co-pilotage cockpit over provider sessions
/// (discover/show/attach/send/checkpoint/drift/takeover/hook)
Sessions(cmd::sessions::Args),
```

They are unrelated domains. `cs session` is the operator's sealed logbook.
`cs sessions` is presence, lease, checkpoint and drift over provider sessions
(ADR-168). A tab-completion of `cs sess` offers both, one `s` apart, and the
help lines are the only thing distinguishing them.

Axis 1 of the operator vision covers both. M7 §8 measured what this costs
before any UI existed: **F2** (identity retyped on every command, a typo
creates a phantom pilot silently) and **F3** (a pilot has two identities and
the operator maintains the correspondence from memory) are both symptoms of a
domain whose nouns are not settled.

### 3.2 · Decision — two nouns with no shared stem; the plural is retired

**A UI must not display the bare word "Sessions", and the fix is not a UI
convention — it is a rename in `cs`.** A convention held only by the cockpit
is a convention the CLI, the API, the man page and every worker brief will
break, which is exactly the confusion the question asks to dissolve rather
than reproduce.

| Domain | Today | Canonical noun | Verb |
|---|---|---|---|
| operator logbook, append-only, BLAKE3-sealed | `cs session` | **carnet** | `cs carnet` |
| provider session under presence + lease | `cs sessions` | **pilot** (a *pilot seat*) | `cs pilots` |

Both new names are already the words the code uses to explain itself: the help
text for `cs session` says *"operator carnet"*, and ADR-168 calls the entities
of `cs sessions` *pilots* throughout (PRIMARY, COPILOT, `peers`, `takeover`,
`PilotLease`, `cosmon-pilot-checkpoint`, `.cosmon/state/pilot/`). The rename
does not invent vocabulary; it promotes the vocabulary the domain already
speaks into the verb that had borrowed a generic one.

**`session` / `sessions` remain as aliases, hidden, permanently.** No flag day,
no breakage: every brief, script, hook and habit keeps working. The visible
surface — `cs help`, `man cs`, the book, the cockpit — shows only `carnet` and
`pilots`. The word *session* survives where it is correct and unambiguous: a
*provider session* is a thing a provider owns, and `cs pilots discover` finds
them. It stops being a cosmon verb.

This is a CLI surface change, therefore out of this doc-only molecule's scope.
**Filed as a child molecule**, carrying its own obligations: `cs help` and
`man cs` update in the same PR (CLI doc sync), a `ux-cli-parity-registry.md`
row per §2, and the ADR-063 vocabulary lineage.

### 3.3 · Falsifier

**F3-171.** If, after the rename, an operator or a worker brief still writes
`cs sessions` more often than `cs pilots` at three months, the alias kept the
old word alive and the rename was cosmetic. Measurement: grep the fleet's
briefs and the operator carnet.

---

## 4 · What this ADR does not decide

Named explicitly so no reader infers a decision from silence:

- **It does not ratify §8k′.** §1.5 states that this decision forces the
  question; it does not answer it. ADR-066 is untouched.
- **It does not choose the cockpit's technology, repository, or hosting.**
  The idea document defers this deliberately; nothing here changes that.
- **It does not decide the Neurion notarisation substrate**
  (`task-20260803-2679`), nor whether the inert Swift apps are dead or paused.
- **It does not write a single line of UI, or an endpoint, or a schema.**

## 5 · Children filed by this ADR

1. **§8ac lands in `docs/architectural-invariants.md`** — the invariant text of
   §1.3, verbatim, in the next free top-level slot (§8ac; §8t is taken by
   bounded-Δ surface coherence, §8ab is the current last). Doc-only.
2. **§8l amendment + registry repatriation + six broken references repaired**
   — §2.2 (i)–(iv), one change, because Alphabet-Closure demands the invariant
   text and its instrument land together.
3. **Half-gate A in CI** — the clap-tree-to-registry enumeration check.
4. **`cs carnet` / `cs pilots` rename** with permanent hidden aliases, `cs
   help` and `man cs` regenerated in the same PR.
5. **Ratify or withdraw §8k′** (§1.5) — a deliberation, not a task.

Children 1–4 are independent of each other except that 2 must precede 3. None
of them is blocked on the cockpit project existing.

---

## 6 · Why these three answers hold together

They are one answer seen three times. §8k′'s superposition test, §8l's
bijection, and the `session`/`sessions` collision all fail the same way: a rule
that is stated where it cannot be checked. Superposition cannot be checked in a
repository with no screenshots; a bijection cannot be checked across a seam
cosmon does not own; a distinction between two words cannot be held by a UI
convention when the CLI itself does not hold it.

So each answer moves the rule to where it is decidable, and accepts that it
says less there. §8ac trades a pixel test for a token-set test. §8l trades a
proven bijection for an enumerated one plus a named seam. The vocabulary
question is answered in the clap tree rather than in a UI style guide.

The cockpit's own thesis is the same shape: an instruction that lives only in a
provider log is a claim nobody can check, so it moves to a substrate that can
hold it. A surface built to make pilotage checkable cannot be founded on three
invariants that are not.
