# ADR-176 — Remote harvest authority is a sealed capability, and the closed list forbids degrees of freedom

**Status:** Accepted (2026-08-31).
**Date:** 2026-08-31.
**Decider:** Noogram.
**Authoring molecule:** `task-20260831-6745` — child C4 of the deliberation.
**Deliberated by:** `delib-20260819-cda2` (five-persona panel — turing ·
buterin · jobs · jurist-contract · adversary; 5/5 answered all eight strata).
**Answers:** GitHub issue #51.

**Scope.** This ADR is the successor document that
[ADR-080](080-remote-pilot-port-https-oidc.md) §5.2 requires as the condition
for any verb leaving the closed list of §5.1. It decides the *authority model*
and the *shape of the interdict*. It ships **no code and no route**: the
harvest route itself is a downstream molecule, and the mechanism this ADR
names is not yet built (§9).

**Binds:**
[ADR-063](063-vocabulary-orbitale-nucleon-noyau-phase.md) (Orbitale ⊂ Nucléon ⊂
Noyau — the tenancy axis),
[ADR-077](077-worker-pilot-signing-regime.md) (signing at the remote push
boundary),
[ADR-080](080-remote-pilot-port-https-oidc.md) §5 (the closed list, and the
`delegate_for` exit path this ADR replaces),
[ADR-110](110-single-writer-trunk-and-coordination-invariants.md) (single-writer
trunk; I4 PROGRESS),
[ADR-124](124-tenant-bounded-drain-run.md) (the bounded-drain request door),
[ADR-138](138-autonomous-runtime-two-loop-client-of-core.md) (autonomous
harvest),
[ADR-156](156-resident-runtime-safety-envelope.md) (human-reserved thresholds),
[ADR-171](171-the-operator-gesture-is-a-signature-not-a-string.md) (an operator
gesture is a signature, not a caller-supplied name),
[ADR-172](172-done-authority-is-an-operator-sealed-capability.md) (`DoneAuthorization`
— the doctrine this ADR imports).

---

## 1 · Context

Issue #51 asks whether a JWT bearer on the §8p Remote Pilot Port may harvest
its own molecule. The panel's first result is that the question is
mis-formed, and the reason is mechanical rather than rhetorical.

`cs done` is one verb carrying **two authorities**:

| Authority | What it disposes of | Whose thing it is |
|---|---|---|
| **Closure** | the molecule's own lifecycle — status, worktree, tmux session, fleet slot | the molecule's holder |
| **Integration** | the resolved base branch — a ref every future molecule inherits as its initial condition | the holder of that trunk |

The closed list of ADR-080 §5.1 forbids the *name* `cs done`. It does not
forbid the *effect*. `POST /v1/molecules/{id}/run` — a route deliberately
opened to the tenant by ADR-124 — reaches step 9 of
`crates/cosmon-cli/src/cmd/run.rs`, iterates every molecule of the requested
DAG whose `status.is_terminal()` holds, and shells out to
`cs done <id>` with no flags. `is_terminal()` is
`Completed | Collapsed` (`crates/cosmon-core/src/spec.rs:57`), so the branch of
an explicitly *abandoned* molecule is merged onto the trunk, and the per-molecule
failure is swallowed as `teardown of {id} failed (non-fatal)`.

The effect the list claims to forbid is therefore already produced, from an
open route, over a set of branches the requester chose, with the failure
suppressed. An interdict that protects only a name is worse than an absent
one: it manufactures false assurance. `docs/guides/api-cli-coverage.md` L76
writes **NO (NEVER)** for an effect the `run` route produces today.

That defect (tracked as its own molecule, and closed independently of this
decision) is not the argument for opening anything. Reasoning from an unrepaired
path toward a doctrine lets any unrepaired path become its own justification.
It is the argument for stating the interdict in terms of *what the requester
may vary*, which is what this ADR does.

---

## 2 · Decision

### D1 — The foundation is `DoneAuthorization::Delegated`, and §5.2 clause 2 is replaced

The authority to harvest across the §8p boundary is an **operator-sealed
capability** — [ADR-172](172-done-authority-is-an-operator-sealed-capability.md)
D2, variant `Delegated`, normally mission-scoped, covering the canonical
`HarvestGrant` whose sealed fields include `galaxy`, `scope`, `base`,
`reservations` and `epoch`.

**ADR-080 §5.2 clause 2 is replaced.** That clause requires a
`delegate_for: <nucleon_id>` JWT claim, validated against the molecule's
authorship. Both halves of it are rejected, on independent grounds, by 5/5 of
the panel:

- **`delegate_for` seals a property of the *bearer*, never the bounds of the
  *act*.** A claim that says "this `sub` acts on behalf of that nucleon" names
  no base branch, no policy digest, no reservation set, and no epoch. It
  therefore **cannot be refused when the base has moved** — nothing in it
  changed. It re-opens exactly the TOCTOU window that ADR-172 D3 closed by
  checking authority at the effect boundary, under the trunk lock, immediately
  before the first git mutation. `DoneAuthorization::Delegated` seals a
  property of the *transaction*, which is the only object whose staleness is
  decidable.
- **Authorship is not a foundation either.** It is a state fact written into a
  directory the constrained principal can write — the family ADR-172 already
  falsified (its rejected alternative 5: cwd, environment, uid or an RR-5 event
  as authority). It also proves the wrong proposition: "this molecule is mine"
  says nothing about the trunk. In the vocabulary the panel's jurist supplied,
  a *fait générateur* founds no power of disposition. Authorship remains, at
  most, a condition of **admissibility of the request** — never a condition of
  effect.

The `delegate_for` claim keeps its ADR-080 §6.2 status of `UnsupportedClaim`.
It is not promoted at V2; it is retired as an authority model.

### D2 — A separation predicate, not a second verb

The two authorities of §1 do not diverge in general. Under per-nucleon tenancy
(ADR-080 §8.1: one kernel state directory per tenant) the trunk a tenant drain
writes **is the tenant's own trunk**, so the holder of the closure and the
holder of the trunk are the same nucleon. They diverge in exactly two
topologies: the **self-hosted galaxy** (cosmon-on-cosmon, where the "tenant"
writes cosmon's own trunk) and the **multi-nucleon galaxy** (several Nucléons,
one trunk).

Splitting the verb universally would pay a general cost for a particular risk.
The robust form is a predicate:

> **The integration authority is distinct from the closure authority when, and
> only when, the molecule's resolved base equals the kernel's reference
> trunk.**

`resolved_base == reference_trunk` is not a new computation. It is already
performed, for a neighbouring reason, at
`crates/cosmon-cli/src/cmd/done.rs::post_merge_deploy_allowed` — which bounds
the deploying `post_merge` hook to `base == trunk`, resolving the two sides
through `cosmon_cli::base_branch::resolve_with_source` and
`cosmon_cli::base_branch::reference_trunk`. A stacked DAG child whose base is
its parent's branch writes only inside its own mission subtree; there, the two
holders coincide and the second authority does not arise.

The harvest door therefore exercises **one** predicate and, depending on its
value, **one or two** authorities. It does not present the tenant with two
verbs to choose between: the requester states an intent, the door decides which
authorities that intent needs.

### D3 — Invariant: a closure that did not merge never deletes the branch

**An unmerged closure MUST NOT delete the branch.** This is raised here from
implementation behaviour to invariant, because it is the condition without
which the decomposition of §1 collapses: deleting the branch of work that never
landed destroys the only copy.

The code already enforces it, and enforces it more strongly than the invariant
demands. `decide_branch_delete` (`crates/cosmon-cli/src/cmd/done.rs:4275`)
returns `Skip` when the branch is absent or deletion is disabled, and
`RefuseUnmerged` whenever ground-truth topology says the branch is not an
ancestor of the base — **overriding `--force`**. Only when the branch is
reachable from the base does `merge_succeeded || force` permit deletion. The
invariant pins the topology guard, not the flag.

### D4 — REVERSED (2026-09-07): the parameters were never what the seal protected

> **Status: reversed.** The decision below is retired. Its original text is
> kept in full, because a reversal that hides what it reverses teaches
> nobody. Read the original first; the reversal argument follows it.

#### D4 as originally decided (2026-09-01) — retired

**No option crosses the wire.** Not `--force`, `--strategy`,
`--skip-pre-done-hook`, `--no-branch-delete`, `--propel-message` — and not
their successors. The reason is one sentence, and it generalises past the
current flag set:

> **A derogation requested by its beneficiary is not a derogation.**

Each of those flags exists to let *the operator* overrule a gate that was built
to protect a third party. `--skip-pre-done-hook` waives the galaxy's
Definition-of-Done. `--force` overrules a refusal to delete. `--strategy`
selects how the trunk absorbs a diff. A remote requester holding any of them
holds the gate's own off-switch, which means the gate protects nothing against
that requester. The door exposes **no parameter that changes a gate's verdict,
a merge's strategy, or a destructive step's precondition.**

**Corollary — how §5.1 must be read.** The closed list is a list of *degrees of
freedom*, not of *verb names*.

#### Why it is reversed

The load-bearing sentence — *a derogation requested by its beneficiary is not
a derogation* — is true, and it is **conditional**. It holds when the
requester is a **constrained principal, distinct from the party the gate
protects**. Strip that condition and the sentence says nothing: every
derogation is requested by someone who benefits from it, including the
operator typing `cs done --strategy ff-only` on their own machine, and by
that reading no flag would exist at all.

The deployment that exists is single-tenant: **one galaxy, one nucleon, one
user, and the requester *is* the operator.** There, beneficiary and protected
party are the same person. `--skip-pre-done-hook` waives a Definition-of-Done
that person wrote, for a molecule that person nucleated, on a trunk that
person owns. The gate protects nobody from them, and withholding the flag is
not a safety property — it is an amputation of their own verb, reachable at
their own terminal one command away.

The corollary goes with it. Reading §5.1 as a list of *degrees of freedom*
rather than of verb names is what allowed `land` to be built: a second name
performing `done`'s effect with a fixed argument set, conforming to the
letter of a classification that was itself wrong. ADR-080 §5.1 is amended
(its new §5.4) and `cs done` leaves the closed list under its own name. The
list is a list of verbs.

#### What is decided instead

`POST /v1/molecules/{id}/done` carries the **full parameter set** of
`cs done`, expressed once in the domain as
`cosmon_core::harvest_door::HarvestOptions` so that the wire, the CLI and
the merge read the same options and a parameter cannot mean one thing in a
request body and another at the merge. Two departures from that totality,
both stated rather than silent:

- **`--dry-run` has no wire counterpart.** It prints a teardown plan for a
  terminal, and the door's own decision half (`harvest_door::decide`) is the
  wire's preview: it answers every pre-effect refusal without mutating
  anything. Two previews with different answers would be two doors.
- **`--reason` is mandatory here and optional at the CLI.** This is the gap
  the reporters named: `land` fabricated a generic reason. An operator at
  their own terminal authors the history a harvest writes; a requester
  reaching over §8p does not, and the trunk-side reason is the only account a
  later reader has of why someone else's molecule was closed. A request
  without one is refused `missing_reason` — the eighth named refusal, exit
  code 77 — and none is invented. The seven refusals of D7 keep 70–76.

**D6 does not travel with D4.** Auto-propel stays **disarmed by default on
this route**, and arming it additionally requires the `cosmon:worker:spawn`
scope. Escalation is not a merge parameter: it injects a natural-language
instruction into a live worker session to resolve a conflict *on the trunk*,
and renders the result under a success label. It is an agent dispatch wearing
a flag, and it is the one option on this route that spends agent budget.

#### What would bring D4 back

**A requester who is not the operator** — the multi-tenant phase. The
condition is exactly the one the original sentence needed and the current
deployment does not satisfy. When it arrives, note what it will actually
need: a restriction on **which molecules** a requester may close, not on
which parameters they may pass. Those are different questions, and D4
answered the second while the first is the one that protects anybody. D5
stands and still refuses an `owner` field; nothing here adds an ownership
notion.

**What the reversal does not touch.** D1 (the authority foundation) stands:
the JWT authenticates the requester, the operator-sealed
`[harvest_authority]` arming authorises the effect, verified at the effect
boundary with facts re-derived there. The seal answers *may this requester
cause this effect at all*; the parameters answer *how*. Reversing D4 does
not dismantle D1 — a galaxy that has armed nothing still refuses every
harvest `not_authorized`. D2 (the door decides which authority the intent
needs), D3 (a closure that did not merge never deletes the branch), D5 and
D7 (the three failure policies, the seven named refusals and their exit
codes) are likewise untouched.

### D5 — The holder is the kernel, read from the path; no `owner` field

**The holder of a molecule is the nucleon, and it is read from the path**
(`<galaxy>/.cosmon/state/`). **No `owner` field is added to `MoleculeData`.**
Three independent reasons:

1. **The function is total on day one.** Every molecule already lives under
   exactly one kernel state directory. There is no molecule without a holder,
   past or future, and therefore no migration to write and no `Unowned` case to
   decide.
2. **A field would create two competing truths** about one question — the path
   says A, the field says B — and they would diverge at the worst moment, which
   is the moment someone edited one of them.
3. **The only property that authorises cannot live in a file its subject can
   rewrite.** This is ADR-172's rejected alternative 5 applied to storage rather
   than to process shape.

**The worker is never the holder.** It is extinguished by the very exercise of
the right one would attribute to it: the transaction it would authorise removes
its worktree and kills its session. In the panel's terms, the worker has the
*jouissance* of its worktree, never the *disposition* of the trunk.

**What does not transpose**, and naming it is part of the decision:

- **No succession.** A dead worker leaves nothing, because it held nothing in
  its own name. Building an inheritance of molecules would manufacture a path
  for acquiring title through the event "a process died" — an event the caller
  knows how to cause.
- **No escheat.** There is no vacancy to fill: the holder is the kernel, and
  the kernel is read from the path.
- **No acquisitive prescription.** Any rule of the form *"after N days without
  activity, the resident loop may harvest"* is prescription under another name
  and is refused **under that name**. Staleness is a scheduling signal; it is
  never a mode of acquiring authority.

Intra-kernel granularity is deliberately not introduced. If two principals
inside one galaxy must be distinguished, the answer is a second nucleon — a
second directory, a second trunk, a second lock — not a field. A Nucléon may
hold N Orbitales (ADR-063), so provisioning a second one is not harder than
provisioning a second token.

### D6 — Auto-propel is disarmed on the remote path

**The remote harvest path MUST NOT auto-propel.** `--propel-message` is
unreachable (D4) and the escalation ladder's propel rung is off, not
merely defaulted off. Three independent reasons, each sufficient:

1. **It is `cs whisper --to-session` in substance.** The escalation propels a
   live worker session with an instruction. Cross-session text injection into a
   live worker is forbidden by the same closed list (ADR-080 §5.1, citing
   ADR-038). Reaching it through `done` rather than through `whisper` does not
   change what it is.
2. **It is agent spend triggered by a refusal**, outside the credit guard of
   the tenant journey. A request that fails becomes a request that costs more
   than a request that succeeds, and the tenant's bounds (ADR-124 B1/B2/B3)
   never saw it.
3. **It is an injection surface.** Conflict resolution *on the trunk* is
   delegated to an agent whose context is partly authored by the requester —
   the molecule briefing, written by the tenant through `POST /v1/molecules` —
   on a natural-language instruction carrying no verifiable constraint. The
   outcome is then rendered as `merged_after_{n}_escalation(s)`
   (`crates/cosmon-cli/src/cmd/done.rs:2296`): a **success label**. Neither the
   closed list nor ADR-124 mentions this path.

### D7 — Three failure policies, and they are three

The door's three refusals are not one policy with three messages. They differ
in *where the missing information lives*, and that is what dictates where each
is handled.

| Refusal | Nature | Handled |
|---|---|---|
| `merge_conflict` | **execution event** — the information is in the repository, and the repository says the two sides disagree | at the request. Named refusal, nothing torn down, nothing merged, branch and worktree preserved, trunk lock released |
| `base_not_fast_forward` | **configuration error** — fully decidable before the first request ever arrives | **at capability arming**, never at the request |
| `pre_done_refused` | **verdict** — the missing information is not in the repository but in a human's judgement | queued, bounded, escalated |

**`base_not_fast_forward` is refused when the capability is sealed.** Whether
the resolved base admits a fast-forward-only policy is a property of the
operator's configuration, knowable at arming time. A door that lets a tenant
discover this refusal at the bottom of a detached loop has converted **an
operator configuration error into a runtime failure class charged to the
tenant**. The seal's `base` field makes the check mechanical: if the named base
cannot accept the configured strategy, the grant does not issue.

**`pre_done_refused` may queue, and the queue must be bounded and refusing.**
The panel divided here between refusal-without-retention and a queue, and the
reconciliation is the operative rule:

> **A bounded, refusing queue is a delayed refusal, not a stall.**

Beyond a **sealed threshold** of molecules in the *closed, not integrated*
condition, the door **refuses subsequent requests**. An unbounded queue would
be a silent block — the request neither failed nor progressed, and nobody is
coming — which violates ADR-110 I4 (PROGRESS). A bounded one always terminates
in a named outcome; the threshold is a field of the operator's seal, not a
tenant parameter (D4).

The queue is accounting for a debt, not a resolution mechanism. It is
observable, it is capped, and reaching the cap is itself a named refusal.

---

## 3 · What this ADR does not decide

- **The route.** No `POST` path, no request body, no `result_status` value is
  fixed here. The downstream molecule that exposes the door decides those,
  inside the bounds above, and owes the CLI/UI parity audit for whatever
  command bytes it adds.
- **The persisted non-integration reason.** The panel converged on: no new
  `MoleculeStatus` variant (a ninth variant breaks `Frontier`, which releases
  successors on `status == Completed && merged_at.is_some()`), a reason field
  persisted next to `merged_at` and written **trunk-side under the lock**, and
  one more `result_status` value so the tenant stops reading `ready` for work
  that never landed. That is a state-shape decision for the implementation
  molecule; this ADR only requires that whatever it chooses supply a
  **retry predicate**, without which each `POST /run` re-attempts the same
  blocked molecules and `max_retries` — capped per invocation — stops being a
  cap.
- **Intra-kernel ownership granularity** (D5, deliberately deferred to a real
  demand for isolation *inside* a galaxy).
- **A defect of ADR-124 itself**, raised by one panelist and out of scope here:
  B1/B2/B3 bound the drain's *intensity*, but `root_id` chooses the *set* — the
  decider decides how much, the requester decides on what.

---

## 4 · Rejected alternatives

1. **Expose `cs done` under its own name with a `delegate_for` claim** — the
   literal §5.2 exit path. Rejected by D1: the claim cannot be refused when the
   base moved.
2. **Found the authority on authorship.** Rejected by D1: a state fact the
   constrained principal writes, proving the wrong proposition.
3. **Split the verb in two universally** (a closure verb and a merge verb).
   Rejected by D2: pays a general cost for a risk confined to two topologies,
   and hands the requester the choice of which authority to exercise.
4. **An `owner: Option<NucleonId>` field, fail-closed or fail-open.** Rejected
   by D5. Note the asymmetry the minority itself supplied: fail-closed on
   closure freezes existing unharvested molecules, turning an attribution gap
   into a resource leak that the operator then works around with `--force` — a
   guard bypassed by `--force` has created the gesture it meant to prevent.
5. **A staleness rule permitting harvest after N idle days.** Rejected by name
   in D5: acquisitive prescription.
6. **Refusal-without-retention for all three failures.** Rejected in part by
   D7: it treats an operator configuration error as a tenant runtime failure,
   and discards the one case (`pre_done`) whose missing input is a human
   judgement.
7. **An unbounded queue of conflicts awaiting an operator.** Rejected by D7:
   ADR-110 I4.

---

## 5 · Consequences

- ADR-080 §5.2's numbered exit path stands, with clause 2 rewritten: a
  successor ADR (this one) plus **`DoneAuthorization::Delegated`** — not
  `delegate_for` — plus the coverage-guide row.
- ~~The closed list of §5.1 acquires a reading rule (D4): it enumerates degrees
  of freedom.~~ **Retired with D4 (2026-09-07).** The list is a list of verbs.
  `cs done` left it under its own name via ADR-080 §5.4 (issue #51), and a
  future row is justified by the verb being administration rather than
  lifecycle.
- `docs/guides/api-cli-coverage.md` L76 and L77 are amended (§10). Both stay
  `NO (NEVER)`; the drift gate (`crates/cosmon-cli/tests/api_cli_coverage.rs`)
  is unaffected because the exposure column does not change.
- ADR-172's obligations become the entry condition for any harvest route
  (§9). Its falsifier 8 applies verbatim here: no document may claim this
  prevents direct same-uid git mutation before repository custody is separated.
- This ADR changes no command bytes and owes no parity-audit row of its own.

---

## 6 · Two verified findings that strengthen D1

The deliberation carried two panel observations its synthesiser had **not**
re-verified, with instructions not to cite them unread. Both were read here.
Both hold, and each is a live defect deserving its own correction — they are
recorded, not fixed, by this document.

**V1 — The only authority guard `cs done` runs today is fail-open and indexed
on an environment variable. CONFIRMED.**
`refuse_unleased_pilot_gesture` (`crates/cosmon-cli/src/cmd/guard.rs:732`,
invoked from `cmd/done.rs:1651`) returns `Ok(())` unconditionally when the
mission carries no pilot lease — which is every molecule on every fleet today —
and, inside its perimeter, resolves the caller from `COSMON_SESSION_ID` or
`CLAUDE_SESSION_ID` (`resolve_pilot_session`). The scoping is deliberate and
documented: the guard exists to make the read-only co-pilot of a *co-piloted*
mission read-only (ADR-168 D6), and reading it as a fleet-wide kill-switch is
explicitly declined at its definition. The finding is therefore not "a guard
was written wrong"; it is that **the guard is not an authority mechanism**, and
`cs done` currently has none. That is precisely the gap D1 fills, and it is why
`DoneAuthorization` cannot be approximated by a caller-supplied name: neither
of those two variables is stripped by
`crates/cosmon-rpp-adapter/src/subprocess.rs`'s `STRIP_VARS`, so a value
present in the container's environment is read by the guard as the caller's
identity.

**V2 — The only implemented reservation gate is satisfiable by the principal it
constrains. CONFIRMED.**
`require_security_review_verdict` (`crates/cosmon-cli/src/cmd/done.rs:5543`)
fires on `needs-review`, `security` or `security:*` and demands a line
`verdict: approved` in `<state_dir>/molecules/<id>/review-verdict.md`. That
tree is the galaxy state root, which the worker writes to by design — worker
briefings direct durable artifacts there. A same-uid worker can therefore write
its own approval. A second, independent defect surfaced while verifying: the
path is built by hand and does **not** match `FileStore::molecule_dir`, which
resolves the fleet layout `fleets/<fleet>/molecules/<id>/` (or legacy
`ops/molecules/<id>/`). Under the shipped layout the gate reads a path that
does not exist, so today it fails **closed** for every tagged molecule — the
correct verdict, reached by a wrong route, and one that flips to fail-open the
moment someone "fixes" the path without moving the file out of the worker's
write set.

Both findings say the same thing in two places: **a gate whose input the
constrained principal can author is not a gate.** Neither is repaired here.

---

## 7 · Relation to ADR-124 — why the dissolution does not simply repeat

ADR-124 dissolved §5.2's objection to `cs run` by making it a *request door*:
the tenant asks for a drain of its own DAG, and the resident loop decides,
under server-sealed bounds. The panel split on whether that reasoning
transposes, and the split is mostly verbal — all five converge on the same
apparatus (no options, server-side decision, named refusals, a seal that names
the base). What is substantive is the asymmetry of the object:

ADR-124 bounded a **quantity** (depth, width, budget), and a quantity is
expressible in a server seal. `done` moves a **ref**, and there is no B3 for
*how much trunk*. So the seal must name **conditions** — base, policy digest,
reservations, epoch — rather than magnitudes. That is exactly `HarvestGrant`,
and it is why D1 imports ADR-172 instead of extending ADR-124's bound vector.

---

## 8 · Falsifiers

This decision is violated if any of these becomes true:

1. ~~Any §8p route accepts a parameter that alters a gate verdict, a merge
   strategy, or a destructive step's precondition (D4).~~ **Retired with the
   D4 reversal (2026-09-07)**: the harvest door carries the full parameter
   set of `cs done`. Replaced by the falsifier the reversal actually owns —
   *a request reaches the merge with a parameter the requester did not send,
   or fails to reach it with one they did*, asserted at the options the
   effect receives rather than at the field that parsed.
2. A harvest is authorised by a JWT claim, an authorship record, a session id,
   a cwd, or any other value the requester can author (D1, D5).
3. A grant issued against one base authorises an effect on another, or survives
   a change to a covered field (ADR-172 D3, inherited).
4. A closure that did not merge deletes the branch (D3).
5. The remote path propels a worker, on any code path, under any flag name
   (D6).
6. `base_not_fast_forward` is first reported at request time rather than at
   capability arming (D7).
7. The `pre_done` queue is unbounded, or reaching its bound is silent rather
   than a named refusal (D7, ADR-110 I4).
8. An `owner` field, an inheritance rule, an escheat rule, or an
   idle-time-confers-authority rule is introduced for molecules (D5).
9. Documentation claims this ADR prevents direct same-uid git mutation of the
   trunk (ADR-172 D5 and falsifier 8, inherited).

**Reversal condition.** If the separation predicate of D2 turns out never to be
true in practice — if no self-hosted and no multi-nucleon galaxy ever runs a
tenant drain — then the second authority never arises, and the door carries the
cost of a distinction that never fires. The honest response then is to collapse
the predicate, not to keep it as decoration.

**A second falsifier, recorded from the deliberation.** If usage shows that
tenants never want closure alone — that they want integration or nothing — then
a closure-only door is a dead verb that widens the §8p surface for nothing. And
if the tenants whose molecules pile up turn out never to have wanted to
integrate anything, the real need was **expiry**, not closure, and this
decision answered a demand nobody made.

---

## 9 · Entry into force

`DoneAuthorization`, `HarvestGrant`, `OperatorHarvestSeal` and
`DelegatedHarvestCapability` are **doctrine, not code**: none of the four has
any occurrence under `crates/` at the time of writing. ADR-172 is an accepted
decision whose mechanism is unbuilt.

This ADR therefore rests on ADR-172 as accepted doctrine and says plainly that
the mechanism is not pledged. **Its entry into force is conditioned on the
implementation of `DoneAuthorization`** — the reducer and both variants, pure
and I/O-free, with authority checked at the effect boundary under the trunk
lock (ADR-172 D3). Until that molecule lands, remote harvest stays refused
**for want of a mechanism, not by doctrine** — and the difference in the reason
says what must be built to lift the refusal, instead of enshrining it.

The panel was unanimous on the ordering, for a reason independent of the
authority question: opening a door beside a broken door does not reduce the
surface. The `run`-step-9 defect of §1, the `COSMON_SKIP_PRE_DONE_HOOK`
deny-list gap, and the missing `COSMON_API_REQUEST` parse-time refusal for
`done` are all repairs to the *current* state and are more urgent than this
decision about the intended one.

**Postscript — the condition is met, and the door exists.** All three repairs
landed (`8b54b0bd`, `6c8eee2f`, `f0bfe4f9`), and `DoneAuthorization` is code
(`crates/cosmon-core/src/harvest_authorization.rs`, verified and consumed at
the effect boundary by `crates/cosmon-cli/src/cmd/done_authority.rs`). The door
this ADR bounds was then built by `task-20260901-3b53` as the verb **`cs land`**
and the route **`POST /v1/molecules/{id}/land`**.

**Second postscript (2026-09-07, issue #51 reopened).** Both were **withdrawn**
by `task-20260907-6ddc`. `land` was a second name for `done`, and it existed
only because ADR-080 §5.1 classed `done` as operator-only — a classification
the issue's reporters showed to be the upstream error (see the D4 reversal
above and ADR-080 §5.4). The operation has one name again: `cs done`, and
`POST /v1/molecules/{id}/done` carrying its full parameter set. What remains
from `land` unchanged is everything that was actually load-bearing: the
`[harvest_authority]` second key, the ordered pre-effect refusals, the seven
named refusals with their exit codes, the never-202 shape, and the
trunk-side non-integration reason projected to the remote client — the change
that made a stranded merge visible instead of silent.

One falsifier of §8 could **not** be satisfied as written and is recorded
rather than papered over: falsifier 6 wants `base_not_fast_forward` refused at
capability arming, and cosmon ships no grant-issuing path to put that check in
(ADR-172 D2 — verification without a signer). The door instead makes the class
unreachable through itself (it fixed `strategy = Merge`, so no request could
select a fast-forward) and answers the residue as `503` — the only refusal on
this route that is not charged to the requester.

**Amended by the D4 reversal (2026-09-07):** a request *can* now select
`ff-only`, so the class is no longer unreachable through the door. The `503`
answer is what carries the policy: `base_not_fast_forward` stays an operator
configuration fault, never charged to the requester as a 4xx, and the
falsifier stays recorded-as-unmet for the same reason as before — cosmon
still ships no grant-issuing path to put an arming-time check in.

---

## 10 · Amendments to `docs/guides/api-cli-coverage.md`

Rows L76 (`cs done`) and L77 (`cs stitch`) are rewritten to name the effect and
the withheld degrees of freedom rather than a gesture, and to cite this ADR in
place of the retired `delegate_for` exit path. Neither row's *Exposed via API?*
column changes: both remain `**NO (NEVER)**`.

---

## 11 · Amendment — the door is a library, and the trunk flock binds at the effect boundary (issue #54 U3)

**Status: adopted, 2026-09-05.**

### The change

The door's body — the ordered refusal checks of §D7, the idempotence read,
the backlog census, and the post-effect interpretation — moves out of the
binary-private `cmd/land.rs` into the library entry
`cosmon_filestore::harvest_door::land(...)`. `cs land` calls it; the §8p
route `POST /v1/molecules/{id}/land` calls the same body **in-process**,
so every pre-effect refusal and the `already_landed` idempotent success now
answer against an image that carries no `cs` binary at all. One body, two
callers: the two doors cannot drift, which is the property the shared
`DoorRefusal` vocabulary was built to protect.

The **effect half** — the sealed `cs done` transaction: the merge with its
lineage trailers, the publish/identity/confidentiality gates, the pre/post
hooks, the teardown — is *injected* through the
`SealedHarvestEffect` port rather than moved. Its one production
implementation is `cmd/done.rs`'s sealed-door path; a second implementation
would be a second door. Until that path is itself library-callable, the §8p
route reaches it through the one subprocess this route still owns, and that
subprocess is the named seam the mission's adapter cut-over unit retires by
implementing the port in a library — without touching the decision half
again.

### The decision this amendment exists to record: the flock

The route's old justification for the subprocess read: *the subprocess
exists so the advisory `trunk.lock` flock binds inside the tenant
container.* Moving in-process, the I1 WRITER-UNIQUE invariant is preserved
by fixing **where the flock binds**, not which process binds it:

**The `trunk.lock` flock binds exactly once per harvest, at the effect
boundary.** Concretely:

1. An effect that owns an effect boundary of its own — the sealed `cs done`
   transaction, whether reached in-process or as a subprocess — acquires
   the flock there, where ADR-172 D3 re-derives every fact under the lock.
   Such an effect declares `binds_trunk_lock`, and the door **must not**
   hold the lock across the call: `flock(2)` does not nest — a child
   process blocks forever against its parent's descriptor, and a second
   descriptor in the same process blocks against the first — so a door
   that held it would deadlock the harvest, not serialize it.
2. An effect with no boundary of its own is serialized **by the door**,
   which wraps it in the existing `StateStore::lock_trunk` helper — the
   same `.cosmon/state/trunk.lock` path, the same blocking semantics, no
   new lock code — whose RAII guard releases on every exit path including
   panic (the guard drops on unwind).

The validity condition the old comment gestured at is a *filesystem*
condition, not a process one: the adapter process and any `cs` child flock
the same file on the same kernel, so an in-process acquisition excludes a
concurrent subprocess holder and vice versa. What actually mattered was
that *somebody* binds it, exactly once, around the mutation.

**Falsifier** (in force, verified red-then-green):
`harvest_door::tests::two_concurrent_in_process_lands_serialize` runs two
concurrent in-process `land` calls against one kernel through two
independent store handles and asserts the effect never observes a second
caller inside it. Removing the door's `lock_trunk` acquisition makes the
overlap observable and the test fails (exit 101, verified before landing).
A companion test pins the panic-release property, and a third pins that a
`binds_trunk_lock` effect can take its own boundary lock — i.e. that the
door is not holding it.

### What does not change

~~The closed list (§D4) is untouched: the library entry takes the molecule
and the effect, nothing else, and the route still refuses any request body.~~
**Amended 2026-09-07:** the D4 reversal gives the library entry a third
argument — the requester's `HarvestOptions` — and the route accepts them.
The seven refusals, their labels, their exit codes 70–76 and their HTTP
statuses are byte-identical (`missing_reason` is added as an eighth, code
77, and displaces none); the only observable route change beside
latency is that a well-formed molecule id the tenant's store has never
seen now answers `404 not_found` from the decision half — the same
no-existence-oracle boundary the rest of the surface holds — where it
previously fell through to the subprocess's anonymous failure.

## 12 · Amendment — the effect half is a typed refusal until the port has a library implementation (issue #54 U6)

**Status: adopted, 2026-09-05 (`task-20260905-b954`).**

The subprocess §11 named as "the one subprocess this route still owns" is
retired with the rest of the ADR-080 §3.5 clause (e) envelope. The
`SealedHarvestEffect` port did **not** gain a library implementation in the
same unit: the sealed `cs done` path in `cmd/done.rs` is ~1 600 lines whose
provenance gates, lineage trailers and teardown do not move cleanly behind
the existing ports without forking the door — the exact drift §11 refuses.

Until the port has its library implementation, a harvest the in-process
decision half ADMITS answers the typed refusal
**`501 land_effect_unavailable`** — a new label, deliberately **outside** the
seven-name closed set (`DoorRefusal` stays closed; the parity gap has its own
name so no client can mistake "this adapter build cannot integrate yet" for a
door refusal). Never a silent `cs` fallback, and never a 202: the answer is
synchronous and true, which is the issue #51 property this route exists for.

What still answers in full, in-process: every pre-effect refusal
(`not_authorized`, `not_completed`, `reservation_requires_seal`,
`backlog_full`), the `already_landed` idempotent success, and the
no-existence-oracle 404. The three execution refusals (`merge_conflict`,
`base_not_fast_forward`, `pre_done_refused`) belong to the sealed transaction
and return with its library implementation — the named follow-up, tracked in
ADR-080 §3.5.3's enumerated parity gap alongside the drain's teardown leg.

### Amendment (2026-09-07, issue #51) — the effect is a port with two implementations

The label is renamed `harvest_effect_unavailable` with the withdrawal of
`land`, and the "until" clause acquires an answer for the deployment that
actually exists.

The sealed transaction still has exactly one implementation — `cmd/done.rs` —
and it still does not move behind a library without forking the door, so §12's
refusal to rewrite it stands. What changes is that the effect half is now an
explicit **port** (`cosmon_rpp_adapter::harvest_effect::HarvestEffectPort`)
with two implementations the operator chooses between:

- `UnavailableHarvestEffect`, **the default** — the honest `501` above, for an
  image that carries no `cs`. Nothing about §12 changes for that deployment.
- `CsBinaryHarvestEffect`, wired when the operator declares
  `harvest_cs_binary` in `rpp.toml` — the harvest runs as that binary, with
  the argv `HarvestOptions::cs_done_argv` builds, in the tenant's galaxy root.

The second is legitimate now and was not before, for one reason: ADR-080 §5.1
listed `done`, so `cs` refused it under the request envelope (§3.5's second
lock) and no child could perform it in any shape. With §5.4 taking `done` off
that list, a child that closes a molecule is an ordinary local gesture, and it
announces itself as one — `api_envelope::hand_off_to_local_child` consumes the
request marker, exactly as the resident drain's own `cs done` teardown already
does, and consumes **no** security posture (the egress variables and the
exposed-host refusal are untouched).

This does **not** restore the general §3.5 clause (e) subprocess envelope that
issue #54 U6 retired. It is one port, one verb, one operator-declared binary,
off by default, with no PATH discovery — a door that found its own executor
would change behaviour the day someone else's `cs` appeared on the host.
