# ADR-038-v0: Fleet Composability — Shipping Scope

## Status

Accepted (2026-04-15). v0 implementation landed in `task-20260415-2107`.

Supersedes ADR-039 "Fleet Composability" as the **implementation** ADR —
that document remains the design rationale; this one documents exactly
what shipped and the boundaries around it.

## Parent

- Deliberation `delib-20260414-778e` (9-persona panel) produced the design.
- Previous implementation attempt `task-20260414-2af4` collapsed with
  `reason = "frozen pending cs doctor"`. Freeze lifted once `cs doctor`
  landed in `task-20260414-c0d5`.

## Decision

Ship fleet composability in one narrow, reversible slice:

### Schema

```toml
[fleet]
schema_version = 1          # required; only `1` is accepted in v0
id             = "master"   # required; pattern [a-z0-9][a-z0-9-]*

[[fleet.include]]           # inline-table array (not plain strings)
source = "file:./fleets/wiki.toml"
as     = "wiki"             # optional; applies prefix on collision
```

- `schema_version` is **required** on every new-form fleet (tolnay — semver
  survival).
- `id` is **required** (wheeler — fleet identity as ontological primitive).
- `[[fleet.include]]` **must** be inline-table form. Plain-string includes
  (`include = ["./wiki.toml"]`) would need a wire-format break to grow the
  `as` field later.
- The resolver in v0 accepts only `source = "file:..."`. Other URI schemes
  (`git+https:`, `cas:sha256-...`) parse-but-error at resolve time so
  authors can declare them today and light them up when ADR-035 ships.

### Backward compatibility

Legacy monolithic `fleet.toml` (the pre-composability form that looks like
`fleet = "example"` + `[[agents]]` at the root) **parses byte-for-byte
unchanged**. `schema_version` defaults to `1`, `id` is taken from
`fleet = "..."`, and `includes` is empty.

A legacy file cannot declare `[[fleet.include]]` — TOML disallows mixing a
scalar `fleet = "..."` with `fleet.include`. This is a feature, not a bug:
old files cannot grow new semantics by accident.

### Composition semantics

`compose: [Fleet] → Fleet` is **pure, total, deterministic, run at load**
(einstein — frame-equivalence with flat fleet). The runtime never sees a
composed fleet; it only sees the flat output of `compose`.

- Every agent from every child is merged into the composite.
- An `as = "wiki"` on an include rewrites every agent name from that child
  as `"wiki:<name>"`. Channels are rewritten to match.
- Every agent carries `origin_fleet_id = Some(child_fleet.id)` on the
  flattened output (einstein — provenance as observability bit preserved
  across the symmetry).
- **Duplicate agent ids hard-fail**. The error names both fleet ids, both
  source paths, line numbers (best-effort), and suggests `as = "..."` as
  the fix. No silent namespacing.
- Constitution and grades from children are dropped in v0 — the master's
  constitution wins (no user-facing merge policy knob, per the unanimous
  decision to ship ONE well-defined behavior).
- **Transitive includes are rejected** — godel's decidability argument.
  A child cannot itself declare `[[fleet.include]]`. The error is loud.

### Command

One new surface area: `cs fleet resolve [--json] [path]`.

- Prints the flat fleet (composite `id`, agent list with roles / clearances
  / `origin_fleet_id`).
- `--json` emits a single-line NDJSON for scripting.
- Walk-up discovery of `fleet.toml` when no path is provided.
- No other CLI verb changes in v0. `cs deploy`, `cs tackle`, and friends
  continue to read the master file directly; they do not call the resolver
  yet. That wiring will land separately once the resolver has been
  field-tested.

### What is NOT in v0 (explicit defer list)

- `cs link` — deferred. Jobs' "replace with intent-carrying formulas"
  argument wins: prove the use case first.
- Non-`file:` URI schemes at resolve time (parse-time acceptance remains).
- User-facing merge policy variants (`namespaced` / `flat-union` /
  `master-only`). v0 is flat-union with hard-fail-on-collision.
- Generated / template-expanded / wildcard `include` entries.
- Transitive `include` (child fleet with its own `[[fleet.include]]`).
- Socket multi-tenancy: **ONE tmux socket per composite** (einstein — a
  composed fleet is ONE state store). Enforced by existing socket
  derivation; no code change needed.

## Implementation map

| Artifact | Role |
|----------|------|
| `crates/cosmon-core/src/fleet.rs` | `FleetSpec::parse` accepts both forms; `FleetSpec::compose` is the pure composition function; `FleetInclude` + `find_agent_line_tag` are the public shapes consumed by the resolver. |
| `crates/cosmon-cli/src/cmd/fleet.rs` | `cs fleet resolve` subcommand + the filesystem-side resolver that walks `file:` includes and renders the flat fleet as text or JSON. |
| `crates/cosmon-cli/tests/fleet_resolve.rs` | Scenario-1 acceptance test (3 files → 4 agents) + negative paths (duplicate, unsupported scheme, transitive, missing file, monolithic back-compat). |

## Acceptance

The deliberation's 1-minute success criterion is encoded as an integration
test:

```text
scenario_1_wiki_dev_blob_resolves_to_four_agents
```

The test writes three child fleet files and one master, invokes
`cs fleet resolve --json`, and asserts exactly four agents with preserved
`origin_fleet_id` metadata. CI runs this on every PR.

## Constitution drift guard

Hawking flagged the "stealth constitutional amendment" risk: if a master's
child fleet is edited mid-flight, live workers would inherit new rules
they never agreed to. This ADR adopts the v0 mitigation:

- Constitution hash-pinning at molecule nucleation is the responsibility
  of the nucleation path, not the resolver. Added to the v0 scope as a
  follow-up task: record the resolved composite's hash on the molecule at
  nucleation time; running workers read constitution from that frozen
  snapshot, not from the live `fleet.toml`.
- The resolver is already pure and deterministic, so the composite's
  hash is well-defined given a master + child file set.

## Coherence checklist

1. Stateless? ✅ — `cs fleet resolve` is one-shot.
2. Idempotent? ✅ — same inputs, same output; no disk writes.
3. Regime-aware? ✅ — v0 affects only Inert-regime configuration parsing.
4. Single perimeter? ✅ — new verb `cs fleet resolve`; no overlap with
   existing verbs.
5. Symmetric undo? N/A — pure query.
6. Runtime-compatible? ✅ — composition happens at load time; the runtime
   sees a flat fleet (einstein).
7. Worker/human boundary? N/A — query, no state change.
8. Write-read asymmetry? ✅ — reads only.
9. Merge-before-dispatch? N/A — composition is pre-dispatch.
10. CLI-first for workers? ✅ — `cs fleet resolve` is a CLI verb; no MCP
    tool needed.

## Related

- ADR-035 (cross-galaxy edges) — same identity machinery; different compose
  semantics on the state layer.
- ADR-039 (fleet composability — design) — superseded by this ADR for the
  shipping scope.
- ADR-039-identity (this PR) — the sibling ADR defining `FleetId`.
