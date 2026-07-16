# ADR-008: Three-Layer Feature Gating

## Status
Proposed

## Context

Cosmon needs a feature gating system that controls which capabilities are
available to agents at different stages of the lifecycle. The inspiration
is Claude Code's three-tier model:

| Layer | Claude Code analogue | When decided | Cosmon equivalent |
|-------|---------------------|--------------|-------------------|
| 1. Compile-time | `bun:bundle` includes/excludes modules | Cargo build | Cargo `cfg(feature = "...")` |
| 2. Runtime identity | `USER_TYPE` (free / pro / team) | Agent spawn | `Clearance` + `Capability` |
| 3. Dynamic config | GrowthBook feature flags | Any time | `FeatureFlag` config map |

Without this design, feature availability is ad-hoc: some things are gated
by Cargo features, some by clearance checks scattered through code, and
dynamic flags don't exist at all. A unified model makes the gating decision
explicit and auditable.

### Why three layers?

Each layer answers a different question:

1. **Cargo features (compile-time):** "Is this code even compiled into the binary?"
   — Controls binary size, dependency tree, and attack surface. Example: the
   `integration` feature in `cosmon-transport` already gates integration tests.
   New examples: `mcp` feature for MCP server support, `dashboard` for the
   thermodynamic dashboard.

2. **Clearance + Capability (runtime, per-agent):** "Is this agent authorized
   to use this capability?" — Already partially exists via `Clearance`
   (Read/Write/Execute). Extended with named `Capability` grants that are
   more granular than the three clearance levels. Example: an agent with
   `Write` clearance might or might not have the `spawn_subagent` capability.

3. **Feature flags (dynamic config):** "Is this feature currently enabled in
   this deployment?" — Read from config at startup, reloadable without restart.
   Example: `experimental_convoy_routing = true` enables a new dispatch
   algorithm for testing before it becomes the default.

### Design principles

- **Layer 1 is the coarsest gate.** If code isn't compiled in, layers 2 and 3
  don't matter. This is the only layer that affects binary size.
- **Layer 2 is per-agent.** Different agents in the same fleet can have different
  capabilities. This is the security boundary.
- **Layer 3 is per-deployment.** All agents in a deployment see the same flags.
  This is the experimentation/rollout boundary.
- **A feature must pass all applicable layers.** If any layer says no, the
  feature is unavailable. The layers compose as AND, not OR.

## Decision

### Layer 1: Cargo Features

Define features in crate `Cargo.toml` files. Convention:

```toml
[features]
default = []
mcp = []              # MCP server/client support
dashboard = []        # Thermodynamic dashboard endpoints
integration = []      # Integration tests requiring external services
```

Code uses `#[cfg(feature = "...")]` to conditionally compile modules.
The `cosmon-core` crate MUST NOT have Cargo features — it is pure domain
types. Features belong in the outer crates (`cosmon-cli`, `cosmon-transport`,
`cosmon-mcp`, etc.).

### Layer 2: Capability (runtime, per-agent)

Extend the existing `Clearance` system with named capabilities.

```rust
/// A named capability that an agent may or may not possess.
///
/// Capabilities are more granular than `Clearance` levels. An agent's
/// effective permissions are: clearance level AND granted capabilities.
pub enum Capability {
    SpawnSubagent,
    ManageFleet,
    AccessMcp,
    ModifyFormula,
    Patrol,
}
```

`AgentDefinition` gains an optional `capabilities: BTreeSet<Capability>` field.
The capability check is: `agent.clearance >= required_clearance AND
required_capabilities ⊆ agent.capabilities`.

### Layer 3: Feature Flags (dynamic config)

A `FeatureFlags` struct loaded from TOML config at startup, queryable at
runtime.

```rust
/// Runtime feature flags loaded from deployment config.
///
/// All agents in a deployment share the same flags. Flags control
/// experimental features, gradual rollouts, and operational toggles.
pub struct FeatureFlags {
    flags: BTreeMap<String, bool>,
}
```

Convention for flag names: `snake_case`, prefixed by subsystem
(e.g., `dispatch_convoy_routing`, `mcp_bidirectional`, `patrol_auto_restart`).

### Gate Check API

A single entry point for checking all three layers:

```rust
/// Check whether a feature is available given all three gating layers.
///
/// Returns `Ok(())` if the feature passes all gates, or `Err` with
/// the specific gate that denied access.
pub fn check_gate(
    agent: &AgentDefinition,
    required_clearance: Clearance,
    required_capabilities: &[Capability],
    flag_name: Option<&str>,
    flags: &FeatureFlags,
) -> Result<(), GateDenied>;
```

Layer 1 (compile-time) is not checked at runtime — it's enforced by the
compiler. The `check_gate` function handles layers 2 and 3.

## Consequences

- **Positive:** Feature availability is explicit and auditable. The three
  layers map cleanly to different concerns (binary composition, agent
  identity, deployment configuration).
- **Positive:** Existing `Clearance` system is preserved and extended, not
  replaced. No breaking changes to `AgentDefinition` serialization (new
  field is optional with empty default).
- **Negative:** Adds a new concept (`Capability`) that overlaps with
  `Clearance`. The distinction (coarse permission level vs. named grant)
  must be documented clearly.
- **Negative:** Feature flag names are stringly-typed. Typos in flag names
  fail silently (flag not found = disabled). Mitigation: provide a
  `FeatureFlags::known_flags()` method that lists expected flags, and
  warn on unknown flags at startup.

## References

- Claude Code feature gating: `bun:bundle` → `USER_TYPE` → GrowthBook
- Existing Cosmon clearance: `crates/cosmon-core/src/clearance.rs`
- THESIS.md Part III (zero-I/O core, trait-first design)
