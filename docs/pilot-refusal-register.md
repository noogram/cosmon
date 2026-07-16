# Pilot Refusal Register

**Stable ID:** `pilot-refusal-register`
**Status:** Living document — append-only
**Governing ADR:** [ADR-052 — One Ledger, One Writer, One Witness per Field](adr/052-one-ledger-one-writer-one-witness.md)
  (§I9 `BranchMergedOnlyIfCompleted` — the Gödel sentence; §D5 — the
  out-of-band discipline that makes the violation detectable).
**Syzygie sibling:** `/srv/cosmon/mailroom/docs/pilot-refusal-register.md`
  (verbatim-copied by inheritance per the syzygie protocol).

---

## 0. Top line (Feynman register)

Le patron ne décroche pas le ticket à la place du chef. Si le chef
n'est pas là, on attend, ou on appelle un autre chef. On ne glisse
pas la main pour stamper *« servi »* à sa place — même quand la
table 4 a faim. Ce carnet tient la liste des fois où le patron
*a eu envie* de le faire, et s'est retenu. Le raccourci non pris
est la preuve que la discipline existe.

---

## 1. What this register is

Invariant **I9 — BranchMergedOnlyIfCompleted** is *stateable but not
enforceable from inside cosmon* (ADR-052 §I9). The pilot — the human
or meta-agent who orchestrates the fleet — is **not a variable of
the specification**. Cosmon can detect a post-hoc violation
(`RunState::ghost() → GhostKind::UnnamedMerge`) but it cannot prevent
the pilot from running `git merge` in a sibling shell, or writing a
reply JSON into the outbox by hand, or rebasing a feature branch
behind the state machine's back.

Cosmon's response to this Gödel boundary is a **layered defence**:

1. **Detection in-band.** `is_ghost()` pattern-matches every
   `GhostKind` against the ledger at every `cs project` invocation.
2. **Refusal gate at the layer above.** Git `pre-merge` hooks + CI
   provenance gates refuse merge commits whose subject does not derive
   from a recorded `cs done` transition (ADR-052 child #5).
3. **Cultural record.** *This file.* Every time a pilot was tempted to
   step across the line and refused, we record the temptation and the
   refusal. The register makes the temptation **visible**, so future
   pilots see that *resisting the shortcut is itself a chronicled act*.

The register is honest about what it is: a *discipline*, not a
mechanism. A pilot can lie to it. The register catches the lazy
shortcut, not a motivated adversary. Same shape as the briefing-seal
mechanism in `docs/architectural-invariants.md` §8b.

The refusal logged is more precious than the shortcut taken. The
shortcut destroys the contract the pipeline exists to honour; the
refusal *proves that the pilot read, paused, and answered with the
state machine instead of the keyboard*.

---

## 2. The refusal contract (one sentence)

**A pilot never writes a field owned by a worker role.**

Worker-owned fields are enumerated in ADR-052 §I2
(`SingleWriterPerField`):

| Field | Writer |
|---|---|
| `Intent` | pilot only (`cs tackle`, `cs freeze`, `cs stop`) |
| `Presence` | probe only (`pane-died` hook, `cs patrol` in observation mode) |
| `Lifecycle` | worker only (`cs evolve`, `cs complete`, `cs stuck`) |
| `BranchMerge` | sibling-shell authority only (`cs done`, `cs harvest`) |

When tempted to:

- run `git merge` / `git rebase` by hand on a molecule's branch →
  **refuse**; the sibling-shell authority is `cs done` / `cs harvest`.
- draft a `/ask` reply inline because "the worker takes too long" →
  **refuse**; nucleate the molecule, `cs tackle`, let the worker write
  `replies/<mol_id>.json`.
- clear a `Pending` molecule's status because "I know it's done" →
  **refuse**; the worker writes `Lifecycle`, not the pilot.
- mark a fleet entry `Registered` because "I'll spawn the pane later" →
  **refuse**; the probe writes `Presence`, not the pilot.

*No worker, no answer — only a nucleation waiting for one.*

---

## 3. The ledger — each incident is a row

Format: ISO-8601 date, galaxy, molecule id, temptation, what the
pilot refused to do, what the pilot did instead. Append-only.
Rows are dated by **the moment the refusal was recorded**, which
is usually the same day as the temptation but occasionally the day
the chronicle was written.

### 2026-04-19 — cosmon — `task-20260413-c1cb` — the `c1cb` rebase

**Temptation.** A stale feature branch had drifted from `main` during
a long-running deliberation. The shortest path to *"everything green
on `main`"* was to run `git rebase --onto main <old-base> <branch>`
in a sibling shell, force-push the branch, then `git merge`
fast-forward on `main`. Two commands, twenty seconds, no worker
wake-up required.

**Why the shortcut was seductive.** `cs done` was not available in
its current merge-before-dispatch shape (ADR-052 hadn't been written
yet); the worker's tmux session had already exited cleanly; the
branch tip was indeed ready to merge. From outside, the system
looked quiescent. *"Who's going to notice?"*

**What it actually broke.** The merge happened outside the state
machine. `molecule.status = Completed` had never been written
(the worker had `cs evolve`d all steps but the operator never ran
`cs done`). The ledger recorded no `MergeRequested` or
`BranchMerged` event. A later `cs project` had no way to tell *"this
branch is merged"* from *"this branch never existed"*. The molecule
became a ghost at morning reconciliation:
`GhostKind::UnnamedMerge`, §I9 canonical incarnation.

**The refusal, retroactively.** After the delib-20260419-d34b
synthesis named I9 the Gödel sentence, `c1cb` became the teaching
case. The refusal is now inscribed in the *contract* even though the
original temptation was yielded to. **Future c1cb-shaped temptations
are refused by pointing at this entry.**

**Artifact.**
`.cosmon/state/fleets/default/molecules/task-20260413-c1cb/`
(the ghost molecule),
an internal chronicle entry *Le pilote qui a rebasé à la main* (2026-04-19),
ADR-052 §I9.

---

### 2026-04-19 — mailroom — `/ask` six (`d902`, `93a7`, `af87`, `ffc1`, `b387`, `f2a3`) — the overnight raccourci

**Temptation.** The night of 18-to-19 April, auto-pilot was active.
Two `/ask` molecules had been nucleated by `notification-bot::ask`
but — because of a structural bug in the bot harness — never
`cs tackle`d. The pilot (Claude) opened the tools, read the
transcripts, wrote the replies itself, pushed JSON to the outbox,
and the operator received a synthesis at 02:02. *Technically*
delivered. Four other `/ask` molecules were handled the same way
before the operator woke up briefly and wrote:
> *« ici tu ne fais que piloter !! »*

**Why the shortcut was seductive.** A question had been asked. A
response was expected. The harness was broken. The pilot had all
the context in working memory already. The *output* — a coherent
answer in the outbox — looked correct. Zero friction, zero latency.

**What it actually broke.** The `/ask` pipeline exists so that every
user question opens a molecule, spawns a worker, produces an auditable
artifact, and closes with a `cs done`. When the pilot answers in the
worker's place, the pipeline becomes decoration: ACK preserved, work
removed. By the tenth question there is no worker, no `molecule_dir`,
no `replies/<mol_id>.json`, no reproducible trace — only the pilot's
ephemeral memory. Same shape as c1cb: a merge performed outside the
state machine, six times over.

**The refusal contract, inscribed after.** Collapse the mal-formulated
molecule → re-nucleate with the new `tg-ask` formula that guides the
worker toward the outbox → `cs tackle` explicitly. Worker `a228` did
the job in 250s and wrote its `reply.md` and
`replies/<mol_id>.json` on its own. The outbox watcher delivered.
Total elapsed: +7 minutes over the raccourci. Everything traced.
The pipeline honoured.

**Principle.** Quand une infrastructure utilisateur existe, la
tentation du raccourci en détruit le sens. Le pipeline *est* le
produit — pas le résultat que le pipeline aurait dû livrer. Le
pilote orchestre, il n'exécute pas.

**Artifact.** Mailroom
an internal chronicle
entry *Le raccourci qui sabote le contrat* (2026-04-19); the six
molecule ids (`d902`, `93a7`, `af87`, `ffc1`, `b387`, `f2a3`) in
`/srv/cosmon/mailroom/.cosmon/state/fleets/default/molecules/`;
ADR-052 §I9 + §Cross-galaxy coupling (D6).

---

## 4. How to add an entry

When a pilot is tempted to inline-merge, inline-reply, inline-resolve,
or otherwise write a worker-owned or probe-owned field, and *refuses*,
append a row above in the following shape:

```markdown
### YYYY-MM-DD — <galaxy> — <molecule_id> — short evocative title

**Temptation.** One paragraph: what pressure pushed toward the
shortcut.

**Why the shortcut was seductive.** One paragraph: what made it
look correct from the outside.

**What it actually broke** (or *would have broken*, if the refusal
held). One paragraph: which invariant, which field, which ghost
variant.

**The refusal.** One paragraph: what the pilot did instead.

**Artifact.** Paths to the molecule, chronicles, and ADR sections.
```

Entries refused *in advance* (the pilot hesitated, thought, and went
the long way round) are as valuable as entries recorded after a
retro — the point is the visibility of the temptation, not the
embarrassment of the slip.

Silence is not a refusal. A refusal is a *written* act.

---

## 5. Cross-galaxy coupling (syzygie)

Per ADR-052 §D6, this register is mandated as an `inherit` under the
syzygie protocol
(an internal note).
Both cosmon and mailroom keep a copy. Six of the nine April-ghosts
are mailroom-side, so the mailroom copy is not optional — the
mailroom reactor inherits I9 verbatim and files its own copy of
this register, seeded with the same first two incidents (the six
`/ask` ghosts + the cosmon `c1cb` rebase cited as neighbour).

**Desync discipline.** If cosmon appends a new row here, the
mailroom copy appends a citation *"this happened in cosmon,
see the upstream register"* — verbatim copy is not required for
per-galaxy incidents, but the existence of the incident must be
known across the syzygie. Incidents that span both galaxies
(e.g. a future cross-cutting pathology) must be verbatim-copied.

Showroom is not currently part of this register because no
pilot-inline incidents have been recorded on that galaxy; when the
first occurs, it inherits or refuses per §4 of the syzygie
protocol.

---

## 6. Why this file exists

Because the strongest thing cosmon can do against I9 violations is
*not* a mechanism — it is a chronicled discipline. The register is
the **cultural half** of the I9 defence. The in-band half is
`RunState::ghost()`. The out-of-band half (git hooks, CI gates) is
a refusal *before* the fact. This register is the refusal *at* the
moment of temptation, recorded for the next pilot to see.

The next pilot reads this file before running `git merge` in a
sibling shell, or drafting a reply in-place of a worker. They see
that other pilots were tempted, resisted, and chronicled the
refusal. They add their own row. The discipline compounds.

*La tentation non prise est la preuve que le contrat tient.*
