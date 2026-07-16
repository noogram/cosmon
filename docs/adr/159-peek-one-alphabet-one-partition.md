# ADR-159 — `cs peek`: one alphabet, one partition

**Molecules:** the eight children of `delib-20260716-a2f1`, all landed —
`task-20260716-94dc` (C0 baseline), `-aaf4` (C1 enums), `-1d56` (C2 cache),
`-cec3` (C3 sort key), `-8456` (C4 partition + default, C5 flags folded in),
`-5994` (C7 worker counter), `-6a4e` (C6 JSON), `-fc86`. · **Parent
deliberation:** `delib-20260716-a2f1` (panel: tolnay, shannon, wheeler,
torvalds, jobs) · **Sources:** `docs/design/peek-redesign/synthesis.md`,
`docs/design/peek-redesign/outcomes.md`. **Status:** Accepted (2026-07-16).
**Decision owner:** Noogram.

Supersedes the `StateFilter { running, future, past }` shape of `cs peek`'s
filter and the `default_watchdog()` `status == "running"` default. Governed by
ADR-028 (pure projection — and see the recorded divergence in §7), ADR-052 (one
ledger, one writer), ADR-062 (`Starved`), ADR-068 (CLI/UI equivalence), ADR-095
(IFBDD), and `docs/vocabulary.md`'s two-register rule.

**Written after the code landed, to record what was done rather than what was
intended.** Where the plan and the tree disagree, the tree wins and the
divergence is named (§7).

---

## Context — the filters were never where the information was lost

The operator asked for a filter redesign: `cs peek` showed 3 rows out of 931 and
an operator could not find their way. Four panelists, on four unrelated axes,
independently returned the same answer — **the filters were downstream of the
damage.**

There were **two enums named `MoleculeStatus`**:

| source | alphabet |
|---|---|
| `cosmon-core` | Pending, Queued, Running, Frozen, **Starved**, Completed, Collapsed — 7, `#[non_exhaustive]` |
| `cosmon-observability` | Pending, Running, Completed, **Stuck**, Collapsed, Frozen — 6, closed |

bridged by a lossy `map_status`:

```rust
S::Starved => MoleculeStatus::Stuck,   // a live status, renamed
_          => MoleculeStatus::Pending, // Queued and every future variant, laundered
```

**Trace one `Starved` molecule** and the cost is legible. ADR-062 defines it as
*external authority refused service*, repaired by *a wait or a rotation; never a
re-prompt*. It is `is_alive() == true` and core's `reconcile` already ruled it
`Degraded`. Six sites in peek overruled core and disagreed with each other —
filing it as `Stuck`, `"stuck"`, `past` (archive), `Dead` (below the fold),
`Frozen` (parked) and `Collapsed` (failed). **The one status whose entire purpose
is to summon the operator was filed with 917 corpses**, invisible by default and
reachable only via `--past`, where it was 1 row in 918.

wheeler named the concealment mechanism, and it is the transferable lesson:
`molecule_health_for_row`'s own doc claimed core *"stays the single source of
truth"* — then fed that source of truth a **pre-destroyed input**. *The
delegation was nominal. The core already knew every answer peek was getting
wrong. peek did not ask it.*

This also answers why the 2026-04 de-conflation re-faulted one layer up: **it
split the filter while the alphabet stayed conflated.** Good work on the wrong
object. A third filter-level cut would have faulted a fourth time.

## Decision

### 1. One alphabet — the observability enum is deleted (C1, the root cause)

`cosmon_observability::molecule::MoleculeStatus` is **gone**; the module now
re-exports core's (`crates/cosmon-observability/src/molecule.rs:44`).
`map_status` is deleted — it survives only in comments explaining why it no
longer exists. The bridge cannot be lossy because there is no bridge.

Unknown-status honesty is achieved without an `Unknown(String)` variant: core's
enum stays `#[non_exhaustive]`, its `FromStr` is exhaustive **inside the crate
that owns it**, and unrecognised values pass through as their own snake_case
name rather than being laundered into `pending`. `MoleculeStatus::ALL` +
`ordinal()` keep "for every status" tests from silently lapsing when a variant is
added — an array does not fail to compile; a match does.

shannon's ruling on why `_ => Pending` was the worst line in the file, recorded
because it generalises: **it converted an erasure — detectable, cheap — into a
substitution error — undetectable, propagating as confident data.** *A decoder
that cannot distinguish "I don't know" from "I know it is Pending" has no error
detection at all.*

### 2. Name the codomain — `Phase`, no wildcard arm (C4)

There was no type in the codebase meaning *"the operator-facing category of a
molecule"*. Instead there were **five hand-written classifications over the same
domain, each ending in a wildcard, each free to disagree — and all five already
disagreeing.** Booleans were never the defect: they were predicates over a domain
with no name. Bitflags would have bought the identical fault in six months with
more bits.

`cosmon-core/src/molecule.rs:304` now names it, and `phase()` (`:256`) is total:

```rust
pub enum Phase { Live, Waiting, Blocked, Parked, Failed, Done }
//  Running → Live · Pending|Queued → Waiting · Starved → Blocked (ADR-062)
//  Frozen → Parked · Collapsed → Failed · Completed → Done
//  NO wildcard arm. This is the point.
```

**`#[non_exhaustive]` is a promise downstream, never a shield upstream.** Adding
a variant must break core's build at exactly one site, and the author who adds it
names the bucket in the same commit. Before, adding a variant broke nothing and
silently mis-rendered in six places.

**torvalds' five tests for a terminal split** — the fifth is the one that explains
the recurrence and is inscribed here as the standing rule:

> **The domain must be the truth, not a projection of it.** Every partition over
> the observability enum was *displaced before it was written* — the information
> needed to make it terminal had already been destroyed upstream.

The taste test he set: *after the fix, do the special cases vanish or did you
move them?* **What vanished is the disagreement, and that is the claim worth
making precisely.** The classifier *functions* still exist — `liveness_band`
(`peek_tui/mod.rs:1011`), `row_kind` (`:452`), `molecule_health_for_row` — because
rendering still needs them. What they no longer own is a **private table**: each
now matches exhaustively on `phase()` or delegates to core's charter, so none can
drift from the others. Six independent opinions became six derivations of one
total function. **One site did not convert, and §7 names it rather than letting
this paragraph round up.**

### 3. The default is `unfinished` (`!terminal`), not `== running` (C4)

`StateFilter { running, future, past }` — 8 states, `label()` naming all 8,
`from_flags()` reaching exactly 4, *"three flags in a trench coat"* — is deleted.
It is replaced by `PhaseFilter` (a bitmask, `peek.rs:100`) over `PhaseSelector`
(`peek.rs:250`), defaulting to `unfinished` (`peek.rs:106`).

jobs' confession is the cleanest statement of the bug in the dossier, and it is
recorded because the fix is only durable if the failure mode is:

> *`default_watchdog()` is my fingerprint, and it is the bug. The operator named
> **one** thing to remove: **the archive**. The code removed **everything that is
> not `Running`**. The predicate that shipped is a **storage** predicate; the
> sentence asked for an **attention** predicate.*

**The governing line — the instrument rule, now standing doctrine:**

> **An instrument may hide what it has already told you. It may never hide what
> it has not yet told you.**

Completed/collapsed are read receipts — hiding them is *subtraction*, and
correct. Frozen, pending and orphans were **never reported** — hiding them was
*amputation*. And the ruling **cuts harder than the status quo, not softer**: the
old default removed 928 of 931 rows and lost the signal; this one removes the
archive and keeps every signal. *The clean version and the honest version are the
same version.*

jobs' IFBDD corollary, which is why nobody had complained: *the instrument that
would have shown the invisible frozen molecules is the instrument that hid them.*
**Silence from an amputated sense is not consent.**

### 4. Flags: one axis each, and `--all` kept by operator verdict (C5)

Perimeter and temporality now have one flag each — `--all-galaxies` (the same
spelling `cs tail` already ships: one word, one meaning, across the binary) and
`--phase` (repeatable, comma-separated, values union). `--past` and `--future`
are **deleted, not aliased**: `--past`'s own doc string enumerated *"completed,
collapsed, frozen, and starved"* — the archive and the parked, welded. *You do
not alias a name onto a set it was created to mis-describe.*

**`--all` survives as documented sugar for `--all-galaxies --phase all`, by
operator verdict (2026-07-16)**, recorded at `peek.rs:95`. The panel deadlocked
2–2 and was unanimous on the one thing that mattered: **silent narrowing is
disqualified**, and keeping the literal meaning is the only option that cannot
commit it. It `conflicts_with` the two flags it expands to — sugar and its
expansion are one way of saying one thing, not two ways of saying it twice.

**tolnay's terminality property — a standing rule for the CLI surface, not a
fact about this command:**

> **A surface is terminal when no flag's meaning is a function of how many axes
> the command has.**

`--all` means `⋀ over all axes`, so its extension is recomputed every time an
axis is born: *not a flag, but a fold frozen into a name.* Both prior cuts
removed the conjunction and left the `∀` standing, so it re-welded at the next
axis. Sugar *documented as* `--all-galaxies --phase all` names its axes
explicitly and does not quantify over the axis set — which is why the verdict
that kept it does not re-arm the recurrence.

**The one-minute test, mechanical, per flag:**

> **For every flag, name the single axis it moves. If the answer needs the word
> "and", delete the flag.**

The test already condemns a second clump — `--snapshot`, `--no-tui`, `--follow`,
`--once`: *four booleans and two `conflicts_with` clauses encoding one enum.*
**Out of scope here by explicit ruling; it is its own molecule.**

### 5. Sorting: never on a thresholded clock (C2 + C3)

**shannon's sort-key law, inscribed:**

> **A sort key must be monotone in time, or constant. Never sort by a thresholded
> clock-derived quantity. Render it; do not order by it.**

**And the invariant that makes it checkable:**

> **The sort key must be a function of the change-detection key.**

`rows_differ` compared `(mol_id, status, step)` while the sort key read
`liveness_band` and `heartbeat` — in **neither**. So the adaptive poller reported
*"nothing changed"* on the exact tick the table reordered itself under the
operator's cursor: two contradictory notions of "the same fleet", 400 lines
apart, in one file. Provable by `grep`, with no telemetry.

`ChangeKey` is now `(mol_id, status, step, updated_at)` (`peek_tui/mod.rs:813`)
and the comparator (`:869`) is `(liveness_band, Reverse(updated_at), mol_id)` —
every term a stored fact the change detector already watches. `heartbeat` left
both the sort key and `liveness_band`; it stays a **column and a colour**. The
invariant is pinned by `sort_key_is_a_function_of_the_change_key` (`:5429`).

**Death-date was free.** `updated_at` was already read every tick, every row —
then immediately stringified into `age: String` and the timestamp dropped. *The
struct discarded the fact and retained its formatting.* For a terminal molecule
`state.json` is never rewritten after death, so `updated_at` **is** the death
date. `RowView.updated_at` (`:239`) is one field and zero new I/O. (`mol_id` was
never a clock: ids are `delib-…` / `task-…` — **kind** first, then date — so
sorting on it ranked `delib-*` above `task-*` forever, and reversing it just
reversed the kinds.)

**C2 — a memo that changes the answer is not a memo.** The `Stalled` promotion sat
*below* the enrichment cache's `continue`, and `CachedEnrichment` had no
`heartbeat` field to restore. So identical disk state rendered `Stuck` on a cache
miss and `Live` on a cache hit — **a memoisation silently deciding semantics**.
`is_stalled_by_progress` is now evaluated unconditionally for every running row
(`:1644`), and `trust_score` joined the cache (`:347`) — it had the identical
defect, blanking the TRUST column on every cache-hit tick.

shannon's corollary names the defect class, and it is why this was fixed
independently of every taxonomy question: *the instrument built to detect the
absence of motion was evaluated only at the instants motion occurs.* **The
enrichment cache is a lossy compressor whose distortion was never measured — and
its dropped fields did not read as *missing*, they reverted to a *different,
confident, wrong* value.** That is the erasure→substitution fault of §1,
recurring at a second layer.

### 6. Register: peek's flags are vernacular by charter (C4)

`docs/vocabulary.md` already rules peek vernacular (delib-20260416-745e):
*"Everything in cosmon is named after physics — except the one command you use
most. Because looking should be easy."*

**So physics nouns on peek's flag surface would violate the two-register rule,
not honour it** — the frame that convened this panel had it backwards. wheeler
killed his own poetic option (`--ground`/`--excited`) on exactly these grounds:
*genuine physics, genuinely correct, and therefore a violation; killed on
principle, not on taste.*

The split: **status values stay physics** (they name molecule state); **filter
flags are vernacular** (they name a human gesture). Rejections, each on its own
fault: `archive` (collides with the live `cosmon-archive` crate, which *archives*
frozen and stuck); `parked` (re-solders the very weld we convened to remove, by
fusing shelved-by-design with held-against-its-will); `live` (begs a question
core answers differently via `is_alive()`).

`stuck` is **re-pointed, not renamed** — `stuck := frozen ∧ stuck_at.is_some()`
(`cosmon-core/src/molecule.rs:285`), a distinction `cosmon-runtime`'s scheduler
**already acts on**: a *delivered* freeze releases its dependents, a *stuck*
freeze does not. *The scheduler always treated these as two things; only the
observer rendered them as one word.* **Total new vocabulary: one word.** Which is
wheeler's constraint, and it governs any future naming here: *every word this
design needs already exists in this repository — they were merely pointed at the
wrong things.*

### 7. Recorded divergences — the tree over the plan

- **`cs peek --json` is one document, not NDJSON — a divergence from a live
  ADR.** ADR-028 §3 sketches it as *"machine-readable NDJSON"*. It ships
  pretty-printed, argued in-code at `peek.rs:624`: every other `--json` in the
  CLI emits one document and peek is specifically the command asked to hold
  ADR-068 parity with `cs observe`; and NDJSON has nowhere to put `filter`, which
  describes the document rather than any molecule in it. **This is a divergence
  from a published contract, not a reading of it — if the ADR is right and this
  is wrong, the fix is a successor ADR, not a quiet reshape.**
- **`--json` is the global flag, not a `peek::Args` field.** peek dispatches on
  `ctx.json` (`peek.rs:500`). The old escape hatch pointed at itself: on a pipe,
  peek printed an error advising the operator to *"run `cs peek --json`"* — a
  flag that parsed, was ignored, and launched the TUI.
- **`LivenessBand::Stuck` was not renamed** (shannon proposed `Unresponsive`,
  tolnay `Adrift`, jobs keep). The homonym with the `cs stuck` verb is
  **documented rather than resolved** (`peek_tui/mod.rs:901`). Recorded as a
  known debt: the risk the panel named here was minting a *sixth* meaning while
  fixing the first five, and not choosing is the one move that mints nothing.
- **The C2 regression test pins `heartbeat`, not the band** (`:5603`). Once C3
  removed heartbeat from `liveness_band`, the band assertion would have passed
  while the bug raged — so it was migrated to the heartbeat, and the reason is
  recorded at `:5597`.
- **C5 has no standalone commit**; it landed folded into C4 (`53732b490`).
- **The one classification site that did not convert — `status_token` still
  launders `starved`, and it is a live defect, not a wording nit.**
  `peek_tui/mod.rs:3790` is a surviving hand-written string→enum table with a
  `_ => Pending` arm, and `"starved"` falls through it. Core's charter maps
  `Starved → Status::Stuck` — vermilion, *needs pilot attention*
  (`cosmon-core/src/visual.rs:174`). peek hands it `Pending` and gets
  `Status::Pending` back: the empty glyph with the dashed border, whose documented
  meaning is *"nothing is working on this yet."* Every rendered row reaches it
  (`:3136`), and **no test covers it** — the glyph tests cover `running`,
  `pending` and `totally-unknown`, never `starved`. This is §1's fault class and
  wheeler's *nominal delegation* alive one layer down, in the glyph path: **the
  core still knows the answer peek is getting wrong, and this site still does not
  ask it.** The redesign moved this special case rather than deleting it. **It is
  its own molecule** — recorded here so the redesign does not claim a completeness
  it has not earned.

### 8. The JSON contract: publish the source, withhold the derivative (C6)

`cs peek --json` emits `{filter, molecules[{id, project, status, heartbeat,
last_activity, updated_at}]}`, sorted by `id` so two captures of an unchanged
fleet diff to nothing.

**No `bucket` field, under any name** (`peek.rs:546`). The bucket taxonomy is the
artefact under active redesign *in this very deliberation* and has been re-cut
twice. Publishing it would freeze, as a machine contract, **the one object with a
demonstrated re-cut cadence** — so every subsequent improvement to the operator's
taxonomy becomes a breaking change for every consumer. `status` is already
contract (`wire_contract.rs`, `parity_with_cs.rs`), so emitting it adds **zero new
surface** and is strictly richer: *`(status, heartbeat)` reconstructs a bucket in
a handful of lines; a bucket cannot reconstruct `status`, because `archive`
erases the difference between finished and failed. Publishing the lossy
derivative while withholding the source is backwards.*

The asymmetry that governs the whole schema: **adding a field later is additive
and breaks nobody; removing or renaming one breaks everyone.** So the default is
omission and the burden falls on the field. Both clock fields ship because they
answer different questions and neither reconstructs the other: `last_activity`
folds in tmux's attach-bumped session clock (**merely attaching moves it**), while
`updated_at` moves only when state is written — *the field a stall or orphan
patrol should read.*

**Provenance caveat, carried from the synthesis:** Q6 was addressed by **tolnay
alone**; four of five panelists were silent. The silence is explained — there was
no contract to have opinions about — and the ruling's *premise* (the two enums)
carries four-way corroboration, but its *conclusion* had no cross-check. It shipped
in the cheap direction to be wrong in.

### 9. The C7 disclaimer — this redesign identifies zero zombies

An always-on header strip (`peek_tui/mod.rs:2816`, wired at `:2465`) renders the
discrepancy, never the inventory:

```
 workers: 30 registered · 3 attached · 27 phantom → cs purge
```

Zero new reads — peek already walks the roster. Never a row: phantoms have no
`mol_id`, and `snapshot_to_rows` is keyed by `mol_id`. The boundary holds — **peek
reads and renders; `cs purge` writes and drains.** The counter tells the operator
to run purge; it never runs it. The remedy is dropped when `phantom == 0`.

Why it earns the default rather than a future `cs workers`: *the 27 phantoms are
the hardest-earned fact in the ground truth, and there was no gesture in the
product that surfaced it at all. Not `--all`. Not `--past`. Nothing. Invisible at
every setting of every flag, because it is not a molecule and every flag we have
filters molecules.* **Not a missing feature — a sense organ the product never
grew.**

> **⚠ The disclaimer torvalds and wheeler both demanded, inscribed verbatim:**
>
> **The operator's "identify zombies" requirement is NOT met by the taxonomy
> redesign.** It is met by this counter (for *workers*) plus the patrol's orphan
> verdict (for *molecules*, `patrol.rs:1363` — *the patrol is the writer of last
> resort*). **Neither is a filter question. No arrangement of `StateFilter` —
> booleans, bitflags, or `Phase` — identifies a single zombie. Do not let the
> taxonomy ruling take credit for it.**

And the rule underneath, which keeps peek a projection (ADR-028) and the ledger
single-writer (ADR-052): **if a fact matters enough to sort by, someone must
write it.** The patrol already is that writer; peek reads the verdict and sorts on
it as a stored fact. No new daemon, and peek does not become a writer.

## Consequences

- **The fleet was 14 molecules, not 931 and not 3.** Both usable numbers were
  wrong; the number that was always right is 14. *917 read receipts were wearing a
  costume.*
- **Six hand-written classifications collapsed into one total function.** Adding a
  status variant is now a compile error in core, two lines from the array that
  must grow — not a silent mis-render in six places.
- **`Starved` reaches the operator.** It bands `Stuck`, not `Dead`
  (`starved_bands_stuck_not_dead`, `peek_tui/mod.rs:5292`) — the one status whose
  purpose is to summon the operator no longer files itself with the corpses.
- **A known cost, recorded in-code (`:1006`):** with heartbeat out of the sort
  key, a wedged worker no longer rises above the fold on its own. That signal is
  the phantom counter's and the patrol's job — a writer's, not a sort's. This is
  the trade the sort-key law demands, taken deliberately.
- **wheeler's dissent survives and is not settled by fiat.** He argued the pulse
  tier *is* safe to sort on. The synthesis resolved against him on evidence he did
  not have (he had not read `enrich_rows`; two code paths computed the tier, so it
  was not tier-stable in practice). **C2 removed exactly that objection.** His
  argument deserves a second hearing on its merits — but the clock-dependence
  survives C2 and still disqualifies it.
- **The baseline is captured and the falsifier is armed.** `docs/baselines/
  peek-20260716.snapshot` + `.md` pin the pre-redesign fleet (completed 1973,
  collapsed 1116, frozen 90, pending 16, running 4; **harvest lag 552 — 28% of live
  completed**). shannon named his own ruling's central risk: `DONE` may hide
  **completed-but-unharvested** work — *alive work wearing a corpse's status*. **The
  falsifier: if harvest lag grows against this baseline, the `DONE` bucket hid work
  the operator needed and the default is wrong** — and the fix is a `HARVEST` eighth
  bucket, whose per-row manifest reads would then be justified by evidence rather
  than assumed away. IFBDD (ADR-095) executed rather than cited.

## Falsifiers

- **`unknown_status_is_surfaced_not_filtered`** / **`row_status_parses_every_variant_of_the_one_alphabet`**
  (`peek_tui/mod.rs:5268`, `:5302`) — restoring a lossy bridge reddens them. The
  `_ => true` in `StateFilter`'s doc contract could never fire while `map_status`
  laundered unknowns into `Pending` *before the filter saw them*; these pin that it
  now can.
- **`starved_bands_stuck_not_dead`** (`:5291`) — the highest-value line in the
  deliberation, encoded. Re-filing `Starved` with the archive reddens it.
- **`sort_key_is_a_function_of_the_change_key`** (`:5429`) + `rows_differ_detects_updated_at_change`
  (`:5508`) — returning `heartbeat` to the sort key reddens them. This is the
  invariant, not a test of the current comparator.
- **`enrichment_cache_hit_renders_the_same_stall_verdict_as_a_miss`** (`:5603`) —
  moving `is_stalled_by_progress` back under the cache `continue` reddens it.
- **The worker-strip line tests** (`:6642`) — the rendered line is pinned
  end-to-end, including that the `→ cs purge` remedy disappears at `phantom == 0`.
- **`cs peek --snapshot` is byte-deterministic by contract** (`peek.rs:11-14`, the
  wheat-paste rule). Two snapshots, before and after, diffed, **is** the
  instrument — no daemon, no telemetry, no code.
- **`Phase::phase()` has no wildcard arm** (`cosmon-core/src/molecule.rs:256`) —
  adding a `MoleculeStatus` variant fails the build there, by construction. The
  absence of a `_ =>` is itself the falsifier.

## The sentence worth keeping

> *There are two `MoleculeStatus` enums. Everything else in this deliberation —
> D1, D2, D3, D4, F3, all 931 rows of it — is the relationship between them,
> misfiled as five separate bugs.* — torvalds

`is_alive()` sat in core, tested and correct, while peek re-implemented it twice
and got it wrong twice. `molecule_health(Starved) = Degraded` sat in core, unread,
while six sites in peek ruled the same molecule archived, dead, parked and failed.
The fix was less an act of design than an act of **returning four words to their
referents and deleting the struct that misdirected them**.
