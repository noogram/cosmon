# API ↔ CLI coverage audit

**Status:** v0 (snapshot of 2026-04-27) — companion to
[ADR-080](../adr/080-remote-pilot-port-https-oidc.md).

This document is the canonical registry of every user-facing `cs` verb
and its **Remote Pilot Port** (RPP, the `cosmon-rpp-adapter` HTTPS+JWT
ingress) exposure. It is the audit instrument for invariant
[`§8p` — *API surface ⊊ CLI surface (subset strict)*](../architectural-invariants.md#8p-api-surface-cli-surface-proposed--adr-080).

**Read this file like a coverage report.** Most rows are `NO` —
that is the point. §8p is a *subset strict*: the RPP exposes a
deliberately small set of network-reachable verbs, and every other
CLI verb stays reachable only from the operator's keyboard or from
inside a worker's worktree. The reverse rule (every CLI verb has a
UI counterpart) is [`§8l` (UX ↔ CLI parity)](ux-cli-parity-audit.md)
— a **different** invariant for a **different** surface.

> **Recallic.** §8l is a *bijection* (CLI ↔ UX). §8p is a *subset*
> (CLI ⊋ API). Same alphabet, opposite asymmetry. ADR-080 §4.1.

---

## How this guide relates to its siblings

| File | Surface | Invariant | Relationship |
|---|---|---|---|
| [`docs/guides/ux-cli-parity-audit.md`](ux-cli-parity-audit.md) | CLI ↔ pilot apps | §8l (bijection) | every row has a UI counterpart, even if `❌ TBD` |
| **this file** (`api-cli-coverage.md`) | CLI ↔ RPP | §8p (subset strict) | most rows are `NO`, by design |
| [`crates/cosmon-rpp-adapter/openapi/v1.yaml`](../../crates/cosmon-rpp-adapter/openapi/v1.yaml) | RPP wire format | ADR-080 §4 | hand-written; lands with V0 |

The three artifacts are kept in sync by the CI gate (see *§ Drift
test*, below). A PR that adds a CLI verb without updating this guide,
or adds an RPP route without updating either this guide or the
freeze snapshot, fails the gate.

---

## Audit table

Legend for the **Exposed via API?** column:

| Symbol | Meaning |
|---|---|
| `V0` | currently shipped in the V0 RPP (axum route exists in `cosmon-rpp-adapter`). |
| `V0 (TBD)` | planned for V0 but not yet shipped on this branch. Promotion to `V0` is the PR that lands the axum route. |
| `V1` | currently shipped in the V1 RPP (V1 mutations land 12–23 May 2026 — ADR-080 §10.2). |
| `V1 (TBD)` | candidate for V1; re-evaluate when V1 mutations stabilise. |
| `V2 (TBD)` | candidate for V2; conditional on tenant_auditor-track-A friction (ADR-080 §10.3). |
| `NO` | not exposed; reason in the rightmost column. Adding a route requires a successor ADR. |
| `**NO (NEVER)**` | structurally never exposable (operator-only or worker-only verb, ADR-080 §5.1). |

Verbs are ordered roughly by lifecycle: capture → nucleate → observe →
advance → terminate → infrastructure → introspection.

| `cs` verb | Exposed via API? | API path | Reason if not exposed (or notes) |
|---|---|---|---|
| `cs spark` | NO | — | Pilot-session inbox gesture; routes via `cs nucleate spark` internally. No demonstrated remote use case. |
| `cs drop` | NO | — | Pilot-session interne (ADR-073). No demonstrated external need. |
| `cs listen` | NO | — | Local voice→whisper.cpp→`cs nucleate spark` MVP. Hardware-bound, not a remote act. |
| `cs ask` | NO | — | Local-shell developer aid; no tenant_auditor use case. |
| `cs pilot` | NO | — | Interactive cognitive-pilot REPL over a client-side model (`task-20260531-c3f6`, ADR-115). `cs pilot --remote` (increment 2, `task-20260601-4997`) is a *second* `cosmon-ops-tools` backend over `cosmon-rpp-adapter`: it **reuses** the existing `GET /v1/molecules/:id` (observe), `GET /v1/molecules` (ensemble), `POST /v1/molecules` (nucleate), `POST /v1/molecules/:id/tackle` (tackle) §8p routes — **it adds no new route**, so the freeze test is unchanged. `peek` is absent remotely (no RPP route); `done`/`evolve`/`complete` are never on the wire (§5). The REPL itself is never an HTTP verb. |
| `cs nucleate` | V0 | `POST /v1/molecules` | V1-mutation cut promoted into V0 (T-V1-MUTATIONS-NUCLEATE 2026-05-04 — tenant_auditor peut donner une mission). Read-write subset; `cs tackle` / `cs done` stay operator-only. |
| `cs observe` | V0 | `GET /v1/molecules/:id` | The first V0 route (ADR-080 §10.1, T-RPP-V0). Now joined by `POST /v1/molecules` for the V1-mutation cut. |
| `cs ensemble` | V1 (TBD) | `GET /v1/molecules` | List view; lands in V1 alongside nucleate. |
| `cs inbox` | NO (V1 TBD) | — | Subset of `cs ensemble --tag temp:hot`; re-evaluate V1 if tenant_auditor asks. |
| `cs init` | NO | — | Bootstrap of a new galaxy; operator-only, hardware/filesystem-bound. |
| `cs trust` | NO | — | Per-repo, human trust grant for repo-supplied shell (B5, RCE-by-clone). The `direnv allow` of cosmon; a local operator gesture recorded outside the repo. Never on the wire — a remote tenant granting trust would defeat the gate. |
| `cs tackle` | V1 (TBD) | `POST /v1/molecules/:id/transitions` (`transition=tackle`) | Operator → propelled. V1 mutation. |
| `cs evolve` | NO | — | Worker-internal (CLAUDE.md *Command perimeters*; ADR-080 §5.1). NEVER exposed. |
| `cs complete` | NO | — | Worker-internal (CLAUDE.md *Command perimeters*; ADR-080 §5.1). NEVER exposed. |
| `cs collapse` | V2 (TBD) | `POST /v1/molecules/:id/transitions` (`transition=collapse`) | Re-evaluate at V2; currently no tenant_auditor use case. |
| `cs decay` | NO (V2 TBD) | — | Decomposition (1 → N); re-evaluate when DAG ops needed remotely. |
| `cs merge` | NO (V2 TBD) | — | Synthesis (N → 1); same as decay. |
| `cs transform` | NO (V2 TBD) | — | Kind change (idea → task); operator-driven. |
| `cs done` | **NO (NEVER)** | — | Operator-only (ADR-077). Closes molecule, merges to `main`, kills tmux, removes worktree, deletes branch. NEVER exposed without successor ADR + `delegate_for` claim model. ADR-080 §5.1. |
| `cs stitch` | **NO (NEVER)** | — | Operator-only (ADR-110 single-writer-trunk). Merges a mission's DAG closure onto `main` in topological order under the trunk lock — the canonical trunk writer. Same NEVER class as `cs done`: a trunk-writing gesture, never exposed over the RPP. ADR-080 §5.1. |
| `cs stuck` | NO (V2 TBD) | — | Records a blocker; could be exposed when V1 mutations stabilise. |
| `cs await-operator` | NO | — | Worker-internal block-on-operator (ADR-123). Emitted by a live worker inside its worktree at an irreversibility boundary; same NEVER-on-the-wire class as `cs evolve` / `cs complete`. No remote use case. |
| `cs freeze` | NO | — | Worker-suspension (preemption). Infrastructure operation, not a remote act. ADR-080 §5.1. |
| `cs thaw` | NO | — | Worker-resume. Same as freeze. |
| `cs resume` | NO | — | Resume an Inert worker (alias of `cs tackle --resume`); operator-driven. |
| `cs claim` | NO | — | Pilot claims a molecule; the runtime defers to the human until it is released. Operator/pilot coordination gesture, not a remote act. |
| `cs release` | NO | — | Releases a pilot claim, returning the molecule to the runtime frontier. Mirror of `cs claim`; operator/pilot-driven. |
| `cs resurrect` | NO | — | Recover a dead worker; operator-driven. |
| `cs teardown` | NO | — | Infrastructure teardown; operator-only. |
| `cs purge` | **NO (NEVER)** | — | Destroys cosmon state. Catastrophic, irreversible. Operator-only by definition. ADR-080 §5.1. |
| `cs prime` | NO | — | Pre-flight checks; local-shell convenience. |
| `cs migrate` | NO | — | Schema/state migration; operator-only. |
| `cs harvest` | NO | — | Scheduler-only (cron-driven); not a remote act. |
| `cs reconcile` | **NO (NEVER)** | — | Idempotent projection across STATUS.md, ISSUES.md, GitHub Issues. Long-running. Not a human act on the network — operator only. ADR-080 §5.1. |
| `cs project` | NO | — | Alias of `cs reconcile`; same answer. |
| `cs verify` | NO (V2 candidate) | (TBD) `GET /v1/molecules/:id/verify` | Read-only seal audit; conditional V2 exposure if turing's oracle-side-channel constraints met (ADR-080 §5.3). |
| `cs verify-trace` | NO | — | Local audit tool; not a remote act. |
| `cs verify-graph` | NO | — | Read-only Tarjan SCC check on a typed-relation subgraph (substrate primitive, task-20260509-75cc). Local audit; the operator runs it before adding new typed relations or in CI. No remote use case. |
| `cs paths` | NO | — | Pure projection of the `cosmon_core::paths::CosmonPath` write-path taxonomy (B7 collapse, `task-20260607-7f58`). Reads nothing, keeps no on-disk index; local audit/generator surface (gitignore stanza, ADR-030 archive manifest). No remote act. |
| `cs spec-audit` | NO | — | Local audit tool. |
| `cs validate` | NO | — | Deliberate heavyweight project-milestone gate; local audit, operator-run. No remote use case. |
| `cs release-audit` | NO | — | Local release-tooling drift detector (dry-run analogue of `reconcile --check`). **Legacy** live-tree analogue of the retired `release-resync` chain (idea-20260531-dc7c); under the one-repo model (ADR-133) the membrane referee is the exogenous `scripts/artifact-map-audit.py` + `scripts/release-checklist.sh`, not this command. No remote act. |
| `cs status` | NO (V1 TBD) | — | Cluster-wide status overlay; re-evaluate at V1. |
| `cs tag` | V1 | `POST /v1/molecules/:id/tags` | V1-mutation cut promoted in T-CST-V0 (`task-20260504-f0f4`, 2026-05-04). Required scope: `cosmon:molecule:write`. Wired into the `cs-thin` mechanical client alongside `observe` and `nucleate`. |
| `cs tail` | NO | — | Local log tail; SSE alternative is V2 (`GET /events?stream`, ADR-080 §10.3 / Q3). |
| `cs tokens` | NO | — | Read-only IFBDD aggregator over the local `tokens.jsonl` sink (T-V1-IFBDD-METER). The HTTP-side analogue is the unauthenticated diagnostic `GET /health/backends` plus the `InvocationCompleted` events.jsonl trail; no per-tenant token surface is needed remotely. Re-evaluate at V2 if tenant_auditor asks. |
| `cs note` | NO (V1 TBD) | — | Append-only molecule note; re-evaluate at V1 if tenant_auditor asks. |
| `cs deps` | NO | — | Read-only DAG visualisation; covered by `GET /v1/molecules/:id` payload. |
| `cs diverge` | NO | — | Local diff tool. |
| `cs heartbeat` | NO | — | Worker liveness pulse; worker-internal. |
| `cs run` | V0 | `POST /v1/molecules/:id/run` | Bounded drain (ADR-124, `task-20260610-56c4`): the client REQUESTS a drain of its own DAG; the resident loop runs in the tenant container under binding-sealed B1/B2/B3 bounds (never unbounded, B3 obligatory). 202-detached, lifecycle on the events bus. The *operator* `cs run` semantics (unbounded, local flags) are NOT exposed — the route forwards only the root id. |
| `cs spore` | NO | — | Germinate a polymer from a `spore.toml` template (ADR-140 N5: `validate`/`run`/`export`). `run` is a local-orchestration verb (it nucleates a whole DAG against the operator's store, like `cs run`'s operator semantics); `validate`/`export` are local dry-run/share tooling. No remote use case yet — a tenant-facing germination route would be a future RPP increment, not a V0 act. |
| `cs wait` | NO | — | Block-until-state CLI helper; not a remote act (`cs observe` polling is the network equivalent). |
| `cs realized-watch` | NO | — | Hidden dispatch plumbing: first-turn realized-model watcher, detached by `cs tackle` for session-log adapters (D4 / COND-1). Speaks only through `events.jsonl`; never operator- or tenant-invoked. Same NEVER-on-the-wire class as `cs heartbeat`. |
| `cs kill` | **NO (NEVER)** | — | Force-terminates a worker process tree. Side effects on disk and tmux. Operator gesture. ADR-080 §5.1. |
| `cs whisper` | **NO (NEVER for `--to-session`)** | — | Cross-session text injection (ADR-038). Bypasses the DAG. Operator-only. ADR-080 §5.1. |
| `cs presence` | NO | — | Session-presence registry; pilot-session interne. |
| `cs session` | NO | — | Pilot-session lifecycle; local. |
| `cs security` | NO | — | Parent verb; sub-verbs each have their own row below. |
| `cs sensorium` | NO | — | Local five-organ vital-strip reader (ADR-109). Powers `cs peek --snapshot`'s in-raster glyphs; the wire-shape (`peau / coeur / visage / carnet / voix / autopilot_off`) is published only to local viewports for UX↔CLI parity (ADR-068). Remote exposure would replicate the body of one operator's galaxy onto another principal — a category error. |
| `cs security activate` | **NO (NEVER)** | — | Switches cosmon-wide security posture (ADR-076). Affects every subsequent operation across all tenants. Operator-only. ADR-080 §5.1. |
| `cs security status` | NO (V1 TBD) | — | Could be safely exposed; re-evaluate. |
| `cs security oidc kill / revoke / unrevoke` | **NO (NEVER)** | — | Self-revocation paradox: a JWT-bearer revoking its own kill-switch is the attack scenario this protects against. Operator-only. |
| `cs notarize` | NO | — | Out-of-band notarisation (ADR-056); operator-only. |
| `cs panel` | **NO (NEVER)** | — | Constitutional-amendment gate: hash-pinned supermajority panel over a DNA-bullet change (mailroom `task-20260527-5fb6`). Exposing convocation/tally over the RPP would let a single remote caller drive the very ratchet whose point is to deny single-party control. Operator-only, on the record. |
| `cs witness` | **NO (NEVER)** | — | Layer-2 witness-quorum seal (ADR-085 §3). The whole point is structural independence from the molecule's tackler — exposing the seal over the RPP would re-couple it to the operator's session and dilute the audit value. Operator-only. |
| `cs notify` | NO | — | Local notification dispatch. |
| `cs key` | NO | — | Notary key generation; hardware-bound (YubiKey). |
| `cs scheduler` | NO | — | LaunchAgent / cron management; operator-only. |
| `cs daemons` | NO | — | HTTP-on-Tailscale daemon supervision; operator-only. |
| `cs apps` | NO | — | Operator view over the daemon health; operator-only. |
| `cs config` | NO | — | Resolved-config view (`show adapters`) and registry-projection enumeration (`adapters`, envelope `cs.adapters.list/v1`); local-only diagnostic. Exposing it would leak `[adapters.*]` env-var bindings and API key presence to a remote principal — a tenant-scoped subset would need its own ADR. |
| `cs vllm-mlx` | NO | — | Pre-flight + diagnostics for the vllm-mlx local-inference sidecar (Path B of `delib-20260519-f6c3`, ship 2026-06-15). `health` probes `127.0.0.1:8000/v1/models` over loopback — the endpoint binds to localhost only, so remote exposure would be a category error. |
| `cs local-worker` | NO | — | Local Ollama worker diagnostics and dispatch are bound to the operator's machine and its confined worktree; no remote act exists. |
| `cs galaxies` | NO | — | Meta-cosmon registry, not tenant-scoped. The RPP is *per-tenant* by construction (ADR-080 §8.1) — exposing the meta-list would breach tenant isolation. |
| `cs cluster` | NO | — | Machine-level topology (ADR-066). Operator-only. |
| `cs mur` | NO | — | Operator-override pin (mur du matin). Operator-only. |
| `cs motion` | NO | — | Local TUI surface. |
| `cs peek` | NO (V2 TBD) | (TBD) `GET /v1/molecules/:id/peek` | Wheat-paste byte raster (ADR-066). Re-evaluate need at V2. |
| `cs topology` | NO | — | Topon-driven structural map; local. |
| `cs replay` | NO | — | Event-log replay; local debug tool. |
| `cs archive` | NO | — | Cold-storage of completed molecules; operator-only. |
| `cs opt-in-share` | NO | — | Opt-in fleet-stats sharing; operator-only. |
| `cs inspect` | NO | — | Low-level state inspector; local debug. |
| `cs artifacts` | NO | — | Operator audit over the artifact map (ADR-057); local. |
| `cs fleet` | NO | — | Fleet template / init management; operator-only. |
| `cs patrol` | NO | — | Patrol sweep dispatch; scheduler-only (cron-driven). |
| `cs health` | NO | — | Read-only molecule-health Witness (ADR-137 P1): the anomaly catalog over local `.cosmon/` state, federation-wide. Local operator/CI snapshot; mutates nothing. The remote health surface, if ever needed, rides the ADR-068 pilot-app health panel over the existing `peek` raster, not a new route. |
| `cs pulse` | NO | — | Runtime-vitality reading (ADR-138 P1): RPM tachometer + six-voyant strip. Zero-mutation local observer over `events.jsonl` + state store. The pilot-app surface (P2 peek `v`-key tab) reads the same `Pulse` struct over the existing wheat-paste raster (ADR-066) — no new remote endpoint. |
| `cs events` | NO (V2 TBD) | (TBD) `GET /v1/molecules/:id/events` | Read-only event stream; lands when SSE is approved (ADR-080 Q3). |
| `cs errors` | NO | — | Read-only IFBDD aggregator over local `events.jsonl` for `MoleculeCollapsed`. Tenant-scoped views ride the same path as `cs tokens` — re-evaluate at V2 if tenant_auditor asks. |
| `cs quench` | NO | — | Energy-budget injection; operator-only. |
| `cs help` | NO | — | Documentation, not a remote act. The RPP serves a hand-written OpenAPI document under `openapi/v1.yaml` instead. (`man cs` is the same row, different surface.) |
| `cs doctor` | NO | — | Local diagnostic tool. |
| `cs test` | NO | — | Local test harness. |
| `cs demo` | NO | — | Local demo runner. |

---

## How to promote a `cs` verb to the API

A new RPP route is **never** added by accident. The PR that adds it
must touch all of the following in a single change:

1. Identify a **tenant_auditor-réel use case** (not anticipation). ADR-080
   §10 places the user-evidence bar before code: track-A (signed
   binary + YubiKey to tenant_auditor) is the 15-minute test. Promote a verb
   only when the binary path has demonstrably failed for that verb.
2. **Open a successor ADR** (or amend ADR-080) with rationale, blast
   radius, and `delegate_for` analysis if the verb is in §5.1
   (operator-only or worker-only).
3. **Update this table** with the API path and a short justification.
4. **Land the route** in `cosmon-rpp-adapter` in the **same PR**.
5. **Update the freeze snapshot** at
   `crates/cosmon-rpp-adapter/tests/api_surface_freeze.rs`.
6. **Update the OpenAPI document** `openapi/v1.yaml`. No
   auto-generation from internal Rust types — the document and the
   types are intentionally decoupled (ADR-080 §1.4 R6).
7. **Document the semver impact**: `/v1/` extension is non-breaking;
   any breaking change cohabits `/v2/` and `/v1/` for at least six
   months.

If any of the seven steps is missing, the CI gate fails the PR.

---

## How to verify a verb stays out of the API

A verb that has no business on the network is *also* tracked here —
explicitly. The mechanism:

1. Add the row to the audit table with `Exposed via API? = NO` (or
   `**NO (NEVER)**` for verbs in ADR-080 §5.1).
2. Spell out the reason in the rightmost column. Cite the governing
   ADR or invariant clause.
3. The CI gate (next section) will fail any PR that adds an axum
   route for a verb whose row says `NO`. The breach is structural,
   not stylistic — file a bead, do not patch the registry to make
   the breach pass.

---

## Drift test (CI gate)

A test in `crates/cosmon-cli/tests/api_cli_coverage.rs` enforces the
following invariants on every CI run:

1. **Every user-facing CLI verb has a row in this table.** If a new
   `cs <verb>` lands without an `api-cli-coverage.md` row, the test
   fails with:

   ```
   cs <verb> is not in docs/guides/api-cli-coverage.md — add a row
   (mark `Exposed via API? = NO` if no remote use case exists yet)
   ```

2. **Every `Exposed = V0` row has a corresponding axum route.** If
   the registry promises a V0 route that the adapter does not
   implement, the test fails with:

   ```
   V0 route `<path>` for `cs <verb>` is promised in
   docs/guides/api-cli-coverage.md but not implemented in
   cosmon-rpp-adapter::routes
   ```

3. **Every axum route has a registry row marked at or before the
   current version.** If a route is added without updating this
   table, the test fails with:

   ```
   Route `<METHOD> <path>` is exposed by cosmon-rpp-adapter but not
   declared in docs/guides/api-cli-coverage.md
   ```

The test is intentionally **defensive** rather than prescriptive: it
catches the *silent* drift that turns §8p (subset strict) into an
accidental §8l (parity). It does not prevent legitimate evolution —
every PR that legitimately changes the surface updates the table
and the test passes.

While `cosmon-rpp-adapter` does not yet exist on the branch (V0
lands week 5–9 May 2026 per ADR-080 §10.1), the test asserts the
empty-route side of the invariant: every CLI verb has a row, and
the registry's promised `V0` rows are visible. The route-side checks
become active when the adapter crate lands; the same test grows the
import without changing structure (`crates/cosmon-rpp-adapter/src/
routes.rs::list_routes()`).

---

## Worker-only and operator-only verbs — the two NEVER classes

Two clusters of verbs are flagged `**NO (NEVER)**` in the table.
They are the structural hard-floor of §8p and ADR-080 §5.1; the
test refuses to admit any axum route for them:

**Worker-only (CLAUDE.md *Command perimeters*).** `cs evolve`,
`cs complete`. Workers run inside their own worktree with their own
`cs` binary; the RPP must not reach into a worker's process. A
network-bearing principal is by definition not a worker.

**Operator-only (ADR-080 §5.1).** `cs done`, `cs purge`,
`cs reconcile`, `cs verify`, `cs whisper --to-session`, `cs drop`,
`cs security activate`, `cs kill`. These verbs' authority chain
ends at the operator's keyboard. Extending the list — i.e.
*shrinking* the NEVER class — requires a successor ADR with an
explicit `delegate_for` claim model (ADR-080 §5.2).

Until that successor lands, the CI gate refuses any axum route
whose `cs` counterpart is in either NEVER class.

---

## Re-snapshot cadence

This audit is regenerated **on every CLI verb change** (mirrors the
[ADR-068](../adr/068-ux-cli-equivalence.md) §1 *Alphabet-Closure*
discipline applied to the API surface). A worker who adds a new
`cs` verb adds a row here in the same PR, default `NO` with a
reason. A worker who adds a new RPP route adds the API path here,
amends ADR-080 (or files a successor), and updates the freeze
snapshot — all in the same PR.

The audit serves as the v0 detection instrument; once the freeze
snapshot lands with the V0 cosmon-rpp-adapter, the round-trip is
mechanically enforced (this guide ↔ axum router ↔ OpenAPI).

---

## How to use this guide

- **As a contributor adding a `cs` verb.** Read the table; pick a
  posture (`NO` is the default); write a one-sentence justification.
  If your verb might warrant an RPP route in V1 or V2, mark it
  `V1 (TBD)` or `V2 (TBD)` and link the bead that re-evaluates.
- **As a reviewer.** Any PR adding a `cs` verb MUST update this
  table. A PR that does not is a §8p breach on the *closure* side
  (the symmetric breach is opening a route without a row).
- **As an operator demoing the RPP to tenant_auditor.** Use the V0 row as
  the entry point; `GET /v1/molecules/:id` is the demo path.
  Everything else is either *future* (V1+) or *deliberately
  off-network* — and that is a feature, not a gap.

The audit is the operational instrument for §8p; ADR-080 is the
principle. Read both together.
