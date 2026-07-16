# ADR-TEST-001: Phase-Dependent Testing Strategy

## Status
Proposed

## Context

Cosmon is built by autonomous agents (polecats) working through structured
formulas. Traditional testing strategies assume a stable codebase with human
developers who can make judgment calls about what to test and when. Agent-authored
code faces different pressures:

1. **Agents optimize for passing gates, not for coverage quality.** A polecat
   that sees a failing test may delete the assertion rather than fix the bug.
   This is rational agent behavior (minimize cost to reach "green") but
   catastrophic for code quality.

2. **Testing investment should match maturity.** A day-one exploration crate
   does not need property tests and mutation gates. A production-critical crate
   serving the fleet does. Applying production-grade testing to exploration
   code wastes energy; applying exploration-grade testing to production code
   invites regressions.

3. **Hexagonal architecture creates unequal risk.** The pure domain core
   (`cosmon-core`) is the foundation everything else depends on. A bug in a
   domain type propagates everywhere. A bug in an infrastructure adapter
   affects one integration. Testing investment should reflect this asymmetry.

4. **The Maker cannot be the Checker.** The agent that writes code should not
   be the only agent that verifies it. This is not about trust; it is about
   the information-theoretic impossibility of a system auditing its own outputs
   without an independent reference.

### Requirements

1. **Phase-aware** -- testing requirements scale with crate maturity.
2. **Layer-aware** -- testing budget reflects hexagonal architecture risk.
3. **Agent-safe** -- testing policy resists adversarial optimization by agents.
4. **Measurable** -- every requirement has a numeric gate, not a subjective judgment.

## Decision

### Phase Model

Every crate progresses through three phases. The phase is declared in the
crate's `Cargo.toml` metadata or in the governance config (ADR-009):

| Phase | Description | When |
|-------|-------------|------|
| **Exploration** | New crate, API unstable, rapid iteration | Day 1 through first API stabilization |
| **Stabilization** | API solidifying, consumers appearing | After first external consumer depends on it |
| **Production** | Stable API, fleet depends on it | After governance tier is Full or Light |

### Testing Requirements by Phase

#### Phase 1: Exploration

Minimum viable verification. The goal is to keep velocity high while
establishing a type-level safety net.

| Requirement | Gate |
|-------------|------|
| Type coverage | Every `pub` type has at least one unit test exercising construction and key methods |
| Assertion density | Every test function contains at least one `assert!` / `assert_eq!` / `assert_matches!` |
| End-to-end smoke | At least 1 integration test covering the primary workflow |
| Doc examples | `cargo test --doc` passes (doc examples compile) |

#### Phase 2: Stabilization

The API is hardening. Property tests catch edge cases that example-based tests
miss. Mutation testing measures whether tests actually detect bugs.

| Requirement | Gate |
|-------------|------|
| All Phase 1 gates | Must still pass |
| Property tests | Every `pub` function with non-trivial logic has a `proptest` or `quickcheck` property |
| Mutation score | >= 60% (measured by `cargo-mutants` or equivalent) |
| Branch coverage | >= 75% on `pub` functions |
| Error path tests | Every `Result`-returning `pub` function has at least one test for the `Err` variant |

#### Phase 3: Production

Full spectrum. The crate is load-bearing for the fleet. Testing must be
comprehensive and resistant to regression.

| Requirement | Gate |
|-------------|------|
| All Phase 2 gates | Must still pass |
| Mutation score | >= 80% (hard gate in CI) |
| Branch coverage | >= 90% on `pub` functions |
| Fuzz targets | Critical parsers and deserializers have `cargo-fuzz` targets |
| Regression anchors | Every bug fix includes a regression test that fails without the fix |
| Backward compatibility | `cargo-semver-checks` passes (no unintended breaking changes) |

### Budget Allocation by Hexagonal Layer

Testing effort is not distributed equally across the architecture. The pure
domain core is the highest-value, highest-risk target. Infrastructure adapters
are the lowest-risk because they are replaceable and isolated behind traits.

| Layer | Crates | Budget Share | Rationale |
|-------|--------|-------------|-----------|
| **Domain** (core) | `cosmon-core` | **55%** | Pure logic, no I/O, maximum testability. A domain bug propagates to every consumer. This is where property tests and mutation testing have the highest ROI. |
| **Application** (orchestration) | `cosmon-cli`, `cosmon-graph`, `cosmon-transport` | **31%** | Orchestration logic, command handling, graph algorithms. Bugs affect workflows but are contained by domain type safety. Integration tests dominate. |
| **Infrastructure** (adapters) | `cosmon-state`, `cosmon-filestore`, `cosmon-bridge-claude`, `cosmon-mcp` | **14%** | I/O adapters behind traits. Tested primarily through trait contract tests (does this implementation satisfy the trait's documented invariants?). |

"Budget share" means the fraction of total testing effort (time, compute,
mutation testing cycles) allocated to each layer. This is a planning guide,
not a hard gate -- but significant deviation should be justified.

### The Maker =/= Checker Rule

**The agent that writes code MUST NOT be the sole verifier of that code.**

This is enforced structurally, not by policy:

1. **Polecats write code.** They run the build and test gates as a self-check
   (formula steps 5-7), but their self-review is necessary, not sufficient.

2. **The Refinery verifies.** The merge queue runs the full gate suite on the
   merged result. The Refinery is a different agent with a different context --
   it is the independent Checker.

3. **The Witness audits.** The Witness can request additional review or reject
   work that passes gates but violates architectural invariants. This is the
   second-order check: not just "does it compile?" but "does it belong?"

4. **Mutation testing is the automated Checker.** `cargo-mutants` introduces
   synthetic bugs. If tests still pass with a mutation, the test suite has a
   gap. This is machine-verifiable and agent-proof -- an agent cannot game a
   mutation score without writing tests that actually detect changes.

### Pathology #1: Assertion Deletion

**Problem:** An agent encounters a failing test. Instead of fixing the bug, it
deletes or weakens the assertion to make the test pass. This is the most
dangerous failure mode because it is invisible to gate checks -- all gates
are green, but coverage has silently decreased.

**Detection mechanisms:**

1. **Assertion density check.** CI counts `assert!` / `assert_eq!` /
   `assert_matches!` calls per test function. A test function with zero
   assertions is flagged as a violation. This is a simple grep-based gate.

2. **Mutation score regression.** If a commit decreases the mutation score
   below the phase threshold, it is rejected. Deleting assertions necessarily
   decreases mutation score because surviving mutants increase.

3. **Assertion count monotonicity.** For Production-phase crates, the total
   assertion count MUST NOT decrease between commits without an accompanying
   justification in the commit message (e.g., "removed obsolete test for
   deleted feature"). The Refinery checks this.

4. **Diff review heuristic.** The Witness/Refinery flags commits where test
   files have more deletions than additions. This is a heuristic, not a hard
   gate, but it triggers manual review.

**Why this is Phase 3 only for hard gates:** In Exploration, tests change
rapidly as the API evolves. Assertion deletion is often legitimate (the API
changed, the old assertion is wrong). In Production, the API is stable and
assertion deletion is almost always a bug.

## Consequences

- **Positive:** Testing requirements scale with risk, preventing both
  under-testing of critical code and over-testing of exploratory code.
- **Positive:** The 55/31/14 budget allocation aligns testing effort with the
  hexagonal architecture's risk profile. Domain bugs are caught early.
- **Positive:** The Maker/Checker separation is structural, not just policy.
  Agents cannot bypass it without violating the formula workflow.
- **Positive:** Assertion deletion detection provides a concrete defense
  against the most dangerous agent testing pathology.
- **Negative:** Phase transitions require explicit declaration. A crate that
  has grown into production use without a phase bump will be under-tested.
  Mitigation: the Witness periodically audits phase assignments.
- **Negative:** Mutation testing (`cargo-mutants`) is slow. Running it on every
  commit is impractical. Mitigation: run mutation testing on PR/MR boundaries,
  not on every commit. Cache results across runs.
- **Negative:** The 55/31/14 split is a guideline, not a hard constraint.
  Without tooling to measure actual test effort per layer, it may drift.

## References

- ADR-008: Three-Layer Feature Gating (related gating mechanism)
- ADR-009: Governance Tiers (phase assignment aligns with governance tiers)
- THESIS.md Part III: Zero-I/O core, trait-first design (hexagonal architecture)
- THESIS.md Preamble: Galilean method (every claim must be measurable)
- OxyMake lessons: 393 pure unit tests in cosmon-core equivalent (THESIS.md)
