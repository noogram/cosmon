<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Cross-machine collaboration — exposure and custody contract

**Status:** Frozen by work unit W1 of issue #147. Implementation belongs to
W2–W8 and is not authorized by this document alone.
**Scope:** one person, two machines, one authoritative host.
**Amends:** [ADR-080](../adr/080-remote-pilot-port-https-oidc.md) (new §6.7),
[ADR-168](../adr/168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md)
and [ADR-182](../adr/182-declared-advisory-work-under-a-provider-neutral-contract.md)
(one amendment paragraph each, see §11).
**Machine check:** `scripts/check-collaboration-contract.py` validates the tables
below; `scripts/check-collaboration-contract.test.sh` holds its fixtures.

Nothing here changes a shipped verb, route, record or output byte. Every route
in §4 is **proposed**: it is absent from
`crates/cosmon-rpp-adapter/data/surface_events.txt` until the unit named in its
Phase column mounts it together with its canon, OpenAPI and help changes.

## 1. Decisions

The eight operator questions of the design plan are answered with the
recommended default. A default is the contract until the operator confirms or
amends it; an amendment is a revision of this file, never a silent
implementation choice.

| ID | Decision | Status |
|---|---|---|
| O1 | Work messaging (W1–W4) is the first acceptance target. Control handover (W5–W8) stays pending until its own units pass; the issue may be closed after W4 only by the operator. | adopted-default |
| O2 | All canonical records live on one reachable deployment and one tenant. A need for separately authoritative molecule stores requires a residence design before W2. | adopted-default |
| O3 | Exact operator-managed bindings of one admitted identity to one `(tenant, work owner, seat)` or `(tenant, mission, attachment)`; one seat per attachment, each attachment with its own revocable proof; no self-enrollment, no cross-tenant discovery. | adopted-default |
| O4 | Lease signing and grant application stay on the authoritative host through the existing local command. No remote signing, grant or grant-import route. | adopted-default |
| O5 | Only declared work limits and confidentiality classes apply; only explicit artifact references and digest-bound bytes cross machines; accepted evidence is kept for the owner's archive and recovery lifetime. | adopted-default |
| O6 | Explicit, bounded pull at a safe point; delivery status is reported as unknown when unobservable. No offline writes, no automatic wake-up. | adopted-default |
| O7 | The two operator machines are the supported client runtimes; compatibility is claimed only after W8 runs on them. | adopted-default |
| O8 | A remote primary is not enabled until every admitted mutation path is fenced or refused (W6). Until then no route in §4 can confer or exercise lifecycle authority. | adopted-default |

## 2. Identity and custody

**Authority.** One deployment profile per machine pins the service identity
through the existing HTTPS/OIDC configuration (ADR-080). The operator binds an
admitted identity to finite capabilities on the host. A request cannot grant
its own membership, role or seat.

**Caller derivation.** The sender or acknowledging seat is derived from the
binding, never from a field of the request. The admission boundary builds an
`AdmittedWorkCaller` with `SenderEvidence::AdmittedNonLocal`
(`crates/cosmon-core/src/work_message.rs`) and calls the typed operations in
`crates/cosmon-state/src/work_ops.rs` (`WorkOperations::send`, `pull`,
`acknowledge`). The wire cannot select that evidence value. Records written
through a local caller keep `CallerEnvSameUid`; old records stay readable.

**Attachments.** The host issues a Cosmon attachment identifier. Two
attachments of one user are distinct and each needs its own proof alongside the
OIDC identity; the host stores only a verifier. The proof lives in the client
credential store, never in argv, payloads, logs or published artifacts.
Native conversation handles stay in a private client-side mapping. A device
label is display metadata and never authority. Same-user filesystem compromise
of either machine remains outside the isolation claim.

**Binding management** is host-local (W2): provision, inspect and revoke
bindings through a new `cs` command that grants collaboration access only. It
confers no spawn, harvest or operator right, and every legacy binding is denied
all collaboration scopes by default. Binding revision and revocation are
rechecked inside the commit of every write, so revocation and effect have an
auditable order.

**Single writer per record class.** The RPP, client caches and pending views
are advisory and rebuildable from the canonical files.

| Writer | Record class | Location |
|---|---|---|
| `owner-store` | Work scope revisions, payloads, envelopes, receipts, delivery observations, acknowledgment notes | The owning molecule directory, through `FileWorkMessageStore` |
| `binding-store` | Collaboration bindings, attachment verifiers and revisions | Host state, written only by the local binding command (W2) |
| `pilot-mailbox` | Pilot messages and their client acknowledgments | Existing pilot mailbox of the mission (W5) |
| `presence-store` | Tagged remote-presence records with server-observed heartbeat | Existing presence store (W5) |
| `checkpoint-store` | Immutable checkpoint revisions, takeover requests | Existing checkpoint store (W5) |
| `lease-ledger` | Signed pilot-lease grants and epochs | Existing lease ledger; no route in this contract writes it |

## 3. Scopes

Scopes are additive. None implies membership or a binding; a token carrying a
scope without a matching binding is refused before any read. No scope in this
table confers lifecycle, harvest, spawn or lease rights.

| Scope | Grants | Implies |
|---|---|---|
| `cosmon:work:read` | List a work's roster and revision metadata the caller may see; peek at or pull its own inbox | - |
| `cosmon:work:write` | Send a message and acknowledge one addressed to the caller's seat | `cosmon:work:read` |
| `cosmon:sessions:read` | List authorized peers, pull pilot messages, read checkpoints and takeover requests | - |
| `cosmon:sessions:write` | Attach, heartbeat, detach, send and acknowledge pilot messages, publish checkpoints, request takeover | `cosmon:sessions:read` |

Missing evidence bytes move through the existing artifact read and write
routes under their own scopes; this contract adds none for them.

## 4. Routes

Each route calls a typed library operation that its local `cs` verb also
calls; no route runs a CLI string, reads a path or fetches a URL supplied by
the caller. Every limit is enforced before an unbounded allocation. "Effect
perimeter" states what the route may change: `read-only` changes nothing
except the request audit line; `observation-write` appends a delivery
observation; `advisory-write` appends an advisory record. No route may change
a molecule's lifecycle or a lease, and none starts, interrupts or retasks a
peer.

Pull is `POST` because it records a delivery attempt; the `GET` peek records
none. A successful HTTP response acknowledges only the stage the server
observed (admission, or submission to the caller); it is never evidence that a
model received or considered the content.

| ID | Verb | Route | Scope | CLI counterpart | Writer | Limit | Retry | Effect perimeter | Phase |
|---|---|---|---|---|---|---|---|---|---|
| W-LIST | work list | `GET /v1/work/{owner}` | `cosmon:work:read` | `cs work list` | `owner-store` | page 100 seats | safe-repeat | read-only | W4 |
| W-SEND | work send | `POST /v1/work/{owner}/messages` | `cosmon:work:write` | `cs work send` | `owner-store` | budget.max_payload_bytes, budget.max_messages_per_seat, budget.max_bytes_per_seat | idempotent-key | advisory-write | W4 |
| W-PEEK | work inbox peek | `GET /v1/work/{owner}/inbox` | `cosmon:work:read` | `cs work inbox --peek` | `owner-store` | page 100 envelopes, 1048576 response bytes | safe-repeat | read-only | W4 |
| W-PULL | work inbox pull | `POST /v1/work/{owner}/inbox` | `cosmon:work:read` | `cs work inbox` | `owner-store` | page 100 envelopes, budget.max_delivery_attempts | bounded-redelivery | observation-write | W4 |
| W-ACK | work ack | `POST /v1/work/{owner}/messages/{key}/ack` | `cosmon:work:write` | `cs work ack` | `owner-store` | note 4096 bytes | idempotent-ack | advisory-write | W4 |
| S-ATTACH | session attach | `POST /v1/pilot/{mission}/attachments` | `cosmon:sessions:write` | `cs sessions attach` | `presence-store` | 2 attachments per mission | idempotent-key | advisory-write | W7 |
| S-BEAT | session heartbeat | `PUT /v1/pilot/{mission}/attachments/{attachment}/heartbeat` | `cosmon:sessions:write` | `cs presence ping` | `presence-store` | 1 per 10 seconds | safe-repeat | advisory-write | W7 |
| S-DETACH | session detach | `DELETE /v1/pilot/{mission}/attachments/{attachment}` | `cosmon:sessions:write` | `cs presence detach` | `presence-store` | 1 per attachment | safe-repeat | advisory-write | W7 |
| S-PEERS | session peers | `GET /v1/pilot/{mission}/attachments` | `cosmon:sessions:read` | `cs sessions peers` | `presence-store` | page 100 attachments | safe-repeat | read-only | W7 |
| S-SEND | pilot send | `POST /v1/pilot/{mission}/messages` | `cosmon:sessions:write` | `cs sessions send` | `pilot-mailbox` | 65536 bytes per message, 1000 pending per attachment | idempotent-key | advisory-write | W7 |
| S-PULL | pilot pull | `GET /v1/pilot/{mission}/messages` | `cosmon:sessions:read` | `cs sessions inbox` | `pilot-mailbox` | page 100 messages, 1048576 response bytes | safe-repeat | read-only | W7 |
| S-ACK | pilot ack | `POST /v1/pilot/{mission}/messages/{key}/ack` | `cosmon:sessions:write` | `cs sessions ack` | `pilot-mailbox` | 1 per message | idempotent-ack | advisory-write | W7 |
| S-CKPUT | checkpoint publish | `POST /v1/pilot/{mission}/checkpoints` | `cosmon:sessions:write` | `cs sessions checkpoint publish` | `checkpoint-store` | 262144 bytes per checkpoint, 100 evidence references | idempotent-key | advisory-write | W7 |
| S-CKGET | checkpoint read | `GET /v1/pilot/{mission}/checkpoints/{revision}` | `cosmon:sessions:read` | `cs sessions checkpoint show` | `checkpoint-store` | 262144 bytes | safe-repeat | read-only | W7 |
| S-CKCMP | checkpoint compare | `GET /v1/pilot/{mission}/checkpoints/{revision}/comparison` | `cosmon:sessions:read` | `cs sessions drift` | `checkpoint-store` | 262144 bytes | safe-repeat | read-only | W7 |
| S-TKREQ | takeover request | `POST /v1/pilot/{mission}/takeover-requests` | `cosmon:sessions:write` | `cs sessions takeover request` | `checkpoint-store` | 1 open request per attachment | idempotent-key | advisory-write | W7 |
| S-TKGET | takeover show | `GET /v1/pilot/{mission}/takeover-requests/{request}` | `cosmon:sessions:read` | `cs sessions takeover show` | `checkpoint-store` | 1 request | safe-repeat | read-only | W7 |

Of the local counterparts above, `cs presence detach` and `cs sessions ack`
do not exist today; W5 adds them so each route has an honest local verb, and
retains every existing local behavior. The local mailbox read acknowledges as it
reads; W5 adds the separate acknowledgment the pull route needs. The singular `GET /v1/molecules/{id}/session`
transcript route is unchanged and stays read-only. Three things stay
distinct: reading a transcript, advisory exchange (everything above) and
authority transfer (the host-local lease ledger, reached by no route here).

### Fields

- **Send** carries `scope_revision`, `recipient`, `idempotency_key`,
  `sender_time` (advisory), optional `reply_to`, `phase`, `confidentiality`,
  `ttl_secs`, and the payload. The sender is never a field. The canonical
  digest is `Hash::of_bytes(payload)` over the payload bytes as stored; the
  identity of a request for retry purposes is the tuple (owner, sender seat,
  recipient, key, digest, scope revision, reply_to, phase, confidentiality,
  ttl). The same key with the same tuple returns the same admitted envelope;
  a changed tuple is a `key_collision`.
- **Pull** carries `scope_revision`, the adapter label, an optional
  `context_observation`, and `peek`. Content is returned only on an explicit
  pull; the server records submitted/unknown and keeps the existing retry
  budget and backoff. A dropped response may cost an attempt; it cannot
  consume the message.
- **Ack** carries `scope_revision`, `payload_digest`, `disposition`
  (`considered`, `deferred` or `rejected`), an optional `reply` key and an
  optional note. It is bound to the recipient seat, the key and the digest, and
  is durable before success is returned. A second ack of a consumed envelope is
  a duplicate and changes nothing.
- **Responses** carry the server receipt time (client time is advisory), the
  current scope revision and a schema version. Unknown request fields are
  refused. DTOs are additive and versioned; old records default on read.
- **Pilot routes** take `mission` and `attachment` from the path and check them
  against the binding. The server fixes author and mission from the attachment.
  Checkpoints are bounded structured records; evidence is a digest-bound
  artifact reference readable by the recipient, never a client path. A
  checkpoint whose references cannot be read is incomplete, and a drift
  comparison alone cannot certify portability. Pilot pull returns messages
  without consuming them; the client acknowledges after its own safe point.

## 5. Operations that are refused

Refusal is a stable typed error and a documented absence from the route table.
It is part of the contract: adding any of these needs an amendment here first.

| ID | Operation | Disposition | Reason |
|---|---|---|---|
| X-EVOLVE | Remote `evolve` | refused | A message or note never advances a step; formula progress stays with the owning worker. |
| X-COMPLETE | Remote `complete` or collapse of a work | refused | Terminal state is a lifecycle authority, not advisory exchange. |
| X-GRANT | Remote lease signing, grant application or grant import | refused | Signing and applying stay host-local (O4); a replay contract would be needed first. |
| X-SHELL | Arbitrary CLI or shell execution | refused | Typed operations only; no argument string crosses the wire. |
| X-TRANSCRIPT | Transcript or native-log synchronization and scanning | refused | Discovery stays local to the client; the singular session route is unchanged. |
| X-WAKE | Automatic wake-up, keystroke injection or retasking of a peer | refused | Pull is explicit (O6); no request starts or interrupts a peer. |
| X-NUCLEATE | Remote `nucleate` from a message | refused | No message becomes a molecule; molecule creation keeps its own scope. |
| X-TACKLE | Remote `tackle`, `run` or `done` conferred by a collaboration scope | refused | Spawn and harvest keep their dedicated scopes and bindings. |
| X-PATH | Caller-supplied filesystem paths or URLs | refused | Evidence moves as digest-bound references into canonical custody. |
| X-STORE | Independently writable mirrors of state directories | refused | One authoritative host; backups are not an active second writer. |

## 6. Errors

Refusals use the existing RPP error envelope and these stable codes. A code is
added by amendment, never reused for another meaning.

| Code | HTTP | Meaning |
|---|---|---|
| unauthenticated | 401 | No admitted identity. |
| binding_missing | 403 | The identity has no binding for this owner, seat or attachment. |
| binding_revoked | 403 | The binding or attachment proof was revoked or its revision is stale. |
| scope_missing | 403 | The token lacks the route's scope; checked before any resource read. |
| not_a_member | 403 | Sender or recipient is not a seat of the work. |
| stale_scope_revision | 409 | The request names a scope revision other than the current one. |
| key_collision | 409 | The key already names different request content. |
| unknown_envelope | 404 | The key, reply or message names no envelope of this work. |
| unknown_work | 404 | The owner names no declared work visible to the binding. |
| payload_too_large | 413 | Payload or note exceeds its limit. |
| budget_exhausted | 429 | The seat used its message, byte or delivery-attempt allowance. |
| envelope_expired | 410 | The envelope's lifetime ended before the operation. |
| digest_mismatch | 422 | The supplied digest does not match the stored payload. |
| evidence_unreadable | 422 | Referenced evidence is missing, unreadable or fails its digest. |
| corrupt_evidence | 500 | Stored records fail validation; nothing is returned or repaired silently. |
| unsupported_feature | 501 | The deployment predates the requested operation; there is no local fallback. |
| custody_unavailable | 503 | The owning store cannot be reached or locked; the outcome is unknown, not failed. |

`WorkMessageError` variants map one-to-one to the 4xx codes above
(`StaleScopeRevision`, `NotAMember`, `PayloadTooLarge`, `KeyCollision`,
`ReplyToUnknown`, `SeatBudgetExhausted`). No raw payload, token or host path
appears in an error body or the request audit.

## 7. Retry and idempotence

- A send retry reuses the saved request and key; a freshly minted key for a
  lost response is a new message. A client may keep its own bounded pending
  request bytes privately; they are retry material, not a second ledger.
- Payload bytes are published before the envelope and the envelope before the
  receipt; a retry after a failure at any of those points repairs a missing
  receipt and yields one logical admission (W3 behavior, kept).
- Network loss yields `custody_unavailable` or a client timeout, retains
  unacknowledged work and elects no new owner. There is no background outbox.
- Pull redelivery follows `budget.redeliver_after_secs` and
  `budget.max_delivery_attempts`; ack is idempotent per key and digest.

## 8. Effect perimeter and lease fencing

The routes in §4 never mutate lifecycle or leases, so they need no lease check
of their own and no collaboration scope implies one. A remote primary, meaning
a remote caller who exercises existing lifecycle verbs under a transferred
lease, is out of this contract's first release (O8). It is gated on W6, which
must (a) bind every covered mutation to an explicit caller attachment and
epoch, (b) check the current epoch at commit with a documented lock order
against molecule and harvest locks, (c) serialize two same-epoch grants so one
commits, and (d) refuse an unsupported mutation while the mission is leased.
If W6 cannot cover a path, checkpoint exchange ships and control handover
stays disabled. A pilot lease never authorizes integration; independent
harvest grants remain mandatory.

## 9. Confidentiality, retention and recovery

- Payloads follow the work's declared confidentiality; copies inherit it.
- Accepted evidence and unresolved retries are archived before caches or
  attachments are deleted. Detach ends access and keeps the evidence the
  declared retention requires.
- Canonical files stay inspectable with the RPP stopped; restart rebuilds
  pending views from immutable envelopes and observations.
- Before a deployment is called active, the off-host durability path of
  ADR-095 RR-4b is configured and a restore to a disposable host validates the
  canonical records (W8).
- Bearer tokens, attachment proofs, native handles and host paths never enter
  published artifacts.

## 10. Phased rollout

| Phase | Unit | Ships | Does not ship |
|---|---|---|---|
| 1 | W2 | Typed bindings and attachment admission, host-local provisioning and revocation; no public route | Any route |
| 2 | W3 | Typed work operations with an injected admitted caller and `AdmittedNonLocal` evidence (already merged) | Transport |
| 3 | W4 | Work routes W-LIST … W-ACK, remote client commands, canon, OpenAPI, help and parity updates | Session routes |
| 4 | W5 | Portable presence, pilot mailbox acknowledgment and checkpoint custody on the local side | Lease fencing |
| 5 | W6 | Fencing of every covered effect | Session routes |
| 6 | W7 | Session routes S-ATTACH … S-TKGET and client commands | Remote signing |
| 7 | W8 | Two-machine acceptance evidence and the off-host restore check | New features |

W4 is the work-only release boundary. Units that share exports, route
registration, the surface canon, specs or parity documents are serialized in
the order above.

## 11. Compatibility and amendments

**With W3.** The route table maps onto `WorkOperations` without a new custody
path: one lock covers scope, quota and admission; `AdmittedWorkCaller` is
built only by the admission boundary; local callers and their records are
unchanged.

**With a later cross-person relay.** The relay reuses the same operations,
envelopes and receipts with its own admission boundary and its own custodians.
This contract does not provide it: it refuses a second tenant, a second
person and any residence other than the one authoritative host, and it
mounts no route that the relay would have to inherit.

**Amendments**, recorded in the ADRs themselves:

- ADR-080 §6.7 names this contract as the exposure list for collaboration
  routes and adds the four scopes of §3 to the scope grid.
- ADR-168 notes that the RPP session surface carries pilot notes and
  checkpoints only; it never carries delivery to a model or a lease.
- ADR-182 notes that an admitted non-local caller is a boundary fact and that
  remote admission does not change the advisory, non-authoritative status of
  work messages.

**Parity.** Mounting a route (W4, W7) updates `cs help`, the man page source,
`docs/guides/ux-cli-parity-audit.md` and the API/CLI coverage guide in the
same change. This file changes none of them because it mounts nothing.
