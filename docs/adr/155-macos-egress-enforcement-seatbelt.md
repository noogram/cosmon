# ADR-155 — Native macOS egress enforcement (seatbelt profile → Network Extension)

**Status:** Proposed (2026-07-13)
**Decision owner:** Noogram
**Origin:** `task-20260713-8acc` (🐛 issue), the residual-platform half of review 5008 — "egress jail is fail-open/advisory on macOS and all non-Linux"
**Depends on:** the egress substrate (`crates/cosmon-core/src/egress.rs`, delib-20260530-0877, C1-F3 task-20260712-8d2d), ADR-016 (autonomy regimes), ADR-080 §3.5 (RPP subprocess envelope)
**Relates to:** architectural-invariants.md §8u (egress enforcement honesty across platforms), the cutover gate `CutoverReport`

## Context

Cosmon's strict-local autonomy is enforced, not merely labelled, by an
**egress jail**: a `deny-external` worker's `exec_command` subprocesses run in
an unprivileged network namespace whose only interface is loopback, so a weak
local model that shells out to `claude -p` hits a *refused syscall*, not a
*detected anomaly* (turing's master finding, `cosmon-core::egress`).

The jail is real **only on Linux**. `EgressJail::enforcement_mode_for` returns
`EnforcementMode::Netns` when the runtime probe finds an unprivileged
user+network namespace is creatable, and `EnforcementMode::Advisory`
everywhere else. On macOS there is *no* `unshare(2)` / network-namespace
facility, so a `deny-external` dispatch is always advisory: the policy is
recorded on the wire, the subprocess runs unjailed, and it *can* reach the
network.

Review 5008 named two follow-ups. The **immediate hardening** — make the
advisory path honest and fail-closed on the exposed multi-tenant surface — is
shipped alongside this ADR (see §Decision, "immediate hardening"). The
**residual platform undertaking** — a *real* egress deny on macOS so the
hosted endpoint can run there at all — is the subject of this ADR.

### Why this blocks

A single-operator dev host degrading to advisory is benign: the one human at
the keyboard is trusted. The **exposed multi-tenant** RPP endpoint is not: a
tenant's `deny-external` worker that is not actually jailed can reach the
network, defeating the isolation the endpoint sells. Until macOS can
kernel-enforce denial, **an exposed multi-tenant cosmon endpoint cannot be
hosted on macOS with strict-local tenants** — the honest state §8u encodes and
`preflight` now enforces fail-closed.

## Decision

### Immediate hardening (ships now, this molecule)

`EgressJail::preflight` gains a fourth axis, `exposed_multi_tenant`. A
non-enforceable `deny-external` on an exposed host is **`Refused` regardless of
the operator's `COSMON_EGRESS_REQUIRE_NETNS` knob**. `cs tackle` computes the
axis from `egress_probe::exposed_multitenant_from_env()` — true when the
dedicated `COSMON_EGRESS_EXPOSED` var is truthy **or** the RPP subprocess
envelope's `COSMON_API_REQUEST` marker (ADR-080 §3.5) is present. This closes
the fail-open on the hosted path *today*, with zero new platform code, and is
the load-bearing guard until a native tier below lands.

### Native enforcement — two tiers

Introduce a third enforcement mode, `EnforcementMode::Seatbelt`, resolved by
the darwin branch of the runtime probe when a tier is available. The pure
`EgressJail::wrap_with_mode` learns to emit the seatbelt-wrapped command the
same way it emits the `unshare` wrapper today — platform-portable construction,
unit-tested on any host, kernel effect exercised only on darwin.

**Tier 1 — Sandbox profile via `sandbox-exec` (ship first).** Wrap the
subprocess in Apple's Sandbox (Seatbelt) with a profile that denies all
network operations but allows loopback:

```scheme
(version 1)
(allow default)
(deny network*)
(allow network* (local ip "localhost:*"))
(allow network-bind (local ip "localhost:*"))
```

The wrapped command becomes, analogously to the netns path:

```text
sandbox-exec -p '<profile>' -- <program> <args...>
```

- **Pro.** Available on every macOS with no entitlement, no signing, no
  admin/root, no daemon — the same "works on a bare laptop" property the netns
  jail has on Linux. Per-process, so it composes with the existing
  `exec_command` wrap seam exactly.
- **Con.** `sandbox-exec` is deprecated-but-present (Apple has kept it
  functional for a decade of "deprecated"). The profile language is
  under-documented. This is a *pragmatic* enforcement, not a supported API —
  its status must be re-probed at runtime, never assumed from `cfg!`.

**Tier 2 — Network Extension content filter (robust follow-on).** A
`NEFilterDataProvider` (system network extension) that drops all non-loopback
flows for the worker's process group. This is the *supported* API and survives
OS upgrades, at the cost of: a signed, notarized system-extension bundle, a
provisioning profile with the Network Extensions entitlement, user approval on
first install, and a resident filter process (a daemon — outside the
transactional core, shipped as an opt-in hosted-runtime component, never
spawned by `cs`). Justified only for a genuinely-hosted macOS multi-tenant
deployment; the dev-host and single-operator cases never need it.

### Probe and mode resolution

`egress_probe::netns_available()` stays Linux-only. Add a sibling
`egress_probe::seatbelt_available()` (darwin-only; probes that `sandbox-exec`
is on `PATH` and a trivial deny profile executes) and a mode resolver that
prefers the strongest available tier: `Netns` (Linux) → `Seatbelt` (darwin,
tier 1) → `Advisory`. Once a tier reports available on darwin, the §8u blocking
rule relaxes automatically: `preflight` returns `Ready` because the host *can*
enforce denial, and the exposed endpoint may run on macOS.

## Consequences

- **Positive.** The exposed endpoint is fail-closed on macOS *today* (immediate
  hardening) and gains a real enforcement path (tier 1) without waiting for the
  entitlement-heavy tier 2. The `wrap_with_mode` seam already isolates the
  platform-specific construction, so adding `Seatbelt` is additive — no change
  to the `exec_command` call site, the cutover gate, or the audit schema.
- **Negative.** Tier 1 leans on a deprecated tool; the probe must fail closed to
  `Advisory` (and therefore `Refused` on the exposed path) if `sandbox-exec`
  ever disappears. Tier 2 introduces a resident filter process — an opt-in
  hosted-runtime daemon, held strictly out of the transactional core (§1
  inviolable layering).
- **Scope boundary (Rice, unchanged).** This ADR closes the *routing* question
  on macOS — *can a worker reach a remote oracle?* — which is enforceable. It
  does not touch the *quality* question, which stays with acceptance tests and
  the loud opt-in.

## Status of implementation

- **Shipped now:** the `exposed_multi_tenant` preflight axis + probe fold-in +
  §8u invariant (this molecule).
- **Proposed, not yet built:** `EnforcementMode::Seatbelt`, the darwin probe,
  and the tier-2 Network Extension. Each is a separate molecule; this ADR is
  the design they cite.
