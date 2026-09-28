<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# ADR-182 — Declared advisory work under a provider-neutral contract

**Status:** Accepted  
**Date:** 2026-09-28  
**Authoring molecule:** `task-20260928-c463`  
**Revising molecule:** `task-20260928-db65` (operator review of 2026-09-28)  
**Source deliberation:** `delib-20260928-3e5d`  
**Decider:** the operator (accepted 2026-09-28)

## Decision requested

Permit bounded, declared advisory attempts inside a molecule, subject to the
contract below. Any two molecules in the same declared work may exchange
messages and intermediate evidence, whether they run on the same provider or
on different ones. The contract must support work that mixes providers; it
does not require any work to mix them. Native delegation is an optional
provider adapter; it cannot be the Cosmon coordination layer. Preserve one
authority for each lifecycle field.

This is a proposal for operator review. It changes no command, permission,
formula, or founding text. The operator authorized opening the amendment and a
measurement-led comparison, then required open messaging among all agents in
the work, and on review required that collaboration be decoupled from the
choice of provider. None of these instructions ratifies the replacement text
below. C2 may implement only an accepted contract; C3 must establish
capability and register its budget before trials. An implementation that lets
molecules communicate only through one provider's native tools fails the
requirement.
If acceptable authority separation or canonical recovery cannot be demonstrated,
decline native attempts and keep separately supervised molecules. A decline
blocks C2's native implementation; it cannot be reinterpreted as permission.

## Why an amendment is needed

THESIS Part X allows different cognitive architectures but explicitly refuses
sub-agents, mailboxes and inter-worker communication. Architectural invariants
§7b already describe native deliberation panels. This inconsistency is not an
implicit exception. The source deliberation's individually authorized panel
established neither general permission nor an enforced isolation boundary.

Two relations remain distinct: a native spawn tree describes executions;
`BlockedBy` describes when a molecule may start after prerequisite integration.
An advisory exchange does not satisfy a dependency, own a work claim or grant
permission to retask a peer. The exception below concerns bounded evidence
exchange within a declared work scope. It does not introduce a broker, replace
the DAG, change credentials, train models or replace the root transport.

## 1. Work, execution and authority

A work scope names the owning molecule and a finite roster of seats. Each seat
has an assignment, required/optional status, provider requirements (including
any declared provider-diversity requirement), evidence contract and resource
allocation. A seat may be a native advisory execution or
an independently supervised molecule linked to the same work. The roster is
artifact metadata; it is not another scheduler or lifecycle state machine.

| Obligation | Sole decision owner and evidence |
|---|---|
| Formula progress and terminal status | Existing Cosmon lifecycle operations and event authority; native completion cannot advance either. The owning worker invokes the authorized CLI transition after verifying evidence. |
| Required reviewers | The declared formula/brief determines seats and independence requirements. Only an authorized scope revision may remove a requirement; a missing response never counts as assent. |
| Artifact acceptance | The owning worker checks the seat contract and records a digest-bound disposition in canonical artifacts, referenced by formula evidence. Required external reviewers retain their own verdicts; the synthesizer cannot manufacture them. |
| Retry or reassignment | The owning worker makes a recorded decision within the approved budget and authority. A peer may request a retry but cannot order one. Independent molecule retries stay with that molecule's lifecycle owner. |
| Resource ceilings | The work declaration bounds total and per-seat attempts, concurrency, elapsed time, input/output budget, messages and bytes, including retries and discussion. A native slot limit is only one local constraint. |
| Integration and teardown | Existing harvest authorization and operator reservations apply. No native attempt, peer message, acceptance receipt or response grants integration authority. |

The lifecycle authority remains `events.jsonl` and its existing projections
(invariants §8d); fields not reconstructed by that log retain their current
owners and limitations. Attempt observations, immutable evidence and message
receipts must not introduce competing `status`, `current_step`, `merged_at` or
formula-completion authority. ADR-052's one-writer rule remains unchanged.
There is no independently authoritative attempt ledger.

Use a separate molecule when an assignment needs its own permission boundary,
branch, lifecycle, retry policy or recovery obligation. The molecule boundary
and the provider choice are independent: two molecules on the same provider,
or a Claude molecule, a Codex molecule and a molecule on another provider (for
example one reached through OpenRouter), can all contribute to one declared
work scope and exchange messages and intermediate evidence under §3. Separate
supervision does not postpone communication until completion, and a provider
difference is neither required for communication nor an obstacle to it.
A native reviewer runs in the owning molecule's runtime, shares its acceptance
and recovery boundary, and is limited to the providers that runtime can reach;
its failure is a missing attempt, not a new molecule status.

### Provider diversity is a seat property

A formula that needs independence of judgment, such as a cross-provider review,
declares provider diversity on the seats concerned: which seats must run on a
provider different from which others, and what counts as different. Dispatch
checks that declaration and records the observed provider for each seat (§2
item 3). A different model label in a shared native runtime does not satisfy a
provider-diversity requirement. The communication mechanism neither implies nor
requires provider diversity; a seat without such a declaration may run on any
provider the work allows, including the same one as its peers.

### Authority is checked at the effect

Ordinary harvest can be delegated. Human-reserved thresholds require operator
authorization under ADR-172, including `hold:human`, `needs-review`, security
reservations and overrides. This mission reserves operator review of the
founding amendment. Its completion delivers a proposal; it does not approve
that proposal or authorize harvest. No `cs done` is part of this work.

ADR-172 has authorization types, a verifier and an opt-in effect boundary; its
accepted grant/challenge command surface remains unimplemented. Role text,
shared uid, cwd, a sibling shell and possession of a native tool do not enforce
authority. Direct same-uid repository writes remain a known custody limitation.
Do not describe the existing mechanism as a universal enforced human-only gate.

Admission must inventory actual access to lifecycle tools, shell, state, refs
and peer-control operations. If a required denial can only be expressed in a
prompt while those effects remain reachable, the isolation claim fails. Use
an adapter with a tested effect boundary, separately constrained execution,
or decline the attempt. A separate molecule provides independent lifecycle;
it does not by itself create a security boundary under the same uid. A bounded
cooperative trial may document convention-only behavior, but cannot pass the
authority-enforcement adoption gate on that basis.

Any agent in the work may message any other agent in that work, whatever
provider each runs on. Discussion
may carry evidence, questions and suggested corrections. Retasking, starting a new turn for an idle peer, interruption,
closing a peer, nested spawning and lifecycle calls require the responsible
owner's separate authority. They are not implied by permission to message.
Native APIs that allow sibling control must be restricted at an effect boundary
or declared unsuitable for a scope requiring that restriction.

## 2. Durable obligations before dispatch and acceptance

Before dispatch, persist in the owning molecule's canonical directory:

1. The scope revision and digest, owning molecule, finite seat/peer roster,
   dependency relation, responsible lifecycle owner and reservation status.
2. Frozen assignments, input/source digests, raw response destinations, rubric,
   required seats, first-pass/discussion phases and the exact independence
   claim. Separate execution does not imply blindness or statistical independence.
3. Requested and observed provider/model/configuration, any provider-diversity
   declaration and the result of checking it, tool capabilities, permission
   envelope and enforcement evidence. Undisclosed effective values
   remain unknown. Preserve ADR-181's profile behavior without claiming that
   child roles receive distinct profiles or credentials.
4. Hard limits and observation availability: total attempts, retries, concurrent
   executions, deadline, tokens or another enforceable spend ceiling, message
   count/bytes and maximum discussion rounds. Unknown usage is not zero. If the
   declared ceiling cannot be enforced, do not dispatch under that ceiling.
5. A unique Cosmon-owned intent key per attempt, its input digest, acceptance
   owner, fallback, uncertain-spawn procedure and stop condition. Persist intent
   before calling the provider. Provider handles remain private adapter material,
   never published provenance or the only recovery key.
6. Work membership, delivery deadline, retention deadline, artifact access
   requirements and recovery owner. Membership is a discovery scope, not a
   sender-recipient graph. No live recipient is discovered solely through native
   ancestry or a provider message board.

Before a response becomes accepted:

- Copy the complete raw response and its cited evidence into canonical custody,
  using an atomic publish after validation. Preserve the original; revisions
  receive new digests and explicitly name what they supersede.
- Match intent, seat, scope revision and input digest. Check completeness,
  readable bytes, references, permissions/configuration exceptions, budget and
  required reviewer status. An empty file or terminal notice is insufficient.
- Save a disposition (accepted, rejected or missing), artifact digest, reviewer
  identity within the work scope, validation performed and limitations. Record
  acceptance only after the referenced bytes are durable. No provider-only link
  is an accepted result.
- Preserve each original first-pass response and later revisions separately.
  A comparison can request first-pass work before discussion, but messaging
  stays open: any early exchange is recorded and invalidates a claim of an
  unexposed first pass. Shared files and inherited context are also disclosed.
  Do not claim blindness without a separately demonstrated access barrier.
- Reference the accepted evidence in the existing formula transition. On a
  restart between artifact publication and acceptance, validate the saved bytes
  before recording a disposition; never infer acceptance from their presence.

## 3. Provider-neutral collaboration contract

These are proposed semantic operations, not shipped CLI verbs or RPC names.
They belong behind injectable ports; reducers validate scope and dispositions
without I/O. One-shot CLI operations and existing supervised transport clients
can perform them. No new broker, always-running queue service or scheduling
loop is required by this decision.

| Operation / fact | Meaning and canonical record |
|---|---|
| Discover peers | Read the scope's finite work roster, assignments and observed delivery capabilities. Every member may message every other member; no pairwise allowlist is required. Addresses are Cosmon seat/molecule identities; adapter handles are private. Discovery conveys no control rights. |
| Submit evidence | Validate sender/recipient membership, scope revision, byte quota and retention; do not require a declared communication edge. Persist an immutable envelope and payload digest before any delivery. Repeating the same key and digest is idempotent; the same key with different bytes is refused. |
| Durable admission | The receiving boundary accepts the envelope into canonical custody after work-membership and storage checks. This acknowledges storage and admissibility, not review, comprehension or artifact acceptance. |
| Delivery attempt | The recipient adapter attempts insertion at a supported safe point and records its observed result. A successful terminal paste or provider enqueue is only submitted unless a stronger observation exists. |
| Context delivered | Record evidence that the envelope was included in the intended recipient's model input, with key and digest. If the harness cannot observe this, record unknown; do not upgrade an enqueue receipt. |
| Consumption reported | The recipient emits an explicit keyed acknowledgment with disposition: considered, deferred or rejected, and any response/evidence reference. Persist it before reporting it. This is an observable protocol report, not proof of semantic understanding. |
| Artifact accepted | The owning acceptance authority validates evidence under §2. Message admission, context delivery and consumption do not confer this status. |

An envelope contains schema version, scope/revision, sender and recipient
Cosmon identities, message key, payload digest and canonical reference,
sender time and durable receipt time, reply-to key if any, phase observation,
confidentiality class, expiry and retry bounds. Phase is recorded, not a
permission to address a peer.
The boundary derives sender identity from the authenticated/constrained caller
where enforcement is required; a free-form sender field or same-uid file is
not authentication. Receipts identify observer, envelope key/digest, observed
stage and time. Missing stages remain missing. No conversation identifier or
vendor deep link belongs in published artifacts.

Receipts and payloads live with the owning molecule's evidence. The scope names
one custodian/writer for each record class; receiving adapters append their
observations through that boundary. Participant molecules reference the same
immutable objects or validated digest-identical copies at a residence boundary.
They do not each maintain an independently writable truth about acceptance.
Rebuild any pending-delivery view from envelopes and observations; never use it
to schedule molecule dependencies or declare completion.

### A Claude worker and a Codex worker exchanging evidence

The example uses two providers to show that the contract crosses a provider
boundary. The same steps apply unchanged to two Claude workers or two Codex
workers; only the adapters named in steps 2–4 differ.

1. Declare both reviewer molecules as peers under a common owning work scope.
   They may start after their actual prerequisites; neither depends on the
   other's completion merely to exchange intermediate evidence. The final
   synthesis retains its proper dependency/acceptance gates. Do not create a
   cyclic `BlockedBy` pair to express conversation.
2. The Claude worker writes a finding with source references to its allowed
   output area, then requests submission to the Codex seat. The Cosmon boundary
   validates and preserves the envelope and bytes, records durable admission,
   and asks the Codex adapter to deliver the bounded content plus its key/digest.
3. The Codex harness inserts this input at a supported message boundary or in
   an explicitly authorized next turn. Its adapter records the strongest actual
   observation. The Codex worker returns a keyed consumption acknowledgment and
   optionally a reply using the same neutral protocol.
4. The reverse path uses the Claude adapter. Neither worker needs the other's
   native agent address, message board, account or conversation database.
5. The acceptance owner preserves the exchange and resolves resulting revisions.
   Deleting both provider histories must leave the accepted findings, open
   questions and retry obligations reconstructible from canonical storage.

An OpenRouter-backed model follows the same contract through its harness: an
API endpoint alone does not provide an agent, durable memory or a peer inbox.
The harness assembles the next model input from canonical pending envelopes,
exports the resulting acknowledgment/evidence, and implements the same limits.
The provider route and realized model observation must be recorded separately.

| Harness | Required capability; availability must be demonstrated |
|---|---|
| Every provider | Export complete outputs; receive bounded evidence with stable keys; emit keyed consumption reports; expose effective settings or mark them unknown; support deadlines, cancellation/stop policy and usage limits; recover from canonical inputs; enforce required access boundaries. |
| Codex | A supported recipient-input path with honest enqueue/context observations. Native same-provider tools are optional shortcuts only when their effects are mirrored into canonical evidence. They cannot be required for discovery or routing, whether the peer runs on Codex or on another provider. |
| Claude | A supported input or resume/next-turn path that safely inserts evidence into the intended model context, plus export and acknowledgment. A tmux paste is insufficient proof of context delivery. No claim is made here that an installed version satisfies the contract. |
| Other API harnesses | Explicit model-input assembly, tool/result export, durable acknowledgment, idempotency handling and bounded execution. Merely accepting chat messages at an API does not satisfy recovery or authority requirements. |

If live insertion is unavailable, bounded polling at declared safe points may
load pending canonical envelopes into the next input and report consumption.
This is an explicit deferred-delivery capability with a latency bound; it does
not meet a workload requiring live delivery. No input goes to a shell whose
agent has exited. Wake rights are separate: messages cannot start or resurrect
an inactive worker without an already authorized owner action. Expired messages
are retained as expired and excluded from automatic delivery.

### Retention and repetition

Keep accepted response bytes, all evidence used to accept them, relevant
message bodies/receipts and unresolved intent records through the owning
molecule's archive/recovery lifetime. Unaccepted material gets a declared
bounded retention deadline and an explicit expired/deleted disposition with
digest and reason. Publication uses a reviewed projection; secret payloads do
not become tracked files. Private native references may expire after recovery
needs end; their deletion cannot destroy accepted evidence. Confidentiality
redactions retain a disposition and invalidate any acceptance that can no
longer be substantiated; a digest alone cannot replace missing proof.

Delivery can repeat after a crash between injection and observation. Deduplicate
by scope/revision/key/digest at admission and consumption; do not promise exactly
once model exposure or reasoning. Late or duplicated reports do not overwrite a
newer accepted response. Preserve every submitted message, its sender,
recipient, time, payload and delivery/consumption status, including explicit
pending, unknown or refused outcomes. Work membership, execution budgets and
artifact permissions remain boundaries; they are not a communication graph.
Early discussion is an observed exposure, not an undeclared messaging offence.

### Future extension: optional communication policy

An optional communication policy in the fleet or formula specification is a
future extension point. This ADR specifies neither its schema nor its rules
and implements no policy. Start with any agent in the work able to message any
other. Consider constraints only after durable observations show a concrete
need: repeated message storms consuming the declared budget, repeated duplicate
requests, measurable first-pass contamination, unwanted disclosure, or measured
delivery backlog that prevents required work. Preserve the relevant message
traces and comparison outcomes, then propose the smallest evidenced constraint
for operator review. Do not preinstall a topology in anticipation of those
findings. Native provider reachability must not become an implicit allowlist.

## 4. Finite failure and fallback decisions

| Case | Required record and action | Acceptance / authority consequence |
|---|---|---|
| Child fails or times out | Save observed failure, partial output and budget consumed/unknown. Owner may retry with a new intent within the cap, or select the declared fallback. | Required seat remains missing; native failure does not collapse or complete the molecule by itself. |
| Root fails | Authorized recovery reconstructs intents, artifacts, dispositions, pending envelopes and limits from canonical disk. Quiesce or fence old attempts before redispatch. | Existing accepted digests survive; orphan/uncertain work cannot become accepted merely through a native resume. |
| Output or completion notice missing | Inspect canonical artifact first. A valid saved response can be checked without a notice; a notice without bytes cannot count. | Retrieve and persist complete bytes if possible, otherwise mark missing; no provider-only acceptance. |
| Spawn acknowledgment uncertain | Record unknown execution, reserve its worst-case budget, reconcile by intent using available adapter observations. Do not blindly repeat spawn. | If termination/nonexecution cannot be established, stop that seat; only a declared safe duplicate policy within the total cap may authorize another attempt. |
| Native tools unavailable | Record exact capability gap. Use separately supervised reviewer molecules if already allowed and budgeted; otherwise leave a blocker. | Sequential persona simulation may be a separately labelled comparison baseline, never independent review or a substitute for required seats. |
| Permission or provider mismatch | Refuse before dispatch; if found later, stop affected work, preserve evidence and request an authorized reconfiguration through lifecycle records. | Do not widen access or silently accept changed provider, model or independence claims. |
| Unauthorized lifecycle call | Effect boundary must refuse; preserve attempted target/action and observation. If it succeeds, stop the trial and flag boundary failure. | Audit authoritative lifecycle state through existing recovery procedures; do not repair it from an attempt artifact or claim conformance. |
| Attempted integration | Refuse absent the applicable authorization; preserve the attempt. If any effect occurred, stop and record affected refs/state for operator audit. | No peer receipt, native result or owner role grants a harvest seal; reserved gate remains closed. |
| Duplicate, late or conflicting message | Deduplicate identical keys/digests; reject a key collision with different bytes; retain late/expired observations. | No duplicate acceptance, scope widening or automatic retasking. |
| Recipient inactive or delivery uncertain | Persist pending/unknown status with deadline; use declared safe-point delivery or owner-authorized recovery. | Do not paste into a shell, forge context delivery or infer consent from silence. |
| Provider history or root artifacts lost | Reconstruct from canonical retained bytes and receipts; detect missing/corrupt digests explicitly. | If required evidence is absent, acceptance cannot be reused. Native history is an optional recovery aid. |
| Work membership revoked, cross-work or malicious sender | Recheck work membership at delivery; reject out-of-work input and retain its disposition. Preserve in-work messages, including hostile content, as untrusted evidence without executing embedded orders. | Evidence is untrusted content; embedded orders cannot mutate formula, permissions, roster or lifecycle. |

Reconstruction does not promise an identical stochastic model response. The
restart invariant is the same accepted evidence, required/missing obligations,
authority and remaining budget from disk, without duplicate authorized effects.
C2 must exercise each row at the actual adapter seams, including the window
between saved intent and spawn and between saved output and acceptance.

## 5. Exact proposed doctrine changes

These replacements are quoted for review only. Apply none until the operator
approves the founding edit. Anchors identify complete bullets/paragraphs;
line numbers are approximate because the founding text can move.

### THESIS Part X, four bullets around lines 901–904

Replace the consecutive bullets beginning **No broker, no message queue**,
**No mailboxes**, **No background bash, no hidden side-channels**, and
**No sub-agents inside workers** with exactly:

> - **No broker or message queue as lifecycle authority.** The DAG owns dependency ordering; canonical filesystem evidence owns content. Declared, bounded advisory exchange may connect any agents of a declared work under ADR-182, whether they run on the same provider or on different ones. It cannot schedule molecules or satisfy dependencies.
> - **No hidden mailboxes.** Any agent in a declared work scope may message any other, whatever provider each runs on. Every message is recorded durably with its sender, recipient, time, delivery and consumption status. Advisory exchange requires durable payloads, explicit admission, context-delivery observations, consumption reports and retention. Native provider mail or boards are optional delivery mechanisms and never the sole record of accepted evidence.
> - **No background bash or hidden lifecycle side-channels.** Every molecule state transition remains a visible `cs` invocation. Advisory messages carry evidence and requests; they do not confer lifecycle, retasking, interruption or integration authority.
> - **No undeclared sub-agents inside workers.** A molecule may contain bounded native advisory attempts under ADR-182, with canonical assignments, outputs, acceptance and recovery obligations. Independent permissions, branch, lifecycle or recovery requirements use separate molecules and typed links. Those molecules may exchange declared advisory evidence within the same work scope through the provider-neutral contract, on one provider or several. Provider diversity is declared on a seat when a formula needs independent judgment; it is neither required for communication nor implied by it.

The architecture-neutral paragraph and all other THESIS bullets remain outside
this amendment. Existing harvest wording is interpreted under accepted ADR-172;
this proposal does not reopen that decision.

### Architectural invariants §3, after “Two boundaries this table encodes”

Insert the following paragraph after item 2:

> Declared advisory attempts do not inherit the owning worker's lifecycle or integration authority. The owner remains responsible for formula progress, required reviewers, acceptance, retries and aggregate limits. Advisory evidence exchange among the agents of a declared work, on one provider or several, uses ADR-182's scoped contract and does not authorize peer retasking, interruption or harvest. Effect boundaries must enforce any claimed denial; roles, shared uid and sibling-shell shape are not authorization. Existing operator reservations and ADR-172 remain binding.

### Architectural invariants §7b, final paragraph

Replace the paragraph beginning “No new command perimeter is required” with:

> Deliberations retain the existing molecule lifecycle commands. A worker may invoke declared advisory attempts only under an accepted ADR-182 contract, with required seats, canonical evidence and explicit capability fallback. Native provider tools are optional adapters. Independent permission, lifecycle or recovery requirements use separate molecules in the same declared work scope; advisory exchange among them follows the provider-neutral contract whatever provider each runs on. A panel that needs independent judgment declares provider diversity on its seats. A native terminal notice does not complete a formula step or satisfy a reviewer requirement.

### Architectural invariants §7c, after “No hidden link-fidelity assumptions”

Insert this bullet before “The restart-fidelity test”:

> - **No provider-only advisory truth.** Accepted responses, required seats, dispatch uncertainty, retry budgets and communication dispositions must be recoverable from canonical molecule evidence without provider history. Attempt observations and message receipts cannot declare molecule completion. Restart tests cover the dispatch, output-publication, acceptance and message-delivery crash windows; stochastic response equality is not the claimed invariant.

### Architectural invariants §7e, “Mailbox reinvention” bullet

Replace that entire bullet with:

> - **No mailbox as control plane.** Any agent in a declared work scope may send advisory evidence to any other member under ADR-182, whether both run on the same provider or on different ones. No communication graph is required. Scope, payloads, admission, context-delivery observations, consumption reports, quotas and retention remain canonical and bounded. No mailbox, file scan or message acknowledgment changes dependency readiness, claims work, advances a formula or grants lifecycle or integration authority.

The §7e control/data table, §8d lifecycle field authority and §8 canonical store
remain unchanged. Pending delivery is an evidence projection, not DAG ordering.

### ADR-038, `038-whisper-perturbation-port.md`

This is the whisper ADR, not the other records sharing number 038. `cs whisper`
keeps its pilot-to-worker command perimeter; peer evidence uses the separately
defined neutral contract. Replace its scope-table Caller row with:

> | Caller | **Human pilot only for `cs whisper`.** Workers use the separately authorized ADR-182 advisory contract for peer evidence; they do not gain permission to invoke this steering command. |

Replace its scope-table Delivery row with:

> | Delivery | Sender-side submission is observable; context delivery and recipient consumption require distinct evidence. This command does not currently establish those stronger facts. Semantic understanding is not implied by a receipt. |

Replace the negative consequence beginning “Delivery is undecidable” with:

> - **Delivery observations are limited.** Storage admission, transport submission, insertion into model input and an explicit recipient consumption report are separately observable protocol events when the relevant adapter supports them. No general receipt proves understanding or changed reasoning. Record only the stage actually observed; terminal paste alone is not proof of context delivery.

Replace the preserved invariant beginning “Worker/human boundary” with:

> - **Worker/pilot boundary.** `cs whisper` remains pilot-to-worker steering. Declared peer evidence exchange follows ADR-182 and does not reuse whisper as an implicit worker permission. Caller role, shared uid and process shape do not establish authorization; any claimed denial requires an effect-boundary check.

Insert after its Scope and authority table:

> ADR-182 proposes a separate, provider-neutral advisory evidence contract. It does not expand `cs whisper` into a peer control command. Messages cannot retask, interrupt or integrate a peer's work. The existing command's caller and regime restrictions remain in force unless separately amended.

### ADR impact register

| Record | Exact proposed treatment |
|---|---|
| THESIS Part X | Four replacement bullets above; founding edit reserved to operator. |
| Invariants §3, §7b, §7c, §7e | Exact insertion/replacement text above. |
| ADR-038 whisper | Exact replacements and insertion above; no change to command semantics. |
| ADR-003 channel taxonomy | No text replacement. Peer exchange remains durable content plus adapter delivery, not a new lifecycle/control plane. ADR-038's existing six-channel account is not a protocol guarantee. |
| ADR-016 / ADR-095 | No text replacement. Transactional core stays stateless; supervised clients cannot become molecule truth. |
| ADR-052 / invariants §8d | No text replacement. Existing lifecycle writer and witness obligations remain; no parallel completion ledger. |
| ADR-172 | No text replacement. Delegated ordinary harvest, reserved seals, missing grant surface and custody limitations remain exactly its contract. |
| ADR-177 / ADR-181 | No text replacement. Preserve effective-setting observations and root profile behavior; native child isolation must be separately evidenced. |

No other ADR is superseded. C2 owes the CLI/UI parity audit if implementation
adds a user-facing command; this proposed document changes none.

## 6. Evidence and adoption gates

Source review uses Cosmon revision
`9c4b76f5261dff9734c70f2503256575ca5f9f2c` and the supplied Codex revision
`44fe510ce3ee61c8ef623adcbf89b901c73ddd61`. These are repository revisions,
not conversation references. The following are source observations, not runtime
measurements or assurances about installed harnesses:

| Source | Observation and decision consequence |
|---|---|
| Codex `codex-rs/core/src/agent/control/completion.rs:1–4,98–117` | Completion delivery is best effort. Require saved evidence, not a terminal notification. |
| Codex `codex-rs/core/src/agent/child_config.rs:50–108,159–193` | Child configuration inherits/refreshed permissions and cwd. A role is not an independent jail. |
| Codex `codex-rs/core/src/agent/control/api.rs:100–191`; `interrupt.rs:14–50` in that directory | Native peer operations are broader than parent-only assignment. Do not infer owner-only control from ancestry. |
| Codex `codex-rs/agent-graph-store/src/store.rs:13–59` | Spawn ancestry is not the molecule dependency DAG. |
| Cosmon `crates/cosmon-core/src/transport.rs:155–193` | Input submission has a provenance seam; it does not promise context delivery or consumption. |
| Cosmon invariants §3, §7b, §7c, §7e, §8d; ADR-038; ADR-052; ADR-172; ADR-181 | Current doctrine, recovery, authority and profile obligations constrain the exception. |
| Source deliberation `synthesis.md` §B and convergence resolution | Recommends declared attempts and measurements; its panel is not blanket authorization or cross-provider evidence. |

The comparison must include sequential persona simulation (labelled as such),
native separate reviewers, separately supervised reviewers exchanging evidence
on a single provider, and the same exchange between at least two actual
providers. The mixed-provider arm demonstrates the capability; it does not make
provider mixing a requirement of adopted work. Use frozen tasks and a matched
rubric; record provider differences as a confounder. Register model pins,
repetitions, source/input digests, scoring, quotas, stopping rules, failures and
recovery windows before execution. Score source correctness, missed conflicts,
unsupported claims, useful disagreement, elapsed time, observed usage and
recovery effort. Missing usage/pricing observations remain unavailable. Agent
counts do not establish savings.

Mandatory counterexamples: reject cross-scope input and sibling control; lose a
spawn receipt; lose a completion notice; crash the root before/after acceptance;
duplicate delivery; kill a recipient; remove provider history; delete a required
canonical output. Test each provider independently and the round trip. Keep
failures in the denominator and do not treat failed runs as successful timings.

Adoption requires recoverable evidence and obligation accounting, demonstrated
authority restrictions, and a useful measured quality/time/resource tradeoff.
The same observations may justify declining native delegation while retaining
the provider-neutral collaboration contract. No runtime comparison was executed by this
C1 proposal; C2 capability/enforcement evidence and C3 preregistration remain
open. The durable C1 `report.md` distinguishes static evidence, gate results,
inference and remaining work.
