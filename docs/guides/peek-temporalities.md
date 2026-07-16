# `cs peek` — phases

Every molecule sits in exactly one **phase**, and `cs peek` shows you the
unfinished ones by default. The phase axis is independent of
[zoom](peek-zoom.md): zoom controls the *spatial scale* (ville → immeuble
→ peau), the phase controls *which molecules* are on screen at any scale.

> **Renamed, not just re-cut.** This guide used to describe *three
> temporalities* — `running` / `future` / `past`. Those words named a
> **timeline** while the operator was asking about a **relationship**, and
> the words are gone. See [Why the timeline was the wrong
> axis](#why-the-timeline-was-the-wrong-axis).

## The six phases

A phase is a total function of a molecule's status —
[`MoleculeStatus::phase`](../../crates/cosmon-core/src/molecule.rs), one
function, no wildcard arm, living in the core beside the status enum.

| Phase     | Status              | What it means to you                                            |
|-----------|---------------------|-----------------------------------------------------------------|
| `live`    | `running`           | a worker is on it right now                                     |
| `waiting` | `pending`, `queued` | nucleated, not started — `cs tackle` it                          |
| `blocked` | `starved`           | an external authority refused service (ADR-062) — wait or rotate |
| `parked`  | `frozen`            | you shelved it — `cs thaw` brings it back                        |
| `failed`  | `collapsed`         | it blew up, and you watched it                                   |
| `done`    | `completed`         | you typed `cs done`                                              |

The last two are **terminal**: nothing you do will move them again.
Everything else is **unfinished**, and unfinished is the default view.

## CLI flags — one per axis

There are two questions, so there are two flags. **Which molecules** is the
phase axis; **which projects** is the perimeter axis. Neither flag can move
the other's axis.

| Flag             | Axis      | Effect                                                        |
|------------------|-----------|---------------------------------------------------------------|
| (none)           | —         | every unfinished molecule (`!terminal`), current project       |
| `--phase <p>…`   | phase     | select phases; repeatable and comma-separated, values union    |
| `--all-galaxies` | perimeter | every project under `$COSMON_CLUSTER_ROOT`; phases unchanged   |
| `--all`          | both      | **sugar** for `--all-galaxies --phase all`, and exactly that   |

`--phase` accepts the six phase names (`live`, `waiting`, `blocked`,
`parked`, `failed`, `done`) plus two set names: `unfinished` (the default)
and `all`. The values union, so no ordering of the same flags produces a
different view.

`--all-galaxies` is spelled the same way in `cs tail --all-galaxies`. One
word, one meaning, across the binary.

`--all` conflicts with the two flags it expands to — sugar and its expansion
are one way of saying one thing, not two ways of saying it twice. It means
all, literally, and it never narrows.

Every flag works identically in TUI mode (`cs peek`), no-tui mode (`cs peek
--no-tui`), and the byte-deterministic snapshot path (`cs peek --snapshot`).

### Examples

```bash
cs peek                              # the daily view — everything still in play
cs peek --phase unfinished,done,failed  # the above, plus the archive
cs peek --phase blocked              # only what an external authority refused
cs peek --all-galaxies               # the daily view, across every galaxy
cs peek --all                        # everything, every project
cs peek --snapshot                   # byte-deterministic snapshot of the unfinished set
```

### `--past` and `--future` are deleted, not aliased

`--past` enumerated *"completed, collapsed, frozen, and starved"* — the
archive and the parked, welded into one word. Aliasing it onto its old set
would keep a name that was created to mis-describe that set: it named a
**timeline** while you were asking about a **relationship**. The set it
delivered is still one command away, and now it says what it is:
`--phase unfinished,done,failed`.

`--future` is gone because `waiting` is unfinished and lives in the default,
so there was nothing left to opt into. A flag that does nothing still costs
a reader the time to find out it does nothing.

## Why the timeline was the wrong axis

The default used to be `running` only, and the flag that showed you a
frozen molecule was called `--past`.

The operator's 2026-04-27 rationale asked for one thing: *pending and
terminal molecules drown the daily signal — surface what is travailling,
not the archive.* The predicate that shipped was `status == "running"`.
Those are not the same set. The sentence named **the archive**; the code
removed **everything that is not running**, and the gap between them was
every frozen, starved and pending molecule in the fleet.

That gap matters because of what the rows are:

- **Completed and collapsed** are read receipts. You typed `cs done`, or
  you watched it blow up. Hiding them is **subtraction**, and correct.
- **Frozen, starved and pending** were never reported anywhere. Hiding
  them is **amputation** — and nobody complained, because the instrument
  that would have shown them is the instrument that hid them.

> **An instrument may hide what it has already told you. It may never hide
> what it has not yet told you.**

The new default cuts *harder* than the old one, not softer: it drops 917
rows where the old one dropped 928, and it keeps every signal the old one
threw away with them.

`past` was the mechanism of the concealment. It welded the archive to the
frozen and starved rows, so the default could not surface the second
without dragging in the first — and `starved`, the one status whose entire
purpose is to summon you (ADR-062: *wait or rotate, never a re-prompt*),
was reachable only as one row in 918.

## TUI cycle — `A` key

Inside the TUI, `A` cycles the phase filter:

```
unfinished → all → unfinished …
```

The status bar shows the active label (`"unfinished"`, `"all"`). Any
off-cycle filter snaps back to `unfinished` on the next press, so you
always reach a known state in one keystroke.

The cycle used to have four stops. Two of them died when the default
stopped lying: the only rows the default withholds are terminal ones, so
the only question the key can ask is whether to show the archive too.

The lowercase `a` key remains the all-projects toggle, independent of the
phase axis — two perpendicular dimensions, never conflated.

## Snapshot — byte-determinism preserved

`cs peek --snapshot` builds the wheat-paste byte stream from the underlying
[`FleetSnapshot`](../../crates/cosmon-observability/src/aggregate.rs). The
phase filter is applied as a *projection* on the snapshot's molecule set
before rendering — workers and sessions are untouched, so the WORKERS /
SESSIONS sections still list every recorded entity.

`--all` (and equally `--all-galaxies --phase all`) produces output
byte-identical to the legacy snapshot behavior on a
fixed fixture, preserving the wheat-paste contract (JR §2 of
`delib-20260422-f52c`). Same fleet state, same flags → same bytes on every
device.

## Architectural shape

The CLI flags resolve to a single
[`PhaseFilter`](../../crates/cosmon-cli/src/cmd/peek.rs) — a set over
`Phase`, threaded through four call sites:

- `peek::Args::phase_filter()` — CLI → set
- `peek::NoTuiOptions.phase_filter` — `--no-tui` baseline filter
- `peek_tui::TuiOptions.phase_filter` — TUI startup state
- `peek_tui::App.phase_filter` — runtime, mutated by the `A` cycle

The single chokepoint inside the TUI is `App::filtered_indices` — every row
is matched against `phase_filter.matches(&row.status)`. The snapshot path
applies `peek_tui::filter_snapshot_by_phase` before handing the snapshot to
`render_canonical`.

**Why a named codomain and not three booleans.** `PhaseFilter` replaces a
`StateFilter { running, future, past }` struct whose booleans were
predicates over a domain that had no name. It had eight representable
states, labelled all eight, and could reach exactly four — `running` was
hardcoded `true` on every constructor path. But the booleans were never the
defect: the missing type was. Five hand-written classifications of the same
domain lived in `cs peek` (`matches`, `matches_status`, `liveness_band`,
`row_kind`, `molecule_health_for_row`), each ending in a wildcard, each free
to disagree — and all five did, most visibly on `starved`, which one filed
as archive, another as dead, another as parked, and another as failed.

Replacing three booleans with a six-bit flag over the same unnamed domain
would have produced the identical fault in six months with more bits. Naming
the codomain is the fix; the filter is then obvious. Adding a status now
breaks the build at exactly one site — `MoleculeStatus::phase` — and the
author who adds it names its phase in the same commit.

## Intentionally not in scope

- **No `--since 7d` time-window flag.** The phase axis is *categorical*, not
  *temporal*. A duration cutoff is a separate orthogonal dimension.
- **No discrete `F` / `P` keys.** The `A` cycle is the only runtime
  mechanism, and it now has two stops.
- **No `--project <id>` selector.** The perimeter axis is currently binary
  (this galaxy, or all of them). Naming an arbitrary subset is a third
  question and would need its own flag, not an overload of `--all-galaxies`.
- **Zombie identification is not a filter question.** No arrangement of
  `PhaseFilter` identifies a single zombie; that is the worker census plus
  the patrol's orphan verdict. Do not let the phase work claim that scalp.

## Reference

- Deliberation: `delib-20260716-a2f1` — see
  [`docs/design/peek-redesign/`](../design/peek-redesign/) (`synthesis.md`
  §D-2 for the granularity divergence, §D-1 for the `--all` positions;
  `outcomes.md` §C4 for the default and §C5 for the flag axes).
- Operator verdict (2026-07-16): keep `cs peek --all` with its exact
  meaning, documented as sugar for both axes at their widest. The panel
  deadlocked 2–2 and was unanimous on the load-bearing point: silent
  narrowing is disqualified, and the literal reading is the only option that
  cannot commit it.
- Prior cut: `idea-20260503-f414`, which split the filter while the alphabet
  stayed conflated — good work on the wrong object.
- INV gate: `tests/inv/peek-default-unfinished.sh` —
  `INV-PEEK-DEFAULT-UNFINISHED` ensures the default never narrows back to
  `running`, `starved` never re-files as archive, and `phase()` never grows a
  wildcard. It replaces `INV-PEEK-DEFAULT-RUNNING-ONLY`, which guarded the
  defect.
- Source of truth: `crates/cosmon-core/src/molecule.rs` (`Phase`,
  `MoleculeStatus::phase`) and `crates/cosmon-cli/src/cmd/peek.rs`
  (`PhaseFilter`).
- Companion guide: [`peek-zoom.md`](peek-zoom.md) — the *spatial* scale
  dimension.
