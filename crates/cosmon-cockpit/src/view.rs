// SPDX-License-Identifier: AGPL-3.0-only

//! Hexagonal port traits for the cockpit dashboard.
//!
//! [`DashboardView`] is the read-only projection consumed by HTTP handlers.
//! [`SparkIntake`] is the write port for ingesting telemetry sparks.
//!
//! All view-model types use plain `String` for status and kind rather than
//! importing `cosmon-core` enums. This keeps the cockpit crate decoupled
//! from the core domain — it reads state, it does not participate in domain
//! logic. The adapter layer (e.g. [`crate::adapter::FileCockpitView`]) maps
//! from core types to these DTOs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Cross-sourced liveness verdict for a molecule.
///
/// Computed from two independent sources: the molecule's persisted status
/// (`state.json`) and the worker's live transport/cognitive state
/// (`cs ensemble --json` `live` field). Green only when both agree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Both sources agree: molecule is running and worker is actively working.
    Healthy,
    /// Zombie detected: molecule says Running but worker is stale/idle/dead/error.
    Zombie,
    /// Sources disagree but not a zombie (e.g. molecule pending, worker alive).
    Mismatch,
    /// Cannot determine — no worker assigned or worker not probed.
    Unknown,
}

impl std::fmt::Display for Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => f.write_str("healthy"),
            Self::Zombie => f.write_str("zombie"),
            Self::Mismatch => f.write_str("mismatch"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// The status string value indicating a running molecule.
const STATUS_RUNNING: &str = "running";

/// Compute cross-sourced liveness from molecule status string and worker live string.
///
/// This is the single source of truth for the zombie detection verdict.
/// The `worker_live` parameter mirrors the `live` field from `cs ensemble --json`.
/// The `status` parameter is the molecule's lifecycle status as a lowercase string
/// (e.g. `"running"`, `"pending"`, `"completed"`).
///
/// # Rules
///
/// - `"running"` + worker `"working*"` → [`Liveness::Healthy`]
/// - `"running"` + worker stale/idle/dead/error/`"-"` → [`Liveness::Zombie`]
/// - `"running"` + no worker → [`Liveness::Unknown`]
/// - Non-running status → [`Liveness::Unknown`]
#[must_use]
pub fn compute_liveness(status: &str, worker_live: Option<&str>) -> Liveness {
    if status != STATUS_RUNNING {
        return Liveness::Unknown;
    }

    match worker_live {
        Some(live) => {
            let trimmed = live.trim();
            if trimmed.starts_with("working") || trimmed.starts_with("loading") {
                Liveness::Healthy
            } else if trimmed.starts_with("waiting") || trimmed.starts_with("done") {
                // Worker is alive but not actively working — mismatch, not zombie.
                Liveness::Mismatch
            } else if trimmed == "-"
                || trimmed == "idle"
                || trimmed == "dead"
                || trimmed.starts_with("error")
                || trimmed == "stale"
                || trimmed.is_empty()
            {
                Liveness::Zombie
            } else {
                // Unknown live value — conservative mismatch.
                Liveness::Mismatch
            }
        }
        None => Liveness::Unknown,
    }
}

/// Lightweight molecule row for the list view.
///
/// All domain enum fields are serialized as plain strings so the cockpit
/// crate stays decoupled from `cosmon-core` types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoleculeSummary {
    /// Molecule identifier.
    pub id: String,
    /// Current lifecycle status (lowercase, e.g. `"running"`, `"pending"`).
    pub status: String,
    /// Molecule kind (e.g. `"task"`, `"idea"`). `None` if unset.
    pub kind: Option<String>,
    /// Formula driving this molecule.
    pub formula: String,
    /// Current step index (0-based).
    pub current_step: usize,
    /// Total steps in the formula.
    pub total_steps: usize,
    /// Assigned worker, if any.
    pub worker: Option<String>,
    /// Worker's live status string (from transport/cognitive probing).
    ///
    /// Mirrors the `live` field from `cs ensemble --json`. `None` when
    /// no worker is assigned or liveness has not been probed.
    pub worker_live: Option<String>,
    /// Cross-sourced liveness verdict.
    pub liveness: Liveness,
    /// Last state change.
    pub updated_at: DateTime<Utc>,
}

/// Full molecule detail for the single-molecule view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoleculeDetail {
    /// Molecule identifier.
    pub id: String,
    /// Fleet this molecule belongs to.
    pub fleet_id: String,
    /// Current lifecycle status (lowercase string).
    pub status: String,
    /// Molecule kind (lowercase string). `None` if unset.
    pub kind: Option<String>,
    /// Formula driving this molecule.
    pub formula: String,
    /// Current step index.
    pub current_step: usize,
    /// Total steps.
    pub total_steps: usize,
    /// Assigned worker.
    pub worker: Option<String>,
    /// Variable bindings.
    pub variables: std::collections::HashMap<String, String>,
    /// Legacy string links.
    pub links: Vec<String>,
    /// Completed step IDs.
    pub completed_steps: Vec<String>,
    /// Collapse reason, if collapsed.
    pub collapse_reason: Option<String>,
    /// When created.
    pub created_at: DateTime<Utc>,
    /// When last updated.
    pub updated_at: DateTime<Utc>,
}

/// Fleet-level summary for the dashboard header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSummary {
    /// Number of registered workers.
    pub worker_count: usize,
    /// Number of registered repos.
    pub repo_count: usize,
    /// Attention budget (max alive molecules), if set.
    pub attention_budget: Option<usize>,
}

/// A revision stamp so clients can detect stale data cheaply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    /// Monotonic-ish timestamp of the last state change observed.
    pub timestamp: DateTime<Utc>,
    /// Total molecule count (quick staleness signal).
    pub molecule_count: usize,
}

/// A single event entry for the event-log tail side panel.
///
/// Projected from [`cosmon_core::event::Envelope`] with fields
/// reduced to display-friendly strings. The cockpit never imports
/// core event enums directly — the adapter maps to this DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEntry {
    /// When the event occurred (ISO 8601).
    pub timestamp: DateTime<Utc>,
    /// Event kind as a `snake_case` string (e.g. `"molecule_nucleated"`).
    pub kind: String,
    /// One-line human summary of the event.
    pub summary: String,
    /// Related molecule ID, if any.
    pub molecule_id: Option<String>,
    /// Related worker ID, if any.
    pub worker_id: Option<String>,
}

/// Error type for dashboard view operations.
#[derive(Debug, thiserror::Error)]
pub enum CockpitError {
    /// Molecule not found.
    #[error("molecule not found: {0}")]
    NotFound(String),
    /// Underlying state store error.
    #[error("state store error: {0}")]
    Store(String),
}

/// Read-only port for dashboard rendering.
///
/// Implementations pull data from the state store and project it into
/// view-model types suitable for JSON serialization and HTML rendering.
///
/// The trait uses plain `&str` for status filtering rather than core domain
/// enums, keeping the port boundary free of `cosmon-core` types.
pub trait DashboardView {
    /// List all molecules (optionally filtered by status string).
    ///
    /// The `status` parameter, when provided, is a lowercase status string
    /// such as `"running"`, `"pending"`, `"completed"`.
    ///
    /// # Errors
    /// Returns [`CockpitError::Store`] on I/O failure.
    fn molecules(&self, status: Option<&str>) -> Result<Vec<MoleculeSummary>, CockpitError>;

    /// Load a single molecule by ID.
    ///
    /// # Errors
    /// Returns [`CockpitError::NotFound`] if the molecule does not exist.
    fn molecule(&self, id: &str) -> Result<MoleculeDetail, CockpitError>;

    /// Fleet-level summary.
    ///
    /// # Errors
    /// Returns [`CockpitError::Store`] on I/O failure.
    fn fleet(&self) -> Result<FleetSummary, CockpitError>;

    /// Typed links for a given molecule.
    ///
    /// # Errors
    /// Returns [`CockpitError::NotFound`] if the molecule does not exist.
    fn links(&self, id: &str) -> Result<Vec<String>, CockpitError>;

    /// Current revision stamp (for polling-based freshness checks).
    ///
    /// # Errors
    /// Returns [`CockpitError::Store`] on I/O failure.
    fn revision(&self) -> Result<Revision, CockpitError>;

    /// Last N lifecycle events from `events.jsonl` in reverse chronological order.
    ///
    /// Returns at most `limit` events, newest first. This is the
    /// "what-just-happened" organ — Shannon's top-3 observable.
    ///
    /// # Errors
    /// Returns [`CockpitError::Store`] on I/O failure.
    fn events_tail(&self, limit: usize) -> Result<Vec<EventEntry>, CockpitError>;
}

/// Write port for ingesting telemetry sparks from external probes.
///
/// Future: energy ticks, status-change events, heartbeat pings.
/// Placeholder for now — will be wired when claudion sparks land.
pub trait SparkIntake {
    /// Ingest a raw spark event (opaque JSON blob for now).
    ///
    /// # Errors
    /// Returns [`CockpitError::Store`] on write failure.
    fn ingest(&self, payload: &serde_json::Value) -> Result<(), CockpitError>;
}
