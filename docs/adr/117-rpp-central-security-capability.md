# ADR-117 — The Remote Pilot Port is a central secure-delivery capability, kept whole in cosmon

**Status:** Proposed (2026-06-05) — operator-ratification points marked inline
**Decider:** Noogram
**Supersedes:** ADR-113 (public/private crate frontier) — closes its `Proposed` status
**Idea source:** `idea-20260605-d58e` (idea-to-plan)
**Binds:**
[ADR-080](080-remote-pilot-port-https-oidc.md) (RPP = §8j HTTPS+OIDC ingress adapter — the reframe touches its framing),
[ADR-023](023-cockpit-hexagonal-read-surface.md) (Layer-B port adapter),
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (RPP is a Layer-B adapter, not a daemon)
**Recoupe:** [ADR-092](092-license-bascule-mpl-to-agpl.md) (license partition — `cosmon-remote` is Apache-2.0 frontier), [ADR-082](082-architecture-baseline.md) (architecture tier `substrate`)

---

## 1. Context

ADR-113 was the **gate before the public flip of cosmon**: it had to decide
whether the workspace carried confidential client coupling that forced a
public/private crate split. Its own §9 correction (2026-06-01) already refuted
the split premise by symbol-level grep — **zero client-named symbols** in the
shipping code path — collapsing its option set to "genericise in place, stay
public." But ADR-113 left the *positive framing* of the RPP unresolved and its
status `Proposed`.

A correction to the operator's understanding now closes that gap and **inverts
the residual framing**:

> Tenant-Demo is conducting a **security-hardening mission** of cosmon — **not** a
> client receiving a bespoke feature. Everything built in `cosmon-rpp-adapter`
> is what was put in place so that a tenant can deploy **their hardware-encryption
> solution** on cosmon during a delivery onto a **VM in an AWS cloud**.
> Consequence: **all the code stays in cosmon.** This module should become
> **central** to delivering cosmon in a cyber-secured way with hardware
> encryption.

This ADR records that the Remote Pilot Port is **not** a client adapter but a
**central secure-delivery capability of cosmon**, that **nothing is split out**,
that the client identity is **anonymised**, and that the literate documentation
is **reframed** to match.

## 2. Decision

### 2a. Keep everything in cosmon — no split (operator-decided)

`cosmon-rpp-adapter` and `cosmon-remote` **stay in the public cosmon workspace,
whole.** ADR-113's options 2 (split to private repo) and 4 (hybrid split) are
**void** (they were already self-voided in ADR-113 §9d as solving a phantom).
The `split-rpp-private` child of ADR-113 §7 is **collapsed, do not dispatch.**
There is no public/private crate frontier; there never was a real one.

### 2b. Reframe the RPP as a central secure-delivery capability — framing-as-purpose

The RPP is reframed from *"an HTTPS/OIDC ingress adapter"* to *"the central,
audited secure-delivery door of cosmon — the §8j boundary through which a remote
pilot reaches a cosmon instance running on a hardened (hardware-encrypted) VM."*

**A load-bearing honesty constraint governs this reframe.** As of 2026-06-05 the
RPP code carries **zero** hardware-encryption surface (`grep -i
'hardware|encryption|HSM|enclave|nitro'` over `cosmon-rpp-adapter/**` → empty;
ADR-080 §8's key-material discussion is explicitly **off-RPP**, on the operator's
IdP). The reframe therefore adopts **framing-as-purpose, not framing-as-feature**:

- ✅ **The RPP *fronts* a tenant's hardware-encryption stack.** It is the
  one-way, causal-closure-enforcing, OIDC-authenticated door into a cosmon
  instance that *runs inside* a hardware-encrypted environment (AWS VM + the
  tenant's HW-encryption solution). The encryption is part of the *deployment
  context the RPP secures access to*, and the RPP's five-clause admission
  boundary is what makes that delivery *cyber-secured*.
- ❌ The RPP does **not** today *own* encryption (envelope-encrypt the
  subprocess payload, attest the boundary in a Nitro Enclave, seal the audit
  log with a KMS key). That is a **distinct, larger design** parked as a
  `temp:warm` bead (§4, deferred); it must not be implied by documentation
  until code backs it.

**Rationale.** Shipping prose that claims "central hardware-encryption
capability" over code that does no encryption would be the exact
prose-claim-≠-code-surface shadow contract that ADR-113 §9 had to demolish once
(token-frequency ≠ structure). Framing-as-purpose is *true today*, ships now,
and unblocks the flip; framing-as-feature is an honest future, not a present
claim.

> **⚖️ Operator-ratification point #1 — reframe depth.** This ADR defaults to
> **framing-as-purpose (A)**. If the operator wants the RPP to *own* hardware
> encryption (B), say so and the deferred design bead (§4·d) is promoted from
> `temp:warm` to a dispatched design molecule.

### 2c. Anonymise the client identity (operator-decided in principle, scope below)

Every client/human token (`tenant-demo`, `tenant_auditor`/`tenant_auditor`, `bob`/`bob`,
`jordan` (the person), `dan`, `carol`) is removed from the **code** and
replaced from a **fixed neutral vocabulary table** (§3). The tokens are
fixtures, doc-comments, and default strings — **never symbol names** (re-confirmed
2026-06-05: zero declarations). So this is a vocabulary substitution, not a
refactor, executed **crate-by-crate with `cargo test -p <crate>` after each
rename** to avoid the `escape_mxid` partial-rename test breakage.

> **⚖️ Operator-ratification point #2 — anonymisation scope.** Two surfaces
> exist. **(i) Code:** the 61 `.rs` files across 11 crates — *in scope for this
> ADR's child task §4·a.* **(ii) Deployment/correspondence artifacts:** 84
> tracked **path-named** files (`dist/tenant-demo-handover/**`,
> `docker/tenant-demo-strace*/**`, `correspondance/tenant-demo/**`,
> `dist/l1-brew-tap/*-TENANT_AUDITOR.md`, `docs/guides/*tenant_auditor*`) — these carry deeper
> identity (a signed letter of intent, a private-registry push script, named
> humans) and several are *instance/deployment data* better **evicted or
> gitignored** than genericised (cf. ADR-113 §9c). This ADR defaults to
> **handling (ii) as a separate eviction task (§4·c)**, keeping the code
> anonymisation crisp. The operator may fold them together.

### 2d. Keep the crate name `cosmon-rpp-adapter` (recommendation, operator-ratify)

`RPP = Remote Pilot Port` names the **architectural role** (ADR-080), is already
**client-neutral**, and is woven through ADR-080's frozen vocabulary (§15) and
the §8j/§8p invariants and surface-freeze test. Renaming it (e.g.
`cosmon-secure-ingress`) would ripple into 5 `Cargo.toml`, ~5 test files,
ADR-080's title and forbidden-vocab list (`endpoint`/`microservice`/`gateway`
adjacency), and the §8p `api_surface_freeze.rs` test — and, under the
framing-as-purpose decision of §2b, a `secure` in the crate slug would itself
**over-claim** (the same shadow contract). A neutral role-name that does not lie
is an asset; the security centrality is carried by the **module headers + this
ADR**, where it can be *precise*, not by a crate slug that compresses.

> **⚖️ Operator-ratification point #3 — crate name.** Default: **keep
> `cosmon-rpp-adapter`.** Alternative on request: rename to foreground security
> (cheap mechanically once decided; the ripple is bounded).

### 2e. Reframe the literate documentation (operator-decided)

`//!` module headers, `///` doc-comments, the README, ADR-080, and invariants
§8j/§8p are audited so that **(1)** no wording leaves the impression the RPP is
client-specific, and **(2)** the secure-delivery framing of §2b is coherent and
honest (no encryption over-claim). This is a reading pass with a checklist
(§4·b), not a `sed`.

## 3. Neutral vocabulary table (authoritative for the child tasks)

Aligns with ADR-113 §9e's `tenant-demo → tenant-demo` precedent.

| client token | neutral replacement | notes |
|--------------|---------------------|-------|
| `tenant-demo` / `Tenant-Demo` | `tenant-demo` / `TenantDemo` | noyau id, sandbox, prose |
| `cosmon-rpp-tenant-demo` (JWT `aud`) | `cosmon-rpp-tenant` | verify no live IdP couples to the old `aud` before renaming |
| `tenant_auditor` / `tenant_auditor` | `operator-demo` | the named human operator/pilot |
| `bob` / `bob` | `operator-b` *(or drop)* | sparse fixtures |
| `jordan` (person) | `pilot-c` *(or drop)* | **keep `jordan-showroom`** — that is a real cross-galaxy pattern name, not a client identity |
| `dan` / `carol` | drop / neutral | sparse, prose-only |
| `T9 Tenant-Demo V2` (milestone) | `T9 remote-tackle V2` | de-name the milestone; **keep the `task-…`/`spark-…` id link** as the proof-of-work anchor |
| `AWS Tenant-Demo live test` | `AWS live-deploy test` | keep the finding, drop the name |

**Provenance rule:** for doc-comments citing real tasks/transcripts, **keep the
`task-…`/`spark-…`/`delib-…` id link, strip the human/client name.** The id is
the durable proof-of-work anchor; the name is the thing to anonymise.

## 4. Plan — child tasks (all `temp:warm`, count-bounded)

This idea-to-plan molecule emits **three** child task molecules plus this ADR.
Each child carries the cosmon Definition of Done — `cargo
check/test/clippy/fmt --workspace` all green — and is tagged `temp:warm` on
nucleation.

- **(a) Anonymise the 61 `.rs` files** across the 11 crates using the §3 table,
  crate-by-crate with `cargo test -p <crate>` after each rename; respect the
  §8p frozen API surface and ADR-080 §15 forbidden vocabulary. → `task-work`.
- **(b) Audit + reframe the literate documentation** (README, `//!`/`///`
  headers, ADR-080, invariants §8j/§8p) for secure-delivery-framing coherence
  and zero client-specific impression and zero encryption over-claim
  (framing-as-purpose per §2b). → `task-work`.
- **(c) Disposition of the 84 deployment/correspondence artifacts** —
  evict / gitignore / genericise the `dist/`, `docker/`, `correspondance/`,
  `*-TENANT_AUDITOR.*` path-named files before the public flip. → `task-work`.

- **(d) [deferred design bead, NOT nucleated as a task]** *Framing-as-feature:
  should the RPP own hardware encryption (envelope-encrypt the subprocess
  payload, enclave-attest the admission boundary, KMS-seal the audit log)?*
  Parked as `temp:warm`; promoted only on operator-ratification point #1.

## 5. Consequences

- **The public flip is unblocked** once (a) + (b) + (c) land and the lexical
  gates are green. There is no crate-frontier decision left to make.
- **The phantom-coupling trap cannot be resurrected.** ADR-113 §9f warned the
  void "private-crate" premise could be re-derived by a future reader; a clean
  anonymisation + an explicit positive reframe make that impossible.
- **Cosmon gains a named central asset:** the §8j secure-delivery door, a
  genuine product story rather than a client favour — provided §2b's honesty
  constraint is respected.
- **Risk managed:** the only misalignment is over-claiming encryption the code
  does not do; §2b's framing-as-purpose default and §4·d's deferral contain it.

## 6. Open ratification summary (for the operator)

1. **Reframe depth** — framing-as-purpose (default) vs framing-as-feature (promote §4·d)?
2. **Anonymisation scope** — 61 `.rs` only (default, §4·a) vs whole tree incl. the 84 artifacts (fold §4·c in)?
3. **Crate name** — keep `cosmon-rpp-adapter` (default) vs rename to foreground security?

The three child tasks (a)(b)(c) are dispatchable under the defaults; only
promotion of the deferred design bead (d) needs answer #1.
