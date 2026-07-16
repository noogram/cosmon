# Synthesis — delib-20260716-a2f1

**Question:** Redesign the default behaviour of `cs peek` (and `cs peek --all`) so an
operator can find their way, without losing the ability to identify dead / frozen /
zombie / orphaned molecules.

**Panel:** tolnay (API/semver), shannon (information theory), wheeler (naming),
torvalds (data structure), jobs (product). Three axes never seated in this lineage.

---

## The headline

**The question as posed cannot be answered, and four of five panelists — on four
unrelated axes, independently, without seeing each other's work — found the same
reason.**

There are **two enums named `MoleculeStatus`**:

| source | alphabet | shape |
|---|---|---|
| `crates/cosmon-core/src/molecule.rs` L110 | Pending, Queued, Running, Frozen, **Starved**, Completed, Collapsed | 7 variants, `#[non_exhaustive]` |
| `crates/cosmon-observability/src/molecule.rs` L32 | Pending, Running, Completed, **Stuck**, Collapsed, Frozen | 6 variants, closed |

Bridged by `map_status` (`crates/cosmon-cli/src/cmd/peek_tui/mod.rs` L4413):

```rust
S::Starved => MoleculeStatus::Stuck,   // a live status renamed
_          => MoleculeStatus::Pending, // Queued and every future variant, laundered
```

D1, D2, D3, D4 and F3 are not five defects. They are the relationship between two
enums, misfiled as five bugs. **No redesign of the filter can recover information the
encoder destroyed upstream of it.** shannon states the consequence formally — the
projection's distortion is fixed at `map_status`, so every downstream filter, band, sort
and bucket operates on an already-degraded signal — and torvalds states it as taste:
*fix the enum, and the filter is then obvious.*

This is also the answer to **F2** (why the previous de-conflation re-faulted one layer
up), and all four converge on it: the 2026-04 fix **split the filter while the alphabet
stayed conflated**. It was good work on the wrong object — displaced before it was
written. A second filter-level cut fails identically, and this deliberation convenes a
third time.

---

## The finding that should change what ships first

Trace one `Starved` molecule. ADR-062 defines it as *"external authority refused
service — quota exhausted, rate-limited"*, whose repair is *"a wait or a rotation;
**never a re-prompt**"*. It is `is_alive() == true`, and `cosmon_core::reconcile`
(`crates/cosmon-core/src/reconcile/molecule.rs` L167) already rules it `Degraded`.

Six sites in peek disagree with core and with each other:

| site | file:line | verdict |
|---|---|---|
| `map_status` | `peek_tui/mod.rs` L4423 | → obs `Stuck` |
| `status_str` | `peek_tui/mod.rs` L3977 | → `"stuck"` |
| `StateFilter::matches` | `peek.rs` L133 | → `past` — **filed as archive** |
| `liveness_band` | `peek_tui/mod.rs` L842 | → `Dead` — **below the fold** |
| `row_kind` | `peek_tui/mod.rs` L373 | → `Frozen` — pastille says *parked* |
| `molecule_health_for_row` | `peek_tui/mod.rs` L795 | → `Collapsed` — health says *failed* |

**The one status whose entire purpose is to summon the operator is unanimously filed
with 917 corpses.** It is invisible by default (not `running`), and reachable only via
`--past`, where it is 1 row in 918.

wheeler adds the mechanism of the concealment, which is the transferable lesson:
`molecule_health_for_row`'s own doc comment claims `cosmon_core::reconcile::molecule_health`
"stays the single source of truth" — and then feeds that source of truth a
**pre-destroyed input** (`"stuck" => CoreMS::Collapsed`). *The delegation is nominal.
The core already knows every answer peek is getting wrong. peek does not ask it.*

**Nobody has noticed because nobody has had a `Starved` molecule in front of them
recently.** The 27 phantom workers say they will.

---

## Per-persona summaries

### tolnay — API minimalism & semver
Corrected the frame twice. **`cs peek --json` does not exist**: no `json` field on
`Args`; the global flag parses, is ignored, and launches the TUI; on a pipe
`peek_tui/mod.rs` L186 prints an error telling the operator to *"run `cs peek --json`"* —
the escape hatch points at itself. Meanwhile `docs/guides/inbox-trial.md` ships a `jq
'.workers[]'` pipeline against a schema that never existed. **Q6 is therefore greenfield,
zero consumers bound.** Ruling: emit raw core `status` + `heartbeat` + `last_activity`,
**no `bucket` field** — publishing `bucket` freezes as a machine contract the one artefact
with a demonstrated recut cadence, so every future improvement becomes a breaking change.
`status` is already contract in `wire_contract.rs` / `parity_with_cs.rs`: zero new surface,
and strictly richer (`archive` erases completed-vs-collapsed). **Q1:** `--all`, `--future`,
`--past` all removed and erroring, each naming its successors; `--past` must not survive as
an alias because *you do not alias a name onto a set it was created to mis-describe*.
**F2 (the sharpest formulation on the table):** *a surface is terminal when no flag's
meaning is a function of how many axes the command has.* `--all` means `⋀ over all axes`,
so its extension is recomputed every time an axis is born — it is not a flag, it is a fold
frozen into a name. Both prior cuts removed the conjunction and left the `∀` standing.
**`--all` is not a flag with a problem; `--all` is the recurrence, spelled.**

### shannon — information theory
Declined the entropy figure explicitly (*"true, flatters the complaint, and decides
nothing"*). Reframed the bucket predicate: **a bucket is an equivalence class under the
operator's next action**; the quantity to maximise is `I(row ; next_action)`. A row that
cannot change what you do next carries zero bits *no matter how much it differs in every
other column* — so the 917 are entitled to **one line, not 917**, and the metric that
condemns `--past` is not its size but its **cost per bit**. Buckets: **LIVE · ALARM ·
READY · BLOCKED · PARKED · DONE · UNKNOWN**. `running` is the only status that splits,
and for a principled reason: **it is the only symbol carrying a noise term** (state.json
claims progress, tmux reports reality, and they can contradict) — ALARM is the bucket for
*"the noise won."* Two free bits found and taken: `RowView.blocked_by` already carries
each blocker *with its status* (READY vs BLOCKED — *"refusing a free bit is the only
unforgivable move on this axis"*), and **`stuck_at` is never read by peek**, so a worker
wedged on a missing prerequisite and a molecule you deliberately shelved render
identically. On `_ => Pending`: it **converts an erasure — detectable, cheap — into a
substitution error — undetectable, propagating as confident data.** A decoder that cannot
distinguish *"I don't know"* from *"I know it is Pending"* has no error detection at all.

### wheeler — naming & framing
Corrected the frame's own invariant: `docs/vocabulary.md` already rules peek
**vernacular by charter** (delib-20260416-745e, which wheeler sat) — *"Everything in
cosmon is named after physics — except the one command you use most. Because looking
should be easy."* So physics nouns on peek's flag surface would **violate** the
two-register rule, not honour it. He killed his own poetic option (`--ground`/`--excited`
— *"genuine physics, genuinely correct, and therefore a violation… killed on principle,
not on taste"*). **Rejected `live`/`parked`/`archive`, each for its own fault:**
`archive` collides with the live `cosmon-archive` crate (whose `Trigger` enum *archives*
frozen and stuck); `parked` **re-solders D2 inside the fix** by fusing frozen-by-design
with starved-against-its-will; `live` begs the question core already answers differently
via `is_alive()`. **Q2: do not mint a third taxonomy — delete the taxonomy.** One bit —
*is the story over?* — and **the bit already exists in core, named and tested and unread
by the observer**: `is_terminal()` / `is_alive()`. peek re-implemented it twice and got it
wrong twice. `stuck` is **re-pointed, not renamed**: `stuck := frozen ∧ stuck_at.is_some()`,
a distinction `cosmon-runtime/src/resident.rs` L484 **already acts on** (a delivered
freeze releases dependents; a stuck freeze does not) — *the scheduler already treats
these as two things; peek renders them as one word.* **Total new vocabulary: one word
(`settled`).**

### torvalds — data structure
*"Not three booleans, and bitflags would not fix it either."* Counted the incoherence in
a 20-line type: `StateFilter` has 8 states, `label()` names all 8, `from_flags()` can
reach exactly **4** (`running` is hardcoded `true` on every path) — *"three flags in a
trench coat"*. But **booleans are not the defect: the booleans are predicates over a
domain that has no name.** There is no type in the codebase meaning "the operator-facing
category of a molecule" — instead there are **five hand-written classifications** over the
same domain (`matches`, `matches_status`, `liveness_band`, `row_kind`,
`molecule_health_for_row`), each ending in a wildcard, each free to disagree, **and all
five already disagreeing**. Ruling: name the codomain — one `Phase` enum in core, one
total `phase()` beside it, **no wildcard arm**, filter = set of `Phase`.
`#[non_exhaustive]` is *a promise downstream, never a shield upstream*: adding a variant
must break core's build at exactly one site. **Five tests for a terminal split**, of which
the fifth explains the recurrence: *the domain must be the truth, not a projection of it* —
every partition over the observability enum is **displaced before it is written**. The
taste test: after the fix, do the special cases vanish or did you move them? Six vanish.

### jobs — product & subtraction
Opened with the confession: **`default_watchdog()` is my fingerprint, and it is the
bug.** The 2026-04-27 rationale quoted in the code asked to remove **the archive**; the
code removed **everything not `Running`**. Those sets differ by exactly the 5 frozen and
the 27 orphans. *The predicate that shipped is a **storage** predicate (`status ==
"running"`); the sentence asked for an **attention** predicate (`status != terminal`). D3
is that crack, and my axis dug it.* **The line the frame asked for:** *an instrument may
hide what it has already told you; it may never hide what it has not yet told you.* The
917 are read receipts — the operator's relationship with them is closed. Frozen, pending,
orphans were **never reported**: hiding them is amputation, not subtraction. *"A gauge
that reads zero when the tank is empty and when the sensor is dead is not a simplified
gauge — it is a lie with a clean bezel."* **Crucially, the line cuts harder than the
status quo, not softer:** today's default removes 928 of 931 rows and loses the signal;
his removes 917 and keeps every signal. **The clean version and the honest version are the
same version.** Default named: **`unfinished`** (`!terminal`, 14 rows). And on IFBDD:
*nobody complained about the invisible frozen molecules because the instrument that would
have shown them is the instrument that hid them — **silence from an amputated sense is not
consent.***

---

## Convergences

1. **The two enums are the root cause.** tolnay, shannon, wheeler, torvalds — four axes,
   independently. The strongest signal this panel produced. (jobs did not look at the
   type layer; his axis had no reason to.)
2. **`Starved` is misfiled as archive, and it is the highest-value line in the
   deliberation.** Unanimous among the four who found it. Core already rules it
   `Degraded`; peek overrules core six ways.
3. **The default must be `!terminal`, not `== running`.** All five. The disagreement is
   only about how many buckets sit inside "not terminal".
4. **The fleet is 14 molecules, not 931 and not 3.** jobs: *"917 read receipts were
   wearing a costume."* wheeler: the default hides 11 of 14 to save eleven rows of
   screen. **Both usable numbers were wrong; the number that was always right is 14.**
5. **D4's stated mechanism is false, and the real one is the enrichment cache.**
   torvalds and shannon found it independently and identically: the `Stalled` promotion
   (L1424-1434) sits **below the cache `continue`** (L1356); `CachedEnrichment` has **no
   `heartbeat` field**; `apply_cached_enrichment` never restores it. So identical disk
   state renders `Stuck` on a cache miss and `Live` on a cache hit — **a memoisation
   silently deciding semantics.** shannon's corollary is the sharpest sentence in the
   dossier: *the instrument built to detect the absence of motion is evaluated only at the
   instants motion occurs.* He found the same bug a second time in `trust_score` (blanks
   on every cache-hit tick), establishing it as a **defect class**, not an incident:
   **the cache is a lossy compressor whose distortion was never measured, and its dropped
   fields do not read as missing — they revert to a different, confident, wrong value.**
6. **Death-date is free.** torvalds and shannon, independently: `m.updated_at` is already
   read at `snapshot_to_rows` L3783 and immediately stringified into `age: String`. **The
   struct discards the fact and retains its formatting.** For a terminal molecule the last
   write *is* the death write. One field, zero new I/O. (shannon adds: `mol_id` sorts by
   *kind* first — `delib-` before `task-` — so reversing the alphabet fixes nothing.)
7. **Phantom workers are a counter, never rows** — jobs, torvalds, wheeler. And two of
   them say the quiet part: **the taxonomy redesign identifies zero zombies.** It is a
   writer problem and a counter problem, not a filter problem. *"Do not let the taxonomy
   ruling take credit for it."*
8. **`rows_differ` is the oracle, already in the tree.** It compares `(mol_id, status,
   step)`; the sort key reads `liveness_band` and `heartbeat`, which are in neither. So
   **the adaptive poller reports "nothing changed" on the exact tick the table reorders
   itself.** torvalds' invariant: *the sort key must be a function of the change-detection
   key.* Provable by `grep`, today, with no telemetry.

---

## Divergences

### D-1 — What happens to `cs peek --all`? **The panel is deadlocked 2–2.**

| position | who | argument |
|---|---|---|
| **Delete it; error naming both successors** | tolnay, wheeler | `--all` is a cross-axis quantifier: its meaning is a function of how many axes exist, so it re-solders at every new axis. A grace window is a phasing device with no clock — ungated, "grace" means forever, and the solder survives the deliberation that removed it. Blast radius is ~24 files, **all docs/help/snapshots, zero callers in anger**. A one-way door with nobody on the other side should be closed now. |
| **Keep it; exact current meaning, as documented sugar** | torvalds, jobs | *We don't break userspace.* Somebody's cron silently loses multi-galaxy scope and nobody finds out for a month. jobs independently: `--all` is the **least ambiguous flag in the CLI** — refusing it is condescension, narrowing it silently is *"the same failure as `default_watchdog()`, committed a second time by the same hand."* Nobody types `--all` twice once the default is right; it goes back to being the sledgehammer its name promises. |

**This is the deliberation's one genuine unresolved decision, and it is an operator
call.** Note both sides agree on the two things that matter most: (a) silent narrowing is
disqualified — the panel is unanimous and vehement; (b) the new orthogonal axes get their
own flags regardless. The disagreement is *only* whether the old spelling survives.

Note also that tolnay's own strongest argument cuts against his conclusion: he ruled the
blast radius is zero real callers. If nothing breaks, `--all` costs nothing to keep — and
torvalds' rule answers his F2 objection without deleting anything, since sugar that is
*documented as* `--projects all --phase all` names its axes explicitly and does not
quantify over the axis set. Conversely jobs' "nobody types it twice" concedes the flag is
not load-bearing, which is tolnay's premise. **Recommendation to the operator: keep
`--all` as documented sugar (torvalds/jobs), because the panel is unanimous that silent
narrowing is the cardinal sin and keeping the literal meaning is the only option that
cannot commit it.** But this is a verdict-door, not a menu — see outcomes.

### D-2 — How many buckets? **1, 6, or 7.**

| position | who | count |
|---|---|---|
| **One bit** — `settled` / `!settled`, calling core's `is_terminal()` | wheeler, jobs | 2 |
| **Six** — a named `Phase` enum, compiler-enforced | torvalds, tolnay | 6 |
| **Seven** — equivalence classes under the operator's next action | shannon | 7 |

All three claim to be the minimal fix, and they are minimal along *different* axes:
wheeler minimises **vocabulary** (one new word), torvalds minimises **classification
sites** (five hand-written tables → one total function), shannon minimises **wasted
channel** (bits per row of screen).

**They are less opposed than they look.** wheeler's one bit is the *filter*; shannon's
seven are *equivalence classes for rendering and ordering*; torvalds' `Phase` is the
*type that makes either enforceable*. A design can have a one-bit filter (`--settled`),
a six-variant `Phase` for ordering, and shannon's action-classes as the row grouping —
these occupy three different layers. **The real divergence is narrower:** wheeler says a
bucket layer is itself the bug because *"you cannot re-solder a partition that does not
exist"*, and torvalds says the absence of a named codomain is *precisely* why five
opinions drifted. **Both are diagnosing F2 and prescribing opposite medicine: delete the
partition vs. name it once and let the compiler hold it.**

Tie-break available on evidence: torvalds' claim that five hand-written tables already
disagree is **verified by four panelists' independent traces of the same `Starved` row**.
wheeler's claim that a partition must re-solder is an induction from two instances.
Verified beats inferred — but wheeler's *"every word this design needs already exists in
this repository; they were pointed at the wrong things"* is the constraint that should
govern the naming inside whatever partition is named.

### D-3 — May the sort key read the heartbeat tier?

**wheeler alone says yes**, and his argument is the frame's F1 turned into a defence:
`Active`/`Idle`/`Quiet` all collapse into one pulse value, so the tiers that move
per-poll never cross a band boundary; the band-crossing edges are `Quiet → Stalled` (a
30-minute threshold, hysteresis-stable) and `→ Orphaned`. *F1's tier-stability is
precisely what makes pulse safe to sort on — and `Orphaned`'s transience is precisely why
it is unsafe to filter on.*

**tolnay, torvalds and shannon say no**, and shannon states the general rule: *a sort key
must be monotone in time, or constant — never a thresholded clock-derived quantity.*
Render it; do not order by it.

**Resolution:** shannon's cache finding decides this against wheeler, and wheeler could
not have known — he did not read `enrich_rows`. Wheeler's argument assumes `heartbeat` is
a clean function of `last_activity`. It is not: on cache-miss it is promoted by
`is_stalled_by_progress`, on cache-hit it silently reverts to the tmux value the code
itself documents as *"attach-bumped and lies"*. **The tier is not tier-stable in
practice, because two different code paths compute it.** Sort on `last_progress_at` /
`updated_at` (monotone, stored, already present) and leave the tier a column.

### D-4 — Where does `stuck` land?

Four rulings, and they are compatible in substance but not in wording — a real risk of
minting the sixth meaning while fixing the first five:
- **wheeler:** delete from all four peek arms; obs `Stuck` → `Starved` (identity,
  lossless); **re-point** `stuck := frozen ∧ stuck_at.is_some()`.
- **shannon:** `stuck` → ALARM; rename `LivenessBand::Stuck` → `Unresponsive`.
- **tolnay:** delete obs `Stuck`; rename `LivenessBand::Stuck` → `Adrift`.
- **torvalds:** kill the observability enum outright; the question dissolves.
- **jobs:** the band keeps the word (the alarm meaning is the one worth having).

Convergent core: **the string `"stuck"` must stop meaning `Starved`.** Divergent: what
`stuck` should mean afterwards, and what the band is called. wheeler's is the only ruling
grounded in a distinction **the runtime already acts on** (`resident.rs` L484) and in the
verb the operator **already types** — and it is the only one that mints nothing.

---

## Surprises

- **Every named substitution trap failed to fire.** tolnay explicitly declined the clap
  how-to (*"the mechanism is a detail and I decline to specify it"*). shannon explicitly
  declined the entropy figure (*"recorded here only as the thing I am declining to do"*).
  wheeler emitted nouns and killed his own poetic option on principle. torvalds ruled the
  data structure before the sort, naming the temptation. jobs — predicted to commit
  subtraction absolutism — instead refused to touch `--all` at all. **The frame's R2
  hypotheses appear to have functioned as guardrails rather than merely as falsifiers.**
  Honest caveat: this is one round with no control, and a panel of strong personas might
  have avoided these traps unprompted. But the effect is worth recording — a named trap
  may be a *prevented* trap, which would make R2 load-bearing beyond audit.
- **Jobs' trap fired in reverse, and productively.** The panelist predicted to sacrifice
  the forensic invariant became the panel's fiercest defender of it — and got there by
  auditing his own prior subtraction. *"Good subtraction on the wrong noun is worse than
  no subtraction, because it looks finished."*
- **The frame was wrong three times, and each correction improved the question.** F3's
  mechanism (no `Stuck` variant exists in core, so nothing falls through); the physics-
  register invariant (peek is vernacular *by charter* — the exception was already earned
  and inscribed); and the assumption that `--json` exists (it does not). **A frame that
  survives contact unchanged is a frame nobody read.**
- **shannon found F3 to be the opposite of what the frame claimed, and worse.** The frame
  said `stuck` is a *contradiction* (Dead says terminal, Stuck says urgent). shannon:
  there is no contradiction — `LivenessBand::Stuck` never receives a `"stuck"` row, so the
  name is a pure homonym, and **all four live sites agree and are unanimously wrong in the
  same direction.** A contradiction is a bug you can find. Unanimous agreement on a
  falsehood is a bug you cannot.
- **The forensic instrument the redesign needs already exists and is free.** shannon: `cs
  peek --snapshot` is **byte-deterministic by contract** (`peek.rs` L11-14, the
  wheat-paste rule). Two snapshots, before and after, diffed, is the falsifier — no
  daemon, no telemetry, no code. **IFBDD's "instrument before behaviour" is satisfiable
  today by running one command before anything ships.** Not capturing it is the only way
  to make the question unanswerable later.

---

## Frame-question coverage (R1)

| Q frame | Treated | Substituted | Declined-w/-rationale | Silent |
|---------|---------|-------------|-----------------------|--------|
| **Q1** — flag surface after decoupling; `--all`'s fate; compat | ✓ (tolnay, jobs, torvalds, wheeler) | — | — | — |
| **Q2** — total bucket partition; `stuck` resolved | ✓ (all five) | — | — | — |
| **Q3** — one taxonomy or two (stored vs derived) | ✓ (wheeler, torvalds, shannon, tolnay) | (*) jobs | — | — |
| **Q4** — stable sort key; the real dance mechanism | ✓ (tolnay, torvalds, shannon, wheeler) | (**) jobs | — | — |
| **Q5** — scope boundary: phantom workers, peek vs purge | ✓ (jobs, torvalds, wheeler) | — | — | (***) tolnay, shannon |
| **Q6** — ADR-068 parity: flag ↔ key ↔ `--json` contract | ✓ (tolnay only) | — | — | (****) shannon, wheeler, torvalds, jobs |
| **Q7** — the IFBDD falsifier | ✓ (shannon, jobs) | — | — | (*****) tolnay, torvalds, wheeler |

**(*) jobs substituted Q3** with the product framing (*"the user should never see a
taxonomy"*) — **exactly the second substitution hypothesis the frame recorded for him.**
The one prediction that landed. Not fatal: four panelists treated Q3 and converge.

**(**) jobs substituted Q4**, and named it: *"I have very little interest in D4… a 14-row
table does not have a sort problem."* This is a defensible axis-boundary claim and it
carries a real insight — D4 may be a **shadow cast by D2**, dissolving when the default
is fixed. **But it is wrong on the evidence**, and it is the one place where his
subtraction instinct still bites: torvalds' Mechanism B and shannon's cache finding are
**correctness bugs at any row count** — a stalled worker reported as `Live` is not a
cosmetic sort issue, and it does not get better at 14 rows.

**(***) Q5 silent from tolnay and shannon** — both were seated on axes with no purchase
on it, and three panelists treated it convergently. Not an alarm.

**(****) Q6 is the alarm.** Four of five panelists never addressed the `--json` contract
or the CLI↔TUI↔JSON parity mapping. **The mitigating fact is decisive, though:** tolnay
discovered that `cs peek --json` **does not exist**, which retroactively explains the
silence — there was no contract to have opinions about, and the other four (reading the
same code) had nothing to find. The parity ruling therefore rests on **one panelist with
no cross-check**, on an axis nobody else was seated for. Since tolnay's Q6 ruling
(no `bucket` field, emit raw `status`) is *directly downstream of the two-enums finding*
that four panelists independently confirmed, it inherits their corroboration on its
premise, but not on its conclusion. **Recommendation: the JSON schema is decided by
tolnay alone and should be treated as provisional until a second reader checks it** — it
is called out as such in the outcomes.

**(*****) Q7 treated by two, silent from three.** shannon's answer is substantive and
self-falsifying (he names his *own* ruling's central risk: `DONE` may hide
completed-but-unharvested work, since `Trigger::Done` lives in the archive manifest and
not in `RowView`). jobs' is the doctrine (*silence from an amputated sense is not
consent*). Adequate coverage; a follow-up round is not warranted for this alone.

---

## The decision-relevant tension points

1. **Sequencing is the whole ruling.** Four panelists say the filter redesign is
   downstream of the alphabet. **Ship the enum unification first, or ship a third cut
   that faults a fourth time.** This inverts the operator's mandate, which framed the work
   as a peek-surface redesign.
2. **`--all`'s fate is deadlocked 2–2 and is genuinely the operator's call** — with the
   panel unanimous that silent narrowing is disqualified.
3. **Two correctness bugs are independent of every taxonomy question and should not wait
   for it:** the cache/promotion fault (`Stalled` and `trust_score` both revert on
   cache-hit) and the sort-key/`rows_differ` disagreement. torvalds: *"fix this regardless
   of the taxonomy ruling — a memo that changes the answer is not a memo."*
4. **The baseline snapshot is time-sensitive in the only legitimate sense** (not a
   deadline — a destroyed measurement). `cs peek --snapshot --all`, captured **before**
   anything changes, is the difference between a falsifiable redesign and an unfalsifiable
   one. It costs one command.
5. **The mandate's "identify zombies" requirement is not met by this redesign, and the
   panel says so plainly.** It is met by a worker counter (fleet-level, zero new reads)
   plus the patrol's orphan verdict (a *writer*, which already exists at `patrol.rs`
   L1363). Neither is a filter question. **Do not let the taxonomy work claim this
   scalp.**

---

## Verdict

The operator asked how to re-cut peek's filters. **The panel's answer is that the filters
are not where the information is lost.** Four independent axes converged on one sentence,
and torvalds wrote it:

> *There are two `MoleculeStatus` enums. Everything else in this deliberation — D1, D2,
> D3, D4, F3, all 931 rows of it — is the relationship between them, misfiled as five
> separate bugs.*

And wheeler wrote the reason it stayed hidden:

> *Every word this design needs already exists in this repository. They were merely
> pointed at the wrong things.*

`is_alive()` sat in core, tested and correct, while peek re-implemented it twice and got
it wrong twice. `molecule_health(Starved) = Degraded` sat in core, unread, while six sites
in peek ruled the same molecule archived, dead, parked and failed. The fix is less an act
of design than an act of **returning four words to their referents and deleting the
struct that misdirected them**.

Decomposition follows in `outcomes.md`.
