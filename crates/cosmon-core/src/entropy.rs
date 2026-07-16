// SPDX-License-Identifier: AGPL-3.0-only

//! Thermodynamic types for measuring entropy, efficiency, and free energy.
//!
//! Implements ADR-COS-002 Phase 1: pure types with constructors, formulas,
//! and full test coverage. No I/O.
//!
//! The four entropy channels (message, code, context, state) decompose system
//! uncertainty into actionable categories. Combined with temperature and the
//! Helmholtz free energy, they form the thermodynamic state of the fleet.
//!
//! Core primitives ([`Entropy`], [`CompressionRatio`]) are defined in
//! [`crate::energy`] and re-exported here for convenience.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::entropy::{Entropy, CarnotEfficiency, HelmholtzFreeEnergy};
//! use cosmon_core::energy::{Temperature, TokenCount};
//!
//! // Entropy is non-negative (measured in bits):
//! let e = Entropy::new(3.2);
//! assert!((e.get() - 3.2).abs() < f64::EPSILON);
//! assert!((Entropy::new(-1.0).get() - 0.0).abs() < f64::EPSILON);
//!
//! // Carnot efficiency bounds agent productivity:
//! let carnot = CarnotEfficiency::new(0.85);
//! let waste = carnot.waste(0.60);
//! assert!((waste - 0.25).abs() < f64::EPSILON);
//!
//! // Helmholtz free energy: F = U - T·S
//! let f = HelmholtzFreeEnergy::compute(
//!     TokenCount::new(10_000),
//!     Temperature::new(0.7),
//!     Entropy::new(1000.0),
//! );
//! assert!((f.get() - 9300.0).abs() < f64::EPSILON);
//! ```

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::energy::{CompressionRatio, Entropy};
use crate::energy::{EnergyBudget, Temperature, TokenCount};
use crate::ensemble::Fleet;
use crate::id::WorkerId;
use crate::worker::WorkerStatus;

// ---------------------------------------------------------------------------
// CarnotEfficiency
// ---------------------------------------------------------------------------

/// The theoretical maximum efficiency of an agent, given its irreducible overhead.
///
/// An agent with Carnot efficiency 0.9 could theoretically achieve 90%
/// productive output. If its actual efficiency is 0.6, there is 0.3 of
/// recoverable waste.
///
/// Computed as: `η_carnot = 1 - landauer_cost / total_tokens`
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarnotEfficiency(f64);

impl CarnotEfficiency {
    /// Create a new Carnot efficiency, clamping to [0.0, 1.0].
    #[must_use]
    pub fn new(efficiency: f64) -> Self {
        Self(efficiency.clamp(0.0, 1.0))
    }

    /// The efficiency value.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// The gap between theoretical maximum and actual efficiency.
    /// This is recoverable waste.
    #[must_use]
    pub fn waste(self, actual: f64) -> f64 {
        (self.0 - actual.clamp(0.0, 1.0)).max(0.0)
    }
}

impl fmt::Display for CarnotEfficiency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "η_carnot={:.1}%", self.0 * 100.0)
    }
}

// ---------------------------------------------------------------------------
// HelmholtzFreeEnergy
// ---------------------------------------------------------------------------

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
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn compute(budget: TokenCount, temperature: Temperature, total_entropy: Entropy) -> Self {
        let u = budget.get() as f64;
        let t = temperature.get();
        let s = total_entropy.get();
        Self(u - t * s)
    }

    /// The free energy value in token-equivalent units.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for HelmholtzFreeEnergy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "F={:.0} tokens", self.0)
    }
}

// ---------------------------------------------------------------------------
// ThermodynamicState
// ---------------------------------------------------------------------------

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
    /// System temperature (from `Fleet::temperature()`).
    pub temperature: Temperature,
    /// Helmholtz free energy: budget available for productive work.
    pub free_energy: HelmholtzFreeEnergy,
}

impl ThermodynamicState {
    /// Total entropy across all four channels.
    #[must_use]
    pub fn total_entropy(&self) -> Entropy {
        self.message_entropy + self.code_entropy + self.context_entropy + self.state_entropy
    }

    /// Compute thermodynamic state from fleet and energy budget (ADR-COS-002 Phase 2).
    ///
    /// State entropy is computed from the worker status distribution using
    /// Shannon entropy: `H = -Σ p(s) · log₂(p(s))`. Message, code, and
    /// context entropy channels are set to zero — they require data sources
    /// not yet available (Phases 3-4).
    ///
    /// # Examples
    ///
    /// ```
    /// use cosmon_core::entropy::ThermodynamicState;
    /// use cosmon_core::energy::{TokenCount, EnergyBudget, BudgetPeriod};
    /// use cosmon_core::ensemble::Fleet;
    /// use cosmon_core::worker::{Worker, WorkerStatus};
    /// use cosmon_core::id::{AgentId, WorkerId};
    /// use chrono::Utc;
    ///
    /// let mut fleet = Fleet::new();
    /// let mut w1 = Worker::new(
    ///     WorkerId::new("quartz").unwrap(),
    ///     AgentId::new("polecat").unwrap(),
    ///     Utc::now(),
    /// );
    /// w1.status = WorkerStatus::Active;
    /// fleet.workers.insert(w1.id.clone(), w1);
    ///
    /// let mut w2 = Worker::new(
    ///     WorkerId::new("jasper").unwrap(),
    ///     AgentId::new("polecat").unwrap(),
    ///     Utc::now(),
    /// );
    /// w2.status = WorkerStatus::Stopped;
    /// fleet.workers.insert(w2.id.clone(), w2);
    ///
    /// let budget = EnergyBudget::new(
    ///     TokenCount::new(10_000),
    ///     BudgetPeriod::Weekly,
    ///     0.8,
    /// );
    ///
    /// let state = ThermodynamicState::from_fleet(&fleet, &budget);
    /// // 2 workers in 2 distinct states → H = log₂(2) = 1.0 bit
    /// assert!((state.state_entropy.get() - 1.0).abs() < 1e-10);
    /// // Other channels are zero in Phase 2
    /// assert!((state.message_entropy.get() - 0.0).abs() < f64::EPSILON);
    /// ```
    #[must_use]
    pub fn from_fleet(fleet: &Fleet, budget: &EnergyBudget) -> Self {
        let state_entropy = worker_status_entropy(fleet);
        let temperature = Temperature::new(fleet.temperature());
        let total_entropy = state_entropy; // other channels are zero
        let free_energy =
            HelmholtzFreeEnergy::compute(budget.remaining(), temperature, total_entropy);

        Self {
            timestamp: Utc::now(),
            message_entropy: Entropy::ZERO,
            code_entropy: Entropy::ZERO,
            context_entropy: Entropy::ZERO,
            state_entropy,
            temperature,
            free_energy,
        }
    }
}

/// Compute Shannon entropy of the worker status distribution.
///
/// `H = -Σ p(s) · log₂(p(s))` where `p(s)` is the fraction of workers
/// in status `s`. Returns zero for an empty fleet.
#[must_use]
#[allow(clippy::cast_precision_loss)]
fn worker_status_entropy(fleet: &Fleet) -> Entropy {
    let total = fleet.workers.len();
    if total == 0 {
        return Entropy::ZERO;
    }

    // Count workers per status bucket. WorkerStatus::Error variants are
    // all counted as a single "error" bucket regardless of message.
    let mut counts = std::collections::HashMap::<StatusBucket, usize>::new();
    for w in fleet.workers.values() {
        let bucket = StatusBucket::from(&w.status);
        *counts.entry(bucket).or_insert(0) += 1;
    }

    let n = total as f64;
    let h = counts
        .values()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum::<f64>();

    Entropy::new(h)
}

/// Bucket for worker status — collapses all Error variants into one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum StatusBucket {
    /// Starting state.
    Starting,
    /// Active state.
    Active,
    /// Paused state.
    Paused,
    /// Stopping state.
    Stopping,
    /// Stopped state.
    Stopped,
    /// Any error state (message ignored for entropy computation).
    Error,
    /// Unresponsive — failed one liveness check.
    Unresponsive,
    /// Stale state.
    Stale,
}

impl From<&WorkerStatus> for StatusBucket {
    fn from(status: &WorkerStatus) -> Self {
        match status {
            WorkerStatus::Starting => Self::Starting,
            WorkerStatus::Active => Self::Active,
            WorkerStatus::Paused => Self::Paused,
            WorkerStatus::Stopping => Self::Stopping,
            WorkerStatus::Stopped => Self::Stopped,
            WorkerStatus::Error(_) => Self::Error,
            WorkerStatus::Unresponsive => Self::Unresponsive,
            WorkerStatus::Stale => Self::Stale,
        }
    }
}

// ---------------------------------------------------------------------------
// AgentThermodynamics
// ---------------------------------------------------------------------------

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
    /// This agent's Carnot efficiency.
    pub carnot_efficiency: CarnotEfficiency,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::Worker;

    // Entropy and CompressionRatio are tested in energy.rs.
    // Tests here focus on thermodynamic types defined in this module.

    #[test]
    fn test_carnot_efficiency_clamped() {
        assert!((CarnotEfficiency::new(0.85).get() - 0.85).abs() < f64::EPSILON);
        assert!((CarnotEfficiency::new(-0.1).get() - 0.0).abs() < f64::EPSILON);
        assert!((CarnotEfficiency::new(1.5).get() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_carnot_efficiency_waste() {
        let carnot = CarnotEfficiency::new(0.85);

        // Normal case: waste = theoretical max - actual
        assert!((carnot.waste(0.60) - 0.25).abs() < f64::EPSILON);

        // Actual exceeds Carnot (impossible but handled): waste = 0
        assert!((carnot.waste(0.95) - 0.0).abs() < f64::EPSILON);

        // Actual clamped: negative actual treated as 0
        assert!((carnot.waste(-0.1) - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_carnot_efficiency_display() {
        let c = CarnotEfficiency::new(0.9);
        assert_eq!(c.to_string(), "η_carnot=90.0%");
    }

    #[test]
    fn test_helmholtz_free_energy_formula() {
        // F = U - T·S = 10000 - 0.7 * 1000 = 9300
        let f = HelmholtzFreeEnergy::compute(
            TokenCount::new(10_000),
            Temperature::new(0.7),
            Entropy::new(1000.0),
        );
        assert!((f.get() - 9300.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_helmholtz_frozen_temperature() {
        // At T=0 (frozen), F = U: all budget is available
        let f = HelmholtzFreeEnergy::compute(
            TokenCount::new(5000),
            Temperature::FROZEN,
            Entropy::new(999.0),
        );
        assert!((f.get() - 5000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_helmholtz_hot_temperature() {
        // At T=1 (hot), F = U - S: entropy fully penalized
        let f = HelmholtzFreeEnergy::compute(
            TokenCount::new(5000),
            Temperature::HOT,
            Entropy::new(2000.0),
        );
        assert!((f.get() - 3000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_helmholtz_display() {
        let f = HelmholtzFreeEnergy::compute(
            TokenCount::new(10_000),
            Temperature::new(0.5),
            Entropy::new(1000.0),
        );
        assert_eq!(f.to_string(), "F=9500 tokens");
    }

    #[test]
    fn test_thermodynamic_state_total_entropy() {
        let state = ThermodynamicState {
            timestamp: Utc::now(),
            message_entropy: Entropy::new(1.0),
            code_entropy: Entropy::new(2.0),
            context_entropy: Entropy::new(3.0),
            state_entropy: Entropy::new(4.0),
            temperature: Temperature::WARM,
            free_energy: HelmholtzFreeEnergy::compute(
                TokenCount::new(10_000),
                Temperature::WARM,
                Entropy::new(10.0),
            ),
        };
        assert!((state.total_entropy().get() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_agent_thermodynamics_construction() {
        let at = AgentThermodynamics {
            worker: WorkerId::new("topaz").unwrap(),
            message_entropy: Entropy::new(2.5),
            code_compression: CompressionRatio::new(0.4),
            context_utilization: 0.75,
            energy_consumed: TokenCount::new(5000),
            carnot_efficiency: CarnotEfficiency::new(0.88),
        };

        assert!((at.message_entropy.get() - 2.5).abs() < f64::EPSILON);
        assert!((at.code_compression.get() - 0.4).abs() < f64::EPSILON);
        assert!((at.context_utilization - 0.75).abs() < f64::EPSILON);
        assert_eq!(at.energy_consumed.get(), 5000);
        assert!((at.carnot_efficiency.get() - 0.88).abs() < f64::EPSILON);
    }

    #[test]
    fn test_carnot_efficiency_serde_roundtrip() {
        let c = CarnotEfficiency::new(0.9);
        let json = serde_json::to_string(&c).unwrap();
        let back: CarnotEfficiency = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn test_helmholtz_serde_roundtrip() {
        let h = HelmholtzFreeEnergy::compute(
            TokenCount::new(10_000),
            Temperature::WARM,
            Entropy::new(500.0),
        );
        let json = serde_json::to_string(&h).unwrap();
        let back: HelmholtzFreeEnergy = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn test_thermodynamic_state_serde_roundtrip() {
        let state = ThermodynamicState {
            timestamp: Utc::now(),
            message_entropy: Entropy::new(1.0),
            code_entropy: Entropy::new(2.0),
            context_entropy: Entropy::new(3.0),
            state_entropy: Entropy::new(4.0),
            temperature: Temperature::WARM,
            free_energy: HelmholtzFreeEnergy::compute(
                TokenCount::new(10_000),
                Temperature::WARM,
                Entropy::new(10.0),
            ),
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: ThermodynamicState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    // -- Phase 2: from_fleet and state entropy --

    use crate::energy::BudgetPeriod;
    use crate::id::AgentId;

    fn make_worker(name: &str, status: WorkerStatus) -> Worker {
        let mut w = Worker::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("polecat").unwrap(),
            Utc::now(),
        );
        w.status = status;
        w
    }

    fn default_budget() -> EnergyBudget {
        EnergyBudget::new(TokenCount::new(10_000), BudgetPeriod::Weekly, 0.8)
    }

    #[test]
    fn test_from_fleet_empty_fleet_zero_entropy() {
        let fleet = Fleet::new();
        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());

        assert!((state.state_entropy.get() - 0.0).abs() < f64::EPSILON);
        assert!((state.message_entropy.get() - 0.0).abs() < f64::EPSILON);
        assert!((state.code_entropy.get() - 0.0).abs() < f64::EPSILON);
        assert!((state.context_entropy.get() - 0.0).abs() < f64::EPSILON);
        assert!((state.temperature.get() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_from_fleet_single_worker_zero_entropy() {
        // One worker in one state → p=1 → -1·log₂(1) = 0
        let mut fleet = Fleet::new();
        fleet.workers.insert(
            "quartz".parse().unwrap(),
            make_worker("quartz", WorkerStatus::Active),
        );

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.state_entropy.get() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_from_fleet_two_distinct_states_one_bit() {
        // 2 workers in 2 different states → H = log₂(2) = 1.0
        let mut fleet = Fleet::new();
        fleet.workers.insert(
            "quartz".parse().unwrap(),
            make_worker("quartz", WorkerStatus::Active),
        );
        fleet.workers.insert(
            "jasper".parse().unwrap(),
            make_worker("jasper", WorkerStatus::Stopped),
        );

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.state_entropy.get() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_from_fleet_uniform_distribution_max_entropy() {
        // 4 workers each in a distinct state → H = log₂(4) = 2.0
        let mut fleet = Fleet::new();
        fleet.workers.insert(
            "w1".parse().unwrap(),
            make_worker("w1", WorkerStatus::Active),
        );
        fleet.workers.insert(
            "w2".parse().unwrap(),
            make_worker("w2", WorkerStatus::Stopped),
        );
        fleet.workers.insert(
            "w3".parse().unwrap(),
            make_worker("w3", WorkerStatus::Starting),
        );
        fleet.workers.insert(
            "w4".parse().unwrap(),
            make_worker("w4", WorkerStatus::Paused),
        );

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.state_entropy.get() - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_from_fleet_all_same_state_zero_entropy() {
        // 4 workers all Active → p(Active)=1 → H=0
        let mut fleet = Fleet::new();
        for name in &["w1", "w2", "w3", "w4"] {
            fleet.workers.insert(
                name.parse().unwrap(),
                make_worker(name, WorkerStatus::Active),
            );
        }

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.state_entropy.get() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_from_fleet_error_variants_collapse_to_one_bucket() {
        // Two workers with different Error messages → same bucket → H=0
        let mut fleet = Fleet::new();
        fleet.workers.insert(
            "w1".parse().unwrap(),
            make_worker("w1", WorkerStatus::Error("timeout".to_owned())),
        );
        fleet.workers.insert(
            "w2".parse().unwrap(),
            make_worker("w2", WorkerStatus::Error("oom".to_owned())),
        );

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.state_entropy.get() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_from_fleet_temperature_matches_fleet() {
        let mut fleet = Fleet::new();
        // 3 active, 1 stopped → temperature = 3/4 = 0.75
        for name in &["w1", "w2", "w3"] {
            fleet.workers.insert(
                name.parse().unwrap(),
                make_worker(name, WorkerStatus::Active),
            );
        }
        fleet.workers.insert(
            "w4".parse().unwrap(),
            make_worker("w4", WorkerStatus::Stopped),
        );

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.temperature.get() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_from_fleet_helmholtz_uses_remaining_budget() {
        let mut fleet = Fleet::new();
        fleet.workers.insert(
            "w1".parse().unwrap(),
            make_worker("w1", WorkerStatus::Active),
        );
        fleet.workers.insert(
            "w2".parse().unwrap(),
            make_worker("w2", WorkerStatus::Stopped),
        );

        let mut budget = EnergyBudget::new(TokenCount::new(10_000), BudgetPeriod::Weekly, 0.8);
        budget.consume(TokenCount::new(3_000));
        // remaining = 7000, T = 0.5, S = 1.0 bit
        // F = 7000 - 0.5 * 1.0 = 6999.5

        let state = ThermodynamicState::from_fleet(&fleet, &budget);
        assert!((state.free_energy.get() - 6999.5).abs() < 1e-10);
    }

    #[test]
    fn test_from_fleet_total_entropy_equals_state_entropy() {
        // In Phase 2, only state_entropy is non-zero
        let mut fleet = Fleet::new();
        fleet.workers.insert(
            "w1".parse().unwrap(),
            make_worker("w1", WorkerStatus::Active),
        );
        fleet.workers.insert(
            "w2".parse().unwrap(),
            make_worker("w2", WorkerStatus::Stopped),
        );

        let state = ThermodynamicState::from_fleet(&fleet, &default_budget());
        assert!((state.total_entropy().get() - state.state_entropy.get()).abs() < f64::EPSILON);
    }
}
