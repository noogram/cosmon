// SPDX-License-Identifier: AGPL-3.0-only

//! Event-log entries projected for observation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::molecule::MoleculeId;

/// A single event-log entry pertaining to a molecule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Molecule this event belongs to.
    pub molecule_id: MoleculeId,
    /// Event kind tag (e.g. `"nucleated"`, `"evolved"`, `"completed"`).
    pub kind: String,
    /// When the event was recorded.
    pub at: DateTime<Utc>,
    /// Free-form evidence attached by the worker that emitted the event.
    pub evidence: Option<String>,
}
