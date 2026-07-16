// SPDX-License-Identifier: AGPL-3.0-only

//! Fleet aggregation — merge state across sockets and projects.
//!
//! [`FleetSnapshot`] is a single in-memory view built by scanning every
//! tmux socket (typically `/private/tmp/tmux-501/*`) and every
//! project-rooted `.cosmon/` directory, then indexing the result for the
//! query surface shared by the TUI and the HTTP dashboard.
//!
//! This crate intentionally does **not** perform the raw scans — that lives
//! in adapters (`cosmon-transport` for tmux, `cosmon-filestore` for
//! `.cosmon/state/`). The snapshot is the boundary where those sources
//! converge.

use std::collections::HashMap;

use crate::event::Event;
use crate::molecule::{Molecule, MoleculeId};
use crate::session::{Session, SessionFilter};
use crate::worker::{EnergyBudget, Worker, WorkerId};
use crate::{ObservabilityError, Result};

/// A merged, queryable view of a multi-socket, multi-project fleet.
#[derive(Debug, Clone, Default)]
pub struct FleetSnapshot {
    sessions: Vec<Session>,
    molecules: HashMap<MoleculeId, Molecule>,
    workers: HashMap<WorkerId, Worker>,
    events: HashMap<MoleculeId, Vec<Event>>,
}

impl FleetSnapshot {
    /// Build an empty snapshot. Adapters populate it via the `with_*` setters
    /// or the builder-style `push_*` methods before queries are issued.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a session to the snapshot.
    pub fn push_session(&mut self, s: Session) {
        self.sessions.push(s);
    }

    /// Insert (or replace) a molecule.
    pub fn insert_molecule(&mut self, m: Molecule) {
        self.molecules.insert(m.id.clone(), m);
    }

    /// Insert (or replace) a worker.
    pub fn insert_worker(&mut self, w: Worker) {
        self.workers.insert(w.id.clone(), w);
    }

    /// Append an event to a molecule's event stream.
    pub fn push_event(&mut self, e: Event) {
        self.events
            .entry(e.molecule_id.clone())
            .or_default()
            .push(e);
    }

    /// Number of sessions in the snapshot.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of molecules in the snapshot.
    #[must_use]
    pub fn molecule_count(&self) -> usize {
        self.molecules.len()
    }

    // -------- queries --------

    /// List sessions matching `filter`. An empty [`SessionFilter`] returns all.
    #[must_use]
    pub fn list_sessions(&self, filter: &SessionFilter) -> Vec<&Session> {
        self.sessions.iter().filter(|s| filter.matches(s)).collect()
    }

    /// Iterate over every molecule in the snapshot in arbitrary order.
    pub fn molecules(&self) -> impl Iterator<Item = &Molecule> {
        self.molecules.values()
    }

    /// Retain only molecules for which `predicate` returns `true`. The
    /// other fields of the snapshot (workers, sessions, events) are
    /// untouched — this is a *projection*, not a re-aggregation.
    /// Used by `cs peek --snapshot --phase …` to apply the phase filter
    /// to the wheat-paste byte stream.
    pub fn retain_molecules<F>(&mut self, mut predicate: F)
    where
        F: FnMut(&Molecule) -> bool,
    {
        self.molecules.retain(|_, m| predicate(m));
    }

    /// Iterate over every worker in the snapshot in arbitrary order.
    ///
    /// Consumers that need liveness per worker (e.g. the HTTP dashboard)
    /// walk this iterator to build their own per-id view.
    pub fn workers(&self) -> impl Iterator<Item = &Worker> {
        self.workers.values()
    }

    /// Return a snapshot of the pane text for the given tmux session.
    ///
    /// The snapshot does not capture panes itself — it returns whatever
    /// text was recorded by the adapter when the snapshot was built. A
    /// missing session yields [`ObservabilityError::SessionNotFound`].
    ///
    /// Today the model carries session identity only; pane-text capture
    /// lives in `cosmon-transport`. This query exists so both adapters
    /// share a single signature even before the body lands.
    ///
    /// # Errors
    /// Returns [`ObservabilityError::SessionNotFound`] if `session` is
    /// not present in the snapshot.
    pub fn peek_pane(&self, session: &str) -> Result<String> {
        if self.sessions.iter().any(|s| s.name == session) {
            Ok(String::new())
        } else {
            Err(ObservabilityError::SessionNotFound(session.to_string()))
        }
    }

    /// Return the molecule attached to `session`, if any.
    ///
    /// # Errors
    /// - [`ObservabilityError::SessionNotFound`] if `session` is unknown.
    /// - [`ObservabilityError::NoWorker`] if the session has no attached molecule.
    /// - [`ObservabilityError::MoleculeNotFound`] if the attached id is dangling.
    pub fn molecule_of(&self, session: &str) -> Result<&Molecule> {
        let s = self
            .sessions
            .iter()
            .find(|s| s.name == session)
            .ok_or_else(|| ObservabilityError::SessionNotFound(session.to_string()))?;
        let mid = s
            .molecule_id
            .as_deref()
            .ok_or_else(|| ObservabilityError::NoWorker(session.to_string()))?;
        let mid = MoleculeId(mid.to_string());
        self.molecules
            .get(&mid)
            .ok_or(ObservabilityError::MoleculeNotFound(mid.0))
    }

    /// Return all events recorded for `molecule`, oldest first.
    #[must_use]
    pub fn events_for(&self, molecule: &MoleculeId) -> &[Event] {
        self.events.get(molecule).map_or(&[], Vec::as_slice)
    }

    /// Return the energy budget for `worker`.
    ///
    /// # Errors
    /// [`ObservabilityError::NoWorker`] if the worker is not in the snapshot.
    pub fn energy_for(&self, worker: &WorkerId) -> Result<EnergyBudget> {
        self.workers
            .get(worker)
            .map(|w| w.energy)
            .ok_or_else(|| ObservabilityError::NoWorker(worker.0.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::molecule::MoleculeStatus;
    use chrono::Utc;

    fn fixture() -> FleetSnapshot {
        let mut s = FleetSnapshot::new();
        s.push_session(Session {
            name: "cosmon-mol-1".into(),
            socket: "/private/tmp/tmux-501/default".into(),
            project_root: "/proj/a".into(),
            molecule_id: Some("mol-1".into()),
            worker_id: Some("w-1".into()),
            last_activity: None,
        });
        s.insert_molecule(Molecule {
            id: "mol-1".into(),
            title: "First".into(),
            kind: "task".into(),
            status: MoleculeStatus::Running,
            project_root: "/proj/a".into(),
            session: Some("cosmon-mol-1".into()),
            updated_at: Utc::now(),
        });
        s.insert_worker(Worker {
            id: "w-1".into(),
            molecule_id: Some("mol-1".into()),
            session: "cosmon-mol-1".into(),
            energy: EnergyBudget {
                input_tokens: 100,
                output_tokens: 50,
                cost_usd: 0.0,
                context_window: Some(1_000_000),
            },
            live: "working".into(),
            role: crate::worker::WorkerRole::Cognition,
        });
        s.push_event(Event {
            molecule_id: "mol-1".into(),
            kind: "nucleated".into(),
            at: Utc::now(),
            evidence: None,
        });
        s
    }

    #[test]
    fn list_sessions_unfiltered_returns_all() {
        let snap = fixture();
        assert_eq!(snap.list_sessions(&SessionFilter::default()).len(), 1);
    }

    #[test]
    fn molecule_of_resolves_through_session() {
        let snap = fixture();
        let m = snap.molecule_of("cosmon-mol-1").unwrap();
        assert_eq!(m.id.to_string(), "mol-1");
    }

    #[test]
    fn molecule_of_unknown_session_errors() {
        let snap = fixture();
        assert!(matches!(
            snap.molecule_of("nope"),
            Err(ObservabilityError::SessionNotFound(_))
        ));
    }

    #[test]
    fn events_for_returns_empty_when_absent() {
        let snap = FleetSnapshot::new();
        assert!(snap.events_for(&MoleculeId("x".into())).is_empty());
    }

    #[test]
    fn energy_for_returns_worker_energy() {
        let snap = fixture();
        let e = snap.energy_for(&WorkerId("w-1".into())).unwrap();
        assert_eq!(e.total(), 150);
    }

    #[test]
    fn peek_pane_of_known_session_is_ok() {
        let snap = fixture();
        assert!(snap.peek_pane("cosmon-mol-1").is_ok());
    }
}
