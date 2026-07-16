# ADR-009: Governance Tiers for Bounded Contexts

## Status
Proposed

## Context

Gas Town manages multiple bounded contexts (rigs) with very different risk
profiles and contribution patterns. A production-critical multi-contributor
project like `cosmon` needs strict review, CI gates, and merge queues. A
single-agent scratch experiment needs none of that ceremony.

Without explicit governance tiers, each rig either inherits a one-size-fits-all
policy (too strict for experiments, too loose for production) or configures
ad-hoc settings that are hard to audit and compare.

### Requirements

1. **Declarative** — tier assignment is configuration, not code.
2. **Per-context** — each bounded context gets its own tier.
3. **Overridable** — individual policies can be adjusted without changing tier.
4. **Auditable** — a single config file shows every context's governance posture.

## Decision

Define five governance tiers, ordered by ceremony level:

| Tier | Branch Strategy | Review | CI Gates | Merge Policy | Main Protected | Use Case |
|------|----------------|--------|----------|-------------|----------------|----------|
| **Full** | Feature branches | Required | Required | Merge queue | Yes | Production-critical, multi-contributor |
| **Light** | Feature branches | Optional | Required | Direct merge | Yes | Important but velocity-sensitive |
| **GuardedMain** | Feature branches | Optional | Required | Direct merge | Yes | Bot/agent-heavy repos |
| **Micro** | Direct to main | None | Optional | Direct push | No | Experiments, single-contributor |
| **AppendOnly** | Direct to main | None | Optional | Direct push | Yes (no force-push) | Audit logs, config stores |

### Configuration format (TOML)

```toml
default_tier = "light"

[contexts.cosmon]
tier = "full"

[contexts.beads]
tier = "guarded_main"
override_review_required = true

[contexts.scratch]
tier = "micro"

[contexts.audit-log]
tier = "append_only"

[contexts.gastown]
tier = "light"
override_ci_gates = false
```

### Implementation

- `GovernanceTier` enum in `cosmon-core` with methods for each policy dimension.
- `GovernanceConfig` struct with TOML serde support and per-context overrides.
- Configuration loaded at startup; the witness and refinery consult it for
  merge decisions.

### Design choices

**Why five tiers, not a continuous spectrum?** Named tiers are easier to reason
about and communicate. "This repo is Full governance" is more meaningful than
"review=true, ci=true, merge_queue=true, feature_branches=true, ...". Overrides
handle the edge cases.

**Why is GuardedMain separate from Light?** Light assumes human contributors
who follow branch naming conventions. GuardedMain is for repos where automated
agents are the primary contributors — the main protection matters, but branch
discipline is relaxed because agents are programmatic.

**Why does AppendOnly protect main?** Append-only semantics require that
existing content is immutable. Protecting main from force-pushes enforces this
at the git level.

## Consequences

- **Positive:** Governance posture is explicit, auditable, and declarative.
- **Positive:** The refinery and witness can enforce merge policies automatically
  based on the config.
- **Positive:** New rigs get sensible defaults without manual configuration.
- **Negative:** Five tiers may not cover every scenario. Overrides mitigate this
  but add complexity.
- **Negative:** The TOML config must be kept in sync with actual branch
  protection rules in the git host (GitHub, etc.).

## References

- ADR-008: Three-Layer Feature Gating (related gating mechanism)
- THESIS.md Part III: Architecture principles
