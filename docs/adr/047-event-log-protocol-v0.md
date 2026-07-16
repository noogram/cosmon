# ADR-047: Event-log Protocol v0 — `*.events.jsonl` shared substrate

**Status:** Proposed
**Date:** 2026-04-17
**Parent deliberation:** `delib-20260417-7e31` (SYZYGIE)
**Blocks:** `task-20260417-5fff` (syzygie protocol chronicle),
`task-20260417-4285` (showroom inheritance chronicle)
**Child of:** `task-20260417-7b93`
**Related chronicle:** `2026-04-17-event-log-as-moat` chronicle
**Related invariant (upstream):** [ADR-046 P_legibility](046-p-legibility-axiom.md)

## Summary

> A `*.events.jsonl` file is an append-only line-delimited log of
> JSON events that **any sibling galaxy can replay without running
> the producer's code**.

## Context — the one centralization the panel allowed

The SYZYGIE deliberation (`delib-20260417-7e31`) converged on a
substrate invariant (Wheeler):

> Each galaxy's state is a projection of an append-only stream of
> human-readable events on disk, and any other galaxy can read that
> stream without running the first galaxy's code.

Niel sharpened it into a moat: *own the `*.events.jsonl` format and
its append-only single-writer invariant, or we have three SaaS with a
shared logo.* Einstein agreed the tension with his anti-ether stance
dissolves at the level of abstraction: this ADR centralizes **syntax
(serialization bytes)**, not **semantics (event meaning)**. Each
galaxy keeps its own `event_type` universe.

This is the only substrate-layer centralization the SYZYGIE panel
permitted. Everything else — principles, vocabulary, chronicles —
propagates by citation, not by shared runtime.

## Decision

### 1. File format

- **Encoding:** UTF-8.
- **Framing:** JSON Lines — exactly one event object per line,
  terminated by `\n`.
- **No trailing commas.** No multi-line JSON. No BOM. No comments.
- **Filename suffix:** `*.events.jsonl` (globbable across galaxies).
- **Whitespace tolerance:** readers MUST skip blank lines; writers
  MUST NOT emit them.
- **Error line policy:** a malformed line is an event-lint error
  (see §6) but does not prevent replay of subsequent lines.

### 2. Mandatory fields on every event

Every event line is a JSON object with exactly these top-level keys:

| Field            | Type     | Purpose                                           |
|------------------|----------|---------------------------------------------------|
| `event_id`       | string   | ULID (monotonic, 26-char Crockford base32)        |
| `timestamp`      | string   | ISO-8601, UTC, tz-aware (`…Z` suffix)             |
| `origin_galaxy`  | string   | `cosmon` \| `mailroom` \| `showroom` \| …    |
| `stream_id`      | string   | stable handle of the append-only stream           |
| `event_type`     | string   | galaxy-local type identifier (opaque to readers)  |
| `payload`        | object   | galaxy-specific body (opaque to readers)          |

Rules:

- `event_id` ULIDs provide **monotone time-ordered identity** per
  stream and cross-stream uniqueness. A reader can sort by
  `event_id` without trusting clocks.
- `timestamp` is the wall-clock time the writer committed the line.
  It is informational; **`event_id` order is authoritative** for
  ordering within a stream.
- `origin_galaxy` is an enum open to future galaxies. Readers MUST
  tolerate unknown values.
- `stream_id` is the append-only handle. Two events with the same
  `stream_id` MUST have been written by the same writer process (see
  §3).
- `event_type` and `payload` are **galaxy-local**: the spec does not
  define their shape. A reader that does not recognize an
  `event_type` skips the payload and still indexes the envelope.

Forward compatibility:

- Writers MAY add extra top-level fields. Readers MUST ignore
  fields they do not understand.
- Removing or repurposing one of the six mandatory fields is a
  breaking change and requires a new ADR.

### 3. Append-only single-writer invariant

The protocol's load-bearing property:

> **At any instant, exactly one writer process owns a given
> `stream_id`. Many processes may read. No process ever rewrites,
> truncates, or reorders lines.**

Consequences:

- Writers append with `O_APPEND`; every line is followed by `fsync`
  (or an equivalent flush to the OS page cache before returning
  success to callers).
- No in-place edits. No `sed -i`. No `>` redirection onto an
  existing `*.events.jsonl` file. Rotation (§4) is the only way to
  stop writing to a file.
- A correction to a prior event is itself **a new event** (e.g.
  `{event_type: "claim.retracted", payload: {target_event_id: …}}`).
  The log remembers errors; it never pretends they did not happen.
- Readers MAY tail a file safely while a writer appends. Partial
  final lines (observed mid-write) are re-read on the next tail
  iteration.

Multi-writer scenarios (forbidden):

- Two cosmon processes writing the same `events.jsonl` — use
  separate `stream_id`s, each with its own file, or a coordinator.
- A human editor opening an `*.events.jsonl` to "clean it up" —
  this invariant is the reason the files are readable but not
  editable. Manual edits corrupt the moat.

### 4. Rotation and compaction

- **Rotation.** A writer MAY close its current file and start a new
  one at any natural boundary (size, time, deploy). The new file
  inherits the same `stream_id` logically, but readers treat files
  as the unit of tailing. Convention: append a `.YYYY-MM-DD` or a
  sequence suffix (e.g. `events.2026-04-17.jsonl`) and keep a
  stable symlink pointing at the current one (e.g. `events.jsonl`).
- **No compaction in v0.** Aggregation, deduplication, or
  materialized views are the reader's job, not the log's. Readers
  that need a compact state replay the log into their own projection
  (the state-as-projection invariant).
- **Retention.** The log is durable by default. A galaxy MAY prune
  rotated files older than its retention window; pruning MUST
  remove whole files, never lines.

### 5. Non-goals (what this protocol is *not*)

Resisting scope creep is the whole point of this spec.

- **Not a pub/sub system.** No topics, no subscriptions, no
  back-pressure, no delivery guarantees beyond "the bytes are on
  disk after fsync."
- **Not a message broker.** No routing, no filtering, no fan-out.
  Readers tail files they already know the path of; discovery is
  out of scope (neurion handles it — see `task-20260417-4285`).
- **Not a semantic schema.** The spec says nothing about what
  `event_type`s exist or what lives inside `payload`. Each galaxy
  owns its own event taxonomy.
- **Not a daemon.** There is no event-log server. There is a file
  format and an invariant. Writers are whoever opens the file in
  append mode; readers are whoever opens it read-only.
- **Not a distributed consensus protocol.** Cross-stream ordering
  between galaxies is undefined by design. ULIDs give partial
  ordering; anything stricter requires a separate coordination
  layer the panel explicitly refused.

### 6. Falsification test — `events-lint`

The spec is useless if it is not mechanically checkable. The
falsification test is a single procedure:

> **Replay a `*.events.jsonl` stream into a read-only projection
> without running the origin galaxy's binary, and verify that every
> line round-trips.**

`events-lint` (to be implemented in `crates/cosmon-core/` and
exposed via `cs events lint <path>`) performs, in order:

1. **Structural check.** Each non-blank line parses as a JSON
   object. All six mandatory fields are present and correctly
   typed. Unknown extra fields are accepted.
2. **ULID monotonicity.** For all lines in a single file, `event_id`
   strings are strictly increasing (ULIDs encode a 48-bit
   millisecond timestamp prefix).
3. **Timestamp sanity.** `timestamp` parses as RFC-3339 with a `Z`
   or `+00:00` suffix. A timestamp that moves backwards by more
   than a configurable slack (default 5 s) is a warning, not a
   failure — clocks lie, ULIDs do not.
4. **Single-writer continuity.** Given a file, every line shares
   the same `stream_id`. (Multi-stream logs are out of v0.)
5. **Replay.** The linter ingests every line into an in-memory
   projection keyed by `event_id` and emits a count per
   `event_type`. No line is skipped except explicitly malformed
   ones (which fail the run).

A galaxy passes `events-lint` iff an external tool that imports
**no galaxy-specific code** can run the above end-to-end. That is
the mechanical proof that the substrate invariant holds.

## Alignment with existing cosmon code

- `crates/cosmon-core/src/event_v2.rs` defines the reference codec
  (`Envelope`, `EventV2`) already used across cosmon. This ADR
  elevates that schema's **public surface** (the six fields above)
  to a cross-galaxy contract; the internal `seq`/`causal_parent`
  fields remain cosmon-local extensions under the "extra fields are
  allowed" rule.
- `.cosmon/state/events.jsonl` and `.cosmon/state/interactions.jsonl`
  already obey single-writer + append-only + JSONL-merge-at-`cs done`
  (see [`../events-jsonl-merge.md`](../events-jsonl-merge.md)). v0
  documents, not invents, what cosmon's transactional core does.

## Consequences

**Gained**

- A sibling galaxy (mailroom, showroom, …) can replay a
  cosmon `events.jsonl` without linking cosmon-core. Unity invariant
  holds mechanically, not aspirationally.
- The moat is narrow (six fields, one invariant) — small enough to
  maintain unanimously across three repos, large enough that a
  drifting galaxy cannot pretend to participate without honoring it.
- Each galaxy retains full sovereignty over its `event_type`
  universe. Einstein's objection to semantic centralization is
  respected.
- P_legibility (ADR-046) gains a substrate: any agent reading the
  log needs only the six-field spec to interpret structure, even if
  it cannot interpret payloads.

**Lost / constrained**

- Writers must honor single-writer per stream. This forbids certain
  naïve patterns (e.g. parallel processes writing the same file
  without a coordinator).
- Rotation must be conservative: once a file is closed, its lines
  are immutable. Mistakes cost a corrective event, not a rewrite.
- Payload evolution is a per-galaxy discipline; there is no
  cross-galaxy schema registry. Galaxies that want to share payload
  shapes must cite a chronicle, not a central catalog.

**Open (deferred)**

- Multi-stream-per-file packing. v0 forbids it (§6 #4). A future
  ADR may relax this if a concrete cross-galaxy use case emerges.
- Cross-stream causal links. ULIDs give partial ordering; any
  stricter claim (a happened-before relation across galaxies)
  requires a separate construct.
- Signing / sealing per event. Today `P_seal` is enforced at the
  chronicle layer; per-event signatures could be layered via an
  optional `signature` field without breaking v0.

## Adoption by the three galaxies

| Galaxy        | Current stream(s)                                        | v0-compliant? |
|---------------|----------------------------------------------------------|---------------|
| cosmon        | `.cosmon/state/events.jsonl`, `…/interactions.jsonl`     | Yes, once `origin_galaxy` + `stream_id` envelope added (tracked in `cosmon-core`) |
| mailroom   | Opération Executor pulse stream (see chronicle, 2026-04-17) | Yes, by construction — see the mailroom chronicle |
| showroom    | project events (future)                                  | Must be designed to comply from day one (blocks `task-20260417-4285`) |

Migration in cosmon is additive: the existing `EventV2` schema is a
superset of v0; emitting the six mandatory fields alongside the
current ones satisfies the contract without breaking any consumer.

## References

- SYZYGIE deliberation: `delib-20260417-7e31`
- SYZYGIE chronicle: `2026-04-17-syzygie` chronicle
- Event-log-as-moat chronicle: `2026-04-17-event-log-as-moat` chronicle
- Opération Executor chronicle: `2026-04-17-operation-executor.md`
  (mailroom galaxy — cite by relative path from that repo root)
- Reference codec: `crates/cosmon-core/src/event_v2.rs`
- JSONL merge discipline: [`../events-jsonl-merge.md`](../events-jsonl-merge.md)
- P_legibility axiom: [ADR-046](046-p-legibility-axiom.md)

## The one-sentence moat

*Own the `*.events.jsonl` format and the append-only single-writer
invariant. Everything else is rentable; this is not.* — Niel,
SYZYGIE panel, 2026-04-17.
