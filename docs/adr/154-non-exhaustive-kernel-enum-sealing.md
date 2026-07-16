# ADR-154 — Selective `#[non_exhaustive]` sealing of growth-prone cosmon-core kernel enums

**Status:** Accepted (2026-07-12)
**Decision owner:** Noogram
**Origin:** F2 of `task-20260712-2897` `review-report.md` (C8 semver-non-exhaustive finding), decided under `task-20260712-a7cc`
**Depends on:** the cosmon-core two-tier public-surface doctrine (`crates/cosmon-core/src/lib.rs` §kernel/hidden, delib-20260622-187a F-TOLNAY-1/2)
**Relates to:** F1 of the same review (kernel-boundary reachability), the `EventV2` migration (`crates/cosmon-core/src/event_v2.rs`)

## Context

`cosmon-core` is `publish = true`. Per its own module doctrine, the ~10
THESIS-named **kernel** modules (`molecule`, `id`, `error`, `fleet`,
`worker`, `formula`, `event`, `tag`, `kind`, `role`) are the frozen semver
contract that `cargo-semver-checks` governs; the other ~81 modules are
`#[doc(hidden)] pub mod` and carry no external warranty.

The `#[non_exhaustive]` doctrine is **partially live**: `CosmonError`,
`MoleculeKind`, `MoleculeStatus`, `CollapseCause`, `FormulaError`,
`FleetSpecError`, `TagError`, `IdError`, `NodeKind`, `EdgeType`, the
event *classification* enums (`ClaimType`/`Verdict`), and the entire
`EventV2` schema all carry it. The **inconsistency is the finding**: several
growth-prone kernel enums do not.

Two hard facts frame the decision.

1. **Adding `#[non_exhaustive]` to a `pub enum` is itself a breaking (major)
   change.** `cargo-semver-checks` classifies `enum_now_non_exhaustive` as
   major, because every downstream crate matching the enum *without a wildcard
   arm* stops compiling. This includes cosmon's own sibling crates — the
   attribute takes effect at the crate boundary, not the workspace boundary. So
   this cannot be foamed as a mechanical patch; it must be planned into a major
   version bump. This is why the finding was dispositioned **decision, not
   auto-foam**.

2. **The internal match-exposure is large and asymmetric.** A rough exhaustive
   grep across sibling crates (upper bound; a match with a wildcard is not
   broken):

   | Enum | file:line | sibling files matching | growth history |
   |------|-----------|:----------------------:|----------------|
   | `WorkerStatus` | worker.rs:106 | 14 | grew (`Stale`, `Unresponsive` added) |
   | `WorkerRole` | worker.rs:43 | 8 | open per ADR-063 nucleon roster |
   | `EffectiveStatus` | worker.rs:279 | 3 | grew (`Suspect`, `Blocked`, `Diverged`) |
   | `EvolveOutcome` | molecule.rs:708 | 1 | binary today, lifecycle-open |
   | `ReconcileAction` | worker.rs:315 | 1 | grew (`Respawn`, `CircuitBreak`) |
   | `QuerySource` | formula.rs:722 | 1 | artifact sources grow |
   | `ParallelLimit` | formula.rs:864 | 1 | ADR-044 foreshadows growth |
   | `Event` | event.rs:168 | 1 (cockpit, 28 arms) | **frozen — see below** |
   | `DesiredState` | worker.rs:174 | — | closed tri-state |
   | `TransportState` | worker.rs:214 | — | closed tri-state |
   | `CognitiveState` | worker.rs:238 | — | closed tri-state |

Sealing an enum for **external** safety therefore costs **internal**
exhaustiveness checking: once `WorkerStatus` is `#[non_exhaustive]`, the ~14
sibling matches must carry a `_ =>` arm, and the compiler no longer tells
cosmon-cli "you forgot to handle the new state." That trade — external safety
bought with internal exhaustiveness — is the axis this decision rules on.

## Decision

**Seal exactly the enums that are open-by-design *and* genuine external
contract; leave the closed alphabets exhaustive; do not seal the surface that
is already dying.** The seals land as one batch in the next `cosmon-core`
major, **0.3.0** (the 0.y breaking cadence), so the "downstream must add a
wildcard" break is spent exactly once.

### Seal in cosmon-core 0.3.0 (7 enums)

| Enum | Why open-by-design |
|------|--------------------|
| `EvolveOutcome` (molecule.rs:708) | Lifecycle outcome; will gain suspended/blocked-style variants as step semantics grow. |
| `WorkerRole` (worker.rs:43) | ADR-063 nucleon roster is explicitly an open set. |
| `WorkerStatus` (worker.rs:106) | State machine that has already grown; more transient states will appear. |
| `EffectiveStatus` (worker.rs:279) | Interpretive health rollup; has grown (`Suspect`/`Blocked`/`Diverged`) and will keep gaining nuances. |
| `ReconcileAction` (worker.rs:315) | Projection-action set grows with runtime capability (`CircuitBreak`/`Respawn` are recent). |
| `QuerySource` (formula.rs:722) | Artifact query sources will grow (`Synthesis`, `Log`, `Notes` are natural additions). |
| `ParallelLimit` (formula.rs:864) | ADR-044 roadmap explicitly foreshadows policy-variant growth. |

For the four heavily-matched worker enums we accept the internal
exhaustiveness cost: the external-safety benefit (a downstream galaxy
monitoring fleet health will not break on a new `WorkerStatus`) outweighs the
loss, and the loss is mitigated by keeping the *authoritative* interpretive
matches (the reconcile loop) inside `cosmon-core` itself — same crate, so
`#[non_exhaustive]` does not apply and exhaustiveness is preserved where it
matters most.

### Do NOT seal (reason recorded per enum)

- **`Event` (event.rs:168) — frozen by migration.** New event kinds now land
  in `EventV2` (event_v2.rs:377), which **already** carries `#[non_exhaustive]`.
  The last variant-level change to legacy `Event` predates the current cycle;
  growth has moved to `EventV2`. The event-sourcing hazard the finding worried
  about is therefore *already covered on the live surface*. Sealing the
  sunsetting `Event` would force cockpit's 28 exhaustive arms to add a wildcard
  for a type slated for removal — a double break (wildcard now, deletion later).
  The correct semver event for `Event` is its **removal** (also a 0.3.0-class
  major), not sealing. This supersedes the finding's "seal `Event` first"
  recommendation.
- **`DesiredState` / `TransportState` / `CognitiveState` — closed alphabets.**
  `{Running, Paused, Stopped}`, `{Alive, Dead, Unknown}`, `{Fresh, Stale, None}`
  are *complete* lattices — operator intent, physical liveness, freshness. There
  is no growth axis. Leaving them exhaustive is a feature: it forces every
  consumer to consciously handle each state, exactly the doctrine already
  written for `adapter_exit.rs:72` ("the five-class alphabet is fixed —
  deliberately **not** `#[non_exhaustive]`").

### Considered and rejected

- **Blanket-seal all 11 (the finding's implicit direction).** Rejected: it seals
  closed alphabets (churn with no upside) and seals a dying `Event` (double
  break). Selectivity is the point of routing this to a decision.
- **Demote the worker-state family to `#[doc(hidden)]` instead of sealing** (the
  F1-adjacent move — removes them from the external contract entirely and
  *preserves* internal exhaustiveness). Rejected here because `worker` is a
  THESIS-named **kernel** module; demoting it contradicts the lib.rs doctrine
  that names `worker` as public contract. If a future F1 decision re-partitions
  the kernel and finds these enums are fleet-runtime internals, demotion becomes
  the superior lever and this seal should be revisited — but that re-partition is
  its own decision, not a rider on this one.

## Consequences

- **One coordinated major.** 0.3.0 adds `#[non_exhaustive]` to the 7 enums and
  sweeps `_ =>` arms into the sibling matches that need them (~20 sites, upper
  bound). Batching with any other pending breaking change keeps the downstream
  wildcard-break to a single version step.
- **Downstream migration note (for the CHANGELOG / release notes):** before
  0.3.0, any external match on `EvolveOutcome`, `WorkerRole`, `WorkerStatus`,
  `EffectiveStatus`, `ReconcileAction`, `QuerySource`, or `ParallelLimit` must
  gain a `_ => …` arm. Matches on `Event`, `DesiredState`, `TransportState`,
  `CognitiveState` are unaffected.
- **Enforcement gap flagged.** The doctrine names `cargo-semver-checks` as the
  governing gate, but it is **not currently wired into CI** (`.github/` has no
  semver-checks job). Wiring it — so a future accidental seal/unseal or variant
  addition is caught as a labelled major/minor — is a separate **foamable**
  follow-up (`temp:warm`), not part of this decision.
- **No source patched by this molecule.** This is a `📐 decision`; the enum
  edits and the version bump are executed later, together, as the 0.3.0 major.

## Verification

The classification is deterministic and reproducible against the committed tree:

```
# has non_exhaustive (contrast) vs. missing (the finding)
grep -n -B1 'pub enum CosmonError' crates/cosmon-core/src/error.rs      # has it
grep -n -B1 'pub enum Event\b'     crates/cosmon-core/src/event.rs      # missing (frozen)
grep -n -B1 'pub enum EventV2'     crates/cosmon-core/src/event_v2.rs   # has it (live surface)

# closed alphabets — variant lists are complete lattices
sed -n '176,190p;216,236p;240,278p' crates/cosmon-core/src/worker.rs

# EventV2 is where new kinds land (frozen-by-migration evidence)
git log --oneline -- crates/cosmon-core/src/event_v2.rs | head
```
