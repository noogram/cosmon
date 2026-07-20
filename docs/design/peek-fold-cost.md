# `cs peek` fold cost — diagnosis and fix

*task-20260720-6699. Follows `docs/guides/diagnosis-discipline.md`: the seam
was instrumented and the cost measured at real scale before any explanation
was trusted.*

## Symptom

On an aged galaxy (`stagecraft`, 3 months old) `cs peek` takes ~11 s CPU. The
public cosmon (near-empty frontier) answers instantly on the same binary. The
cost grows with the age of the galaxy.

## Reproduction, at real scale

Measured against the live `stagecraft` state directory (`~/galaxies/stagecraft`):

| fixture | count |
|---|---|
| molecule `state.json` files | 871 |
| total `state.json` bytes | 2.6 MB |
| global `events.jsonl` | 80 655 lines / 21 MB |
| fleet workers, each with a `current_molecule` | **94** |

`cs peek --json` before the fix:

```
8.12 real   5.85 user   0.79 sys
```

## What actually dominates the fold — measured, not guessed

A sampling profile (`sample` on the live run) is unambiguous. The hot leaves
are all **journal deserialization**:

```
serde_json …parse_str                             279
chrono::format::parse::parse_internal             262
…Deserialize for event_v2::Envelope…              82
…Deserialize for event_v2::EventV2…               24
cosmon_core::id::WorkerId::TryFrom<String>        54
```

i.e. the time goes into parsing `events.jsonl` envelopes and their chrono
timestamps — **not** into the molecule `state.json` scan.

That refutes the starting hypothesis. The briefing supposed the cost was
"reconciling every non-archived molecule + folding the whole journal once."
The molecule reconciliation is cheap: 871 files / 2.6 MB parse in tens of
milliseconds. And the journal is not folded *once* — it is folded **94
times**.

### Root cause: an `O(W × J)` re-read, not an `O(J)` fold

`cs peek` (every mode — `--json`, `--snapshot`, and the TUI's first frame)
builds its snapshot through `peek_tui::build_snapshot` →
`energy_probe::load_worker_energy`. That function looped over every fleet
worker and, for each one, called `last_adapter_for(state_dir, molecule)` to
learn which adapter (claude / codex) had run it. `last_adapter_for` does a
full `event_log::read_all` of the global `events.jsonl` and scans it for the
last `AdapterSelected`.

So with `W` workers and a journal of `J` lines, peek parsed **`W × J`**
envelopes. On stagecraft that is `94 × 80 655 ≈ 7.6 million` envelope
deserializations to answer a question that one fold of 80 655 answers. The
worker roster grows with the age of the galaxy (heartbeat-orphan inflation —
see `reference_drainage_discipline`), so the factor `W` climbs over time and
the symptom worsens linearly in `W`, on top of the linear growth in `J`. Two
compounding linear terms multiplied together — hence "empire linéairement
(voire pire) avec l'historique."

## The fix

Fold the journal **once**. `load_worker_energy` now calls
`fold_last_adapters(state_dir)` — a single `read_all` that reduces the journal
to a `mol_id → last adapter` map (forward scan, last selection wins) — and
reads each worker's adapter from that map. The per-worker probe becomes
`probe_worker_energy_with_adapter`, which takes the already-resolved adapter
instead of re-reading the journal.

`O(W × J) → O(J + W)`. The journal is parsed exactly once per peek regardless
of worker count.

The single-molecule `last_adapter_for` is unchanged and still used by the
completion-seam capture path (one molecule, one fold — correct there).

### Result, same fixture

`cs peek --json` after the fix:

```
2.11 real   0.42 user   0.32 sys      (was 5.85 user)
cs peek --snapshot: 1.07 real         (was 7.24 real)
```

The **fold** — the mission's target — drops from 5.85 s to 0.42 s of user
CPU, a 14× reduction, well under the one-second acceptance criterion. The
~1.7 s of residual real time is I/O and `posix_spawn`/`poll` from tmux socket
enumeration (`__posix_spawn`, `poll` in the profile), a separate concern from
the fold and out of scope here.

### Non-regression guard

`energy_probe::tests::load_worker_energy_does_not_scale_with_worker_count`
builds a 64-worker fleet over a multi-thousand-line journal and asserts
`load_worker_energy` completes under a coarse ceiling that the `O(W × J)`
shape would blow through by more than an order of magnitude. It is a
structural guard (does the cost scale with `W`?), not a brittle absolute
latency. `fold_last_adapters_keeps_last_selection_per_molecule` pins the fold
semantics (last selection per molecule wins; molecules independent).

## On the proposed frontier snapshot / compaction

The briefing also proposed a materialized folded-frontier file that peek would
read in `O(live molecules)`, plus a sweep separating "archive a terminal
molecule out of the live tree" from the full `cs done` (merge + teardown).

The measurement says that machinery is **not needed to hit today's target**:
the molecule scan is not the bottleneck, and one journal fold is already
sub-second. Building a snapshot with correct incremental invalidation is a
large, error-prone surface (a stale or wrong snapshot is a *fail-wrong*, the
worst failure mode for an observer) and would not have addressed the actual
`O(W × J)` dominator at all — a snapshot of the *molecule* set leaves the
per-worker journal re-read untouched.

It remains a legitimate *future* optimization for the day the single journal
fold itself becomes the floor — i.e. when `J` alone (not `W × J`) is the cost.
Two lighter, correctness-preserving levers should come first, in order:

1. **Journal compaction / rotation.** `events.jsonl` grows without bound (21 MB
   at 3 months). Rotating terminated-molecule events into the existing
   monthly archive streams (`archive/events/events-YYYY-MM.jsonl`, ADR-030)
   would shrink the live `J` peek folds, with no new projection to keep
   coherent. This is the highest-value next step and is coherent with the
   durable archive already in place.
2. **Frontier sweep (archival ≠ `cs done`).** A completed-but-never-done
   molecule keeps its `state.json` in the live tree forever (stagecraft: 677
   completed + 161 collapsed still resident). `MoleculeData::archived` already
   exists but `list_molecules` ignores it. A `cs sweep`-style compaction that
   marks terminal molecules archived, and a `list_molecules` that skips
   archived by default, would bound the molecule scan independently of `cs
   done`'s merge+teardown — the clean separation the briefing asked for.
   Deferred here because the scan is not currently the bottleneck; captured so
   the next worker starts from the measured floor, not a guess.

Only after those, if a single `O(J)` fold is still too slow, is a materialized
frontier snapshot (with fold-complete fallback on absence/corruption —
fail-safe, never fail-wrong) worth its invalidation complexity.

Coherence: ADR-030 (durable archive) and ADR-095 (runtime) are untouched by
this change; the fix is a pure hot-path deduplication of an existing read.
