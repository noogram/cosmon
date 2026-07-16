# ADR-029: Molecule TTL and Expiry Policy

## Status
**Superseded by [ADR-052](052-one-ledger-one-writer-one-witness.md)** (2026-04-19).

Originally **Proposed (2026-04-12)**. The data-model half of this ADR
(`MoleculeData.expires_at`, `MoleculeData.expiry_policy`, `ExpiryPolicy`,
the `EventV2::Expired` variant, the pure `evaluate_expiry` function) **did
land** and remains part of cosmon-core: `cs nucleate` still accepts
`--ttl` / `--expires-at` and the state store carries the fields.

What ADR-052 §CLI delta retired — and what this ADR therefore no longer
prescribes — are the two CLI verbs **`cs touch`** and **`cs expire`**,
plus the `cs patrol --expire` sub-flag. The Torvalds/Tolnay panel
derivation behind ADR-052 was that a per-molecule TTL refresh verb
("touch") masks the more fundamental pathology — *absence of liveness
witness* — rather than fixing it. TTL-as-poll (`cs expire` sweeps) was
rejected for the same reason: liveness is derived from an external
probe (ADR-052 invariant I8), not from a wall-clock deadline.

This ADR is retained in-tree as historical context for the surviving
data-model fields and as a reading note: if a future sweeper verb is
proposed, it must reconcile with ADR-052 before adoption. A re-open
should come as a successor ADR, not as a direct edit to this one.

## Context

Cosmon's state store has accumulated several recurring hazards around
molecule longevity that the current model cannot express:

- **State sediment.** Pending molecules linger past their relevance window
  and silently re-enter the runtime's scope. The convoy cascade chronicle
  documented how a greedy
  `cs run` / `cs patrol --respawn` resurrected stale pendings as if they
  were fresh work. Temperature tags (`temp:hot/warm/cold/frozen`) address
  the *cognitive* curation half of this problem, but there is no
  *mechanical* expiry — humans must tag every molecule, forever.

- **Deadline reminders.** Certain molecules are only useful until a real
  external deadline. The GCP credit seed case is canonical: a molecule
  captured "apply the $5k credit before 2026-06-30" and had no way to
  decay into a visible warning when the window closed. The deadline lives
  in the body text, invisible to every projection.

- **The curation pair is incomplete.** `temp-review` + temperature tags
  form the cognitive half of backlog curation. TTL + expiry policy are
  the mechanical half: the system, not the human, notices that a molecule
  is stale or past its window.

Deliberation `delib-*-TTL` (internal) framed the boundaries: TTL must not
become a backdoor scheduler, must not violate the stateless-CLI invariant,
and must not silently destroy work. A cron-driven background sweeper was
rejected; a new `expired` molecule status was rejected. What remains is
an additive, pull-based, opt-in expiry model.

## Decision

Add **two additive fields** on `MoleculeData`, **two CLI verbs**, and a
**sub-flag** on `cs patrol`. No changes to the status enum, no new daemon,
no new state store.

### Data model additions

```rust
pub struct MoleculeData {
    // ...existing fields...

    /// Absolute UTC timestamp past which this molecule is "expired".
    /// `None` means no TTL — current behavior, indefinite retention.
    pub expires_at: Option<DateTime<Utc>>,

    /// What to do when `expires_at` is in the past. `None` means inherit
    /// the per-kind default from `.cosmon/config.toml`; if no default is
    /// configured, behavior is `ExpiryPolicy::Warn`.
    pub expiry_policy: Option<ExpiryPolicy>,
}

pub enum ExpiryPolicy {
    /// Surface a warning badge; no state change. Safe default.
    Warn,
    /// Transition `pending` → `collapsed` with reason "expired (TTL)".
    /// Applies only to non-running molecules.
    Collapse,
    /// Re-tag `temp:cold` and surface a badge. Gentler than Collapse.
    Cool,
}
```

Both fields are `Option<_>` and serde-default-skipped: existing state
files stay byte-identical until a molecule opts in. Legacy JSON without
these keys round-trips untouched.

### CLI verbs

- **`cs touch <id> [--ttl <duration> | --expires-at <iso8601>]`**
  Sets or refreshes `expires_at`. With no flag, clears it (tombstone the
  TTL). Idempotent: same input → same state. Analogous to Unix `touch`
  for freshness, not creation.

- **`cs expire [<id>...]`** — evaluates expiry for the given molecules
  (or the full fleet if no ids) and applies each molecule's
  `expiry_policy`. Pure pull model; caller is a human or `cs patrol`.
  **Idempotent**: running twice produces the same surfaces as once; a
  `Collapse` policy that already collapsed the molecule is a no-op.

### Patrol integration

- **`cs patrol --expire`** — runs `cs expire` across the fleet as one of
  patrol's sweeps. No new process; patrol remains the single external
  scheduler touchpoint (per ADR-016 Propelled regime). When a pilot or
  cron already invokes `cs patrol` periodically, adding `--expire` costs
  nothing architecturally.

### Per-kind defaults (opt-in)

`.cosmon/config.toml` may declare:

```toml
[expiry.defaults.signal]
ttl = "72h"
policy = "cool"

[expiry.defaults.task]
# no ttl by default; individual molecules opt in via `cs touch`
policy = "warn"
```

Defaults are **applied at nucleation** only — they do not retroactively
expire historical molecules. Without configuration, nucleation produces
`expires_at = None` (today's behavior).

### Integration with `temp-review`

The `temp-review` formula learns one new step: **auto-annotate**. When
the formula finds a stale untagged pending and the molecule has an
expired TTL, it chooses the tag dictated by the expiry policy (`Cool` →
`temp:cold`, `Warn` → no tag change, `Collapse` → collapsed by `cs
expire` before the formula sees it). This wires the mechanical half into
the cognitive half without creating a second decision loop.

## Invariants

1. **Running molecules never silently collapse.** If `expires_at` fires
   while a molecule is in an active (non-terminal, non-pending) status,
   `ExpiryPolicy::Collapse` **degrades to `Warn`**. The worker is
   presumed closer to the truth than the TTL; the human resolves the
   conflict. A surface badge makes the conflict visible.

2. **`cs expire` is idempotent.** Twice equals once. This mirrors
   `cs reconcile` and is enforced by tests.

3. **No implicit polymer TTL propagation.** A molecule's TTL does not
   propagate across typed links (`Blocks`, `DecayProduct`, `Refines`,
   `Entangled`). Polymers are explicit; TTL is per-monomer. If a user
   wants chain-level expiry they express it by setting `expires_at` on
   each link, or by writing a formula that does.

4. **TTL is advisory, not a scheduler.** `cs expire` never runs
   autonomously. Patrol invocation is external. This preserves the
   stateless-CLI invariant (ADR-016): cosmon never wakes up on its own.

5. **Status enum is unchanged.** No `expired` status. Expired-ness is a
   derived boolean (`expires_at < now`), not a state.

## Consequences

### Positive

- The curation pair is complete. `temp-review` + TTL together give both
  cognitive and mechanical backlog hygiene.
- Deadline semantics become a first-class, queryable property.
- Surface consumers (`STATUS.md`, cockpit, `cs observe --json`) can
  render an "⏰ expired" badge without cosmon having to invent one.
- Zero migration: every existing molecule file keeps working unchanged.

### Negative

- One new `EventV2::Expired { molecule_id, policy_applied }` variant is
  needed for event-sourced replay. Consumers of the event log must
  handle it; the `#[non_exhaustive]` attribute on `EventV2` means this
  is additive but visible.
- Surface schema gains optional `expires_at` / `expired_at` fields on
  rendered JSON. Downstream tooling that pins schemas will need to bump.
- `cs wait` gains an exit-code contract: **exit 2** when the awaited
  molecule reached a terminal state via expiry (distinct from exit 0 for
  normal completion and exit 1 for stuck/collapsed-by-worker). Pilots
  scripting around `cs wait` should learn the new code.

### Neutral

- Per-kind defaults are opt-in; teams that don't want TTL never encounter
  it. The feature is discoverable through `cs --help` and the ADR.

## Alternatives Considered

1. **Background cron-driven expiry sweeper.** Rejected. Violates the
   stateless-CLI invariant (no daemon, no long-lived process in the
   transactional core — see ADR-016). The resident runtime, when it
   lands, may choose to call `cs expire` on its own clock; that is a
   runtime concern, not a core concern.

2. **New `expired` status in the molecule lifecycle.** Rejected.
   Creates state churn (where does it live — terminal or pending?),
   multiplies the typestate matrix, and conflates a *temporal derived
   predicate* with a *lifecycle transition*. Expiry is a property of
   time, not a decision a worker or human makes.

3. **TTL encoded in tags (e.g. `ttl:72h`).** Rejected. Tags are not
   typed; timestamps would be stringly-typed; no schema for `cs touch`
   to refresh them cleanly; polymer-level queries would need a tag
   parser. The two fields on `MoleculeData` are the right weight.

4. **Propagating TTL through polymers automatically.** Rejected. Inverts
   the explicit-polymer principle; a chain author should say what they
   mean, not inherit time pressure by accident.

## Implementation Notes

Out of scope for this ADR, but the implementation order is predictable:

1. `cosmon-core`: add `expires_at`, `expiry_policy`, `ExpiryPolicy`;
   add `EventV2::Expired`; serde-default-skip both fields.
2. `cosmon-state`: project `expires_at` into the state store; no schema
   migration needed for the JSON backend; SQLite backend gains two
   nullable columns.
3. `cosmon-cli`: `cs touch`, `cs expire`, `cs patrol --expire`,
   `cs wait` exit-code 2.
4. `cosmon-surface`: badge renderer for expired / soon-to-expire.
5. `temp-review` formula: auto-annotate step.
6. Tests: `proptest` for idempotence of `cs expire`; integration test
   for running-molecule degradation invariant.

## References

- an internal chronicle — state sediment.
- `CLAUDE.md` § Molecule Temperature Tags — cognitive curation.
- ADR-016 — stateless-CLI invariant and autonomy regimes.
- ADR-006 (event-sourced ops state) — `EventV2` evolution policy.
