# ADR-185 — Turn and effect evidence is durable before it is acted on

**Status:** Accepted (2026-10-03).  
**Decider:** Noogram.  
**Amends:** [ADR-102](102-cosmon-agent-harness-and-agentloop-port.md), [ADR-184](184-provider-termination-truth.md).  
**Tracks:** issue #151, unit W8.

## Context

The in-process loop keeps its conversation in memory. A process killed after the
model answered, or after a tool had run, lost the native envelope, the call
identities, the work-turn input that had been delivered, the budgets already
spent, and any text received before a limit. A restart began from a fresh log
with fresh budgets and no way to tell whether a tool effect had happened.

ADR-167 makes the molecule journal a projection of the fleet ledger, and
ADR-052 gives that ledger intent and receipt records with one writer. The
recovery evidence therefore must not become a second lifecycle log.

## Decision

The loop writes typed records through a port, in an order that carries the
guarantee:

1. `attempt_started` carries the ceilings and pins (tool registry digest,
   briefing digest, adapter, requested model) before the first request.
2. `request_intent` precedes network I/O. A request with no later
   `assistant_received`, `request_failed` or `terminal` may have been billed.
3. `assistant_received` makes the provider-native envelope durable before any
   of its tools can run.
4. `tool_intent` precedes the effect. If it cannot be written, the tool does
   not run. An intent with no receipt is an effect of unknown outcome.
5. `tool_receipt` carries the classified result and precedes the next call.
6. `checkpoint` marks a complete tool-result boundary and stores the native log.
7. `terminal` stores the disposition and the text received before it.

Records are ledger rows (`harness_turn_recorded`) holding counts, call
identifiers and digests. Raw content lives in immutable blobs under the
molecule directory, named by their SHA-256 digest, written by temporary file and
rename, and validated by length and digest on every read. A blob has no status;
the ledger owns ordering. Each provider encodes its own envelope and log, so the
two native schemas stay non-isomorphic.

The domain core defines the records, the `TurnEvidenceStore` port and a pure
`reconstruct` fold that refuses a sequence breaking the ordering above. The
state crate implements the port over the ledger writer and the blob directory.
The harness depends only on the port. A write failure stops the loop with a
typed error before the next side effect; a failed receipt is reported as an
unconfirmed effect because the tool had already run.

The history id of a turn record is the id of the attempt's usage records, so
the usage accounted before a restart is traceable from the same attempt.

## Consequences

A killed attempt can be rebuilt from disk: native call identity, delivered input
keys, retained partial text, spent budgets, compactions, the last checkpoint and
the list of unresolved effects and possibly-billed requests. Damaged or missing
blobs are reported next to the surviving records instead of discarding them.

This decision does not resume anything. Resuming from a checkpoint, refusing to
repeat an unresolved effect, and re-admitting a changed formula or model are
separate work that consumes this evidence. An unresolved shell effect is
reported as unresolved and never classified as failed.

Blobs hold the native conversation, so they inherit the access of the molecule
directory and are not published. A blob larger than the store's cap is refused
and stops the loop. The `local` floor's loop has no progress channel and does
not write turn evidence yet.
