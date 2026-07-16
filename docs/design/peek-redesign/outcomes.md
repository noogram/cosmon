# Outcomes — delib-20260716-a2f1

**Mode:** `decompose=recommend`. This document is a **plan the pilot executes**. No
children were nucleated by this step — the pilot owns nucleation, tackle, and done.

**Source:** `synthesis.md` (this molecule_dir). Panel: tolnay, shannon, wheeler,
torvalds, jobs.

---

## The one-paragraph ruling

The operator asked to re-cut `cs peek`'s filters. Four panelists on four unrelated axes
independently found that **the filters are not where the information is lost**: there are
two enums named `MoleculeStatus` (core, 7 variants, `#[non_exhaustive]`; observability, 6
variants, closed), bridged by a lossy `map_status` that renames `Starved` → `Stuck` and
launders `Queued` and every future variant into `Pending`. D1–D4 and F3 are the
relationship between those two enums, misfiled as five bugs. **A third filter-level cut
without fixing the alphabet re-faults a fourth time** — that is the answer to F2 and the
reason this decomposition is ordered the way it is.

---

## ⚠ One atomic question for the operator — blocks C5 only

The panel is **deadlocked 2–2** on the fate of `cs peek --all`, and it is a
design call with real trade-offs, not something a worker should decide silently.

> **Faut-il garder `cs peek --all` avec son sens actuel exact (931 lignes, toutes
> galaxies), documenté comme sucre pour `--projects all --phase all` ?**
>
> - **oui** *(recommandé)* — torvalds + jobs. On ne casse pas l'userspace. Le panel est
>   unanime sur un point : le rétrécissement silencieux est le péché cardinal, et garder
>   le sens littéral est la seule option qui ne peut pas le commettre. Coût : le mot
>   `--all` reste un quantificateur inter-axes (l'objection de tolnay).
> - **non** — tolnay + wheeler. `--all` erreure en nommant ses deux successeurs. Rayon
>   d'impact mesuré : ~24 fichiers, **tous docs/help/snapshots, zéro appelant réel**.
>   Une porte à sens unique sans personne derrière se ferme maintenant.
> - **plus tard** — on livre C1–C4, C6, C7 (qui ne dépendent pas de ce verdict) et on
>   tranche `--all` quand le nouveau défaut aura tourné.

Everything else in this decomposition is **independent of this verdict** and can proceed.

---

## Recommended decomposition — 7 children

Ordering is the ruling, not a preference. Edges are typed `--blocked-by`.

```
C0 (baseline)  ─┐
                ├─→ C4 (partition + default)
C1 (enums)     ─┤
                └─→ C6 (json)
C2 (cache)      ── independent, correctness
C3 (sort key)   ── blocked-by C2 (shares enrich_rows)
C5 (flags)      ── blocked by the operator verdict above
C7 (counter)    ── independent
```

---

### C0 — Capture the pre-redesign baseline snapshot

- **Formula:** `task-work` · **Tag:** `temp:hot` (see note) · **Blocks:** C4
- **Topic:** `peek-baseline-snapshot-before-partition`

**Note on temperature — the only genuine urgency in this dossier, and it is not a
deadline.** Every other child is `temp:warm`. This one is `temp:hot` because it is a
**measurement that the other children destroy**. Once the default changes, the
pre-redesign state is unrecoverable and Q7's falsifier becomes unanswerable forever. It
costs one command.

#### Assignment
Capture and commit a dated baseline artifact **before any other child in this
decomposition merges**:
- `cs peek --snapshot --all` → commit under `docs/baselines/peek-20260716.snapshot`
- The count of `Completed` molecules with no `Trigger::Done` in their archive manifest
  (harvest lag), recorded alongside it.

#### Context from the deliberation
shannon (Q7): peek's own doc contract makes this free. `crates/cosmon-cli/src/cmd/peek.rs`
L11-14 — *"`cs peek --snapshot` — **byte-deterministic**, fixed-width (120 cols),
ASCII-only… Output must diff to **zero bytes** across every device for the same underlying
fleet state, per the wheat-paste rule."* Two snapshots, before and after, diffed, **is**
the instrument. No daemon, no telemetry, no code.

This is IFBDD (ADR-095) executed rather than cited: *instrument before behaviour*. shannon
names the redesign's central risk as created by his own ruling — the `DONE` bucket may
hide **completed-but-unharvested** molecules (`Trigger::Done` lives in
`crates/cosmon-archive/src/lib.rs` L91-92, in the archive manifest, **not** in `RowView`,
and reading it would cost 917 per-row manifest reads). A completed-but-unharvested
molecule is *alive work wearing a corpse's status*. The falsifier: **if harvest lag grows
against this baseline after the redesign, the `DONE` bucket hid work the operator needed
and the default is wrong** — and the specified fix is a `HARVEST` eighth bucket, whose 917
manifest reads would then be justified by evidence rather than assumed away.

jobs (Q7): *"Nobody complained about the invisible frozen molecules — because the
instrument that would have shown them is the instrument that hid them. Silence from an
amputated sense is not consent."*

---

### C1 — Unify the two `MoleculeStatus` enums *(the root cause)*

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Blocks:** C4, C6
- **Topic:** `unify-molecule-status-enums-core-vs-observability`

#### Assignment
Delete `cosmon_observability::molecule::MoleculeStatus`
(`crates/cosmon-observability/src/molecule.rs` L32) and make the observability layer use
`cosmon_core::molecule::MoleculeStatus` (`crates/cosmon-core/src/molecule.rs` L110).
`peek_tui::map_status` (`crates/cosmon-cli/src/cmd/peek_tui/mod.rs` L4413) becomes the
identity function and then disappears.

If full deletion proves too large in one step, the **minimum acceptable** intermediate is:
give the observability enum `Starved` and `Queued`, add an explicit `Unknown(String)`
carrying the unrecognised wire value, and **delete the `_ =>` arm** so the
`#[non_exhaustive]` core enum forces a compile error the next time a variant is added.
Deleting the enum remains the goal; a lossless bridge is the floor.

Gates: `cargo check/test/clippy/fmt --workspace`.

#### Context from the deliberation
**Four panelists found this independently, on four unrelated axes. It is the strongest
signal the panel produced.**

The bridge today:
```rust
S::Starved => MoleculeStatus::Stuck,   // a live status renamed
_          => MoleculeStatus::Pending, // Queued and every future variant, laundered
```

**Consequence — trace one `Starved` molecule.** ADR-062 defines `Starved` as *"external
authority refused service — quota exhausted, rate-limited"*, repair = *"a wait or a
rotation; **never a re-prompt**"*. It is `is_alive() == true`, and
`crates/cosmon-core/src/reconcile/molecule.rs` L167 already rules it `Degraded`. Peek
overrules core at six sites, and they do not agree with each other:

| site | file:line | verdict |
|---|---|---|
| `map_status` | `peek_tui/mod.rs` L4423 | obs `Stuck` |
| `status_str` | `peek_tui/mod.rs` L3977 | `"stuck"` |
| `StateFilter::matches` | `peek.rs` L133 | `past` — **archive** |
| `liveness_band` | `peek_tui/mod.rs` L842 | `Dead` — **below the fold** |
| `row_kind` | `peek_tui/mod.rs` L373 | `Frozen` — *parked* |
| `molecule_health_for_row` | `peek_tui/mod.rs` L795 | `Collapsed` — *failed* |

The one status whose entire purpose is to summon the operator is filed with 917 corpses.

**shannon (Ruling 0b) — why `_ => Pending` is the worst line in the file.** It converts an
**erasure** (detectable, recoverable, cheap) into a **substitution error** (undetectable,
propagating as confident data). *"A decoder that cannot distinguish 'I do not know' from
'I know it is Pending' has no error detection at all."* And it kills the contract
`StateFilter`'s own doc comment (`peek.rs` L140-143) declares — *"Unknown future variants
are surfaced — refusing to render them silently would mean the watchdog hides molecules
whose status the binary does not understand, the worst possible failure mode for an
observer"* — because `map_status` launders the unknown into `Pending` **before the filter
ever sees it**. The careful `_ => true` can never fire.

**shannon's rate-distortion ruling, which sets this child's position in the DAG:** *"No
partition of the peek surface can recover bits that `map_status` has already thrown away.
The de-conflation must move upstream to the alphabet. Everything else is contingent on
that."*

**torvalds (F2, test 5) — why the 2026-04 fix failed:** *"`StateFilter` partitions a
6-variant closed enum that is a lossy image of a 7-variant open one and that invents a
variant with no referent. You cannot cut a thing cleanly when its shape is already wrong.
Every partition over `cosmon-observability::MoleculeStatus` is displaced **before it is
written** — the information needed to make it terminal was destroyed upstream in
`map_status`. Good work on the wrong object."*

**wheeler (§0a) — the concealment mechanism, and the transferable lesson:**
`molecule_health_for_row`'s doc (mod.rs L781-783) claims
`cosmon_core::reconcile::molecule_health` *"stays the single source of truth"* — then
feeds it a **pre-destroyed input** (`"stuck" => CoreMS::Collapsed`). *"The delegation is
nominal. peek re-implements the classification it claims to delegate, and gets a different
answer each time it re-implements. The core already knows every answer peek is getting
wrong. peek does not ask it."*

Also verify and fix if it holds (torvalds, flagged explicitly as needing verification
against `build_snapshot` before quoting): `MoleculeStatus` derives `Deserialize` with **no
`#[serde(other)]`**, so a `state.json` written by a newer binary fails to parse,
`load_molecule` returns `Err`, and `enrich_rows` L1362 (`if let Ok(mol) = …`) **drops it on
the floor silently** — the "surface unknown" policy is aspirational and the actual
behaviour is silent-drop, which is the exact failure the comment forbids.

#### Upstream / Downstream
Blocks **C4** (no partition is stable over a lossy alphabet) and **C6** (the JSON schema
must emit the core vocabulary, not the observability one).

---

### C2 — Fix the enrichment cache: a memo that changes the answer is not a memo

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Blocks:** C3
- **Topic:** `peek-enrichment-cache-silently-decides-semantics`

#### Assignment
In `crates/cosmon-cli/src/cmd/peek_tui/mod.rs`:
1. Move the `is_stalled_by_progress` call **out of the cache-miss branch** and evaluate it
   unconditionally for every running row, after enrichment, before the sort. Do **not**
   grow the cache to fix this: the function was deliberately built pure and unit-testable
   (L824-835, `#[must_use]`, no `FileStore`), and both its inputs are already cached
   (`CachedEnrichment.last_progress_at` L332; `formula_cache` keyed by path). **Zero new
   I/O.**
2. Add `trust_score` to `CachedEnrichment` (same defect, same fix).
3. Add a regression test: identical disk state must render an identical band across a
   cache-miss tick and a cache-hit tick.

**This child is independent of every taxonomy question and must not wait for one.**
torvalds: *"Fix this regardless of the taxonomy ruling."*

#### Context from the deliberation
**torvalds (Mechanism B) and shannon (Q4) found this independently and identically — the
panel's second-strongest convergence after the two enums.**

The fault, in three code facts:
1. `enrich_rows` L1348-1359: on a cache hit with matching mtime, `apply_cached_enrichment`
   runs and then `continue` — jumping past everything below.
2. The stall promotion at L1424-1434 (`row.heartbeat = HeartbeatTier::Stalled`) sits
   **below that `continue`**, i.e. on the cache-**miss** path only.
3. `CachedEnrichment` (L323-334) has **no `heartbeat` field** — ten fields, heartbeat is
   not among them — so `apply_cached_enrichment` (L717-730) never restores it, and
   `snapshot_to_rows` L3768 has already rebuilt it fresh from tmux on every tick.

**Therefore, for a running molecule stalled for 45 minutes:** cache **miss** → promoted →
`Stalled` → `LivenessBand::Stuck` → row sits high, flagged. Every subsequent **hit** →
promotion skipped → heartbeat reverts to the tmux value **which the code itself documents
as attach-bumped and lying** (L1421-1423) → `Active` → `LivenessBand::Live` → **the row
jumps back into the healthy band.** Identical disk state, two renderings; the
discriminator is whether a memoisation happened to hit.

**shannon's corollary — the sharpest sentence in the dossier:** `is_stalled_by_progress`
fires only on ticks when `state.json` just changed, *"which is very nearly the definition
of **not** stalled. The instrument built to detect the absence of motion is evaluated only
at the instants motion occurs."* Concretely: at cold start the operator sees the correct
stall picture for **exactly one frame (~250ms)**, then it disappears and never returns; in
steady state any non-evolve write (`cs whisper`, a tag edit, `cs reconcile`, an
energy-budget update) busts the cache without bumping `last_progress_at`, producing a
one-tick flash at up to 4 Hz.

**It is a defect class, not an incident.** shannon found the identical bug in
`trust_score` (set only on the cache-miss path L1393, defaults to `None` at L3805, absent
from `CachedEnrichment`) — **the TRUST column blanks itself on every cache-hit tick**.
*"The enrichment cache is a lossy compressor whose distortion was never measured — and its
dropped fields do not read as **missing**, they silently revert to a **different,
confident, wrong** value."* That is shannon's Ruling 0b (erasure → substitution) recurring
at a second layer.

torvalds: *"A memo that changes the answer is not a memo."*

---

### C3 — Sort key: remove the clock, restore the discarded timestamp

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Blocked-by:** C2
- **Topic:** `peek-stable-sort-key-updated-at-not-heartbeat`

#### Assignment
1. Add `updated_at: DateTime<Utc>` to `RowView`; compute `age: String` at render and drop
   it from the struct.
2. Sort key becomes `(band/phase, updated_at DESC, mol_id)` — all three stored,
   clock-independent. (Use `last_progress_at` for the live band if the panel's per-bucket
   refinement is adopted in C4; `updated_at` is the archive key regardless.)
3. Remove `heartbeat` from the sort key. It stays a **column and a colour**.
4. Encode the invariant as a test: **the sort key must be a function of the
   change-detection key** (`rows_differ`).

#### Context from the deliberation
**The frame's F1 was right and the operator's D4 was wrong — and the panel found the real
mechanisms anyway.** `RowView.heartbeat` is a 5-variant `Ord` enum (`Active`/`Idle`/
`Quiet`/`Stalled`/`Orphaned`, boundaries 30s/5min/30min), not a seconds counter. The sort
at L1159-1164 is already a total order and ties break deterministically on `mol_id`.
**Adding a tie-breaker fixes nothing.** The rows still dance, for three reasons:

- **(A) The sort key is a function of `now`, not of state** (tolnay, torvalds, shannon).
  `HeartbeatTier::classify` (`crates/cosmon-observability/src/session.rs` L57-71)
  re-evaluates against `Utc::now()` on every reload, so a row with a *fixed*
  `last_activity` walks Active→Idle→Quiet→Stalled unassisted and reorders against every
  peer as it crosses each boundary. **The instrument that proves it is already in the
  tree:** `rows_differ` (L735-751) compares `(mol_id, status, step)` — and `liveness_band`
  and `heartbeat` are in **neither**. So *"the adaptive poller can report `idle_ticks += 1`
  — 'nothing changed' — on the exact tick the table reorders itself."* Two contradictory
  notions of "the same fleet", 400 lines apart, in one file. **Provable by `grep`, today,
  with no telemetry.**
- **(B) The cache fault** — see C2, which this child is blocked by.
- **(C) The observer bumps the observed** (torvalds). `propel_stale_molecules`
  (`patrol.rs` L1340+) send-keys into stale tmux sessions; tmux `session_activity` is
  bumped by that write; `HeartbeatTier` derives from `session_activity`. **A feedback loop
  with period `propel_every`** (default `min(60, stale_after/5)`) — an oscillator the
  operator can time with a watch. *"The code already knows: L1421-1423 says tmux activity
  lies and `last_progress_at` is authoritative. It says so and then sorts by the liar
  anyway."*

**shannon's general rule:** *"A sort key must be monotone in time, or constant. Never sort
by a thresholded clock-derived quantity. Render it; do not order by it."*

**Death-date is free** (torvalds and shannon, independently). `snapshot_to_rows` L3783:
```rust
age: age_since(m.updated_at),
```
`m.updated_at` is **already read**, every tick, every row — then immediately stringified
and the timestamp dropped. For a terminal molecule `state.json` is never rewritten after
death, so **`updated_at` *is* the death date**. torvalds: *"The struct discards the fact
and retains its formatting. Textbook."* shannon adds why the current archive order is
worse than it looks: mol_ids are `delib-20260716-a2f1` — **kind prefix first, then date** —
so `mol_id ASC` sorts by *kind*, and `delib-*` outranks `task-*` forever; reversing to DESC
just reverses the kinds. *"The alphabet is not a clock and cannot be made into one."*
(`created_at_utc` is the wrong key — birth, and `None` until enrichment.)

**Recorded divergence — wheeler dissents and should be read before this ships.** wheeler
argued pulse *is* safe to sort on: `Active`/`Idle`/`Quiet` collapse into one pulse value,
so the per-poll tiers never cross a band boundary, and the crossing edges (`Quiet →
Stalled` at 30min, `→ Orphaned`) are hysteresis-stable. The synthesis resolves this
**against** him on evidence he did not have — he did not read `enrich_rows`, and his
argument assumes `heartbeat` is a clean function of `last_activity`. It is not: two
different code paths compute it (C2). **The tier is not tier-stable in practice.** If C2
lands first and the promotion becomes deterministic, wheeler's argument deserves a second
hearing — but the clock-dependence (A) survives C2 and still disqualifies it.

---

### C4 — The partition and the default

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Blocked-by:** C0, C1
- **Topic:** `peek-default-shows-unfinished-not-running`

#### Assignment
Replace `StateFilter { running, future, past }` with the partition the panel converged on,
and change the default from `== running` to `!terminal`. Update `cs help` + `man cs` in the
same change (CLI doc-sync discipline). Update the CLI/UI parity audit.

**The panel converged on the default and diverged on the granularity — the implementer
must read the divergence in `synthesis.md` §D-2 before choosing.** The three positions are
minimal along *different* axes and are **layered, not opposed**: wheeler minimises
vocabulary (one new word, `settled`), torvalds minimises classification sites (five
hand-written tables → one total function), shannon minimises wasted channel (bits per row).
A design can carry a one-bit *filter*, a `Phase` enum for *ordering*, and shannon's
action-classes as *row grouping* — three layers, three answers.

**Recommended resolution (synthesis §D-2):** take torvalds' named codomain as the
mechanism, wheeler's naming constraint as the governor, shannon's action-classes as the
grouping. Tie-break rationale: torvalds' claim that five tables already disagree is
**verified by four panelists' independent traces of the same `Starved` row**; wheeler's
claim that any partition must re-solder is an induction from two instances. **Verified
beats inferred.** But wheeler's *"every word this design needs already exists in this
repository — they were pointed at the wrong things"* must govern the naming.

#### Context from the deliberation

**The default — all five agree it is `!terminal`, not `== running`.**

jobs' confession is the reason, and it is the cleanest statement of the bug in the dossier:
> *"`default_watchdog()` is my fingerprint. It is a clean, confident, one-line
> subtraction, and it is the bug. The operator said (2026-04-27, quoted at `peek.rs`
> L86-88): 'pending and terminal molecules drown the daily signal — surface what is
> travailling, not the archive.' The operator named **one** thing to remove: **the
> archive**. The code removed **everything that is not `Running`**. Those are not the same
> set. The gap between them is exactly five frozen molecules and twenty-seven orphans. The
> predicate that shipped is `status == "running"` — a **storage** predicate. The sentence
> asked for `status != terminal` — an **attention** predicate. D3 is that crack. My axis
> dug it."*

And: *"Good subtraction on the wrong noun is worse than no subtraction, because it looks
finished."*

**The line governing what may be hidden** (jobs, answering the frame's hardest question):
> **An instrument may hide what it has already told you. It may never hide what it has not
> yet told you.**

- Completed/collapsed (917) — read receipts; `cs done` was typed or it blew up and was
  seen. Hiding them is **subtraction**. Correct.
- Frozen (5) — nobody was ever told. *"On a shelf, in a room with the lights off."*
  **Amputation.**
- Orphans (27) — the fleet is *actively asserting a falsehood*. **Amputation with an
  alibi.**
- Pending (6) — an unpaid debt. *"A fleet that hides its own backlog is how backlogs reach
  900."*

**Note the ruling cuts harder than the status quo, not softer.** Today's default removes
928 of 931 rows and loses the signal; this one removes 917 and keeps every signal there is.
*"The clean version and the honest version are the same version. They usually are, when you
have the noun right."*

**wheeler's counterpart, arriving at the same place from naming:** the bit already exists
in core, named, tested, and unread by the observer — `crates/cosmon-core/src/molecule.rs`
L139-149, `is_terminal()` / `is_alive()`. *"peek re-implemented this predicate twice — as
a boolean, then as a three-way struct — and got it wrong both times, while the correct
one-line version sat in core with tests on it."* And the true statement of D2: *"`past`
solders the 917-that-must-be-hidden to the 11-that-must-be-shown, so the default cannot
show the 11 without dragging in the 917."*

**wheeler's naming rulings — binding constraints on this child:**
- **`archive` — rejected, live collision.** `crates/cosmon-archive/src/lib.rs` L90-100
  defines `Trigger { Done, Collapse, Freeze, Stuck }`. **`cs freeze` and `cs stuck` both
  write to the archive.** A bucket named `archive` that *excludes* frozen contradicts a
  shipping crate that *archives* frozen.
- **`parked` — rejected, re-solders D2 inside the fix.** It fuses `frozen` (shelved by
  design) with `starved` (held off the shelf against its will, self-healing on quota
  refresh). *"The very fault we are convened to remove, re-committed in the fix."*
- **`live` — rejected, begs the question** core already answers differently (`is_alive()`
  includes frozen).
- **The physics register is FORBIDDEN here, and the frame was wrong about this.**
  `docs/vocabulary.md` already rules peek **vernacular by charter** (delib-20260416-745e):
  *"Everything in cosmon is named after physics — except the one command you use most.
  Because looking should be easy."* Status **values** stay physics (they name molecule
  state); filter **flags** are vernacular (they name a human gesture). wheeler killed his
  own `--ground`/`--excited` proposal on exactly these grounds — *"genuine physics,
  genuinely correct, and therefore a violation; killed on principle, not on taste."*

**torvalds' mechanism — name the codomain:**
```rust
// crates/cosmon-core/src/molecule.rs — beside MoleculeStatus, not downstream of it.
pub enum Phase { Live, Waiting, Blocked, Parked, Failed, Done }
impl MoleculeStatus {
    pub fn phase(self) -> Phase {
        match self {
            Self::Running   => Phase::Live,
            Self::Pending | Self::Queued => Phase::Waiting,
            Self::Starved   => Phase::Blocked,   // ADR-062: alive. Rotate. Never re-prompt.
            Self::Frozen    => Phase::Parked,
            Self::Collapsed => Phase::Failed,
            Self::Completed => Phase::Done,
            // NO wildcard arm. This is the point.
        }
    }
}
```
*"`#[non_exhaustive]` is a promise downstream, never a shield upstream.*  Adding a variant
must break the build **at exactly one site**, and the author who adds it names the bucket
in the same commit. Today, adding a variant breaks nothing and silently mis-renders in six
places." And on why bitflags are not the fix: *"Replace three booleans with a six-bit
bitflag over the same unnamed domain and you get the identical fault in six months with
more bits. Name the codomain first."* (Note the type's current incoherence: 8 states,
`label()` names all 8, `from_flags()` reaches exactly 4 — `running` is hardcoded `true` on
every path. *"Three flags in a trench coat."*)

**shannon's grouping — bucket = equivalence class under the operator's next action**,
maximising `I(row ; next_action)`. **LIVE · ALARM · READY · BLOCKED · PARKED · DONE ·
UNKNOWN**, total and disjoint. Key rulings inside it:
- *"A row that cannot change what you do next carries zero bits **no matter how much it
  differs from its neighbours in every other column**."* The 917 are entitled to **one
  line, not 917**. The metric condemning `--past` is not its size but its **cost per bit**.
- **`running` is the only status that splits, and the reason is principled:** it is the
  only symbol carrying a **noise term** — `state.json` claims progress, tmux reports
  reality, and they can contradict. Every other status is self-certifying. **ALARM is the
  bucket for "the noise won."**
- **Two free bits, currently discarded at the last step.** `RowView.blocked_by` already
  carries each blocker *with its status* (populated at `enrich_rows` L1371-1381, already in
  `CachedEnrichment`) → READY vs BLOCKED. *"Refusing a free bit is the only unforgivable
  move on this axis."* And **peek never reads `stuck_at`** — so a worker wedged on a
  missing prerequisite and a molecule you deliberately shelved render **identically** as
  `frozen`. Readable from the `load_molecule` call `enrich_rows` already makes: **zero new
  I/O**.
- **DONE is not split** — `Completed` (809) and `Collapsed` (108) differ forensically but
  demand the same action (none), so they are one class. *"A bucket is an attention
  allocation; a column is a fact."* The ratio goes on the collapsed line:
  `DONE 917 (809 ✓ / 108 ✗)          cs peek --done`
- **UNKNOWN earns a bucket at population zero.** Maximum surprise ⇒ maximum bits. *Blocked
  on C1* — until `map_status` stops laundering unknowns into `Pending`, this bucket is
  unreachable by construction.
- **The collapsed line MUST advertise its own flag.** *"If the archive is hidden without
  advertising its door, the operator cannot generate the retransmission request that would
  falsify the design. Hiding without an advertised door is not compression — it is
  censorship, and it destroys the evidence that would prove it wrong."*

**`stuck` — the wording (synthesis §D-4).** Convergent core: **the string `"stuck"` must
stop meaning `Starved`.** Divergent: what it means afterwards. wheeler's is the only
ruling grounded in a distinction **the runtime already acts on** —
`cosmon-runtime/src/resident.rs` L484 discriminates `"frozen" => m.stuck_at.is_none()`,
because a *delivered* freeze releases its dependents and a *stuck* freeze does not:
> **`stuck` := `frozen` ∧ `stuck_at.is_some()`**
>
> *"The scheduler already treats these as two different things. peek renders them as one
> word."* The verb `cs stuck` already exists, is inscribed as an operator verb, is
> vernacular, and the operator already types it. *"The word was never wrong. It was
> attached to the wrong thing."*

Band renaming is **open** (shannon: `Unresponsive`; tolnay: `Adrift`; jobs: keep `Stuck`
for the alarm meaning). Pick one and only one — the risk here is minting a sixth meaning
while fixing the first five.

#### Upstream / Downstream
Blocked-by **C1** (the alphabet) and **C0** (the baseline, which this child destroys).

---

### C5 — Decouple the flag axes *(blocked on the operator's verdict above)*

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Blocked-by:** C4 + operator verdict
- **Topic:** `peek-decouple-perimeter-from-temporality-flags`

#### Assignment
Give perimeter and temporality one flag each. **Spelling is not a free choice:** `cs tail
--all-galaxies` already ships (`crates/cosmon-cli/src/cmd/tail.rs` L38) — do not mint
`--all-projects` next to it. One word, one meaning, across the binary. Update `cs help` +
`man cs` + the parity audit in the same change.

`--past` is **deleted, not aliased**, and the panel is unanimous on the reason: its doc
string (`peek.rs` L236-238) enumerates *"completed, collapsed, frozen, and starved"* — the
archive and the parked, welded. tolnay: *"Do not alias a name onto a set it was created to
mis-describe."* jobs: *"D2 is not a filter bug, it is a **naming** bug — the bucket was
named after a **timeline** when the operator was asking about a **relationship**. You
cannot fix that with a better sort. You fix it by taking the word away."*

`--future` dies too: pending is unfinished and now in the default, so there is nothing left
to opt into. jobs: *"Two flags die so that eleven rows can live."*

#### Context from the deliberation
**The `--all` verdict is the operator's** (see the atomic question at the top). Both
positions are recorded in `synthesis.md` §D-1. **The panel is unanimous on one point and
it is the load-bearing one: silent narrowing is disqualified.** jobs: *"The moment `--all`
returns something that is not all, the operator can never again trust **any** peek output,
because they now know the tool has opinions about what they meant. You do not get to make
that trade twice in one file."*

**tolnay's F2 property — the design rule this child must satisfy regardless of the
verdict:**
> **A surface is terminal when no flag's meaning is a function of how many axes the command
> has.**

*"`--all` means `⋀ over all axes`, so its extension is recomputed every time an axis is
born. It is not a flag; it is a fold over the surface, evaluated at authorship time and
frozen into a name that keeps promising the fold. `show_all_states` failed this in 2026-04
with two axes. `--all` failed it identically with two axes wearing different names. Both
cuts removed a **symptom** — the specific weld — and left the **quantifier** standing, so
the quantifier re-welded at the next axis. **The cut removed the conjunction and kept the
`∀`.**"*

And the honest half, which is why "keep it as sugar" is defensible: *"Periodic re-cutting
is not avoidable — but it is supposed to happen one dimension at a time, and it is supposed
to be cheap. Under the rule above, a third axis costs exactly one new flag and changes the
meaning of zero existing ones. That is the difference between growth and churn."*

**The operational test, mechanical, one minute per flag:**
> **For every flag on `cs peek`, name the single axis it moves. If the answer needs the
> word "and", delete the flag.**

tolnay flags a clump this test already condemns and explicitly leaves out of scope:
`--snapshot`, `--no-tui`, `--follow`, `--once` — *"four booleans and two `conflicts_with`
clauses encoding one enum."* **That is a separate molecule; do not absorb it here.**

---

### C6 — Make `cs peek --json` real *(provisional — one panelist, no cross-check)*

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Blocked-by:** C1
- **Topic:** `peek-json-emit-raw-status-no-bucket-field`

#### Assignment
`cs peek --json` today **does not exist**: `peek::Args` (L180-255) has no `json` field, and
neither `peek.rs` nor `peek_tui/mod.rs` reads `ctx.json`. `--json` is a *global* clap flag
(`crates/cosmon-cli/src/main.rs` L50-51), so `cs peek --json` parses, is ignored, and
launches the TUI. On a pipe it hits `peek_tui/mod.rs` L186, which prints an error advising
the operator to *"run `cs peek --json`"* — **the escape hatch points at itself.**

Implement it, emitting **raw core `status`**, `heartbeat`, `last_activity` — and **no
`bucket` field**. Also: `docs/guides/inbox-trial.md` ships a pipeline
(`cs peek --json | jq '.workers[] | .energy…'`) against a schema that has never existed —
**delete or rewrite the guide's pipeline against `molecules[]`; do not grow a schema to
make a broken doc true retroactively.**

**⚠ Provisional.** Per the synthesis's R1 coverage table, **Q6 was addressed by tolnay
alone; four of five panelists were silent on it.** The silence is explained (there was no
contract to have opinions about), and the ruling's *premise* — the two enums — carries
four-way corroboration. But its *conclusion* has no cross-check. **A second reader should
check the schema before it ships.** Adding a field later is additive and breaks nobody;
this is the cheap direction to be wrong in.

#### Context from the deliberation
**tolnay's ruling and its rationale:**
- **No `bucket`, under any name.** *"The bucket taxonomy is the object under active
  redesign **in this very deliberation**. It has been recut at least twice already.
  Publishing `bucket` freezes, as a machine contract, the one artefact with a demonstrated
  recut cadence. Every subsequent improvement to the operator's taxonomy becomes a breaking
  change to every consumer. Blast radius: **unbounded and recurring** — it renews at each
  recut, forever, and it is precisely the improvements we want that trigger it. Are we
  willing to maintain `"bucket": "archive"` forever? We are changing that word in this
  document. Then do not ship it."*
- **`status` is already contract** — `crates/cosmon-remote/tests/wire_contract.rs`,
  `crates/cosmon-thin-cli/tests/parity_with_cs.rs` (which asserts `cs_out["status"] ==
  thin_out["status"]`), `cs observe --json`, `crates/cosmon-state/src/ops/stuck.rs` L233.
  Emitting it adds **zero new surface** and is strictly richer: *"`(status, heartbeat)`
  reconstructs the bucket in eight lines; `bucket` cannot reconstruct `status`, because
  `archive` erases the difference between **finished** and **failed**. Publishing the lossy
  derivative and withholding the source is backwards."*
- **The ADR-068 parity trap this closes:** with `map_status` in the path, `cs peek --json`
  would report `"stuck"` for a molecule `cs observe --json` reports as `"starved"` — *"two
  commands, one molecule, two answers — a live equivalence violation, currently invisible
  only because peek emits no JSON."* **This is why C1 blocks this child.**
- **Why `heartbeat` is admitted when `bucket` is not** — *"freeze what is settled,
  withhold what is in flight."* `HeartbeatTier` is settled: five variants, fixed
  thresholds, shipped, `Ord` already load-bearing. It is not what this deliberation is
  cutting, and a consumer re-deriving it from `last_activity` would re-derive it wrongly.
  `last_activity` rides along because `heartbeat` is lossy for anyone wanting their own
  thresholds.
- **Unknown future status serialises as its own snake_case name, unmapped. Never
  `"pending"`.** *"Reporting a molecule the binary does not understand as **pending** is a
  fabricated fact, not a fallback."*
- **The asymmetry that makes withholding cheap:** *"Adding a JSON field later is additive
  and breaks nobody. Removing or renaming one breaks everyone. So the default is omission
  and the burden falls on the field. If a consumer ever appears who needs `bucket`, name
  them and add it in a minor."*

---

### C7 — Surface the phantom-worker discrepancy

- **Formula:** `task-work` · **Tag:** `temp:warm` · **Independent**
- **Topic:** `peek-phantom-worker-counter-in-header`

#### Assignment
Add one always-on fleet-level line to the peek header/status bar:
```
workers: 30 registered · 3 attached · 27 phantom → cs purge
```
**Zero new reads** — peek already walks the roster (`snap_find_worker`, `list_sessions`,
`peek_tui/mod.rs` L3739-3747). Never a row. **Boundary held: peek reads and renders; `cs
purge` writes and drains.** The counter tells the operator to run purge; it never runs it.

#### Context from the deliberation
**Three panelists converged (jobs, torvalds, wheeler), and two of them insist on the
disclaimer below.**

jobs: *"What the operator needs to know is: **the fleet is lying to me.** 30 workers
registered, 3 alive. That is not 27 rows of information. **That is one number, and the
number is 27.**"* And: *"The instrument's job is to show the **discrepancy**, not the
**inventory**. A fuel gauge does not enumerate the molecules of petrol. **Readings are free
and always on. Queries cost a keystroke.**"*

Why it belongs in the default rather than in a future `cs workers`: *"The 27 phantoms are
the single hardest-earned fact in the entire ground truth, and there is currently **no
gesture in the product that surfaces it at all**. Not `--all`. Not `--past`. Nothing. It is
invisible at every setting of every flag, because it is not a molecule and every flag we
have filters molecules. **That is not a missing feature. That is a sense organ the product
never grew.**"*

torvalds on why the boundary objection fails: *"'peek is a molecule projection' is not a
real objection: peek is a **fleet** projection. The fleet contains workers. Peek already
renders `worker_name` per row and role glyphs. Rendering a **count** of the same objects it
already reads is not a boundary crossing."* And why they can never be rows: *they have no
`mol_id`, and `snapshot_to_rows` is keyed by `mol_id` (L3736) — synthesised rows would
collide or corrupt the key.*

**⚠ The disclaimer both torvalds and wheeler demand — carry it into the ADR:**
> **The operator's "identify zombies" requirement is NOT met by the taxonomy redesign.** It
> is met by this counter (for *workers*) plus the patrol's orphan verdict (for
> *molecules*). **Neither is a filter question. No arrangement of `StateFilter` —
> booleans, bitflags, or `Phase` — identifies a single zombie. Do not let the taxonomy
> ruling take credit for it.**

wheeler: *"`orphaned` on the **molecule** axis (running-in-state, no tmux) is a different
animal from a stale worker-roster entry. That is roster hygiene. **Do not let the taxonomy
redesign absorb it** — that is how the partition grows a fourth arm and faults a fourth
time."*

**A related ruling that belongs to C4, not here** (torvalds, Q3): the one genuinely derived
fact worth *sorting* by is `Orphaned`, and **`Phase` alone cannot see it**. His rule: *"If a
fact matters enough to sort by, someone must write it."* The patrol **already is** that
writer — `project_liveness_onto_process(store, &mid, Liveness::Dead, stale_after)`
(`crates/cosmon-cli/src/cmd/patrol.rs` L1363, *"the patrol is the writer of last resort"*).
**Peek reads that verdict and sorts on it as a stored fact.** Peek does not
derive-at-render-for-sort and does not become a writer. ADR-028 (pure projection) and
ADR-052 (one ledger, one writer) both hold. **No new daemon — the writer already exists.**

---

## Also recommended: an ADR

The synthesis converges hard enough to warrant a durable record. Recommended:
**`docs/adr/NNN-peek-one-alphabet-one-partition.md`**, capturing:
1. **The two-enums root cause** and the ruling that the observability enum is deleted.
2. **tolnay's terminality property** — *no flag's meaning may be a function of how many
   axes the command has* — as a standing rule for the CLI surface, with the one-minute
   test.
3. **torvalds' five tests for a terminal split**, especially test 5: *the domain must be
   the truth, not a projection of it.*
4. **shannon's sort-key law** — *never sort by a thresholded clock-derived quantity* — and
   the invariant *the sort key must be a function of the change-detection key.*
5. **jobs' instrument line** — *an instrument may hide what it has already told you; it may
   never hide what it has not yet told you.*
6. **wheeler's register ruling** — peek's flags are vernacular by charter; the two registers
   do not mix (cross-ref `docs/vocabulary.md`, delib-20260416-745e).
7. **The C7 disclaimer**: the taxonomy redesign identifies zero zombies.

The ADR should be written **after C1 lands**, so it records what was done rather than what
was intended.

---

## Q6 follow-up (open, low priority)

`docs/guides/inbox-trial.md` documents a `cs peek --json | jq '.workers[]'` pipeline
against a schema that never existed. Whoever takes C6 should either delete that pipeline or
rewrite it against `molecules[]`. Recorded here so it is not lost: **it is the only
would-be consumer of peek JSON in the repo, and it asks for a key that has never been
emitted.**

## Out of scope, recorded so it is not lost

tolnay's one-minute test already condemns a second clump on `cs peek`: `--snapshot`,
`--no-tui`, `--follow`, `--once` — *"four booleans and two `conflicts_with` clauses
encoding one enum."* He explicitly declined to rule on it (*"not my seat today"*). **This
is its own molecule.** Do not absorb it into C5.
