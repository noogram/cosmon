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

The semantic contract remains frozen. W4 mounts the five work routes
W-LIST through W-ACK together with their remote client, canon, OpenAPI and
help changes. W7a mounts the four presence routes S-ATTACH, S-BEAT, S-DETACH
and S-PEERS the same way, and W7b the three mailbox routes S-SEND, S-PULL and
S-ACK. The checkpoint routes and the evidence routes S-EVPUT and S-EVGET in
§4 remain **proposed** and absent from
`crates/cosmon-rpp-adapter/data/surface_events.txt` until their own W7
increment (W7c, whose contract the 2026-11-21 W7c amendment settles). The contract checker requires the shipped W4, W7a and W7b routes
and continues to refuse other mounted collaboration routes.

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
are advisory and rebuildable from the canonical files. Evidence bytes are not a
record class of their own: they stay in the existing host content-addressed
store (CAS), which no admission record duplicates, and a blob is readable for a
mission only through that mission's admission record (§4).

| Writer | Record class | Location |
|---|---|---|
| `owner-store` | Work scope revisions, payloads, envelopes, receipts, delivery observations, acknowledgment notes | The owning molecule directory, through `FileWorkMessageStore` |
| `binding-store` | Collaboration bindings, attachment verifiers and revisions | Host state, written only by the local binding command (W2) |
| `pilot-mailbox` | Pilot messages and their client acknowledgments | Existing pilot mailbox of the mission (W5) |
| `presence-store` | Tagged remote-presence records with server-observed heartbeat | Existing presence store (W5) |
| `checkpoint-store` | Immutable checkpoint revisions, and evidence admission records (one immutable file per admitted digest: mission, digest, size, admitting attachment or session, server time; no bytes) | Existing checkpoint store (W5); admission records under the mission's checkpoint directory |
| `lease-ledger` | Signed pilot-lease grants and epochs | Existing lease ledger; no route in this contract writes it |

## 3. Scopes

Scopes are additive. None implies membership or a binding; a token carrying a
scope without a matching binding is refused before any read. No scope in this
table confers lifecycle, harvest, spawn or lease rights.

| Scope | Grants | Implies |
|---|---|---|
| `cosmon:work:read` | List a work's roster and revision, plus metadata for messages sent or received by the admitted caller seat; peek at or pull its own inbox | - |
| `cosmon:work:write` | Send a message and acknowledge one addressed to the caller's seat | `cosmon:work:read` |
| `cosmon:sessions:read` | List authorized peers, pull pilot messages, read checkpoints | - |
| `cosmon:sessions:write` | Attach, heartbeat, detach, send and acknowledge pilot messages, publish checkpoints | `cosmon:sessions:read` |

Evidence bytes move only through S-EVPUT and S-EVGET (§4), under
`cosmon:sessions:write` and `cosmon:sessions:read`; this contract adds no
scope for them. The existing artifact routes are not evidence custody. They
admit by the `cosmon:artifact:read` and `cosmon:artifact:write` scopes and the
caller's noyau, not by the collaboration binding, so the host could report a
checkpoint `complete` to a recipient who cannot read the bytes, which
contradicts "readable by the recipient". Their files are also addressed by
name and can be overwritten after publication, and their default root is
`/tmp/cosmon`. This reverses the sentence of the frozen contract that named
those routes (operator decision of 2026-11-21).

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
| W-LIST | work list | `GET /v1/work/{owner}` | `cosmon:work:read` | `cs work list` | `owner-store` | page 100 seats and 100 message states | safe-repeat | read-only | W4 |
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
| S-EVPUT | evidence upload | `PUT /v1/pilot/{mission}/evidence/{sha256}` | `cosmon:sessions:write` | `cs sessions evidence put` (confirmed by the operator on 2026-11-28, see §11 2026-11-21 W7c) | `checkpoint-store` | 1048576 bytes per blob | safe-repeat | advisory-write | W7 |
| S-EVGET | evidence read | `GET /v1/pilot/{mission}/evidence/{sha256}` | `cosmon:sessions:read` | `cs sessions evidence get` (confirmed by the operator on 2026-11-28, see §11 2026-11-21 W7c) | `checkpoint-store` | 1048576 bytes | safe-repeat | read-only | W7 |

Of the local counterparts above, both W5 verbs now exist.
`cs presence detach [--session <SID>] [--json]` removes the session's presence
snapshot through the presence store and nothing else: pending mailbox envelopes,
checkpoints, logs and leases are untouched, and it neither requests nor grants a
lease. A session with no snapshot exits 0, writes no file and reports "nothing
to detach", which is the safe-repeat behavior S-DETACH needs.
`cs sessions ack <key> [--session <SID>] [--json]` now acknowledges one envelope
separately from reading it. A duplicate succeeds without writing; an unknown
key is refused. The local mailbox read still acknowledges as it reads, while
`inbox --peek` reads without acknowledging. W7a mounts the presence routes
and W7b the mailbox routes; the checkpoint and evidence routes remain W7 work.
`cs sessions checkpoint publish` is now idempotent on its id and bounded.
The same id with the same content, publication time aside, succeeds and writes
nothing; the same id with other content is refused and the first record stays.
A checkpoint over 262144 serialised bytes, one citing more than 100 evidence
references (claims included), or one whose evidence locator is a path or
`file:` URL of the publisher's machine is refused before anything is written.
`cs sessions checkpoint show` refuses a stored record over 262144 bytes before
reading it. `cs sessions checkpoint list` and `cs sessions drift` read only,
and a drift verdict never states that a checkpoint is portable. The local
publication still accepts the schema `cosmon.checkpoint/v1` and its free
`locator`; the remote rule below does not change it.
The singular `GET /v1/molecules/{id}/session`
transcript route is unchanged and stays read-only. Three things stay
distinct: reading a transcript, advisory exchange (everything above) and
authority transfer (the host-local lease ledger, reached by no route here).

### Fields

- **List** returns the roster and paginated message metadata under `messages`
  (`key`, `delivery_state`). Roster visibility is unchanged. Message metadata
  is limited to messages whose sender or recipient is the admitted caller
  seat; no owner or unrelated-member exception applies. Filter messages by
  caller seat before pagination. The same `offset` and `limit` apply
  independently to seats sorted by seat name and visible messages sorted by
  key. `next_offset` points to the next page while either collection has more
  entries, and is null otherwise. Each page reads current records;
  offsets do not provide a snapshot across concurrent submissions. No message
  payload is returned. Unreadable or unsound message records refuse W-LIST
  with a custody or corrupt-evidence error.
  Local `cs work list` requires the current member identity even when `owner`
  is explicit, and is restricted to that member's declared work and messages
  sent or received by that member. It includes `delivery_state` in each
  envelope projection and `delivery=...` in text output, without payload.
  Local diagnostics are limited to findings attributable to visible messages.
  `cosmon-remote work list` shows the W-LIST response.
- **Delivery state** is `pending` until an adapter's durable attempt count
  reaches the envelope's frozen `max_delivery_attempts`, then `exhausted`.
  Counts are per adapter, not summed across adapters. Once any adapter reaches
  the bound, no adapter offers that message again. This state is derived from
  existing receipts, with no new canonical record, and remains visible after
  consumption or expiry. It is independent of admission, context delivery,
  consumption and expiry: `pending` alone does not imply deliverability.
  An exhausted message does not block delivery of other messages; the pull
  succeeds with those still deliverable, or an empty list. It never returns
  `budget_exhausted` for delivery-attempt exhaustion.

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
  consume the message. A deliverable envelope the wire cannot carry (payload
  not UTF-8, or larger than one response) is listed under `undeliverable` with
  its metadata and records no attempt; it never blocks the envelopes behind
  it. Outgoing receipts in a pull are the caller's most recent 100.
- **Ack** carries `scope_revision`, `payload_digest`, `disposition`
  (`considered`, `deferred` or `rejected`), an optional `reply` key and an
  optional note. It is bound to the recipient seat, the key and the digest, and
  is durable before success is returned. A second ack of a consumed envelope is
  a duplicate and changes nothing. An exhausted message may still be acknowledged
  as evidence of consumption; the acknowledgment does not reset delivery attempts
  or make the message deliverable again. Existing expiry rules still apply
  (the remote route refuses acknowledgments after expiry).
- **Responses** carry the server receipt time (client time is advisory), the
  current scope revision and a schema version. Unknown request fields are
  refused. DTOs are additive and versioned; old records default on read.
- **Pilot routes** take `mission` and `attachment` from the path and check them
  against the binding. The server fixes author and mission from the attachment.
  Checkpoints are bounded structured records. Pilot pull returns messages
  without consuming them; the client acknowledges after its own safe point.
  The checkpoint and evidence rule follows.
- **Evidence reference.** An evidence reference is the object
  `{"sha256": "<64 lowercase hex>", "size": <1..1048576>}` and nothing else.
  `sha256` is the SHA-256 of the exact bytes, the algorithm of the host CAS
  (`ContentHash`), written as 64 lowercase hexadecimal characters; it is
  mandatory, and the algorithm is named by the field because BLAKE3 values,
  used by work payloads and the artifact routes, have the same textual shape.
  `size` is the byte length. Any other spelling, an unknown field or a
  `locator` field is refused with `invalid_json_body` and is never trimmed,
  case-folded or rewritten; the refusal does not echo the value. The grammar is
  closed, so the only request bytes that reach path construction are a value
  parsed as a digest, and no client path, URL or encoding of one can be cited.
  A record that holds such references declares `cosmon.checkpoint/v2`. A record
  of the schema `cosmon.checkpoint/v1` stays readable without rewrite and its
  references are reported `unresolvable`; S-CKPUT refuses it. Whether a
  display label accompanies a reference is not specified; the choice is none,
  because a second free string would carry client paths into published
  records (§9). The pilot mailbox's `payload_ref` strings
  (`cas/<prefix>/<hash>`) are unchanged and are not evidence references.
- **Admission.** A digest is readable for a mission only if bytes with that
  digest and size were uploaded under a binding of that mission; knowing a
  digest, or holding it as a mailbox body, grants nothing. S-EVPUT takes the raw
  bytes as the body and hashes them; a body that does not hash to the path
  digest is `digest_mismatch`. It verifies any blob the store already holds,
  replaces it if it fails its digest, and writes the bytes before it creates
  the immutable admission record. Its response does not depend on whether the
  store already held the blob, and S-EVGET answers a digest that no mission
  admitted exactly as one not admitted for this mission.
- **Publication.** S-CKPUT answers an identical retry before resolving
  anything; it then accepts a checkpoint only when every cited reference is
  `readable` for the path mission, and otherwise refuses with
  `evidence_unreadable`, listing the status of each reference, and writes
  nothing. Evidence is therefore uploaded before the checkpoint that cites it.
- **Status and completeness.** Completeness is never stored. S-CKGET, S-CKCMP
  and S-EVGET resolve at each read, hashing at most `size` + 1 bytes per
  reference, and give each reference exactly one status: `readable`
  (admitted for the mission, bytes present and matching), `unknown` (not
  admitted for the mission), `missing` (admitted, bytes absent), `mismatched`
  (bytes present, digest or size differs) or `unresolvable` (schema v1). A store
  I/O failure other than absence answers the whole request with
  `custody_unavailable` and never yields a status. A checkpoint is `complete`
  when it cites at least one reference and every status is `readable`,
  `none_cited` when it cites none, and `incomplete` otherwise. S-CKGET returns
  the record, each reference's status, the completeness and the server time of
  resolution; the status list does not count toward the 262144-byte record
  bound and has at most 100 entries. S-EVGET returns the bytes of a `readable`
  reference, refuses `unknown` and `missing` with `evidence_unreadable`, and
  refuses `mismatched` with `corrupt_evidence` without sending bytes. A
  checkpoint becomes complete again only when identical bytes are uploaded
  again for the mission. Complete means the cited bytes exist and match; it
  does not mean they support the claim, and a drift comparison alone cannot
  certify portability.
- **Comparison.** S-CKCMP's second operand is not specified; the choice is a
  required `against` query parameter naming a revision of the same mission. Its
  response carries the drift report and each side's completeness, and no
  portability field. Each reference that is not `readable` adds an INCONCLUSIVE
  record of class `unresolved_evidence`, so the comparison never reports AGREE
  over an incomplete side. The local `cs sessions drift` supplies no statuses
  and its output is unchanged.
- **Allowance.** The per-mission allowance is not specified; the choice is 1000
  admitted digests and 104857600 admitted bytes, refused with `evidence_limit`.
  The limit of 100 references per checkpoint is unchanged. How admissions and
  bytes are removed is not specified; the choice is that no route removes them
  and they leave only with the mission's archive under §9 (pending operator
  confirmation, see §11 2026-11-21 W7c).

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
| X-TAKEOVER | Remote takeover request or show | refused | A request exists only to be answered by a host-local grant (O4) and is stored in the lease ledger directory, which no route writes. |
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
| budget_exhausted | 429 | The seat used its message or byte allowance. |
| envelope_expired | 410 | The envelope's lifetime ended before the operation. |
| digest_mismatch | 422 | The supplied digest does not match the payload sent or stored. |
| evidence_unreadable | 422 | Referenced evidence is missing, unreadable or fails its digest; for S-CKPUT the body lists the status of each reference. |
| evidence_limit | 429 | The mission's admitted evidence reached 1000 digests or 104857600 bytes (S-EVPUT); confirmed by the operator on 2026-11-28, see §11 2026-11-21 W7c. |
| invalid_json_body | 400 | The request body is not valid JSON for its route, or carries an unknown field or a malformed evidence reference; already emitted by the work and pilot routes and recorded here. |
| attachment_limit | 409 | The mission already holds two attachments with valid bindings (S-ATTACH); confirmed by the operator on 2026-11-20, see §11. |
| not_attached | 404 | The attachment has no presence record on the mission (S-BEAT); confirmed by the operator on 2026-11-28, see §11 2026-11-20. |
| heartbeat_too_soon | 429 | The attachment's last heartbeat is less than 10 seconds old; carries Retry-After; confirmed by the operator on 2026-11-28, see §11 2026-11-20. |
| pending_limit | 429 | The sending attachment already has 1000 unacknowledged pilot messages (S-SEND); confirmed by the operator on 2026-11-28, see §11 2026-11-21. |
| corrupt_evidence | 500 | Stored records or evidence bytes fail validation; nothing is returned or repaired silently. |
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
stays disabled. W6 is not shipped (§11, 2026-11-17), so that case applies to
every path. A pilot lease never authorizes integration; independent
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
| 4 | W5 | Portable presence, pilot mailbox acknowledgment and checkpoint custody on the local side: `cs presence detach`, `cs sessions ack`, `cs sessions checkpoint publish\|show\|list`, `cs sessions drift` (done 2026-11-17, see §11) | Lease fencing; evidence-reference resolution |
| 5 | W6 | Nothing: not shipped, by decision of 2026-11-17 (see §8 and §11) | Fencing of every covered effect; control handover |
| 6 | W7 | Session routes S-ATTACH … S-CKCMP, the evidence routes S-EVPUT and S-EVGET, and client commands, in four serialized increments. W7a, the presence routes S-ATTACH, S-BEAT, S-DETACH, S-PEERS and `cosmon-remote pilot`, is delivered (2026-11-20, see §11). W7b, the mailbox routes S-SEND, S-PULL, S-ACK and `cosmon-remote pilot send\|inbox\|ack`, is delivered (2026-11-21, see §11). W7c (checkpoint and evidence routes; contract settled by the 2026-11-21 W7c amendment) and W7d are not delivered | Remote signing; takeover routes |
| 7 | W8 | Two-machine acceptance evidence and the off-host restore check, which resolves every evidence reference of every stored checkpoint on the restored host | New features |

W4 is the work-only release boundary. Units that share exports, route
registration, the surface canon, specs or parity documents are serialized in
the order above.

## 11. Compatibility and amendments

**With W3.** The route table maps onto `WorkOperations` without a new custody
path: one lock covers scope, quota and admission; `AdmittedWorkCaller` is
built only by the admission boundary. Canonical local records are unchanged.

**With a later cross-person relay.** The relay reuses the same operations,
envelopes and receipts with its own admission boundary and its own custodians.
This contract does not provide it: it refuses a second tenant, a second
person and any residence other than the one authoritative host, and it
mounts no route that the relay would have to inherit.

**2026-11-10 amendment (operator decision).** Delivery-attempt exhaustion is
per-message `exhausted` metadata in W-LIST and local work-list output, derived
from durable observations. HTTP 429 `budget_exhausted` is reserved for the
seat's message and byte allowances. An exhausted message remains
acknowledgeable without reopening delivery.

**2026-11-10 amendment (operator decision).** W-LIST message metadata and
local work-list messages are visible only to their sender and recipient seats.
W-LIST filters messages before pagination; roster visibility and pagination
remain unchanged. Local listing requires current member identity, including
when the owner is explicit.

**2026-11-17 amendment (operator decision).** W5 is complete on the local
side for the verbs it names: `cs presence detach`, `cs sessions ack`,
`cs sessions checkpoint publish`, `show` and `list`, and `cs sessions drift`,
with the bounds and the retry rule of S-CKPUT and S-CKGET now enforced by the
checkpoint store (§4). One part of the checkpoint rule is not implemented and
is not guessed: the syntax of an artifact reference, whether a reference must
carry a digest, and how a recipient resolves one, so that a checkpoint with an
unreadable reference can be reported as incomplete. W7 settles it before it
mounts S-CKPUT, S-CKGET or S-CKCMP. W6 is not shipped: pilot missions run
without leases (decision of 2026-10-16), so lease fencing is not built and
control handover stays disabled, as §8 foresees. W7 mounts the session and
checkpoint routes through S-CKCMP and not S-TKREQ or S-TKGET. Their only
consumer is a grant, which stays host-local (O4), and the local request record
lives in the lease ledger directory, which §2 says no route writes; they are
therefore dropped from §4 and refused as X-TAKEOVER, and the local
`cs sessions takeover` verbs are unchanged. O1 and O8 stand as written: no
route confers or exercises lifecycle authority.

**2026-11-20 amendment (W7a; the attachment cap rule is confirmed by the operator on 2026-11-20, the rest on 2026-11-28).** W7a mounts
S-ATTACH, S-BEAT, S-DETACH and S-PEERS; W7b to W7d are not delivered. Remote
presence is its own record class in the presence store,
`presence/remote/<mission>/<attachment>.json`, tagged by attachment and
carrying only mission, attachment, attach key, optional display label (at
most 128 bytes) and the server times of attach and last heartbeat. No host
path, pid, native handle, seat role or lease epoch is stored or returned, and
the local session scan, `gc` and `cs sessions peers` do not read it. The
S-ATTACH body is `idempotency_key` plus optional `label`; mission and
attachment come from the binding. The same key and label return the stored
record without a write; another key or label for an attached attachment is
`key_collision`. A record counts toward the cap of two only while its
attachment holds an active pilot binding of the mission (confirmed by the
operator on 2026-11-20). When a new attach finds the mission full, the
`presence-store` writer removes, under the mission lock, the records whose
binding is revoked or gone, and accepts the attach if that leaves room;
otherwise it refuses with `attachment_limit` and removes nothing. A record of
a valid binding is removed only by its own S-DETACH. The binding set is read
under the shared binding lock the attach already holds, so no revocation
interleaves with the decision. Until an attach needs its place, a revoked
record stays stored and S-PEERS lists it. Heartbeat age does not free a
place: no staleness duration is specified, and none is assumed. An attach
after detach is a new attach whatever its key.
S-BEAT refuses a heartbeat sooner than 10 seconds after the last accepted
one, including on a repeat. S-DETACH removes the record only and reports
`detached=false` without writing when there is none. S-PEERS lists attachments
sorted by attachment id, at most 100 per page, with the last id of a page as
the cursor; it takes no lock and creates no file. Three codes are added to §6
for these refusals (`attachment_limit`, `not_attached`, `heartbeat_too_soon`)
because no existing code has their meaning and `budget_exhausted` is reserved
for seat allowances. The local verbs keep their current behaviour: the caps
apply to the remote routes only.

**2026-11-21 amendment (W7b; confirmed by the operator on 2026-11-28).** W7b mounts
S-SEND, S-PULL and S-ACK; W7c and W7d are not delivered. The store is the
existing pilot mailbox (`presence/<sid>.inbox.jsonl`, the body in the CAS,
acknowledgments in `presence/<sid>.inbox.ack.jsonl`); no second store and no
new record class is added. The contract does not say how a mailbox keyed by
session id is addressed from a mission; the choice is that an attachment's
mailbox is the one whose session id is its host-issued attachment id, which
is unique on the host, so `cs sessions send --to <attachment>` reaches it from
the host unchanged. The S-SEND body is `idempotency_key`, `to`, `message` and
optional `expires_in_secs`; author, mission and sequence are not fields and
unknown fields are refused. The author written into the envelope is the
caller's attachment and the mission is the binding's. Who may receive is not
specified; the choice is the pilots of the mission: an attachment with an
active pilot binding of the mission, or a local session whose presence
snapshot names the mission, whatever its heartbeat age. Any other recipient
is refused with `not_a_member`, reused from §6 with the meaning "not a pilot
of the mission", before any path is built from it. The retry identity is the
mission, the sender, the key, the recipient, the body digest and the
lifetime: the message id is derived from the first three (`rmsg-…`), the
same key with the same rest returns the stored message without a write, and
any other rest is `key_collision`. The key is looked up in the mailboxes of
the current recipients only. "1000 pending per attachment" is not specified
further; the choice is the sending attachment's envelopes that their
recipient has not acknowledged, expired ones included, counted in the
mailboxes of the current recipients; the 1001st is refused with the new code
`pending_limit` (HTTP 429), because `budget_exhausted` is reserved for seat
allowances. A retry of an accepted send is answered before the cap. The
65536-byte limit applies to the message body; a larger body is
`payload_too_large`. Sends and acknowledgments take a per-mission lock,
`presence/remote/<mission>.mailbox.lock`; local `cs sessions send` takes
it too when the recipient's presence snapshot names the mission, so a local
and a remote sender cannot pick the same sequence for one recipient. Two
local senders to a recipient that names no mission still race, as before;
a local send to a remote attachment's mailbox is not specified. S-PULL reads the caller's own
mailbox only, lists unacknowledged messages (expired ones included, with their
state) in sequence order, at most 100 per page and 1048576 response bytes,
with the last sequence of a page as the cursor; it takes no lock, records no
delivery and writes nothing. A body that is unreadable, not UTF-8 or larger
than one response is listed under `undeliverable` with its metadata and a
reason, as W-PULL does. S-ACK acknowledges a message of the caller's own
mailbox; a duplicate succeeds without writing and an unknown key, including a
key of another mailbox, is `unknown_envelope`, reused from §6 with the
meaning "names no envelope of this mailbox". Whether S-ACK refuses an expired
message is not specified; the choice is to accept it, as the local
`cs sessions ack` does. No route here wakes a peer: the local mailbox reader
and the remote S-PULL see a message only when they ask. The local verbs keep
their current behaviour: the limits apply to the remote routes only.

**2026-11-21 amendment (W7c; the reversal of §3 and the routes S-EVPUT and S-EVGET are decided by the operator on 2026-11-21, the rest is confirmed on 2026-11-28).**
W7c settles the evidence-reference rule left open on 2026-11-17 and fixes the
contract of S-CKPUT, S-CKGET, S-CKCMP, S-EVPUT and S-EVGET. It mounts nothing:
the five routes stay proposed and absent from the surface until the W7c
implementation, and W7d is not delivered. The operator decided to reverse the
sentence of §3 that named the artifact routes and to add S-EVPUT and S-EVGET;
the reason is in §3. The evidence reference, the admission rule, the status
list and the completeness rule are in §4 and are not repeated here. Choices the
contract did not make, each recorded as a choice: the digest is mandatory,
SHA-256 and named by the field; a reference carries no label; a size of 0 is
refused, because it is the one reference anyone can admit without holding any
content; S-EVPUT is `safe-repeat` because the digest is its identity; the
admission record is written under the checkpoint store's directory because its
writer is `checkpoint-store` and §2 gives each writer one tree; a mismatch is a
per-reference status in S-CKGET and `corrupt_evidence` in S-EVGET, which sends
no bytes; the allowance is 1000 admitted digests and 104857600 admitted bytes,
refused with the new code `evidence_limit`, because `budget_exhausted` is
reserved for seat allowances. `invalid_json_body` is added to §6, where it was
missing although the work and pilot routes already emit it. Two further
choices were **confirmed by the operator on 2026-11-28**. First, removal: not specified;
the choice is that no route removes evidence and it leaves only with the
mission's archive (§9), so reaching the allowance is an operator event.
Second, host-local admission: not specified; the choice is a host-local
`cs sessions evidence put <file>`, the parity counterpart of S-EVPUT, which
reads a file of the host, on the host, and admits it for the mission, and a
matching `cs sessions evidence get`. It is a client-side path on the host
itself, not a path supplied over the wire, so X-PATH does not apply. Without it
a checkpoint published locally cites v1 locators and reads as permanently
`incomplete` to a remote reader. This amendment does not change the local
`cs sessions checkpoint publish`, which keeps accepting v1 records.

Where the code differs from this text, the code is the earlier state and the
W7c implementation closes the gap: `EvidenceRef` is today `{locator, digest}`
with a free optional `digest`; no `cosmon.checkpoint/v2` schema, status or
completeness exists; `is_client_path` is a prefix test, which is why the
grammar above is closed instead of extending it; the compare in
`cosmon-pilot-checkpoint` reports `missing_evidence` on emptiness only and has
no `unresolved_evidence` class; `FileCas::get` is unbounded and does not
re-hash, `FileCas::put` returns success when the target exists without
checking it, and its temporary file name is fixed per hash, so S-EVPUT needs
a verifying write and a bounded read that the mailbox, which shares `FileCas`,
must not see change; and the CAS is host-wide, with no record of which mission
a blob belongs to, which is why admission is per mission. The 1048576-byte blob
bound equals the adapter's default request body limit.

**Amendments**, recorded in the ADRs themselves:

- ADR-080 §6.7 names this contract as the exposure list for collaboration
  routes and adds the four scopes of §3 to the scope grid.
- ADR-168 notes that the RPP session surface carries pilot notes and
  checkpoints only; it never carries delivery to a model or a lease.
- ADR-182 notes that an admitted non-local caller is a boundary fact and that
  remote admission does not change the advisory, non-authoritative status of
  work messages.

**Parity.** Mounting a route (W4, W7, including S-EVPUT and S-EVGET) updates `cs help`, the man page source,
`docs/guides/ux-cli-parity-audit.md` and the API/CLI coverage guide in the
same change. This file changes none of them because it mounts nothing.
