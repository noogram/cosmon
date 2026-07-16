// SPDX-License-Identifier: AGPL-3.0-only

//! Ensemble (fleet) types.
//!
//! The [`Fleet`] is the set of all agent definitions and active workers.
//! In statistical-mechanics terms, it is the ensemble: individual worker
//! trajectories matter less than the distribution of states and overall health.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent::AgentDefinition;
use crate::id::{AgentId, WorkerId};
use crate::worker::{Worker, WorkerStatus};

/// The fleet: all agent definitions and active workers.
///
/// JSON serialization is implemented manually to support schema evolution:
/// unknown top-level fields are silently ignored on read, and the schema
/// version is embedded for future migration logic.
#[derive(Debug, Clone, PartialEq)]
pub struct Fleet {
    /// Agent definitions keyed by name.
    pub agents: HashMap<AgentId, AgentDefinition>,
    /// Active workers keyed by worker ID.
    pub workers: HashMap<WorkerId, Worker>,
}

impl Fleet {
    /// Create an empty fleet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            workers: HashMap::new(),
        }
    }

    /// Count of workers in the Active state.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.workers
            .values()
            .filter(|w| w.status == WorkerStatus::Active)
            .count()
    }

    /// Workers currently idle (Starting or Stopped).
    #[must_use]
    pub fn idle_workers(&self) -> Vec<&Worker> {
        self.workers
            .values()
            .filter(|w| matches!(w.status, WorkerStatus::Starting | WorkerStatus::Stopped))
            .collect()
    }

    /// System temperature: ratio of active workers to total workers.
    ///
    /// Returns 0.0 if there are no workers.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn temperature(&self) -> f64 {
        let total = self.workers.len();
        if total == 0 {
            return 0.0;
        }
        self.active_count() as f64 / total as f64
    }
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Manual JSON serialization for schema evolution
// ---------------------------------------------------------------------------

/// Current schema version embedded in serialized fleet JSON.
const SCHEMA_VERSION: u32 = 1;

impl Serialize for Fleet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let agents_vec: Vec<&AgentDefinition> = self.agents.values().collect();
        let workers_vec: Vec<&Worker> = self.workers.values().collect();

        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("version", &SCHEMA_VERSION)?;
        map.serialize_entry("agents", &agents_vec)?;
        map.serialize_entry("workers", &workers_vec)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Fleet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        let obj = v
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("Fleet must be a JSON object"))?;

        // Version field is read but not enforced yet — future migrations go here.
        let _version = obj
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);

        let agents_val = obj
            .get("agents")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        let agents_vec: Vec<AgentDefinition> =
            serde_json::from_value(agents_val).map_err(serde::de::Error::custom)?;
        let agents = agents_vec
            .into_iter()
            .map(|a| (a.name.clone(), a))
            .collect();

        let workers_val = obj
            .get("workers")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        let workers_vec: Vec<Worker> =
            serde_json::from_value(workers_val).map_err(serde::de::Error::custom)?;
        let workers = workers_vec.into_iter().map(|w| (w.id.clone(), w)).collect();

        Ok(Self { agents, workers })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentRole;
    use crate::clearance::Clearance;
    use crate::id::{AgentId, MoleculeId, SessionId, WorkerId};
    use crate::worker::WorkerStatus;
    use chrono::Utc;

    fn sample_fleet() -> Fleet {
        let mut fleet = Fleet::new();

        let witness_def = AgentDefinition::new(
            AgentId::new("witness").unwrap(),
            AgentRole::Orchestration,
            Clearance::Execute,
        );
        let polecat_def = AgentDefinition::new(
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
        );
        fleet.agents.insert(witness_def.name.clone(), witness_def);
        fleet.agents.insert(polecat_def.name.clone(), polecat_def);

        let mut worker = Worker::new(
            WorkerId::new("ep-quartz").unwrap(),
            AgentId::new("polecat").unwrap(),
            Utc::now(),
        );
        worker.status = WorkerStatus::Active;
        worker.session = Some(SessionId::new("sess-001").unwrap());
        worker.current_molecule = Some(MoleculeId::new("cs-20260401-abcd").unwrap());
        fleet.workers.insert(worker.id.clone(), worker);

        let idle_worker = Worker::new(
            WorkerId::new("jasper").unwrap(),
            AgentId::new("polecat").unwrap(),
            Utc::now(),
        );
        fleet.workers.insert(idle_worker.id.clone(), idle_worker);

        fleet
    }

    #[test]
    fn test_fleet_json_roundtrip() {
        let fleet = sample_fleet();
        let json = serde_json::to_string_pretty(&fleet).unwrap();
        let back: Fleet = serde_json::from_str(&json).unwrap();
        assert_eq!(fleet.agents.len(), back.agents.len());
        assert_eq!(fleet.workers.len(), back.workers.len());
        for (id, def) in &fleet.agents {
            assert_eq!(back.agents.get(id), Some(def));
        }
    }

    #[test]
    fn test_fleet_active_count_and_temperature() {
        let fleet = sample_fleet();
        assert_eq!(fleet.active_count(), 1);
        assert_eq!(fleet.idle_workers().len(), 1);
        // 1 active out of 2 total
        assert!((fleet.temperature() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_fleet_empty_temperature() {
        let fleet = Fleet::new();
        assert!((fleet.temperature() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_fleet_json_ignores_unknown_fields() {
        let json = r#"{
            "version": 1,
            "agents": [],
            "workers": [],
            "future_field": "ignored"
        }"#;
        let fleet: Fleet = serde_json::from_str(json).unwrap();
        assert!(fleet.agents.is_empty());
    }

    #[test]
    fn test_fleet_json_defaults_missing_arrays() {
        let json = r#"{"version": 1}"#;
        let fleet: Fleet = serde_json::from_str(json).unwrap();
        assert!(fleet.agents.is_empty());
        assert!(fleet.workers.is_empty());
    }
}
