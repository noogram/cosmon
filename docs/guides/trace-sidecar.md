# Trace sidecar

`scripts/trace-sidecar.sh` produces an always-on, compile-independent record of
a cosmon polymer (a root molecule and every node reachable from it through
foaming, merge, and DAG-gate edges). It exists so that **even if the gate DAG
stalls, a worker dies, or the Rust build itself is broken**, a third party can
reconstruct what actually ran.

It was born from COSMON-DEV #20, where the `claude` adapter would not run inside
a root container at all. A tracer whose job is to explain a failure must not
depend on the thing that failed — so this is a shell + `python3` sidecar over
the append-only event log and on-disk molecule state, not a Rust organ. This is
the "Rust où il pèse — pas où il étouffe" rule: pure host-side plumbing, no
logic core, so no compile dependency is imposed on a diagnostic tool.

## What it emits

Into `--out` (default `<state>/fleets/<fleet>/molecules/<mol>/trace/`):

| File          | Content                                                                    |
|---------------|----------------------------------------------------------------------------|
| `events.jsonl`| Append-only, deduped-by-`seq` copy of every `EventV2` touching the polymer. |
| `briefs.md`   | Each node's germinated brief: topic, formula, kind, status, tags, links.    |
| `hashes.tsv`  | `mol_id · rel_path · bytes · sha256` for every artifact any node wrote.      |

`events.jsonl` is strictly append-only across re-runs (new `seq`s only).
`briefs.md` and `hashes.tsv` are regenerated snapshots of current state.

## Guarantees

- **Read-only on `.cosmon/state/`.** It only reads state files and hashes
  artifact bytes; it writes exclusively under `--out`. It never mutates state.
- **Independent of DAG completion.** It reads whatever exists the instant the
  polymer germinates; a not-yet-germinated or cross-galaxy member is recorded as
  a link-only node (its absence is itself evidence).
- **Idempotent.** Safe to run at any cadence, including repeatedly after a crash.

## Usage

```sh
scripts/trace-sidecar.sh --mol <root-molecule-id> [--fleet default] \
  [--state <.cosmon/state>] [--out <dir>]
```

`--state` defaults to a walk-up from the current directory; `--mol` may also come
from `$COSMON_MOLECULE_ID`. Run it as a tick from a supervisor, or by hand.

## Polymer membership

Membership follows provenance and progression edges only: `DecayedFrom` /
`DecayProduct` (foaming), `MergedFrom` / `MergedInto` (merges), and `Blocks` /
`BlockedBy` (the DAG gate edges the resident runtime schedules on). `Refines`,
`Refutes`, and `Entangled` are semantic citations, not membership, and are
excluded.

## Tests

`scripts/trace-sidecar.test.sh` builds a synthetic two-node polymer and asserts
capture, exclusion of unrelated events, append-only behaviour, correct hashes,
and read-only-on-state.
