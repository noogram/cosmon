// SPDX-License-Identifier: AGPL-3.0-only

//! Adapter that builds a [`FleetSnapshot`] from the on-disk state store
//! and live tmux backends.
//!
//! Horizon shares its fleet data model with `cs peek` through
//! [`cosmon_observability`]. The observability crate is deliberately
//! I/O-free; this module is the adapter that fills a snapshot from
//! `cosmon-filestore` + `cosmon-transport` so both surfaces read the
//! same types from the same fixture shape.
//!
//! Only *session-level* fleet state is projected here — molecule and
//! event queries still flow through `cosmon-cockpit::DashboardView`,
//! which owns the richer projections that Horizon exposes over HTTP.
//! The single purpose of the snapshot in Horizon today is to replace
//! the former ad-hoc `fleet.json` + `cognitive/*.json` reads with a
//! typed view that cannot drift from the TUI.

use std::collections::HashMap;
use std::path::Path;

use cosmon_core::transport::TransportBackend;
use cosmon_observability::{EnergyBudget, FleetSnapshot, Worker, WorkerId};
use cosmon_state::StateStore;
use cosmon_transport::TmuxBackend;

/// Build a [`FleetSnapshot`] for the fleet rooted at `state_dir`, probing
/// `backends` for transport liveness and reading cognitive self-declarations
/// from `<state_dir>/cognitive/`.
///
/// The returned snapshot is populated with one [`Worker`] per entry in
/// `fleet.json`. Session and molecule rows are intentionally left empty —
/// Horizon consumes the snapshot only for the worker-liveness view today.
pub(crate) fn build_snapshot(state_dir: &Path, backends: &[TmuxBackend]) -> FleetSnapshot {
    let mut snap = FleetSnapshot::new();

    let store = cosmon_filestore::FileStore::new(state_dir);
    let Ok(fleet) = store.load_fleet() else {
        return snap;
    };

    let cognitive_dir = state_dir.join("cognitive");

    for w in fleet.workers.values() {
        let (_, session_str) = observe_transport(backends, &w.id);
        let cognitive_display = observe_cognitive(&cognitive_dir, w.id.as_str());
        let live = cognitive_display
            .or(session_str)
            .unwrap_or_else(|| "-".to_owned());

        snap.insert_worker(Worker {
            id: WorkerId(w.id.as_str().to_owned()),
            molecule_id: None,
            session: String::new(),
            energy: EnergyBudget::default(),
            live,
            role: match w.worker_role {
                cosmon_core::worker::WorkerRole::Runtime => {
                    cosmon_observability::worker::WorkerRole::Runtime
                }
                cosmon_core::worker::WorkerRole::Cognition => {
                    cosmon_observability::worker::WorkerRole::Cognition
                }
            },
        });
    }

    snap
}

/// Project the snapshot's worker list into the `worker_id → live` map
/// expected by the `/api/molecules` enrichment code.
pub(crate) fn worker_liveness_map(snap: &FleetSnapshot) -> HashMap<String, String> {
    snap.workers()
        .map(|w| (w.id.0.clone(), w.live.clone()))
        .collect()
}

/// Probe transport liveness across all backends.
///
/// Returns `(is_alive, session_display_string)`. Mirrors `cs ensemble`'s
/// composition: transport-first, readiness-enriched.
fn observe_transport(
    backends: &[TmuxBackend],
    worker_id: &cosmon_core::id::WorkerId,
) -> (bool, Option<String>) {
    for be in backends {
        if let Ok(true) = be.is_alive(worker_id) {
            let session_str = cosmon_transport::readiness::detect_status(be, worker_id)
                .ok()
                .map(|s| s.to_string());
            return (true, session_str);
        }
    }
    (false, None)
}

/// Read cognitive self-declaration from the agent's status file.
///
/// Returns the display string when the cognitive state is fresh
/// (<5 min old); otherwise `None` so the caller falls back to the
/// transport probe.
fn observe_cognitive(cognitive_dir: &Path, worker_id: &str) -> Option<String> {
    let path = cognitive_dir.join(format!("{worker_id}.json"));
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    if let Some(updated) = json["updated_at"].as_str() {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(updated) {
            let age = chrono::Utc::now() - ts.with_timezone(&chrono::Utc);
            if age > chrono::Duration::minutes(5) {
                return None;
            }
        }
    }

    let status = json["status"].as_str()?;
    let detail = json["detail"].as_str().unwrap_or("");
    if detail.is_empty() {
        Some(status.to_owned())
    } else {
        let short = if detail.len() > 20 {
            format!("{}…", &detail[..19])
        } else {
            detail.to_owned()
        };
        Some(format!("{status}:{short}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_snapshot_on_empty_state_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let snap = build_snapshot(tmp.path(), &[]);
        assert_eq!(snap.workers().count(), 0);
    }

    #[test]
    fn worker_liveness_map_empty_on_empty_snapshot() {
        let snap = FleetSnapshot::new();
        assert!(worker_liveness_map(&snap).is_empty());
    }
}
