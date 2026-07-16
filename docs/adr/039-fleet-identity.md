# ADR-039-identity: Fleet Identity — Minimal Form

## Status

Accepted (2026-04-15). v0 implementation landed in `task-20260415-2107`.

## Parent

- Wheeler's audit in `delib-20260414-778e`: fleets are the one primitive
  without identity. Molecules have `MoleculeId`, formulas have versioned
  TOML, universes (ADR-035) get content-addressed ids. Fleets are the
  unnamed primitive.

## Context

Retrofitting identity onto an already-shipped `fleet.toml` format is
painful. Every event, audit log, and cross-reference that was written
without it becomes ambiguous. Two lines of TOML today (`schema_version`,
`id`) are cheap insurance.

## Decision

**Every `fleet.toml` declares `fleet.id`** — a human-chosen string matching
`[a-z0-9][a-z0-9-]*`. This is required in v0 for any file using the
`[fleet]` block (i.e. any file that opts into composability). Legacy
monolithic files continue to use `fleet = "name"`, and the name IS the id
— no migration required, no friction for existing users.

### What this ADR does NOT add

- No separate publish step.
- No registry.
- No `cs fleet publish` command.
- No content-addressed `FleetId` type at the Rust level (the string is
  already enough in v0).

### What it leaves a seat for

- A content-addressed `FleetId = hash(resolved_composition)` can be
  computed on demand once the resolver is stable. Every flattened agent
  already carries `origin_fleet_id: Option<String>` (see ADR-038-v0),
  so the wiring exists.
- ADR-035 (cross-galaxy edges) needs the same identity machinery.
  Keeping the shape identical (`String` now, hash later) means both
  features can converge on one implementation without a naming fight.

## Implementation map

| Artifact | Role |
|----------|------|
| `FleetSpec.name` | String, set from `fleet = "..."` (legacy) or `[fleet].id` (new form). |
| `FleetSpec.schema_version` | u32, always 1 in v0. Lets future versions evolve the composable form without breaking old files. |
| `FleetAgentSpec.origin_fleet_id` | `Option<String>`, set by `FleetSpec::compose` on every flattened agent. Preserves provenance across the frame-equivalence flattening (einstein). |
| `validate_fleet_id` in `fleet.rs` | Private helper enforcing the `[a-z0-9][a-z0-9-]*` grammar on `fleet.id` and on `as` prefixes in `[[fleet.include]]`. |

## Why `[a-z0-9][a-z0-9-]*`?

- Lowercase-only: no case-folding surprises between filesystems.
- Digits + dash: conventional DNS-label shape, familiar to operators.
- First char must be letter or digit: prevents `-foo` style collisions
  with CLI flag parsing if anything downstream ever treats a fleet id as
  an argument.

The same grammar applies to `as = "wiki"` prefixes in
`[[fleet.include]]`, keeping the address space uniform.

## Validation site

Enforced exactly once, at parse time, by `FleetSpec::parse`. Malformed ids
fail loudly with `FleetSpecError::MalformedFleetId(...)`. The CLI surfaces
the error verbatim.

## What about `fleet = "..."` in legacy files?

Legacy files do not go through `validate_fleet_id`. This is a deliberate
courtesy: every existing `fleet.toml` that happens to use a name like
`"Wiki Editor Fleet"` still parses. Authors who want strict validation
opt into it by migrating to the `[fleet]` block, at which point the
grammar applies.

## Coherence checklist

1. Stateless? ✅ — pure parse-time validation.
2. Idempotent? ✅ — same string, same result.
3. Regime-aware? ✅ — affects only the parse layer.
4. Single perimeter? ✅ — one validator, one error variant.
5. Symmetric undo? N/A.
6. Runtime-compatible? ✅ — runtime treats `name` as opaque string; no
   downstream code needs to change.
7. Worker/human boundary? N/A — parse-time.
8. Write-read asymmetry? ✅ — parse only.
9. Merge-before-dispatch? N/A.
10. CLI-first for workers? ✅ — the id appears in `cs fleet resolve`
    output; no MCP tool required.

## Related

- ADR-038-v0: Fleet Composability — Shipping Scope (sibling ADR).
- ADR-035: Cross-galaxy edges — will share the same identity grammar.
- ADR-039 "Fleet Composability" (design): this sibling ADR carves off
  the identity concern into its own file, per wheeler's request for a
  standalone identity document.
