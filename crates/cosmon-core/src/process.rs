// SPDX-License-Identifier: AGPL-3.0-only

//! Inline process record for a molecule — the structural fold-in.
//!
//! # Why this module exists
//!
//! Before this fold-in, "is a worker bound to this molecule?" required
//! cross-checking three back-pointers:
//!
//! * [`crate::molecule::Molecule::assigned_worker`] (molecule → worker)
//! * `WorkerData::current_molecule` (worker → molecule)
//! * `MoleculeData::session_name` (molecule → tmux session)
//!
//! The three could disagree. The disagreement was the **phantom-worker
//! class**: a Molecule could
//! advertise an `assigned_worker` while the matching `WorkerData` had
//! already been purged, or a `session_name` could outlive its tmux
//! session, leaving every reader of the trio to invent a different
//! reconciliation.
//!
//! [`MoleculeProcess`] collapses the trio into one inline slot owned by
//! the molecule. Presence (`Some(_)`) means the pilot believes a live
//! process is bound; absence (`None`) means no process. The reverse
//! pointers are kept during the migration window for backwards
//! compatibility, but every new reader should consult
//! [`MoleculeProcess`] first.
//!
//! This fold-in is the lasting fix after two earlier phantom-worker
//! patches — each reconciling the three pointers at read time — did not
//! hold.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::WorkerId;
use crate::run_state::{Liveness, Witness};
use crate::worker::WorkerStatus;

/// A live-process record inlined on a [`crate::molecule::Molecule`].
///
/// A molecule has at most one such record. Presence (`Some(_)`) means
/// the pilot has bound a worker to the molecule — the tmux session,
/// the worker identity, the start instant — and that none of those
/// facts have been retracted yet. Absence (`None`) is the only valid
/// representation of "no live process".
///
/// `cs tackle` writes this record. `cs done` clears it on successful
/// teardown. `cs collapse` and `cs stuck` clear it on terminal
/// transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MoleculeProcess {
    /// Worker identity bound to this molecule.
    ///
    /// In the legacy three-back-pointer arrangement this lived on
    /// [`crate::molecule::Molecule::assigned_worker`]; the canonical
    /// reading post-fold-in is here. The legacy field is kept during
    /// the migration window — readers should prefer this value when
    /// it is present.
    pub worker_id: WorkerId,
    /// Tmux session name owning the worker process.
    ///
    /// Replaces the legacy `MoleculeData::session_name`. Renames stay
    /// in lockstep with teardown because both `cs done` paths read
    /// from this field.
    pub tmux_session: String,
    /// When this process record was created (typically by `cs tackle`).
    pub started_at: DateTime<Utc>,
    /// Last lifecycle status we recorded for the worker.
    ///
    /// This is the transport-layer status of the worker process —
    /// distinct from the molecule's own `status` (the pilot's view of
    /// progress along the formula). Defaults to
    /// [`WorkerStatus::Active`] at construction.
    pub status: WorkerStatus,
    /// Operating-system PID, when the transport backend surfaced one.
    ///
    /// Probed lazily; `None` is normal when the backend does not
    /// expose PIDs. Recorded for forensic value (post-mortem on a
    /// crashed worker can grep journals by PID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Opaque operating-system launch fingerprint captured with [`Self::pid`].
    ///
    /// A PID alone is not an identity: after a crash the kernel can assign its
    /// numeric value to an unrelated process. Adapters that use a PID as an
    /// external liveness witness therefore compare this platform-specific
    /// start-time token as well. `None` preserves compatibility with records
    /// written before the identity witness existed; it is not sufficient for
    /// PID-based liveness decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start_time: Option<u64>,
    /// Adapter name validated at `cs tackle` time (`claude`, `aider`,
    /// `openai`, `anthropic`, …).
    ///
    /// Recorded so observer-side commands (`cs ensemble`, `cs peek`)
    /// can branch on the adapter's supervision mode (tmux pane vs
    /// in-process) without re-running the adapter selection logic.
    /// `None` for legacy `MoleculeProcess` records and for migration
    /// rows written before the field existed — observers treat the
    /// absence as the conservative default (tmux-postulated). This
    /// is the observer-side dual of the GAP #6 / ADR-101 in-process
    /// completion fix (chronicle 2026-05-18-gap7-observer-side-fix.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_name: Option<String>,
    /// The per-molecule **model** pin resolved at `cs tackle` time
    /// (delib-20260704-b476 C1), or `None` when nothing pinned a model and
    /// the adapter's own default (the floor) applied.
    ///
    /// Recorded as the durable sibling of [`Self::adapter_name`] so a
    /// **re-dispatch** can reproduce the molecule's *original* resolution
    /// instead of re-resolving from ambient environment (`$ANTHROPIC_MODEL`,
    /// `$COSMON_DEFAULT_MODEL`). This is the persistence half of the
    /// noogram/cosmon#3 Defect 2 fix: without it, an orphan-reclaimed local
    /// worker re-dispatched by the runtime bled an ambient Claude model id
    /// into the Ollama-backed floor and every re-dispatch was refused by the
    /// preflight, stranding the molecule `Pending` until the deadline
    /// (exit 124). `None` for legacy records, tmux/in-process adapters, and
    /// the floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl MoleculeProcess {
    /// Create a fresh process record at `Utc::now()` in
    /// [`WorkerStatus::Active`] with no PID recorded.
    #[must_use]
    pub fn new(worker_id: WorkerId, tmux_session: impl Into<String>) -> Self {
        Self {
            worker_id,
            tmux_session: tmux_session.into(),
            started_at: Utc::now(),
            status: WorkerStatus::Active,
            pid: None,
            pid_start_time: None,
            adapter_name: None,
            model: None,
        }
    }

    /// Builder: record the validated adapter name chosen at `cs tackle`
    /// time (`claude`, `aider`, `openai`, `anthropic`, …). Read back by
    /// observer-side commands (`cs ensemble`, `cs peek`) to branch on
    /// the adapter's supervision mode.
    #[must_use]
    pub fn with_adapter_name(mut self, adapter: impl Into<String>) -> Self {
        self.adapter_name = Some(adapter.into());
        self
    }

    /// Builder: record the per-molecule model pin chosen at `cs tackle` time.
    ///
    /// Read back by the runtime's re-dispatch path to reproduce the original
    /// model resolution (noogram/cosmon#3 Defect 2). Passing `None` is a
    /// no-op so the floor (`None`) is representable without a spurious write.
    #[must_use]
    pub fn with_model(mut self, model: Option<impl Into<String>>) -> Self {
        self.model = model.map(Into::into);
        self
    }

    /// Builder: attach the operating-system PID surfaced by the
    /// transport backend.
    #[must_use]
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }

    /// Builder: attach the opaque launch fingerprint observed for the PID.
    ///
    /// This is kept separate from [`Self::with_pid`] because the core does not
    /// perform operating-system I/O and therefore cannot obtain the token.
    #[must_use]
    pub fn with_pid_start_time(mut self, pid_start_time: u64) -> Self {
        self.pid_start_time = Some(pid_start_time);
        self
    }

    /// Builder: override the lifecycle status (default
    /// [`WorkerStatus::Active`]).
    #[must_use]
    pub fn with_status(mut self, status: WorkerStatus) -> Self {
        self.status = status;
        self
    }

    /// Builder: stamp an explicit `started_at` instant. Useful for
    /// tests and for migration code that must reconstruct a record
    /// from legacy state.
    #[must_use]
    pub fn with_started_at(mut self, when: DateTime<Utc>) -> Self {
        self.started_at = when;
        self
    }

    /// Returns `true` when the recorded transport status indicates the
    /// worker is up and accepting work.
    ///
    /// This is the local definition — it consults the persisted
    /// status only. Callers wanting transport-truth must additionally
    /// probe the tmux session via `cosmon_transport`.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            WorkerStatus::Starting | WorkerStatus::Active | WorkerStatus::Paused
        )
    }
}

/// Project a fresh external observation onto the two-step worker-status
/// scale, escalating from the previously-recorded [`MoleculeProcess::status`].
///
/// This is the **reader** half of the worker-liveness watchdog:
/// an external probe writes a [`Witness`] (`pane-died` hook on hard death,
/// `cs patrol` pure-observation on the figé-mais-vivant case), and this
/// function projects that witness onto `MoleculeProcess.status`. It reuses
/// the already-shipped [`WorkerStatus::{Active, Unresponsive, Stale}`]
/// scale (`worker.rs`) rather than inventing a third liveness register —
/// `Witness.process` is the single owner of the signal, `process.status`
/// is its projection.
///
/// The scale is deliberately two-coup (slow to condemn):
///
/// | observation                     | projected status        |
/// |---------------------------------|-------------------------|
/// | `Alive` and fresh (≤ `ttl`)     | [`WorkerStatus::Active`] |
/// | `Unknown` once (prev was fresh) | [`WorkerStatus::Unresponsive`] |
/// | `Dead`                          | [`WorkerStatus::Stale`] |
/// | `Unknown` again (prev already   | [`WorkerStatus::Stale`] |
/// | `Unresponsive`/`Stale`)         |                         |
///
/// An `Alive` witness older than `ttl` is **read as `Unknown`** (I10 —
/// `SilenceIsSignal`): a stale "alive" is no longer trustworthy, so it
/// counts as a missed check on the escalation ladder. A hard `Dead`
/// (kernel saw the pane die) skips straight to `Stale` — there is nothing
/// to wait for.
///
/// Pure and total: no I/O, deterministic in its arguments. The probe that
/// records the witness is responsible for persisting the returned status;
/// the worker process itself must NEVER call this (writer discipline I2).
#[must_use]
pub fn project_process_status(
    prev: &WorkerStatus,
    witness: &Witness,
    now: DateTime<Utc>,
    ttl: Duration,
) -> WorkerStatus {
    // I10 — a recorded `Alive` older than its TTL is demoted to `Unknown`
    // before it touches the ladder. "Alive" is only ever "alive as of N
    // seconds ago".
    let effective = match witness.process {
        Liveness::Alive if witness.is_stale(now, ttl) => Liveness::Unknown,
        other => other,
    };

    match effective {
        Liveness::Alive => WorkerStatus::Active,
        // Hard death observed by the kernel — no second chance.
        Liveness::Dead => WorkerStatus::Stale,
        // First miss → slow, not dead (don't kill). A second consecutive
        // miss (prev already on the failing rungs) → presumed dead.
        Liveness::Unknown => {
            if matches!(prev, WorkerStatus::Unresponsive | WorkerStatus::Stale) {
                WorkerStatus::Stale
            } else {
                WorkerStatus::Unresponsive
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_defaults_to_active_no_pid() {
        let wid = WorkerId::new("worker-test").unwrap();
        let p = MoleculeProcess::new(wid.clone(), "task-20260426-deadbeef");
        assert_eq!(p.worker_id, wid);
        assert_eq!(p.tmux_session, "task-20260426-deadbeef");
        assert!(matches!(p.status, WorkerStatus::Active));
        assert!(p.pid.is_none());
        assert!(p.is_active());
    }

    #[test]
    fn test_with_pid() {
        let wid = WorkerId::new("w").unwrap();
        let p = MoleculeProcess::new(wid, "s")
            .with_pid(4242)
            .with_pid_start_time(123_456);
        assert_eq!(p.pid, Some(4242));
        assert_eq!(p.pid_start_time, Some(123_456));
    }

    #[test]
    fn test_with_status_stopped_is_not_active() {
        let wid = WorkerId::new("w").unwrap();
        let p = MoleculeProcess::new(wid, "s").with_status(WorkerStatus::Stopped);
        assert!(!p.is_active());
    }

    #[test]
    fn test_serde_roundtrip_minimal() {
        let wid = WorkerId::new("w").unwrap();
        let p = MoleculeProcess::new(wid, "session");
        let json = serde_json::to_string(&p).unwrap();
        let back: MoleculeProcess = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn test_serde_roundtrip_with_pid() {
        let wid = WorkerId::new("w").unwrap();
        let p = MoleculeProcess::new(wid, "session")
            .with_pid(123)
            .with_pid_start_time(456);
        let json = serde_json::to_string(&p).unwrap();
        let back: MoleculeProcess = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn test_with_adapter_name_round_trips_through_serde() {
        let wid = WorkerId::new("w").unwrap();
        let p = MoleculeProcess::new(wid, "session").with_adapter_name("openai");
        assert_eq!(p.adapter_name.as_deref(), Some("openai"));
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            json.contains("\"adapter_name\":\"openai\""),
            "adapter_name must be serialized when present, got: {json}"
        );
        let back: MoleculeProcess = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn test_adapter_name_defaults_to_none_for_legacy_records() {
        // Legacy JSON without the adapter_name field must deserialize
        // cleanly — the field opted in via serde(default).
        let json = r#"{
            "worker_id": "w",
            "tmux_session": "s",
            "started_at": "2026-05-18T00:00:00Z",
            "status": "active"
        }"#;
        let back: MoleculeProcess = serde_json::from_str(json).unwrap();
        assert!(back.adapter_name.is_none());
    }

    #[test]
    fn test_serde_pid_omitted_when_none() {
        let wid = WorkerId::new("w").unwrap();
        let p = MoleculeProcess::new(wid, "session");
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("\"pid\""),
            "pid: None should be skipped in serialization, got: {json}"
        );
    }

    // ── project_process_status — the two-coup scale (C2) ──────────────────

    use crate::run_state::BranchState;

    fn ttl() -> Duration {
        Duration::from_secs(300)
    }

    fn witness_at(process: Liveness, age_secs: i64) -> Witness {
        Witness::at(
            Utc::now() - chrono::Duration::seconds(age_secs),
            process,
            BranchState::Unmerged,
        )
    }

    #[test]
    fn project_fresh_alive_is_active() {
        let w = witness_at(Liveness::Alive, 5);
        assert_eq!(
            project_process_status(&WorkerStatus::Active, &w, Utc::now(), ttl()),
            WorkerStatus::Active
        );
    }

    #[test]
    fn project_hard_dead_is_stale_in_one_coup() {
        // The kill -9 / pane-died case: a kernel-observed death goes
        // straight to Stale regardless of the previous status.
        let w = witness_at(Liveness::Dead, 0);
        assert_eq!(
            project_process_status(&WorkerStatus::Active, &w, Utc::now(), ttl()),
            WorkerStatus::Stale,
            "a hard Dead witness must condemn in one coup"
        );
    }

    #[test]
    fn project_first_unknown_is_unresponsive() {
        let w = witness_at(Liveness::Unknown, 0);
        assert_eq!(
            project_process_status(&WorkerStatus::Active, &w, Utc::now(), ttl()),
            WorkerStatus::Unresponsive,
            "a first missed check is slow-not-dead"
        );
    }

    #[test]
    fn project_second_unknown_escalates_to_stale() {
        let w = witness_at(Liveness::Unknown, 0);
        assert_eq!(
            project_process_status(&WorkerStatus::Unresponsive, &w, Utc::now(), ttl()),
            WorkerStatus::Stale,
            "a second consecutive miss is presumed dead"
        );
    }

    #[test]
    fn project_stale_alive_reads_as_unknown_i10() {
        // An `Alive` older than the TTL is no longer trustworthy: it must
        // count as a missed check, not a fresh heartbeat (the figé-mais-
        // vivant case — process up, artefact mtime frozen).
        let w = witness_at(Liveness::Alive, 600); // > 300s ttl
        assert_eq!(
            project_process_status(&WorkerStatus::Active, &w, Utc::now(), ttl()),
            WorkerStatus::Unresponsive,
            "a stale Alive reads as Unknown → first miss → Unresponsive"
        );
        // And on the next sweep it escalates.
        assert_eq!(
            project_process_status(&WorkerStatus::Unresponsive, &w, Utc::now(), ttl()),
            WorkerStatus::Stale,
        );
    }

    #[test]
    fn project_alive_recovers_from_unresponsive() {
        // A worker that goes quiet then produces again recovers — the
        // ladder is not a ratchet.
        let w = witness_at(Liveness::Alive, 2);
        assert_eq!(
            project_process_status(&WorkerStatus::Unresponsive, &w, Utc::now(), ttl()),
            WorkerStatus::Active,
            "fresh progress recovers an Unresponsive worker to Active"
        );
    }

    #[test]
    fn project_is_pure() {
        let w = witness_at(Liveness::Unknown, 0);
        let now = Utc::now();
        let a = project_process_status(&WorkerStatus::Active, &w, now, ttl());
        let b = project_process_status(&WorkerStatus::Active, &w, now, ttl());
        assert_eq!(a, b);
    }
}
