# ADR-080 — Remote Pilot Port (RPP) — §8j HTTPS+OIDC ingress adapter

> **Reframed by [ADR-117](117-rpp-central-security-capability.md) (2026-06-05).**
> This ADR fixes the RPP's *architecture* (the §8j HTTPS+OIDC ingress
> adapter). ADR-117 fixes its *purpose*: the RPP is cosmon's central,
> audited secure-delivery door — the boundary through which a remote pilot
> reaches a cosmon instance running on a hardware-encrypted VM. It is **not**
> a client-specific feature, and (honesty constraint, ADR-117 §2b) it
> *fronts* a tenant's hardware-encryption stack without itself owning
> encryption today. Read ADR-117 for the framing; this document for the
> mechanism.

**Status:** Proposed (2026-04-27)
**Decider:** Noogram, on operator request to formalise the OIDC ingress decision before any code lands
**Parent deliberation:** `delib-20260427-d2ce` (5-persona panel: forgemaster · turing · tolnay · jobs · wheeler)
**Authoring task:** `task-20260427-16aa`
**Spec source:** `docs/delib-prep/2026-04-27-api-rest-oidc-ingress-layer.md`

**Binds:**
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (autonomy regimes — RPP is a Layer-B adapter, not a daemon),
[ADR-035](035-cross-galaxy-edges.md) (cross-galaxy edges — multi-tenant routing precedent),
[ADR-047](047-event-log-protocol-v0.md) (`events.jsonl` — the audit log),
[ADR-056](056-notary-protocol-v0.md) (notary key segregation),
[ADR-058](058-step-progress-invariant.md) (briefing-seal pattern — re-used for OIDC identity files),
[ADR-061](061-pilot-session-and-causal-closure.md) (pilot-session, `nucleon_id`, causal closure),
[ADR-063](063-vocabulary-orbitale-nucleon-noyau-phase.md) (Orbitale ⊂ Nucléon ⊂ Noyau — multi-tenant axis),
ADR-064 (postman's-uniform / wheat-paste — §8k preempted),
[ADR-066](066-ux-v2-substrate.md) (wheat-paste viewport — UI side),
[ADR-068](068-ux-cli-equivalence.md) (UX ↔ CLI parity = §8l/§8m — *not* extended to API/CLI here),
[ADR-072](072-session-route-formula-and-sidecar-invariants.md) (session-route — output cascade precedent),
[ADR-073](073-ensemble-substrate.md) (`Utterance` primitive — RPP transports utterances),
[ADR-076](076-cs-security-binary-posture.md) (`prepared` / `active` security postures),
[ADR-077](077-worker-pilot-signing-regime.md) (operator-only verbs — `cs done` is operator gesture).

**Architectural invariants:** `docs/architectural-invariants.md`
§8a (control-plane shared state),
§8d (`events.jsonl` source-of-truth),
§8e (causal closure),
§8j (ingress bindings — *RPP is the second instantiation, after Matrix*),
§8k/§8k' (postman's uniform / wheat-paste — preserved by construction; the §8k slot is *not* re-used here),
§8l/§8m (UX ↔ CLI parity — explicitly *not* extended to API/CLI),
**§8p (proposed by this ADR)** (API surface ⊊ CLI surface — subset strict).

---

## 1. Context

### 1.1 What the operator asked

`delib-20260427-d2ce` (panel forgemaster, turing, tolnay, jobs, wheeler) deliberated where to land an HTTPS+OIDC ingress layer that lets external pilots — operators at tenant orgs, and future tenants — drive cosmon over the network without an OS-native `cs` install. The operator's framing was *"REST/JSON/JWT-OIDC façade for cosmon multi-tenants"*.

The panel converged on a strictly narrower object than *"a REST API"*:

> *"L'API n'est pas une couche cosmon — c'est un adaptateur HTTPS sur le port d'ingress §8j qui existe déjà"* (synthesis §0).

The question this ADR answers is therefore **not** *"how do we build an API?"* but *"how do we instantiate §8j on the HTTPS+JWT substrate without breaking the stateless-CLI / no-daemon / JSON-on-disk founding invariant?"*.

### 1.2 What §8j already says

§8j (`docs/architectural-invariants.md`) governs **any non-CLI spark source**. Its four clauses are: (a) identity mapping → `nucleon_id`; (b) causal closure (materialise on disk before `state.json` write); (c) pre-admission rate limit; (d) one-way topology (no reflexive feedback channel). The Matrix bridge (`crates/cosmon-matrix-tick/src/admission.rs`) is the first instantiation. §8j carries an explicit meta-rule:

> *"Future ingress ports rewrite §8j for their own substrate; no §8k / §8l / ... is admitted. The invariant surface remains finite by construction."*

This ADR honours that meta-rule: the HTTPS+JWT ingress is **the second instantiation of §8j**, not a new §8 invariant class. The substrate forces one *additional* clause (subprocess envelope — clause **(e)** below) which Matrix did not need because Matrix admission terminates by writing inbox files that `cs` later picks up; HTTPS, by contrast, must invoke `cs` synchronously per request. Clause (e) is therefore documented as the **substrate-specific extension** of §8j for substrates where the adapter invokes `cs` in-band, not as a new top-level invariant.

### 1.3 What `cs-api` already is (prior art, distinct scope)

`crates/cosmon-api` (see `docs/guides/cs-api.md`) is a **loopback** adapter: localhost-only, used by native pilot apps (mac-pilot, ios-pilot) on the operator's own machine. It does **not** authenticate, has **no** OIDC, **no** kill-switch, **no** multi-tenant. It is the local pilot transport — out of scope here.

The new component this ADR governs is `cosmon-rpp-adapter` (Remote Pilot Port). It is **internet-exposed**, **OIDC-authenticated**, **multi-tenant**, and **kill-switchable**. It shares with `cs-api` exactly one structural property: every request shells out to the real `cs` binary. It shares nothing else.

### 1.4 What the synthesis explicitly rejected

| # | Rejected | Reason |
|---|----------|--------|
| R1 | A new daemon Layer A. | The HTTP server is a Layer B *port adapter* (ADR-023 hexagonal), not a long-lived state holder. |
| R2 | Shared in-RAM state between requests. | Five panel personas converge: zero molecule cache, zero session store, zero in-RAM pile. JWKS / rate-limiter / kill-switch only — all idempotent projections of the filesystem. |
| R3 | An OAuth Authorization Code flow inside `cosmon-rpp-adapter`. | Resource Server, not Authorization Server — RFC 6749 §1.4. The IdP is external (YubiKey-bound JWT in V0; Keycloak in V1+). |
| R4 | A web dashboard. | Cascade of six unrelated features (login UI, password reset, CSRF, SPA, designer, accessibility audit). Out of scope, *forever* unless a separate ADR rescinds it. |
| R5 | Cognito, IAM Identity Center, Auth0. | Vendor lock-in (DELIB-2 §13 niel red-line). Confirmed by all five personas. |
| R6 | Auto-derived OpenAPI from Rust types. | A refactor of internal types would silently break the public API (tolnay). OpenAPI is hand-written and decoupled. |
| R7 | Extending §8l (UX ↔ CLI parity) to API/CLI. | §8l is a bijection for human surfaces; the RPP is a *strict subset* of the CLI by design (§8p, below). |

---

## 2. Decision

Cosmon adopts the **Remote Pilot Port (RPP)** as the canonical pattern for any HTTPS+OIDC ingress to the cosmon DAG, instantiated by a new crate `cosmon-rpp-adapter`. The pattern is governed by:

1. The **§8j HTTPS+JWT instantiation** (five-clause admission boundary — §3 below), a substrate-specific instance of §8j, *not* a new §8 invariant letter.
2. **§8p — API surface ⊊ CLI surface** (new architectural invariant — §4 below): the network-exposed verb set is a strict, explicit subset of the CLI verb set.
3. An exhaustive **operator-only verb list** that the RPP MUST refuse to expose (§5).
4. A **YubiKey-bound JWT** identity model in V0; pluggable `OidcVerifier` trait for Keycloak self-hosted in V1+ (§6).
5. A **roadmap** that ratifies this ADR before any code lands and stages V0 / V1 / V2 with verifiable exit criteria (§7).

The RPP is **not** a daemon Layer A; it is a long-lived Layer B port adapter (ADR-023 hexagonal). The two layers cohabit by ADR-016, and this ADR strengthens that separation: see §8 *Coherence checklist*.

### 2.1 Naming

| Concept | Canonical name | Forbidden synonyms |
|---------|----------------|--------------------|
| The pattern | **Remote Pilot Port (RPP)** | *daemon*, *cosmon-server*, *microservice*, *ingress server* |
| The crate | **`cosmon-rpp-adapter`** | `cosmon-api`, `cosmon-api-server`, `cosmon-oidc-server` |
| External-facing prose | *API* (allowed *only* in operator-facing docs for tenant operators, end-users) | — |
| Internal docs / code / commits / chronicles | *Remote Pilot Port*, *RPP adapter*, *§8j HTTPS+JWT instantiation* | *API* (banned in code, ADR titles, crate names, type names) |
| The unit transported | **`Utterance`** (ADR-073) | *call*, *RPC*, *event* |
| The endpoint surface | *route* (axum vocabulary) | *endpoint* in isolation (acceptable as *route on the §8j HTTPS+JWT port*) |

This vocabulary discipline mirrors ADR-064 §C4 (*postman's uniform stays outside the house*) and ADR-072 §8 (*forbidden vocabulary*). The word *API* carries OpenAPI / Swagger / microservices baggage that systematically forces over-design (panel §S1 — wheeler).

---

## 3. The §8j HTTPS+JWT instantiation — five clauses

Every request admitted to the cosmon DAG via the RPP MUST pass through an admission boundary that enforces five clauses. The first four are direct substrate-specific instances of §8j (a)–(d); the fifth (subprocess envelope) is required because, unlike Matrix, the RPP invokes `cs` synchronously inside the request lifecycle.

### 3.1 Clause (a) — Identity mapping

> The JWT's `sub` claim resolves to a sealed `nucleon_id` via a file at
> ```
> .cosmon/state/nucleons/<nucleon_id>/oidc-identity.toml
> ```
> The mapping file is **briefing-sealed** (ADR-058 model): a BLAKE3 hash of its content is stored in `state.json` so retroactive edits are detectable. Unmapped `sub` claims are rejected with `RejectReason::UnknownSub`. The RPP **never** defaults, **never** auto-admits, **never** derives a `nucleon_id` from the raw `sub` string. *Implements §8j(a) on HTTPS+JWT.*

The mapping file structure is:

```toml
# .cosmon/state/nucleons/nuc-01HZ.../oidc-identity.toml
nucleon_id = "nuc-01HZW3MK..."
phase = "Biological"           # ADR-063 — Biological | LlmFrontier
noyau = "tenant-demo"          # tenant / org — ADR-063 layer 3

[oidc]
issuer = "https://accounts.google.com"     # IdP `iss` claim
sub = "1234567890.apps.googleusercontent.com"  # IdP `sub` claim
audience = "cosmon-rpp-tenant"             # RPP `aud` claim
sealed_at = "2026-04-27T14:00:00Z"         # set at provisioning
```

Multiple `oidc-identity.toml` files MAY exist under the same `nucleon_id` — one Nucléon may have several Orbitales (ADR-063), each authenticating through a different IdP claim. Cross-tenant pivot is impossible: a `sub → nucleon_id` mapping is scoped at provisioning to exactly one `noyau`; the RPP rejects requests whose JWT `sub` resolves to a `nucleon_id` outside the request's tenant routing.

### 3.2 Clause (b) — Causal closure

> Every admitted request is materialised on disk before any `cs` invocation:
> ```
> .cosmon/whispers/inbox/api/<request_id>.json
> ```
> The materialised file carries the JWT claim digest (`sub`, `iss`, `aud`, `iat`, `exp`, `jti` — never the raw token), the requested verb, the request body, and the `request_id`. The `cs` subprocess (or the next `cs reconcile`) reads this file back; the RPP itself **never** writes `state.json` or `events.jsonl` directly. *Implements §8j(b) on HTTPS+JWT.*

This is the structural guarantee that prevents L0 / L1 boundary erosion: every causal input from the network is observable from the `.cosmon/` referential before it perturbs the DAG. A test in CI (`tests/rpp_adapter_no_state_write.rs`) MUST verify by `strace` (or platform equivalent) that the adapter binary opens no file under `.cosmon/state/` for write — only reads, plus writes to `.cosmon/whispers/inbox/api/`.

### 3.3 Clause (c) — Pre-admission rate limit

> A per-`claim.sub` leaky bucket persisted to disk (`.cosmon/state/security/oidc-rate-limit/<sub_hash>.toml`) rejects bursts before any `cs` invocation. State persists across adapter restarts (re-loaded at boot). Token-bucket capacity and refill rate are operator-tuned per `noyau`. *Implements §8j(c) on HTTPS+JWT.*

Default V0 budget: 10 requests / minute / `sub`, burst 30. Tightened in V1+ once usage data is collected. A per-`noyau` global budget overlays the per-`sub` budget (defence in depth against pivot via multiple `sub`).

### 3.4 Clause (d) — One-way topology

> The RPP serves **request → response only**. No reflexive webhook. No server-initiated callback into a channel a cosmon worker reads from. Any future *push notification* of cosmon state to a remote pilot (e.g. SSE on `GET /events?stream`) MUST go through a **separate** non-reading channel; the receiving client MUST NOT be authorised to write back via the same endpoint.
> *Implements §8j(d) on HTTPS+JWT.*

Bidirectional designs (operator initiates a request, RPP answers, a worker reads the response and acts on it) are explicitly forbidden in V0/V1. Any future bidirectional design requires a successor ADR that re-derives §8j(d) for the bidirectional case.

### 3.5 Clause (e) — Subprocess envelope *(substrate-specific extension of §8j)*

> The RPP MUST invoke `cs` as a subprocess for every admitted request, with the following non-negotiable envelope:
> - Environment variable `COSMON_API_REQUEST=1` (forces `cs` to (i) refuse operator-only verbs even if the verb name is reachable, (ii) emit `--json`-formatted output, (iii) treat its stdin/stdout as non-TTY, (iv) attach the `request_id` and `claim.sub` to every event written to `events.jsonl`);
> - Environment variable `COSMON_API_REQUEST_ID=<request_id>` (correlates the inbox file, the subprocess audit line, and the molecule's `events.jsonl` entries);
> - Environment variable `COSMON_API_NUCLEON=<nucleon_id>` (the resolved identity from clause (a));
> - Working directory rooted at the tenant galaxy: `/srv/cosmon/<noyau>/`;
> - Subprocess timeout (default 30 s; operator-tunable per route);
> - Subprocess stdout/stderr captured into the `.cosmon/whispers/inbox/api/<request_id>.json` *response* sibling, never back-channelled to the operator's HTTP response except through fields the response schema explicitly allows.

The CLI side honours the envelope: `cs` MUST detect `COSMON_API_REQUEST=1` and refuse operator-only verbs at parse time (return a typed `OperatorOnlyVerbInApi` error before any state mutation). The list of operator-only verbs is enumerated in §5.

### 3.6 Reject taxonomy

```rust
/// Ordered so match-arms document the §8j HTTPS+JWT clause sequence.
pub enum RppRejectReason {
    // Shape (clause b — every payload must materialise)
    InvalidJsonBody,
    UnknownVerb,
    OperatorOnlyVerb(&'static str),

    // Identity (clause a)
    MissingAuthorization,
    MalformedJwt,
    UnsupportedAlg(String),         // anything not RS256 / ES256
    SignatureInvalid,
    Expired,
    NotYetValid,
    AudienceMismatch,
    IssuerNotPinned,
    UnknownSub,
    SealBroken(NucleonId),
    CrossTenantPivot { sub: ClaimSub, expected_noyau: Noyau, found_noyau: Noyau },

    // Rate (clause c)
    RateLimited { sub_hash: String, retry_after: Duration },
    NoyauBudgetExhausted(Noyau),

    // Kill-switch (clauses a + c — operator override)
    SubKilled(ClaimSub),
    JtiKilled(Jti),
    NoyauKilled(Noyau),
    GlobalKill,

    // Topology (clause d)
    BidirectionalForbidden,

    // Subprocess envelope (clause e)
    SubprocessSpawnFailed(std::io::Error),
    SubprocessTimeout(Duration),
    SubprocessExitNonZero { code: i32, stderr_excerpt: String },

    // Substrate (clause b)
    InboxMaterializationFailed(std::io::Error),
}
```

The taxonomy is canonical for the RPP. Adapters or transports added later (gRPC, QUIC) re-instantiate §8j again with their own substrate-specific extensions — they do **not** add reject reasons to this enum.

---

## 4. §8p — API surface ⊊ CLI surface (new invariant)

> **§8p. API surface ⊊ CLI surface (subset strict).**
> Every route exposed by `cosmon-rpp-adapter` MUST correspond to exactly one user-facing `cs` verb (or a strict combination via a `transitions` discriminator), and that correspondence MUST be recorded in [`docs/guides/api-cli-coverage.md`](../guides/api-cli-coverage.md). The reverse is **not** true: most `cs` verbs MUST NOT have an RPP route. New `cs` verbs are by default *out of API*; promotion to API is an explicit PR that updates the audit guide and cites a successor ADR (or this ADR's amendment trail).

### 4.1 Why §8p is *not* §8l

§8l (ADR-068) establishes a **bijection** between every user-facing `cs` verb and a UX surface counterpart on native pilot apps (mac-pilot, ios-pilot, future Skylight/Souffleur). §8l is symmetric because UX surfaces target *humans* who can be told *"this verb is operator-only, run it from your terminal"*. The RPP, by contrast, transports operations across a network where:

- **third-party clients** (CI, scripts, future SDKs) cannot be told to fall back to a CLI;
- **operator-only verbs** (§5) MUST stay reachable only from the operator's own keyboard, never from a JWT;
- **fine-grained scope discipline** (tolnay, jobs) requires the network surface to start small and grow only on demand, not by symmetry.

Therefore §8l ≠ §8p. §8l is a bijection (UX ↔ CLI). §8p is a *subset strict* (API ⊊ CLI). Both invariants stand. ADR-068 is unaffected.

### 4.2 What §8p requires concretely

1. **An audit guide MUST exist** at `docs/guides/api-cli-coverage.md`, listing every RPP route and its corresponding `cs` verb. Maintenance is a CI gate (see §9 *Coherence checklist* item 11).
2. **A frozen snapshot test** at `crates/cosmon-rpp-adapter/tests/api_surface_freeze.rs` pins the route set. Adding or removing a route fails the test until the snapshot is regenerated *and* the audit guide is updated *and* either this ADR or a successor is amended.
3. **No SDK auto-generation** in V0/V1 (tolnay). A hand-written OpenAPI document is the contract, decoupled from internal Rust types. A future generated SDK is a successor decision.
4. **Versioning is path-based** (`/v1/` prefix). Breaking changes cohabit `/v2/` and `/v1/` for at least six months. `/v1/` is frozen at first ship.
5. **No verb is added to the API by anything other than an explicit PR** that touches the audit guide, the freeze snapshot, and the OpenAPI spec in a single change. No silent route definitions.

The `task-20260427-4745` (api-cli-coverage audit) child task is the implementation of this clause.

---

## 5. Operator-only verbs — exhaustive non-exposable list

The RPP MUST refuse to expose the following verbs even by accidental routing. The list is canonical and **closed**: extending it requires a successor ADR with an explicit `delegate_for` claim model (§5.2).

### 5.1 The closed list (V0 / V1)

| Verb | Why operator-only | Source |
|------|-------------------|--------|
| `cs done` | Closes molecule → merges to `main` → kills tmux → removes worktree → deletes branch. Irreversible. Operator gesture. | ADR-077 §2 R5; CLAUDE.md *Pilot patterns* |
| `cs evolve` | Worker-internal advance. Workers run in their own worktrees with their own `cs` binary; the RPP must not reach into a worker's process. | CLAUDE.md *Command perimeters* |
| `cs complete` | Worker-internal terminal transition (Active → Completed). Same reasoning as `cs evolve`. | CLAUDE.md *Command perimeters* |
| `cs security activate` | Switches the cosmon-wide security posture from `prepared` to `active` (ADR-076). Affects every subsequent operation across all tenants. Operator-only. | ADR-076 |
| `cs kill` | Force-terminates a worker process tree. Side effects on disk and tmux. Operator gesture. | CLAUDE.md *Anti-patterns* (implicit) |
| `cs purge` | Destroys cosmon state. Catastrophic, irreversible. Operator-only by definition. | (same) |
| `cs reconcile` | Rewrites every projected surface (STATUS.md, ISSUES.md, GitHub Issues). Long-running. Operator-only. | ADR-017, ADR-018 |
| `cs verify` | Audits briefing seals across the fleet; long-running and potentially noisy. Read-only by nature, but exposing it gives an oracle for state existence (turing) — kept operator-only in V0. | ADR-058, ADR-080 §5.3 |
| `cs whisper --to-session <sid>` | Cross-session text injection into a live worker. Bypasses the DAG. By construction operator-only (ADR-038). | ADR-038 |
| `cs drop` | Pilot-inbox gesture; whole-fleet semantics. Operator-only. | ADR-073 |

### 5.2 Why the list is closed

A request bearing a JWT is by definition a *third-party authenticated principal*. The verbs above are either (i) terminal lifecycle gestures whose authority chain ends at the operator's keyboard (turing red-line), (ii) worker-internal verbs that have no semantic meaning across the network, or (iii) operations whose blast radius (`cs purge`, `cs reconcile`) exceeds what a stateless request can reasonably commit to.

Extending the list requires:

1. A successor ADR proposing the verb's exposure and explaining why the JWT-bearer's authority chain *includes* the verb's blast radius.
2. A `delegate_for: <nucleon_id>` JWT claim that the RPP validates against the molecule's authorship — i.e. the JWT's `sub` is acting *on behalf of* an operator who explicitly delegated the verb. The mapping `sub → delegate_for` is briefing-sealed (clause (a) extension).
3. An explicit row in `docs/guides/api-cli-coverage.md` flagging the route as *delegated authority* and citing the successor ADR.

In V0 and V1 *no* such successor exists; the list is materially closed.

### 5.3 The `cs verify` exception path (V2 candidate)

`cs verify` is read-only and idempotent; its only objection is the *oracle for state existence* turing flagged. A V2 successor ADR may expose it as `GET /v1/molecules/:id/verify` if and only if (i) the rate-limiter caps verify queries below the leaked-information threshold turing computed in `delib-20260427-d2ce/responses/turing.md` §Oracle side-channels, and (ii) the response body redacts existence (`200 OK` and `404 Not Found` are timing-equivalent, response-body-equivalent, and response-size-equivalent — turing G14). Until that ADR lands, `cs verify` is operator-only.

### 5.4 The `cs run` exit path — bounded drain (resolved 2026-06-11)

`cs run` left the closed list via the successor path of §5.2:
[ADR-124](124-tenant-bounded-drain-run.md) (B2 bounded drain,
`task-20260610-56c4`, delib-20260610-9a0c K4). The exposed route
`POST /v1/molecules/{id}/run` does **not** hand the JWT-bearer the
operator orchestrator: it is a *request door* — the client asks for a
drain of its own DAG, and the resident loop inside the tenant
container decides what to tackle, when, under the binding-sealed
B1/B2/B3 bounds (readable via `GET /v1/quota`, never writable through
any §8p route). The §5.2 objection (blast radius of an unbounded,
long-running orchestrator) is dissolved by construction, not waived:
a tenant drain is never unbounded (B3 obligatory, server defaults),
and the loop's every exit is NAMED (I4). The `delegate_for` claim
model of §5.2 is intentionally NOT used — the tenant drains *its own*
molecules under *its own* budget; there is no operator authority to
delegate.

---

## 6. JWT specification — frozen at V0

### 6.1 Algorithm whitelist

The RPP accepts **only** these JWT signing algorithms:

- `RS256` (RSASSA-PKCS1-v1_5 with SHA-256)
- `ES256` (ECDSA with P-256 and SHA-256)

It rejects with `UnsupportedAlg` everything else, **without exception**:

- `none` (the historical alg-none vulnerability — JWT RFC 7519 §6.1)
- `HS256` / `HS384` / `HS512` (RS256-to-HS256 confusion attack — turing G2)
- `EdDSA` (V0 simplicity; revisitable in V1+)
- `RS384` / `RS512` (collision-rare but non-load-bearing complexity)

The whitelist is enforced at parse time, *before* the `kid` lookup. JWKS rotation does not relax it.

### 6.2 Claim envelope

| Claim | Required | Constraint |
|-------|----------|------------|
| `iss` | yes | Pinned at adapter boot from `.cosmon/state/security/oidc-issuers.toml`. Not configurable per request. |
| `sub` | yes | The principal identifier; resolved to `nucleon_id` via clause (a). |
| `aud` | yes | Pinned per RPP instance (`cosmon-rpp-<noyau>`). |
| `iat` | yes | Token issuance. |
| `exp` | yes | **Absolute maximum 15 minutes** in posture `active` (ADR-076). In posture `prepared` (dev), 24 hours is acceptable but logged. |
| `jti` | yes | Token unique identifier; the kill-switch (§7) uses it. |
| `nonce` | recommended | Replay defence in conjunction with `jti`. |
| `delegate_for` | optional | V2+ only — see §5.2. Rejected as `UnsupportedClaim` in V0/V1. |

### 6.3 JWKS handling

- **Pinning at boot.** The RPP loads JWKS (per issuer) at startup from `.cosmon/state/security/jwks/<iss_hash>.json`. This file is fetched and signed by the operator on rotation; the RPP does not auto-fetch from the network.
- **No `jku` / `x5u` claim resolution.** Both are explicit reject-list entries (turing G7, G8).
- **`kid` lookup is constant-time** against the pinned JWKS set; missing `kid` rejects with `SignatureInvalid` (no oracle on which keys are pinned).

### 6.4 Refresh tokens

- **No refresh token in V0.** Operators re-issue JWTs from their YubiKey-bound signer when expiry hits.
- **V1+ refresh tokens MUST carry a `family_id`** (RFC 6819 §5.2.2.3). The RPP detects family-id reuse (token-family poisoning) and forces revocation of the entire family.

### 6.5 Posture switching (ADR-076)

| Posture | JWT exp max | DPoP | Kill-switch | Notes |
|---------|-------------|------|-------------|-------|
| `prepared` | 24 h | optional | armed but advisory | dev posture; warnings emitted on every laxity |
| `active` | 15 min | **YubiKey-bound DPoP required** (RFC 7800 §3.2 + RFC 9449) | enforced | post-Day-J production flow; no-fallback |

`cs security activate` (operator-only, §5) switches between them. The RPP re-reads the posture file (`.cosmon/state/security/posture.toml`) on every request (cached ≤ 30 s); switching does not require a redeploy.

---

## 7. Kill-switch — disk deny-list

### 7.1 Files

The RPP consults two files on every request (cached ≤ 30 s, re-read on cache miss):

```
.cosmon/state/security/oidc-kill.toml         # global blast-door
.cosmon/state/security/oidc-policy.toml       # fine-grained per-sub / per-jti / per-noyau
```

### 7.2 Format (V0)

```toml
# oidc-kill.toml
[global]
# If present and `enabled = true`, every RPP request is rejected.
enabled = false
reason = ""
since = ""

# oidc-policy.toml
[[deny.sub]]
sub_hash = "blake3:abcd..."
reason = "compromised credential 2026-04-27"
since = "2026-04-27T15:00:00Z"

[[deny.jti]]
jti = "tok-01HZW..."
reason = "leaked in CI artifact"
since = "2026-04-27T16:00:00Z"

[[deny.noyau]]
noyau = "tenant-demo"
reason = "tenant pause"
since = "2026-04-27T17:00:00Z"
```

The TOML format is canonical; future encodings (JSON, signed CBOR) are successor-ADR territory.

### 7.3 Operator commands

- `cs security oidc kill` — flips `oidc-kill.toml [global].enabled = true`.
- `cs security oidc revoke --sub <claim_sub>` — appends to `oidc-policy.toml [[deny.sub]]`.
- `cs security oidc revoke --jti <jti>` — appends to `[[deny.jti]]`.
- `cs security oidc revoke --noyau <name>` — appends to `[[deny.noyau]]`.
- `cs security oidc unrevoke …` — operator-only undo; appends a tombstone with `since = ""` (left to the implementation to enforce ordering).

These commands are **operator-only** (§5). The RPP does **not** expose them.

### 7.4 Latency target

Operator command → effect at the RPP: **≤ 30 s** (cache TTL). No remote round-trip required: the deny-list lives on the same filesystem as the adapter binary. For multi-host deployments, the deny-list is replicated by the operator's existing filesystem replication (`rclone sync`, `rsync`, NFS). Replication latency is the operator's responsibility; the RPP guarantees its own re-read latency.

---

## 8. Identity, multi-tenant, and the four-key segregation

### 8.1 Multi-tenant via galaxy filesystem

Each tenant is one cosmon galaxy: `/srv/cosmon/<noyau>/`. The RPP routes by `claim.sub → nucleon_id → noyau`, then sets the subprocess `cwd` to `/srv/cosmon/<noyau>/`. There is **no** cross-tenant write path: a JWT scoped to `noyau = tenant-demo` cannot reach `/srv/cosmon/other-noyau/.cosmon/state/`. Cross-tenant pivot is rejected at clause (a) with `CrossTenantPivot`.

### 8.2 The Orbitale ⊂ Nucléon ⊂ Noyau mapping (ADR-063)

| Cosmon vocabulary | Multi-tenant mapping |
|-------------------|----------------------|
| **Orbitale** | One device authenticated by one IdP claim. A single Nucléon may have many Orbitales (laptop, mobile, CI runner) — each with its own `oidc-identity.toml`. |
| **Nucléon** | The pilot-cognition. One Nucléon authenticates as one principal across all its Orbitales; per-device JWTs all resolve to the same `nucleon_id`. |
| **Noyau** | The community / org / tenant. The RPP's tenant-routing axis. |

The multi-tenant model is therefore **inherited free** from ADR-063. There is no separate "tenant model" to specify in this ADR (panel §S2, wheeler).

### 8.3 Four-key segregation

The RPP must not couple any of the following four signing surfaces; a single key compromise must not cascade.

| Surface | Key | Where |
|---------|-----|-------|
| **JWT signing (IdP-side)** | YubiKey IdP-dedicated **or** Keycloak HSM | Off-RPP — operator's IdP infrastructure |
| **Git push to `main`** | Operator YubiKey (ADR-077 §2) | Operator workstation; CI-OIDC alternative path |
| **Audit log signing** (V1+) | KMS-dedicated **or** YubiKey audit | Off-RPP — out-of-band notarisation |
| **Cosmon notary** (ADR-056) | YubiKey notary | Off-RPP — content-hash anchoring |

A CI test (or a documented operator audit, if CI cannot inspect hardware) MUST flag any configuration where two of these surfaces share the same key material. The detection logic lives in `crates/cosmon-rpp-adapter/tests/key_segregation.rs` (configuration audit) — implementation in V1+.

---

## 9. Audit log

### 9.1 Cosmon-native audit (V0, sufficient)

Every admitted RPP request emits one or more events into `events.jsonl` (ADR-047) of the touched molecule (or, for nucleate, the parent fleet's `events.jsonl`):

```jsonl
{"ts":"2026-04-27T15:00:01Z","kind":"UtteranceAdmitted","port":"rpp-https-jwt","request_id":"req-...","claim_sub_hash":"blake3:...","nucleon_id":"nuc-...","verb":"observe","mol_id":"task-..."}
{"ts":"2026-04-27T15:00:01Z","kind":"SubprocessSpawned","request_id":"req-...","cmd":"cs observe task-... --json"}
{"ts":"2026-04-27T15:00:02Z","kind":"SubprocessExited","request_id":"req-...","exit_code":0,"duration_ms":840}
{"ts":"2026-04-27T15:00:02Z","kind":"UtteranceClosed","request_id":"req-...","status":"ok"}
```

Briefing-sealing (ADR-058) covers tampering of the molecule's `briefing.md`. Events are append-only in `events.jsonl` and integrity is a property of the cosmon ledger, not the RPP. `cs verify` (operator-only) audits the chain.

### 9.2 S3-Object-Lock notarisation (V1+, out-of-band)

For non-repudiation against a determined adversary with filesystem access, V1+ supports an out-of-band write of an NDJSON audit stream to an S3 bucket with Object Lock (compliance mode), signed by an audit-dedicated KMS key (one of the four keys in §8.3). This is an *additional* channel, not a replacement: the cosmon-native audit (§9.1) remains the source of truth for the molecule lifecycle. Implementation is deferred to a successor ADR; this ADR records the slot.

---

## 10. Roadmap

The roadmap respects three constraints:

1. **No code before this ADR ratifies.** The two child tasks (`task-20260427-4e84` for V0 implementation, `task-20260427-4745` for the api-cli-coverage audit) are explicitly `--blocked-by task-20260427-16aa`. If `cs link` is unavailable, re-nucleation with the `--blocked-by` flag is mandatory.
2. **AWS phase 1 (Monday 2026-04-28) is unaffected.** This ADR is editorial; no infrastructure change is required this week.
3. **Operator-demo track A runs in parallel.** Distributing `cs` as a signed brew binary + YubiKey to the demo operator is the *15-minute test* (panel §S5, jobs). If track A succeeds, V1 may shrink. If track A fails, the failure point dictates the V1 endpoint set — not our imagination.

### 10.1 V0 (semaine 5–9 mai 2026, post-AWS-phase-1)

| Item | Detail |
|------|--------|
| Crate | `cosmon-rpp-adapter` (axum + tower-http + jsonwebtoken + tracing-opentelemetry) |
| Routes | **One** route: `GET /v1/molecules/:id` (read-only, observe) |
| Identity | YubiKey-bound JWT, RS256 / ES256, JWKS pinned from `.cosmon/state/security/jwks/` |
| Clauses | All five (§3) wired and tested |
| Posture | `prepared` (warns on laxity) |
| Audit | `events.jsonl` emit (§9.1) — no S3 yet |
| Kill-switch | `oidc-kill.toml` global only — per-sub deferred to V1 |
| OpenAPI | hand-written `openapi/v1.yaml` |
| Freeze test | `tests/api_surface_freeze.rs` pinning the route set |

### 10.2 V1 (semaine 12–16 mai et 19–23 mai 2026)

| Item | Detail |
|------|--------|
| Routes | Three additions: `POST /v1/molecules` (nucleate), `POST /v1/molecules/:id/transitions` (`tackle` only — **not** `done`), `GET /v1/molecules` (list) |
| Identity | Per-`noyau` rate-limit budget (§3.3) layered over per-`sub` |
| Posture | Operator-tunable `prepared` / `active` switch (still defaulting to `prepared`) |
| Kill-switch | per-`sub`, per-`jti`, per-`noyau` (§7.2 fully populated) |
| Audit guide | `docs/guides/api-cli-coverage.md` populated by `task-20260427-4745` |
| Refresh tokens | with `family_id` rotation (§6.4) |
| RBAC | two roles only — `pilot`, `observer`. No fine-grained scopes. |

### 10.3 V2 candidate (juin 2026 — only if operator-demo track A surfaces a real friction)

| Item | Detail |
|------|--------|
| Posture | bascule to `active` post YubiKey full enrolment |
| DPoP | RFC 9449 demonstrating proof-of-possession (turing) |
| OidcVerifier | trait-pluggable Keycloak self-hosted derrière Tailscale (forgemaster) |
| Audit | S3 Object Lock notarisation (§9.2) with audit-key-dedicated signing |
| `cs verify` exception path | per §5.3 — only if turing's oracle-side-channel constraints are met |
| Routes | `GET /v1/molecules` server-paged; possibly `GET /events?stream` (clause (d) with audit successor) |

V2 is conditional: if operator-demo track A has shown the binary path is enough for the actual user set, V2 may not happen. Sur-design would be a regression.

### 10.4 Calendar reference

| Week | Track A (jobs) | Track B (this ADR) | Track C (V0 code) |
|------|----------------|---------------------|-------------------|
| 28 avr – 3 mai | Brew tap signed binary + YubiKey to the demo operator | this ADR drafted (today) | — |
| **week-end 4–5 mai** | (collect feedback) | **this ADR ratified** | — |
| 5–9 mai | (demo operator uses `cs`) | (any amendments from review) | V0 squelette + 5 clauses + 1 route |
| 12–16 mai | (continued) | — | observability + per-sub kill-switch |
| 19–23 mai | (verdict on operator-demo friction) | — | V1 mutations (`POST /v1/molecules`, `POST /v1/.../transitions`) |
| 26 mai+ | — | — | re-evaluate V2 in light of operator-demo evidence |

---

## 11. Coherence checklist (CLAUDE.md §5 — all 10 + 1 new)

The CLAUDE.md coherence checklist is reproduced and answered for the RPP, plus one additional item that §8p introduces.

| # | Question | Answer | Reason |
|---|----------|--------|--------|
| 1 | Stateless? One-shot invocation, no background loop? | **Yes** for the cs subprocess; *partial* for the long-lived RPP process. | The RPP is a *Layer B port adapter* (ADR-023), not a state holder. Each request shells out to a one-shot `cs` invocation; the adapter's RAM only holds JWKS / rate-limiter / kill-switch — all idempotent projections of the filesystem. (panel C1, C2) |
| 2 | Idempotent? Twice = once? | **Yes** for `GET /v1/molecules/:id`, **as defined by the underlying CLI verb** for write routes. | The RPP adds no idempotency layer; nucleate's idempotency property follows `cs nucleate` (ADR-073). |
| 3 | Regime-aware? Inert / Propelled / Autonomous? | **Yes** — the RPP only exposes Inert-side verbs and a future Propelled `tackle` (V1). It exposes no Autonomous-side verb. | Operator-only verbs (§5) are hard-refused. |
| 4 | Single perimeter? Not duplicating an existing command? | **Yes** — every route is a strict subset of an existing CLI verb (§8p). | New routes require explicit ADR amendment. |
| 5 | Symmetric undo? | **Yes** for nucleate (collapse via successor `POST /transitions`), **N/A** for read routes. | `done` / `purge` / `kill` are not exposed. |
| 6 | Runtime-compatible? | **Yes** — the RPP is a Layer B client of the transactional core; the future Resident Runtime is a peer client. | Both share `.cosmon/state/` via the cs subprocess, never directly. |
| 7 | Worker / human boundary respected? | **Yes** — `cs evolve`, `cs complete` are worker-only and refused (§5). `cs done` is operator-only and refused (§5). | The JWT-bearing principal is neither a worker nor (in V0/V1) a delegated operator. |
| 8 | Write-read asymmetry preserved? | **Yes** — the RPP never returns a coupling report and never writes state directly. | Clause (b) materialises before the cs subprocess; the response is a projection of the subprocess output. |
| 9 | Merge-before-dispatch respected? | **N/A in V0/V1** — `cs done` is not exposed. | When V2 (or later) considers exposing a `done`-equivalent, the successor ADR must re-derive this row. |
| 10 | CLI-first for workers? | **Yes** — workers continue to use `cs` directly via walk-up discovery. The RPP is for *external pilots*, never for workers in their own worktree. | Worker harness is unchanged by this ADR. |
| 11 *(new)* | API surface ⊊ CLI surface? | **Yes** — §8p enforces it; the audit guide and freeze test are mandatory. | This row is added by §8p and SHOULD be incorporated into CLAUDE.md §5 in a follow-up edit. |

---

## 12. Risks (instrumented from day J+1)

| # | Risk | Instrumentation |
|---|------|-----------------|
| R1 | L0 (RPP) drifts into L1 (in-RAM business state). | CI test `tests/rpp_adapter_no_state_write.rs` — `strace` (or platform equivalent) shows zero open-for-write under `.cosmon/state/`; only `.cosmon/whispers/inbox/api/` writes are allowed. |
| R2 | API verb set drifts from CLI. | CI test parses `cs help`, lists user-facing verbs, compares against `axum::Router` route table. Mismatch fails unless `docs/guides/api-cli-coverage.md` is updated in the same PR. |
| R3 | Endpoint *snowball* (the *"and one more endpoint"* drift). | `tests/api_surface_freeze.rs` snapshot pins routes; addition forces snapshot regen + audit guide update + ADR amendment. |
| R4 | OpenAPI drifts from Rust types. | OpenAPI is hand-written. Coherence test deserialises representative payloads against Rust types, asserts schema match. Drift = test failure. |
| R5 | Two of the four signing keys are accidentally the same. | Operator audit (V0); CI configuration audit at `tests/key_segregation.rs` (V1+). |
| R6 | A worker LLM exfiltrates an API token from operator's home directory. | The RPP token (if persisted at all on the operator's box) lives at `~/.config/cosmon/api-credentials.toml`, mode 0600. Worker harness refuses to start if the file is readable from its sandbox (V1+). |
| R7 | Cross-tenant pivot via crafted JWT. | Clause (a) `CrossTenantPivot` reject; per-`noyau` rate-limiter; per-`sub → nucleon_id → noyau` is briefing-sealed. |
| R8 | Forbidden vocabulary creeps into code or commits. | Lint script `scripts/forbid-rpp-daemon-vocab.sh` (sibling of `forbid-matrix-features.sh`) greps for `daemon`, `cosmon-server`, `microservice`, `endpoint` (in isolation) under `crates/cosmon-rpp-adapter/`. Fails CI on hit. |

---

## 13. Open questions (deferred to V0 implementation review)

| # | Question | Default position (overridable in V0 PR) |
|---|----------|------------------------------------------|
| Q1 | One RPP binary per tenant, or one multi-tenant binary that routes via `--cosmon-root`? | **Multi-tenant binary** (single port, routing via `claim.sub → noyau`). One binary per tenant is reserved for the case where two tenants demand strictly disjoint signing keys without shared filesystem. |
| Q2 | HTTP/2 or HTTP/3 for V0? | **HTTP/2 with TLS 1.3** — well-supported by axum and reverse proxies; HTTP/3 deferred to V1+ if benchmark warrants. |
| Q3 | SSE on `GET /events?stream` in V2? | **Conditional yes** — only if §9.2 audit notarisation lands first and clause (d) one-way topology is preserved (the SSE channel must not be writable). |
| Q4 | Format of the disk deny-list — TOML stays canonical? | **Yes** for V0 / V1 (TOML, human-editable). V2+ may add a JSON-Lines append-only log for auditability, *in addition* to TOML, never replacing it. |
| Q5 | Naming of clause (e) — *subprocess envelope* vs *invocation envelope* vs *request envelope*? | **subprocess envelope** in this ADR. Naming is revisitable in successor amendment if implementation surfaces a clearer term. |
| Q6 | Should §8p be promoted to a section header in `docs/architectural-invariants.md`? | **Yes, in this ADR's PR** — see §14 below. The §8j HTTPS+JWT instantiation goes as a sub-rider under the existing §8j section, **not** as a new §8 letter (per §8j's own meta-rule). |

The synthesis (§7.1) proposed the new clauses under the label **§8k**. That naming was preempted by ADR-064 §C4 (postman's uniform / wheat-paste — *"§8k"* is already a published reference inside ADR-079 and `architectural-invariants.md` line 1226). Per §8j's explicit meta-rule (`docs/architectural-invariants.md` line 1297: *"Future ingress ports rewrite §8j for their own substrate; no §8k / §8l / ... is admitted"*), the HTTPS+JWT instantiation is **not** awarded a new §8 letter; it is documented as a substrate-specific rider under §8j (wheeler-aligned framing). §8p remains a new letter because it expresses an orthogonal invariant (subset relationship between two surfaces), not a new ingress port.

---

## 14. Diff against `docs/architectural-invariants.md`

This ADR introduces two amendments (full text in §3 above and §4 above; the diff reproduced here is the operative summary):

### 14.1 New rider under §8j — *HTTPS+JWT instantiation (five clauses)*

A new sub-section under **§8j. Ingress bindings** (after the *RejectReasons enumerated* block, before *Why this is not a new §8-primitive*) titled:

> **§8j HTTPS+JWT instantiation (Remote Pilot Port).**
>
> The second instantiation of §8j, after Matrix. The four clauses (a)–(d) of §8j apply directly to HTTPS+JWT with substrate-specific substitutions (Matrix MXID → JWT `sub`; whisper inbox → `.cosmon/whispers/inbox/api/`; rate-limiter per Matrix sender → per JWT `claim.sub`; one-way topology preserved).
>
> A **fifth clause (e) — subprocess envelope** — applies because the HTTPS adapter invokes `cs` synchronously inside the request lifecycle (Matrix terminates by writing inbox files that `cs` later picks up; HTTPS does not have that asynchrony). The envelope mandates `COSMON_API_REQUEST=1`, `COSMON_API_REQUEST_ID=<id>`, `COSMON_API_NUCLEON=<nucleon_id>`, tenant-rooted `cwd`, subprocess timeout, and refusal of operator-only verbs at CLI parse time.
>
> Reference implementation: `crates/cosmon-rpp-adapter/`. Governing ADR: [ADR-080](080-remote-pilot-port-https-oidc.md).
>
> *This rider is consistent with §8j's "no §8k / §8l / ... admitted" meta-rule — it does not create a new §8 invariant letter; it is an annotated instance of §8j with one substrate-specific extension clause.*

### 14.2 New section §8p — *API surface ⊊ CLI surface*

A new top-level section, placed after §8o (low-confidence transcription) at the end of the §8 series:

> ## 8p. API surface ⊊ CLI surface *(proposed — [ADR-080](080-remote-pilot-port-https-oidc.md))*
>
> Every route exposed by `cosmon-rpp-adapter` (the Remote Pilot Port) corresponds to exactly one user-facing `cs` verb (or a strict combination via a `transitions` discriminator). The reverse is **not** true: most `cs` verbs MUST NOT have an RPP route.
>
> ### The rule
>
> > **§8p. API surface ⊊ CLI surface (subset strict).** The set of routes exposed by the RPP is a strict subset of the user-facing CLI verb set. Every route must be recorded in `docs/guides/api-cli-coverage.md` with its CLI counterpart. Adding a route requires an explicit PR that updates the audit guide, the freeze test (`tests/api_surface_freeze.rs`), and either ADR-080 or a successor.
>
> ### Why §8p is not §8l
>
> §8l (UX ↔ CLI parity) is a **bijection** for human surfaces. §8p is a **subset** for the network surface. The two invariants are independent: §8l targets pilot apps where every CLI verb has a UI counterpart; §8p targets the RPP where most CLI verbs have *no* network counterpart and *must not* gain one without explicit decision.
>
> ### Status
>
> **Proposed (ADR-080).** Until ratified, the rule applies to `cosmon-rpp-adapter` only; `cosmon-api` (loopback) is grandfathered and out of scope.

### 14.3 No change to other §8 letters

- §8k is *not* re-used (preempted by ADR-064 §C4).
- §8l, §8m unchanged (UX ↔ CLI parity is independent of API/CLI subset).
- §8a–§8o otherwise unchanged.

---

## 15. Forbidden vocabulary (ADR-072 §8 discipline applied)

The following words are **banned** from `crates/cosmon-rpp-adapter/`, ADR titles, type names, file names, and chronicle entries (CI lint forthcoming, R8):

- `daemon` (the RPP is a Layer B port adapter, *not* a daemon — §1.1)
- `cosmon-server` (no server semantics; the cosmon ledger is the filesystem)
- `microservice` (cosmon is a monolith of file conventions, not a microservice grid)
- `endpoint` (in isolation; allowed only as *"route on the §8j HTTPS+JWT port"*)

The word **API** is allowed *only* in operator-facing prose (tenant operators, end-user docs). Internal documentation (this ADR, code, commits, chronicles) refers to the **Remote Pilot Port** or **§8j HTTPS+JWT instantiation**.

---

## 16. Closing — three sentences for the chronicle

> *"L'API n'est pas une couche. C'est un transport. Le port existe déjà ; on lui pose un nouveau réceptacle, c'est tout."* — wheeler

> *"L'API cosmon, c'est un binaire signé sur ta machine, pas un service sur la nôtre."* — jobs

> *"§8p ne s'invente pas — c'est §8l qu'on refuse de symétriser."* — tolnay (paraphrased from `delib-20260427-d2ce/responses/tolnay.md`)

The doctrine emerges strengthened: the RPP is a substrate instance, not a new layer; the API is a strict subset, not a parity; and the operator's keyboard remains the only seat from which `cs done` is run.

---

*Authored 2026-04-27 by `task-20260427-16aa`, parent deliberation `delib-20260427-d2ce`, panel forgemaster · turing · tolnay · jobs · wheeler. Children to unblock after ratification: `task-20260427-4e84` (V0 implementation), `task-20260427-4745` (api-cli-coverage audit).*
