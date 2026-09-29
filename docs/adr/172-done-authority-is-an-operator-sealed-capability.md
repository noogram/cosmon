# ADR-172 — `cs done` authority is an operator-sealed capability

**Status:** Accepted (2026-08-05). Amended 2026-09-29 (issue #120, see the
last section).
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

## Implementation status

Gaged by `task-20260901-6da6` (C5 of `delib-20260819-cda2`), 2026-09-01.

- `cosmon_core::harvest_authorization` — `DoneAuthorization`, `HarvestGrant`
  and the `cosmon-harvest-grant-v1` canonical encoding, `GrantEpoch`,
  `HarvestScope`, the reservation vocabulary of §D1, and the I/O-free
  `authorize` reducer.
- `cosmon_filestore::harvest_authority` — the pinned trust root
  (`$COSMON_HARVEST_PUBKEY`, then `<galaxy>/.cosmon/harvest.pub`, then the
  galaxy's `takeover.pub` as the domain-separated subkey §D2 permits), the
  monotone epoch, the sealed-grant store, and the append-only consumption
  ledger.
- `cosmon_cli::cmd::done_authority` — the §D3 effect boundary. It takes the
  trunk guard as a parameter so "call it before locking" is a compile error,
  re-derives every fact there, and appends the receipt before the first git
  mutation.

Two things this molecule deliberately did **not** do:

- **No grant/challenge command surface.** The operator seals the canonical
  bytes out of band with stock `minisign` and drops the result under
  `.cosmon/state/harvest/grants/`. The parity audit this ADR anticipates is
  therefore still owed by the molecule that adds that surface, and no command
  bytes changed here.
- **Not on by default.** `[harvest_authority] required` in the galaxy's
  tracked `.cosmon/config.toml` turns it on. A fail-closed mechanism shipped
  as a default would refuse every harvest in every galaxy that had not yet
  pinned a key. Once on, §D4 holds exactly: deleting the trust root stops
  harvests instead of unlocking them.

Falsifier 6 — the central one — is asserted by
`cosmon-cli/tests/done_authorization_unforgeable.rs`, alongside a check that
the refusal text keeps the §D5 claim bound.

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
6. ~~Any shipped `cs` path can produce the operator seal.~~ **Replaced by
   the 2026-09-29 amendment:** *a shipped unattended beneficiary, worker or
   service path can mint accepted authority without independent operator
   signing custody.* The current assertion
   (`cosmon-cli/tests/done_authorization_unforgeable.rs`) stays in force
   unchanged until the unit that ships operator-side signing tooling
   replaces it with denied-signing, denied-import and admin-route tests plus
   a custody witness; unrelated takeover-signing assertions are not weakened.
7. A broker is later treated as authority without presenting the same typed
   grant.
8. Documentation claims this prevents direct same-uid git mutation before
   repository custody has actually been separated.

## Amendment (2026-09-29, issue #120 W0) — operator-side tooling, scoped remote policy, and what the effect boundary must truthfully claim

**Status:** adopted as contract; **not implemented**. The implementation
status section above stays accurate for the code as it is. The contract
these clauses belong to is
[`docs/specs/remote-harvest-contract.md`](../specs/remote-harvest-contract.md).

**Why.** An external operator of the remote service reported that the sealed
remote `done` cannot be completed from shipped tooling: cosmon verifies a
grant and ships no production path to create the key, the challenge, the
signature or the installed grant. The first custody sentence of D2 ("ships no
path that can produce it") made that gap a doctrine. The deliberation behind
#120 kept the seal's purpose — bounded authority that possession of the API
credential cannot create — and moved the prohibition to where the purpose
lives: the unattended beneficiary.

**D2 custody, replaced.** In sealed policy, authority is independently
issued. Operator-side cosmon tooling may construct challenges and orchestrate
an external signer on an operator device. The unattended beneficiary, the
worker and the service receive no private signing material and expose no
signing operation. Trust-root administration (public key, policy, epoch) is
separate from tenant harvest authority and uses the existing disjoint admin
credential. The sealed sum stays closed: the new remote `scoped` policy is
its own typed decision on the remote path, not a third `DoneAuthorization`
variant.

**D2 semantics, made explicit.** Receipt identity derives from the signed
scope, not from the unsigned outer variant: a molecule grant uses its
fingerprint, a mission grant derives a per-member identity. Decoding
enforces the variant invariants (no mission-scoped ratification, no
delegated reservation). The v1 canonical bytes are unchanged and legacy
receipt aliases are read. v1 signs neither arbitrary request options nor a
reviewed commit: `base` is a branch name. A molecule ratification names its
reservations exactly (normalized, sorted equality, not containment).

**D3, replaced as the target contract.** The transaction reloads the
required authorization facts under its effect lock, refuses unavailable or
changed facts, and durably reserves authority before integration.
Reservation is not evidence of success. Durable effect and finalization
records, with verified Git and state evidence, decide retry behaviour; an
ambiguous recovery refuses (`harvest_recovery_required`). Cosmon never infers
"landed" from a receipt alone and never erases a spend to retry. Legacy
receipts are treated as reserved-with-unknown-outcome until other evidence
establishes the outcome. The lock excludes cooperating writers only.

**Unchanged.** D1's reservation list, D4, and D5's bound — this is
"authorized cosmon harvest"; a same-uid process can still call Git directly.
Claims in the implementation status section about facts, epoch monotonicity
and replay are corrected by the units that implement them, when they have
evidence, and not before.

