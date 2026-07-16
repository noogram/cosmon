# ADR-040: Fleet-as-Internal-Abstraction

## Status

Accepted (2026-04-16).

## Parent

- Deliberation `delib-20260416-60d1` (10-persona panel: torvalds, tolnay,
  einstein, wheeler, feynman, jobs, shannon, godel, hawking, niel).
- Related ADRs: ADR-038-v0 (fleet composability shipping scope), ADR-039
  (fleet identity).

## Context

The deliberation evaluated a proposal called "fleet-as-default" — the axiom
`∀m ∃f : m ∈ f` (every molecule constitutionally belongs to a fleet). The
panel rejected the axiom 9/10 but converged on a productive alternative:
fleet-as-internal-abstraction.

The rejection was near-unanimous because the axiom:

- **Breaks backward compatibility** (tolnay): every existing galaxy has no
  fleet.toml and would fail. Making fleet.toml mandatory is a semver-major
  break.
- **Violates the composability principle** (einstein, godel, tolnay):
  molecules + formulas are the only extension points. Elevating fleet to a
  required primitive creates a third extension point.
- **Misapplies IFBDD** (feynman, niel, shannon): routing 100% of traffic
  through fresh, lightly-tested fleet code creates outage risk, not
  Darwinian hardening. 47 workers dispatched, zero with fleet_id. Fleet has
  zero production reps.
- **Adds a config file to the happy path** (jobs, niel): the new-user
  experience degrades — `cs init` generates a file they didn't ask for,
  `cs tackle` adds an invisible indirection. Decision-at-setup is strictly
  worse than decision-at-need.

The productive insight came from torvalds and shannon: unify the INTERNAL
code path so `cs tackle` always works with a `FleetSpec`, without changing
the external API or requiring new config files.

## Decision

**`cs tackle` always works with a `FleetSpec`** — either loaded from
`fleet.toml` (if present on disk) or synthesized via
`FleetSpec::default_singleton()` (an infallible, zero-I/O in-memory
constructor). Fleet.toml remains optional.

### Architecture

```
cs tackle <id>
  1. resolve_molecule()
  2. load_fleet_spec()
     a. Try read fleet.toml from .cosmon/ or project root
     b. If found: FleetSpec::parse(contents)
     c. If not found: FleetSpec::default_singleton()  ← in-memory, no I/O
  3. resolve_agent(fleet_spec, molecule)
     (for fleet-of-one: always returns the single agent)
  4. load_formula()
  5. build_prompt(agent_context)
  6. TmuxBackend::spawn()
```

### What this achieves

1. **Zero config change** for existing users. No fleet.toml required. No
   new concepts at `cs init`.
2. **Unified code path** internally. The fleet-of-one and fleet-of-N paths
   share the same dispatch logic, just with different FleetSpec sources.
3. **IFBDD at the code level**. Every tackle exercises FleetSpec resolution,
   hardening the code path, without requiring a config file on disk.
4. **Seamless opt-in**. When the user creates fleet.toml (manually or via
   `cs fleet init`), `cs tackle` reads it automatically. No migration, no
   breaking change.

### Red lines (non-negotiable)

1. **No mandatory config file.** `cs tackle` without fleet.toml produces
   identical behavior to today.
2. **No daemon.** Fleet dispatch stays one-shot, stateless.
3. **No fan-out in tackle.** `cs tackle` = one molecule, one worker. Period.
   Fleet-of-N dispatch is a DAG/formula concern: nucleate N sub-molecules.
4. **No third extension point.** Fleet.toml is configuration (WHO does the
   work), not structure (WHAT is done). Molecules + formulas remain the only
   extension points.
5. **Fleet-of-one path must be infallible.** `default_singleton()` produces
   no parse errors, no file I/O errors, no resolution ambiguity. Zero error
   states for the degenerate case.
6. **Fleet.toml self-modification by workers requires P_external.** Changes
   take effect only after merge-before-dispatch (human witness). Fleet.toml
   is a parameter, not an output — violating this creates self-referential
   regress (godel).

### Composability principle preserved

Fleet.toml configures deployment (WHO), not structure (WHAT). The
composability principle remains intact:

- **Molecules** — the unit of tracked work.
- **Formulas** — the unit of workflow composition.
- **Fleet.toml** — optional configuration, not an extension point.

### Naming

"Fleet" is correct for its current meaning (runtime worker registry). If
dispatch policy grows beyond agent selection, it belongs in `config.toml`
as a `[dispatch]` section — not in a renamed fleet file. Galaxy = genotype,
fleet = phenotype (wheeler).

### IFBDD reframe

IFBDD at the **code level** (always have FleetSpec in hand) is sound and
low-risk — every invocation exercises the resolution logic.

IFBDD at the **config level** (mandatory fleet.toml) is premature — zero
production reps on fleet dispatch code, maximum blast radius. The Darwinian
pressure argument requires survival; routing 100% of traffic through
untested code kills the organism before it can adapt.

## Rejected Alternative

**Fleet-as-default** (mandatory fleet.toml, axiom `∀m ∃f : m ∈ f`).

Rejected because:
- Semver-major break for every existing galaxy (tolnay).
- Violates composability principle — introduces a third extension point.
- IFBDD misapplied to untested code at maximum blast radius (feynman).
- Adds cognitive overhead to the happy path (jobs, niel).

## Coherence Checklist

1. **Stateless?** ✅ — `default_singleton()` is a pure constructor.
   `load_fleet_spec()` is one-shot file read or in-memory synthesis.
2. **Idempotent?** ✅ — same fleet.toml (or absence) produces same FleetSpec.
3. **Regime-aware?** ✅ — affects Propelled regime only (tackle dispatch).
   Inert and Autonomous regimes unaffected.
4. **Single perimeter?** ✅ — fleet resolution is internal to tackle, not a
   new command.
5. **Symmetric undo?** N/A — internal refactor, no new user-facing state.
6. **Runtime-compatible?** ✅ — the resident runtime (ADR-016 Phase 3+) will
   use the same `load_fleet_spec()` path.
7. **Worker/human boundary?** ✅ — workers never modify fleet.toml. P_external
   enforced via merge-before-dispatch.
8. **Write-read asymmetry?** ✅ — `load_fleet_spec()` is pure read.
9. **Merge-before-dispatch?** ✅ — fleet.toml changes visible to dependents
   only after merge.
10. **CLI-first for workers?** ✅ — workers use `cs` CLI; fleet resolution is
    transparent to them.

## Consequences

### Benefits

- Eliminates the wasted `stat()` on fleet.toml in `try_inject_fleet_briefing()`
  (torvalds: the fleet-of-one path already pays a syscall; unify it).
- Every `cs tackle` exercises FleetSpec resolution, hardening the code path
  through genuine IFBDD at the code level.
- Seamless upgrade path: drop a fleet.toml and the system uses it. No
  migration, no flag changes.
- Molecules already live under `fleets/default/` in the state store (shannon) —
  the internal concept already exists; this ADR makes the code match.

### Costs

- Minor internal refactor of `cs tackle` to thread FleetSpec through dispatch.
- `default_singleton()` must be maintained as an infallible constructor —
  any change to FleetSpec that requires new mandatory fields must preserve
  the degenerate case.

### Risks

- If `default_singleton()` grows complex or acquires I/O, the infallibility
  guarantee breaks. Guard with a unit test: `FleetSpec::default_singleton()`
  always succeeds, produces exactly one agent, and performs zero I/O.

## References

- Parent deliberation: `delib-20260416-60d1`
- Synthesis: `.cosmon/state/fleets/default/molecules/delib-20260416-60d1/synthesis.md`
- Related ADRs: ADR-038-v0 (fleet composability v0), ADR-039 (fleet identity),
  ADR-016 (autonomy regimes)
