# ADR-172 — `cs done` authority is an operator-sealed capability

**Status:** Accepted (2026-08-05).
**Date:** 2026-08-05.
**Decider:** Noogram.
**Authoring task:** `task-20260727-7f01`.

**Entry artefact.** [ADR-165](165-resources-are-created-under-the-identity-that-consumes-them.md)
made the pilot and workers share one non-root uid. That removed the POSIX
ownership difference which had incidentally stood between a worker and
`cs done`, while also showing that the difference had never been a designed
authority boundary.

**Related ADRs:**
[ADR-032](032-p-external-witness-axiom.md) (the external witness),
[ADR-077](077-worker-pilot-signing-regime.md) (signing at the remote push
boundary),
[ADR-138](138-autonomous-runtime-two-loop-client-of-core.md) (autonomous
harvest),
[ADR-156](156-resident-runtime-safety-envelope.md) (human-reserved thresholds),
and [ADR-171](171-the-operator-gesture-is-a-signature-not-a-string.md) (an
operator gesture is proved by a signature, not a caller-supplied name).

---

## Context

### `done` already has non-human callers

`cs done` is one transaction with two effects: integrate a completed branch,
then tear down its worktree, session and fleet projection. The command perimeter
has never meant “a biological human must type these bytes.” It admits a sibling
shell, a transport watchdog through `cs harvest`, and the resident runtime.
Merge-before-dispatch depends on that last caller: requiring a new human gesture
for every ordinary completion would turn Autonomous back into Propelled at each
DAG edge.

The human-only claim in ADR-077 predates the runtime and is therefore too broad.
What must remain human is not the spelling `cs done`; it is the decision to
cross a threshold the operator reserved.

### Caller shape is not authority

The following signals do not establish authority after ADR-165:

- uid, file ownership, cwd and “outside the worktree” — the worker can share or
  reproduce all four;
- `COSMON_MOL_DIR` being absent — an environment variable can be unset;
- a preceding `RuntimeMergeDispatched` event — the shared state directory is
  writable, and in any case RR-5 specifies this as forensic evidence;
- `DoneToken<Human>` or `DoneToken<Runtime>` — a phantom type prevents an
  accidental in-process call, not a new `cs` process or a direct git write;
- `--by operator` — ADR-171 has already falsified free-string identity.

These remain useful perimeter and audit signals. None may be described as the
authorisation.

### The apparent three-way choice is two different questions

A capability answers **what this caller may do**. An operator seal answers
**who delegated that authority**. A broker answers **where the transaction is
executed**. Treating them as substitutes confuses authorisation with custody.

---

## Decision

### D1 — `cs done` is a transaction, not intrinsically a human gesture

An ordinary, completed, auto-harvestable molecule may be harvested without a
per-molecule human gesture. A human gesture remains mandatory when the harvest
crosses a human-reserved threshold: `hold:human`, `needs-review`, `security` or
`security:*`, `no-auto-harvest`, `harvest_to:*`, a supervised merge detent, or
an override such as bypassing a refusing gate.

This preserves both halves of the existing architecture: Autonomous can drain,
and RR-SAFE-2/RR-SAFE-5 reservations remain authority boundaries rather than
scheduling hints.

### D2 — The typed authorisation is an operator-sealed capability

The domain type is a closed sum, not a boolean and not a caller label:

```rust
/// Authority to perform one bounded harvest transaction.
pub enum DoneAuthorization {
    /// Delegation to policy for an auto-harvestable scope.
    Delegated(DelegatedHarvestCapability),
    /// One explicitly ratified human-reserved harvest.
    Ratified(OperatorHarvestSeal),
}
```

Both variants cover the same canonical `HarvestGrant`; they differ in scope.
The grant includes at least:

```text
cosmon-harvest-grant-v1
galaxy=<galaxy-id>
scope=<molecule-id | mission-id-and-policy-digest>
base=<resolved-integration-branch>
action=done
reservations=<none | exact-reservations-crossed>
epoch=<monotone-grant-epoch>
expires=<timestamp | none>
```

Every field that changes the meaning of the authority is sealed. The version
line domain-separates this signature from takeover grants, notary seals and git
commit signatures. Control characters are refused in every textual field.

- `Delegated` is normally mission-scoped. The operator approves an autonomous
  policy once; the runtime derives molecule-specific permits only for members
  of that scope that are `Completed` and carry no reservation excluded by the
  grant. A policy or base-branch change invalidates the delegation.
- `Ratified` is molecule-scoped and names the exact reservation or override it
  crosses. Approval for one reviewed molecule cannot approve its sibling, and
  approval to cross `needs-review` cannot silently become approval to skip the
  pre-done gate.

The seal follows ADR-171 custody: cosmon verifies it but ships no path that can
produce it. The signing secret is not available to an unattended process on the
host. A future implementation may use a distinct key or a domain-separated
subkey of the operator trust root; it must not reuse a plaintext host-readable
notary key.

### D3 — Authority is checked at the effect boundary

The integration path accepts `DoneAuthorization`, re-derives the current
harvest facts under the trunk lock, and consumes the grant immediately before
the first git mutation. It refuses when the molecule, resolved base, policy
digest, reservations or epoch no longer match.

Checking only when a grant is written would be advice: a same-uid process can
write the state files directly. Checking only before the trunk lock creates a
TOCTOU window. The domain decision is I/O-free; loading the grant, resolving
git/state facts, verifying the seal and taking the lock live behind injected
ports.

Consumption is append-only and idempotent. Replaying a consumed grant returns
the already-recorded outcome when the same harvest landed, and refuses if the
grant is being applied to a different effect. The `cs done` transaction and
the grant receipt share one invocation id in the ledger.

### D4 — A broker is deferred custody, not the authorisation

No merge broker is introduced by this decision. A broker that runs as the same
uid and reads the same writable repository adds a process but no boundary. A
broker that actually enforces custody needs separate repository/ref storage or
a separate principal, and must fetch the worker result into a broker-owned
integration checkout. That is the repository-isolation design already deferred
by ADR-166, not a prerequisite for naming the authority correctly.

If that broker lands, it will consume `DoneAuthorization`; it will not replace
it. The capability says *may*, the broker says *where*.

### D5 — The protocol claim is intentionally bounded

An operator-sealed capability prevents `cs done` from treating ambient process
shape as consent. It does not prevent a motivated same-uid worker from invoking
git plumbing against the shared repository and bypassing `cs` altogether.

That stronger claim requires custody outside the worker's writable substrate:
protected remote refs, a separate integration repository/principal, or both.
Until then, out-of-band mutations remain detectable through the ADR-052
provenance ledger and git/CI gates, not impossible. The implementation must say
“authorised cosmon harvest,” never “a worker cannot mutate trunk.”

---

## Rejected alternatives

1. **Keep every `cs done` as a human gesture.** Rejected because the resident
   runtime and transport harvest path are legitimate callers, and forcing a
   gesture at every DAG edge destroys autonomous drain without adding a
   boundary against direct git.
2. **A bare capability in process memory or a readable token file.** Rejected
   as the complete mechanism. The phantom catches programming mistakes and a
   file can carry the grant, but only the external seal supplies delegation a
   same-uid worker cannot mint.
3. **An operator seal for every completion.** Rejected as the default. It
   authenticates a person but provides no bounded autonomous delegation.
   Per-molecule ratification is reserved for human thresholds.
4. **A broker as the answer.** Rejected as a category error. Without separate
   custody it is theatre; with separate custody it is a valuable enforcement
   adapter that still needs an authorisation type.
5. **cwd, environment, uid or an RR-5 event as authority.** Rejected because
   each is reproducible by the caller under the shared-uid model. Keep them as
   safety and forensic signals only.

---

## Consequences

- The operator gesture moves from every ordinary merge to the bounded act of
  delegation. Human-reserved work still requires a fresh, explicit seal.
- ADR-077 §2 item 5 and §4.1 are superseded only where they call all local
  `cs done` invocations human-only. Its remote push/signing decision is
  unchanged: local integration authority and authority to publish a protected
  remote ref are separate boundaries.
- ADR-138's `DoneToken<A>` remains an in-process correctness aid. It is not the
  authorisation. RR-5 events remain forensic evidence, not credentials.
- ADR-156's monotone reservation tags become inputs to capability derivation
  and effect-time validation. Removing or adding a reservation after a grant
  changes the facts and refuses the stale grant.
- The CLI/UI parity audit is owed by the implementation molecule that adds the
  grant/challenge surface. This ADR changes no command bytes by itself.

## Implementation obligations and falsifiers

This ADR is the decision, not the command implementation. The implementation
must begin with a pure authorisation reducer and readable tests for both
variants, then place I/O behind ports.

The decision is violated if any of these is true:

1. A delegated capability harvests a molecule outside its galaxy, mission,
   policy digest or base branch.
2. A delegated capability crosses any reservation it did not name.
3. Changing a covered field after signing still verifies.
4. Deleting the trust root changes refusal into permission.
5. A consumed grant authorises a second, different effect.
6. Any shipped `cs` path can produce the operator seal.
7. A broker is later treated as authority without presenting the same typed
   grant.
8. Documentation claims this prevents direct same-uid git mutation before
   repository custody has actually been separated.
