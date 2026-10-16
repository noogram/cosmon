# ADR-187: A collapse can be conditional, idempotent and receipted

**Status:** Accepted (2026-10-16) for the local surfaces; the API routes and
`cosmon-remote` follow in the next increment of issue #207.

**Decider:** Noogram.

**Amends:** [ADR-168](168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md)
§D6 (the lease guard on `cs collapse` moves into the shared operation and
reaches MCP).

**Tracks:** issue #207 (lot B1); design in deliberation `delib-20261009-8005`.

## Context

A broker that confirms a collapse with an operator and then executes it could
not do so safely. `cs collapse`, MCP `cosmon_collapse` and the V1 route took
an id and a reason only: no expected state, no caller key, nothing to read
back after a lost response, and a load-check-save that was not atomic. MCP
had its own copy of the transition and no lease guard. A lock or a read in
the broker cannot fix that, because a direct `cs` caller bypasses the broker
and the broker is not the lifecycle authority.

## Decision

1. **Revision.** A molecule's revision is `b3:<hex>`, BLAKE3 over the
   canonical JSON of its parsed `state.json`, computed on read and never
   stored. Canonicalising the parsed value keeps unknown keys (two binaries
   agree) and ignores key order (a `HashMap` rewrite does not move it). Only
   equality is compared. `cs observe --json` exposes it.

2. **One operation.** `cosmon_state::ops::collapse::execute` is the collapse
   every surface calls: `cs collapse`, MCP `cosmon_collapse` on stdio and on
   the RPP `/mcp` connector, and the V1 `POST` route through its wrapper.
   Surfaces keep only their projections (the CLI's archive and legacy event
   mirror, the renderers). Without a key it behaves as before.

3. **Critical section.** A keyed collapse runs under the molecule's dispatch
   lock, promoted to `StateStore::lock_molecule` (an adapter without one
   refuses rather than pretending): re-read the key record, the state and its
   revision; ask the `CollapseAuthority`; run the pure
   `cosmon_core::collapse_contract::decide`; on admission reserve the key
   (create-only, `link(2)`), write the collapsed state with the receipt
   embedded in one synced rename — **the commit point** — and mark the key
   committed. Log and events follow after the lock and are best-effort. The
   acknowledgement comes last.

   The unconditional mode does not take the lock. An in-process agent loop
   runs while `cs tackle` holds it; a tool call of that loop collapsing its
   own molecule would wait on its parent forever. A keyed collapse comes from
   a broker and waits out a concurrent dispatch by design.

4. **Key and digest.** The key is caller-generated,
   `[A-Za-z0-9_-]{16,128}`. The digest is
   `BLAKE3("cosmon.collapse-request.v1\0" || canonical_serialize(intent))`
   over a fixed `CollapseIntentV1` (target, exact reason bytes, cause,
   account, kind, reason kind, expected status and revision, actor, approval
   digest, `not_after`), every field present. Cosmon computes and returns it.

5. **Outcomes, first match wins:** `idempotency_key_reused` (422; also a key
   bound to another molecule), `replayed` (200; before every precondition,
   because the collapse itself moved the revision and the approval may have
   expired since), `unleased_pilot_gesture` (403), `already_completed` (409),
   `already_collapsed{by_key}` (409), `approval_expired` (412; cosmon enforces
   `not_after`), `unexpected_status` (409), `stale_revision` (412),
   `transitioned` (201). Refusals write nothing. The CLI maps them to exit
   16 and 20–25; `--json`'s `outcome` field is the contract.

6. **Receipt.** A tenant-wide record
   `collapse-receipts/<hex(BLAKE3("cosmon.collapse-key.v1\0"||key))>.json`
   (reserved → committed) plus a copy embedded in `state.json`. Hashing the
   key into the name removes traversal and case-folding collisions. No expiry:
   expiry would turn a known outcome into an unknown one. A lock-free lookup
   (`cs collapse-receipt <ID> <KEY>`) answers `committed`,
   `reserved_not_applied`, `inconsistent` or `unknown`, and
   `idempotency_key_reused` for a key bound elsewhere, without disclosing it.
   Because the state rename is the commit point, a receipt in the state with
   the key record still reserved reads as committed, and the next identical
   request finishes the key commit and announces the one transition nobody
   announced.

7. **Guard.** The `CollapseAuthority` port is checked inside the operation,
   with the caller passed explicitly: the CLI and stdio MCP resolve it from
   their own environment; a network caller is `Remote` and, in v1, cannot
   collapse a leased mission (no JWT → session mapping yet). The server's
   environment is never consulted for a network caller. The lease ledger is
   read from the state directory the collapse mutates.

## Terminal states are not made sticky in this increment

Issue #207's first decision made terminal states sticky in `save_molecule`,
gated by an audit of every write out of a terminal status: *"a legitimate
exit found there stops this item."* The audit found one.
`cosmon-runtime/src/lib.rs` `apply_freeze` moves `Completed → Frozen` by
design for formulas with `freeze_on_last_step` (called at
`lib.rs:1545` and `lib.rs:1754`). The item therefore stopped. Two other
writes out of a terminal status were found and are bugs, not exits:
`cs stuck` (`cmd/stuck.rs:28-37`, no terminal check, unlike `ops::stuck`) and
the energy-exhausted branch of `cs evolve` (`cmd/evolve.rs:608-618`). Three
read-then-write races could write a stale non-terminal copy
(`cosmon-runtime/src/lib.rs:1822`, `cmd/tackle.rs:5064`,
`cmd/tackle.rs:3263`).

Consequence: an unlocked writer holding a stale copy can still overwrite a
committed collapse. The key record survives that write, and the lookup then
reports `inconsistent` rather than `committed`: the receipt stays
historically true, but the molecule is not guaranteed to stay collapsed. A
fence on `Collapsed` alone, which the audit found no legitimate exit from, is
an operator decision for a later increment.

## Consequences

- `cosmon_collapse` now behaves as `cs collapse`: an already-collapsed
  molecule is a no-op instead of an error, the collapse writes `log.md` and
  the `EventV2` records and releases the process record, and a leased mission
  is refused.
- The V1 `POST /v1/molecules/{id}/collapse` route is unchanged in this
  increment and still runs without the lease guard (`NoLeaseCheck`, named at
  its one call site). Its keyed replacement, `PUT`/`GET
  /v1/molecules/{id}/collapse-receipts/{key}` with `deny_unknown_fields`, the
  revision on `GET /v1/molecules/{id}` with a strong ETag, and the
  `cosmon-remote` flags are the next increment. They are new routes, not new
  fields on the POST: `CollapseBody` ignores unknown fields, so an older
  server would collapse unconditionally.
- `MoleculeData` gains `collapse_receipt` (absent ⇒ none; additive under
  `STATE_SCHEMA_VERSION = 1`).
- The write-path taxonomy gains `collapse-receipts/<key-hash>.json`.

## Falsifiers

1. Two processes sending the identical keyed request produce two transition
   events or different receipt bytes
   (`cosmon-filestore/tests/keyed_collapse.rs`,
   `two_processes_with_the_identical_request_converge_on_one_transition`; it
   goes red when `FileStore::lock_molecule` returns a no-op guard).
2. A crash after the reservation or after the state write leaves a state from
   which the identical retry does not converge on exactly one event.
3. A refused keyed request changes any byte under the state directory.
