# ADR-COS-002: Entropy as Computable Observable

## Status
Proposed

## Context

Cosmon's physics vocabulary (THESIS.md Parts V, XI) uses entropy, temperature,
and free energy as conceptual tools. But they remain metaphors: `Fleet::temperature()`
is a ratio of active-to-total workers, not a thermodynamic quantity derived from
information content. `EnergyReport::free_energy_ratio()` is productive-tokens /
total-tokens — useful accounting, but not connected to the information-theoretic
entropy that gives the physics vocabulary its precision.

The gap matters operationally. An orchestrator deciding whether to dispatch a new
molecule needs to know: *how much of the system's state is uncertain?* A patrol
cycle diagnosing a stalled fleet needs to know: *where is information being lost?*
These questions have precise answers in information theory, and those answers are
computable from data the system already collects.

This ADR proposes making entropy a first-class computable observable — not a
metaphor, but a measured quantity with types, formulas, and operational semantics.

## Decision

### Four entropy metrics

Entropy in Cosmon is not a single number. It decomposes into four measurable
channels, each capturing a distinct source of uncertainty:

| Metric | What it measures | Source data | Unit |
|--------|-----------------|-------------|------|
| **Message entropy** | Uncertainty in inter-agent communication | Message payload sizes, channel utilization | bits |
| **Code entropy** | Compressibility of generated artifacts | Diff sizes, compression ratios of committed code | bits |
| **Context window entropy** | Information density of agent context | Token counts, context utilization ratios | bits |
| **State entropy** | Uncertainty in fleet-wide state | Distribution of worker statuses, molecule step states | bits |

Each metric is independently computable and independently useful. Together they
form the **thermodynamic state** of the system.

#### Message entropy

Shannon entropy of the message stream between agents. Measures how much
information is actually flowing through the communication channels versus noise
and redundancy.

```
H_message = -Σ p(m) · log₂(p(m))
```

where `p(m)` is the probability of message type `m` in the recent window. High
message entropy means diverse, information-rich communication. Low message entropy
means repetitive or formulaic exchanges (a coordination smell).

**Observable:** computed from `Message` variants and payload sizes over a sliding
window.

#### Code entropy

The compressibility of code artifacts produced by agents. Measures how much
genuine information content exists in generated code versus boilerplate and
repetition.

```
H_code = 1.0 - (compressed_size / raw_size)
```

This is the complement of the compression ratio. Code with high entropy (low
compressibility) carries dense information. Code with low entropy (high
compressibility) is repetitive — potentially indicating copy-paste patterns or
boilerplate generation.

**Observable:** computed from `git diff --stat` output and gzip compression of
committed diffs.

#### Context window entropy

Information density of an agent's context window. Measures how efficiently the
finite context budget is used.

```
H_context = tokens_informative / tokens_total
```

where `tokens_informative` estimates the non-redundant token count (via
compression or deduplication analysis). An agent whose context is 80% prime
injection and 20% new work has low context entropy — most of its budget is spent
reconstructing known state (Landauer cost).

**Observable:** computed from `EnergyRecord` data and prime injection sizes.

#### State entropy

Shannon entropy of the fleet's state distribution. Measures how spread out
workers and molecules are across their possible states.

```
H_state = -Σ p(s) · log₂(p(s))
```

where `p(s)` is the fraction of workers in state `s`. A fleet where all workers
are active has zero state entropy (fully determined). A fleet with workers evenly
distributed across Starting, Active, Stalled, and Stopped has maximum state
entropy (maximum uncertainty about what the fleet is doing).

**Observable:** computed from `Fleet::workers` status distribution.

### Types

Four new types in `cosmon-core/src/entropy.rs`:

```rust
/// A measured entropy value in bits. Non-negative.
///
/// Wraps `f64` with a non-negativity invariant. Entropy cannot be negative
/// in information theory; this type enforces that at construction.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entropy(f64);

impl Entropy {
    /// Create a new entropy value, clamping negative inputs to zero.
    pub fn new(bits: f64) -> Self {
        Self(bits.max(0.0))
    }

    /// Zero entropy — complete certainty.
    pub const ZERO: Self = Self(0.0);

    /// The entropy value in bits.
    pub fn bits(self) -> f64 {
        self.0
    }
}
```

```rust
/// The ratio of compressed size to raw size. Clamped to [0.0, 1.0].
///
/// A compression ratio of 0.3 means the data compresses to 30% of its
/// original size — high redundancy. A ratio of 0.95 means nearly
/// incompressible — high information density.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompressionRatio(f64);

impl CompressionRatio {
    /// Create a new compression ratio, clamping to [0.0, 1.0].
    pub fn new(ratio: f64) -> Self {
        Self(ratio.clamp(0.0, 1.0))
    }

    /// The ratio value (0.0 = perfectly compressible, 1.0 = incompressible).
    pub fn get(self) -> f64 {
        self.0
    }

    /// Convert to an entropy estimate: 1.0 - ratio.
    /// High compression ratio → low entropy (redundant data).
    /// Low compression ratio → high entropy (dense information).
    pub fn to_entropy(self) -> Entropy {
        Entropy::new(1.0 - self.0)
    }
}
```

```rust
/// The thermodynamic state of the system at a point in time.
///
/// Combines the four entropy channels with temperature and free energy
/// into a single snapshot. This is the "equation of state" for the fleet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermodynamicState {
    /// When this state was measured.
    pub timestamp: DateTime<Utc>,
    /// Shannon entropy of the message stream.
    pub message_entropy: Entropy,
    /// Information density of generated code.
    pub code_entropy: Entropy,
    /// Context window utilization entropy.
    pub context_entropy: Entropy,
    /// Fleet state distribution entropy.
    pub state_entropy: Entropy,
    /// System temperature (from Fleet::temperature()).
    pub temperature: Temperature,
    /// Helmholtz free energy (see below).
    pub free_energy: HelmholtzFreeEnergy,
}
```

```rust
/// Thermodynamic analysis of a single agent (worker).
///
/// Per-agent decomposition of the system thermodynamics. Enables
/// identifying which agents are entropy sources (increasing system
/// uncertainty) vs. entropy sinks (reducing it through productive work).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentThermodynamics {
    /// The worker being analyzed.
    pub worker: WorkerId,
    /// Entropy contributed by this agent's communication.
    pub message_entropy: Entropy,
    /// Compression ratio of this agent's code output.
    pub code_compression: CompressionRatio,
    /// Context window utilization (0.0 = empty, 1.0 = full).
    pub context_utilization: f64,
    /// Tokens consumed by this agent.
    pub energy_consumed: TokenCount,
    /// This agent's Carnot efficiency (see below).
    pub carnot_efficiency: CarnotEfficiency,
}
```

### Carnot efficiency

In thermodynamics, the Carnot efficiency is the theoretical maximum efficiency of
a heat engine operating between two temperatures:

```
η_carnot = 1 - T_cold / T_hot
```

No real engine exceeds Carnot efficiency. The gap between actual and Carnot
efficiency measures how much room for improvement exists.

For an agent, the analogy is precise:

- **T_hot** = total tokens consumed (the energy input).
- **T_cold** = tokens spent on overhead (entropy tax: prime injection, retries,
  coordination, error recovery).
- **Actual efficiency** = productive tokens / total tokens.
- **Carnot efficiency** = the theoretical maximum achievable if all overhead were
  reduced to its irreducible minimum (Landauer cost of context reconstruction).

```rust
/// The theoretical maximum efficiency of an agent, given its irreducible
/// overhead.
///
/// An agent with Carnot efficiency 0.9 could theoretically achieve 90%
/// productive output. If its actual efficiency is 0.6, there is 0.3 of
/// recoverable waste.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarnotEfficiency(f64);

impl CarnotEfficiency {
    /// Create a new Carnot efficiency, clamping to [0.0, 1.0].
    pub fn new(efficiency: f64) -> Self {
        Self(efficiency.clamp(0.0, 1.0))
    }

    /// The efficiency value.
    pub fn get(self) -> f64 {
        self.0
    }

    /// The gap between theoretical maximum and actual efficiency.
    /// This is recoverable waste.
    pub fn waste(self, actual: f64) -> f64 {
        (self.0 - actual.clamp(0.0, 1.0)).max(0.0)
    }
}
```

The Carnot bound is computed per-agent:

```
η_carnot(agent) = 1 - landauer_cost(agent) / total_tokens(agent)
```

where `landauer_cost` is the irreducible minimum tokens needed to reconstruct
this agent's context at each session boundary. This is measurable: it is the
size of the prime injection plus the minimum checkpoint needed to avoid rework.

### Helmholtz free energy

In thermodynamics, Helmholtz free energy `F = U - TS` is the energy available
to do useful work at constant temperature, where:

- `U` = internal energy (total token budget)
- `T` = temperature (system activity level)
- `S` = entropy (system uncertainty)

The Cosmon equivalent:

```
F = total_budget - temperature × total_entropy
```

This captures an insight that the simple `free_energy_ratio` in `EnergyReport`
misses: **the cost of entropy depends on temperature.** At high temperature
(exploration mode), entropy is expensive — each bit of uncertainty costs more
tokens to resolve because agents are pursuing diverse paths. At low temperature
(convergence mode), entropy is cheap — agents follow known paths and uncertainty
resolves quickly.

```rust
/// Helmholtz free energy: the token budget available for productive work
/// after accounting for the entropy cost at the current temperature.
///
/// `F = U - T·S` where U is the total budget, T is temperature, and S is
/// total entropy.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HelmholtzFreeEnergy(f64);

impl HelmholtzFreeEnergy {
    /// Compute Helmholtz free energy.
    ///
    /// - `budget`: total token budget (U)
    /// - `temperature`: system temperature (T), [0.0, 1.0]
    /// - `total_entropy`: sum of all entropy channels (S)
    pub fn compute(budget: TokenCount, temperature: Temperature, total_entropy: Entropy) -> Self {
        let u = budget.get() as f64;
        let t = temperature.get();
        let s = total_entropy.bits();
        Self(u - t * s)
    }

    /// The free energy value in token-equivalent units.
    pub fn get(self) -> f64 {
        self.0
    }
}
```

Helmholtz free energy answers the orchestrator's key question: *given the current
temperature and uncertainty, how many tokens are effectively available for
productive work?*

### Relationship to existing types

| Existing type | Relationship |
|--------------|-------------|
| `Temperature` | Used directly in `ThermodynamicState` and Helmholtz computation |
| `EnergyBudget` | Provides `U` (total budget) for Helmholtz; `consumed` informs actual efficiency |
| `EnergyReport` | `free_energy_ratio()` becomes a special case: Helmholtz at T=1, ignoring entropy channels |
| `Fleet::temperature()` | Provides `T` for the thermodynamic state snapshot |
| `PatrolReport` | Can include `ThermodynamicState` as an optional diagnostic |

### Implementation phases

**Phase 1: Types only.** Define `Entropy`, `CompressionRatio`, `ThermodynamicState`,
`AgentThermodynamics`, `CarnotEfficiency`, and `HelmholtzFreeEnergy` in
`cosmon-core/src/entropy.rs`. Pure types with constructors, no I/O, no computation
beyond the formulas above. Full test coverage.

**Phase 2: State entropy computation.** Implement `ThermodynamicState::from_fleet()`
— computes state entropy from the worker status distribution. This requires only
`Fleet` and the existing `Temperature` type.

**Phase 3: Patrol integration.** Add an optional `ThermodynamicState` field to
`PatrolReport`. The transport patrol computes state entropy; the cognition patrol
interprets the thermodynamic state and recommends temperature adjustments.

**Phase 4: Per-agent thermodynamics.** Compute `AgentThermodynamics` from
`EnergyRecord` history. Requires historical data, so this phase depends on
energy tracking being operational.

## Consequences

### Positive

- Entropy becomes measurable, not metaphorical — enabling data-driven dispatch
  and temperature control decisions.
- Carnot efficiency provides an upper bound on agent productivity, distinguishing
  "this agent is inefficient" from "this agent has high irreducible overhead."
- Helmholtz free energy captures the temperature-dependent cost of entropy that
  the simpler free energy ratio misses.
- All types are pure (zero I/O) and live in `cosmon-core`, consistent with the
  architectural principle.
- The four entropy channels decompose system uncertainty into actionable
  categories — the orchestrator can target specific entropy sources.

### Negative

- Adds conceptual complexity: users must understand four entropy channels and
  their relationship to temperature and free energy.
- The physics vocabulary becomes load-bearing: if the thermodynamic analogy
  breaks (entropy doesn't correlate with operational problems), the types are
  misleading.
- Computing message and code entropy requires historical data that may not exist
  in early deployments.

### Risks

- **Over-fitting the metaphor.** Thermodynamic entropy has specific mathematical
  properties (extensivity, the second law) that may not hold for agent systems.
  Mitigation: treat the formulas as useful heuristics, not physical laws. Document
  where the analogy breaks.
- **Measurement overhead.** Computing compression ratios and Shannon entropy on
  every patrol cycle could become expensive. Mitigation: compute on a longer
  interval than the transport patrol, and cache results.

## References

- THESIS.md Part V (Vocabulary): entropy, temperature, free energy definitions
- THESIS.md Part XI (Energy Principle): token tracking, entropy tax, Landauer
- Shannon, C. E. (1948). "A Mathematical Theory of Communication"
- Landauer, R. (1961). "Irreversibility and Heat Generation in the Computing Process"
- ADR-COS-001: State Storage — establishes the pure-types-first pattern this ADR follows
