// SPDX-License-Identifier: AGPL-3.0-only

//! Shared event-log rendering for `cs run` and `cs watch`.
//!
//! Both commands observe the same source of truth — the `.cosmon/state/`
//! JSON files — and project changes into a diff-based, timestamped event
//! log. Extracting the rendering here keeps their output formats identical:
//! `cs run` (action-bearing, dispatches the DAG) and `cs watch` (read-only
//! observer) look the same to the operator.
//!
//! ## What lives here
//!
//! - [`Snapshot`] and [`build_snapshot`] — the minimal view of fleet +
//!   molecule state that the renderer diffs across polls.
//! - [`WatchEvent`] and [`diff`] — the structural transitions the log can
//!   report. New mutation kinds must land as new variants so the log
//!   never silently loses information.
//! - [`render_event`], [`render_heartbeat`], [`render_baseline_header`],
//!   [`format_elapsed`] — pure rendering functions (clock + data in,
//!   string out). Testable without any I/O.
//! - [`poll_and_diff`] — load from a [`FileStore`], diff against a prior
//!   snapshot, return the resulting events.
//! - [`print_events`] and [`print_baseline`] — the tiny I/O helpers that
//!   write lines to stdout while keeping the rolling heartbeat footer
//!   intact.
//!
//! Watch-specific concerns (propel cadence, `Deadlines`, `Tier`) stay in
//! [`crate::cmd::watch`]; run-specific concerns (runtime dispatch, Ctrl-C
//! wiring) stay in [`crate::cmd::run`]. Only the rendering contract is
//! shared.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use colored::Colorize;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::worker::{DesiredState, WorkerStatus};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, MoleculeFilter, StateStore};
use cosmon_style::{format_status, format_worker_status};

// ---------------------------------------------------------------------------
// Constants shared by watch and run.
// ---------------------------------------------------------------------------

/// Heartbeat refresh cadence. ~4 frames per second feels alive without
/// being distracting; the spinner rotation is what signals liveness, not
/// the refresh frequency.
pub(crate) const HEARTBEAT_INTERVAL_MS: u64 = 250;

/// Upper bound on the main loop sleep. Keeps Ctrl-C responsive even when
/// the next deadline is far away: we never sleep longer than this, so the
/// process wakes often enough to react cleanly.
pub(crate) const LOOP_SLEEP_MS: u64 = 50;

/// Minimal braille spinner — eight frames, rotated once per heartbeat
/// refresh.
pub(crate) const SPINNER_FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

// ---------------------------------------------------------------------------
// Snapshot — the minimal view of state the event log cares about.
// ---------------------------------------------------------------------------

/// Worker fields that the event log tracks for diffing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerView {
    pub desired: DesiredState,
    pub status: WorkerStatus,
}

/// Molecule fields that the event log tracks for diffing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoleculeView {
    pub status: MoleculeStatus,
    pub assigned_worker: Option<WorkerId>,
    pub current_step: usize,
    pub total_steps: usize,
}

/// A poll-scoped snapshot of the fleet and molecule pool.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub workers: HashMap<WorkerId, WorkerView>,
    pub molecules: HashMap<MoleculeId, MoleculeView>,
}

/// Build a [`Snapshot`] from raw store data. Pure — no I/O.
pub(crate) fn build_snapshot(fleet: &Fleet, molecules: &[MoleculeData]) -> Snapshot {
    let workers = fleet
        .workers
        .iter()
        .map(|(id, w)| {
            (
                id.clone(),
                WorkerView {
                    desired: w.desired,
                    status: w.status.clone(),
                },
            )
        })
        .collect();
    let molecules = molecules
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                MoleculeView {
                    status: m.status,
                    assigned_worker: m.assigned_worker.clone(),
                    current_step: m.current_step,
                    total_steps: m.total_steps,
                },
            )
        })
        .collect();
    Snapshot { workers, molecules }
}

// ---------------------------------------------------------------------------
// Diff — the set of state transitions observed between two snapshots.
// ---------------------------------------------------------------------------

/// A single state transition the event log emits as a log line.
///
/// Variants intentionally mirror the structural mutations the transactional
/// core allows — nothing more. New mutation kinds require a new variant
/// so the log never silently loses information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WatchEvent {
    /// A worker was not in the previous snapshot.
    WorkerAdded { id: WorkerId, view: WorkerView },
    /// A worker disappeared from the fleet (purge, teardown).
    WorkerRemoved { id: WorkerId },
    /// Worker status changed (the observed/health side).
    WorkerStatusChanged {
        id: WorkerId,
        from: WorkerStatus,
        to: WorkerStatus,
    },
    /// Worker desired state changed (the intent side).
    WorkerDesiredChanged {
        id: WorkerId,
        from: DesiredState,
        to: DesiredState,
    },
    /// A molecule appeared (nucleation or first observation).
    MoleculeAdded { id: MoleculeId, view: MoleculeView },
    /// A molecule disappeared (purge).
    MoleculeRemoved { id: MoleculeId },
    /// Molecule status changed — the single most important signal.
    MoleculeStatusChanged {
        id: MoleculeId,
        from: MoleculeStatus,
        to: MoleculeStatus,
    },
    /// Molecule step advanced (evolve).
    MoleculeStepChanged {
        id: MoleculeId,
        from: usize,
        to: usize,
        total: usize,
    },
    /// Molecule was reassigned (rare — orphan recovery).
    MoleculeWorkerChanged {
        id: MoleculeId,
        from: Option<WorkerId>,
        to: Option<WorkerId>,
    },
    /// A propel nudge was sent to a worker. Emitted by the watch propel
    /// tier, not by the raw disk diff.
    Propelled {
        worker: WorkerId,
        molecule: MoleculeId,
        stale_seconds: i64,
    },
    /// A stale molecule was detected but no nudge was sent (dry-run or
    /// `--no-tmux`).
    StaleDetected {
        worker: WorkerId,
        molecule: MoleculeId,
        stale_seconds: i64,
    },
}

/// Diff two snapshots into an ordered list of transition events.
///
/// Pure: no clock, no I/O, no randomness. Events are sorted by id within
/// each category (workers first, then molecules) — a stable order that
/// makes scrollback readable.
pub(crate) fn diff(prev: &Snapshot, next: &Snapshot) -> Vec<WatchEvent> {
    let mut events = Vec::new();

    // Workers — additions and mutations, iterated in id-sorted order.
    let mut worker_ids: Vec<&WorkerId> = next.workers.keys().collect();
    worker_ids.sort_by_key(|id| id.as_str());
    for id in worker_ids {
        let new_view = &next.workers[id];
        match prev.workers.get(id) {
            None => events.push(WatchEvent::WorkerAdded {
                id: id.clone(),
                view: new_view.clone(),
            }),
            Some(old_view) if old_view == new_view => {}
            Some(old_view) => {
                if old_view.status != new_view.status {
                    events.push(WatchEvent::WorkerStatusChanged {
                        id: id.clone(),
                        from: old_view.status.clone(),
                        to: new_view.status.clone(),
                    });
                }
                if old_view.desired != new_view.desired {
                    events.push(WatchEvent::WorkerDesiredChanged {
                        id: id.clone(),
                        from: old_view.desired,
                        to: new_view.desired,
                    });
                }
            }
        }
    }
    // Workers — removals.
    let mut removed_workers: Vec<&WorkerId> = prev
        .workers
        .keys()
        .filter(|id| !next.workers.contains_key(*id))
        .collect();
    removed_workers.sort_by_key(|id| id.as_str());
    for id in removed_workers {
        events.push(WatchEvent::WorkerRemoved { id: id.clone() });
    }

    // Molecules — additions and mutations, sorted by id string.
    let mut mol_ids: Vec<&MoleculeId> = next.molecules.keys().collect();
    mol_ids.sort_by_key(ToString::to_string);
    for id in mol_ids {
        let new_view = &next.molecules[id];
        match prev.molecules.get(id) {
            None => events.push(WatchEvent::MoleculeAdded {
                id: id.clone(),
                view: new_view.clone(),
            }),
            Some(old_view) if old_view == new_view => {}
            Some(old_view) => {
                if old_view.status != new_view.status {
                    events.push(WatchEvent::MoleculeStatusChanged {
                        id: id.clone(),
                        from: old_view.status,
                        to: new_view.status,
                    });
                }
                if old_view.current_step != new_view.current_step {
                    events.push(WatchEvent::MoleculeStepChanged {
                        id: id.clone(),
                        from: old_view.current_step,
                        to: new_view.current_step,
                        total: new_view.total_steps,
                    });
                }
                if old_view.assigned_worker != new_view.assigned_worker {
                    events.push(WatchEvent::MoleculeWorkerChanged {
                        id: id.clone(),
                        from: old_view.assigned_worker.clone(),
                        to: new_view.assigned_worker.clone(),
                    });
                }
            }
        }
    }
    let mut removed_mols: Vec<&MoleculeId> = prev
        .molecules
        .keys()
        .filter(|id| !next.molecules.contains_key(*id))
        .collect();
    removed_mols.sort_by_key(ToString::to_string);
    for id in removed_mols {
        events.push(WatchEvent::MoleculeRemoved { id: id.clone() });
    }

    events
}

// ---------------------------------------------------------------------------
// Rendering — stable, human-readable log lines.
// ---------------------------------------------------------------------------

/// Format a timestamp as the `HH:MM:SS` log prefix. UTC matches the rest
/// of cosmon's event log.
fn format_ts(ts: DateTime<Utc>) -> String {
    ts.format("%H:%M:%S").to_string()
}

/// Format a monotonic elapsed duration for the heartbeat footer.
pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Fixed visible width for padded molecule status columns. Covers the
/// longest rendered status (`"◌ completed"` = 11 visible chars: 1 glyph
/// + 1 space + 9-char slug).
const STATUS_PAD_WIDTH: usize = 11;

/// Fixed visible width for padded short molecule IDs (the suffix).
/// Typical suffixes are 4 hex chars; 8 is generous future-proofing.
const MOL_SHORT_PAD_WIDTH: usize = 8;

/// Short display form for a molecule ID — uses the suffix (last segment)
/// to reduce noise in sequential log lines.
fn short_mol(id: &MoleculeId) -> &str {
    id.suffix()
}

/// Render a molecule status with trailing padding so that subsequent
/// columns align vertically. The padding compensates for variable-length
/// status slugs (e.g. `"stuck"` vs `"completed"`) while preserving ANSI
/// color codes.
fn padded_status(status: MoleculeStatus) -> String {
    let rendered = format_status(status);
    let slug = cosmon_core::visual::Status::for_molecule_status(status).slug();
    // Visible width: 1 (glyph) + 1 (space) + slug length.
    let visible = 2 + slug.len();
    let pad = STATUS_PAD_WIDTH.saturating_sub(visible);
    format!("{rendered}{:pad$}", "")
}

/// Pad a short molecule ID (suffix) to a fixed width for column alignment.
fn padded_short_mol(id: &MoleculeId) -> String {
    format!("{:<width$}", short_mol(id), width = MOL_SHORT_PAD_WIDTH)
}

/// Format an optional worker for display. When the value is `None`, returns
/// `"(none)"` instead of a cryptic dash.
fn fmt_worker(w: Option<&WorkerId>) -> String {
    w.map_or_else(|| "(none)".to_owned(), |w| w.as_str().to_owned())
}

/// Render one [`WatchEvent`] as a single log line (no trailing newline).
///
/// Status words are painted via [`format_status`], which routes
/// through the charter in `cosmon-style`. The prefix glyphs here
/// (`+`, `-`, `~`, `⚛`, `!`) are ASCII-visible markers for the
/// event kind — they are not on the six-status STROKE axis and so
/// live locally rather than in the charter.
///
/// ## Conventions
///
/// - **Prefix glyphs**: `+` new, `~` changed, `-` removed, `⚛` propel, `!` stale.
/// - **Separator**: colon before field values, consistently in both snapshot
///   and diff lines (e.g. `status: ○ pending`, `step: 0/2`).
/// - **Arrows**: `→` separates old → new in diff lines.
/// - **None workers**: shown as `(none)`, never as a bare dash.
/// - **Molecule IDs**: short suffix form (e.g. `0922`) to reduce noise.
///   The full ID appears in the baseline header; subsequent lines use the
///   suffix.
pub(crate) fn render_event(ts: DateTime<Utc>, ev: &WatchEvent) -> String {
    let t = format_ts(ts);
    let added = "+";
    let removed = "-";
    let splice = "~";
    let propel = "\u{269B}"; // ⚛
    let stale = "!";
    match ev {
        WatchEvent::WorkerAdded { id, view } => {
            format!(
                "[{t}] {added} worker {id}  status: {}  desired: {}",
                format_worker_status(&view.status),
                view.desired
            )
        }
        WatchEvent::WorkerRemoved { id } => {
            format!("[{t}] {removed} worker {id}")
        }
        WatchEvent::WorkerStatusChanged { id, from, to } => {
            format!(
                "[{t}] {splice} worker {id}  status: {} → {}",
                format_worker_status(from),
                format_worker_status(to)
            )
        }
        WatchEvent::WorkerDesiredChanged { id, from, to } => {
            format!("[{t}] {splice} worker {id}  desired: {from} → {to}")
        }
        WatchEvent::MoleculeAdded { id, view } => {
            let worker = fmt_worker(view.assigned_worker.as_ref());
            format!(
                "[{t}] {added} molecule {id}  status: {}  step: {}/{}  worker: {worker}",
                padded_status(view.status),
                view.current_step,
                view.total_steps,
            )
        }
        WatchEvent::MoleculeRemoved { id } => {
            format!("[{t}] {removed} molecule {}", padded_short_mol(id))
        }
        WatchEvent::MoleculeStatusChanged { id, from, to } => {
            format!(
                "[{t}] {splice} molecule {}  status: {} → {}",
                padded_short_mol(id),
                padded_status(*from),
                format_status(*to)
            )
        }
        WatchEvent::MoleculeStepChanged {
            id,
            from,
            to,
            total,
        } => {
            format!(
                "[{t}] {splice} molecule {}  step: {from} → {to}/{total}",
                padded_short_mol(id)
            )
        }
        WatchEvent::MoleculeWorkerChanged { id, from, to } => {
            let f = fmt_worker(from.as_ref());
            let t2 = fmt_worker(to.as_ref());
            if f == "(none)" {
                format!(
                    "[{t}] {splice} molecule {}  worker assigned: {t2}",
                    padded_short_mol(id)
                )
            } else {
                format!(
                    "[{t}] {splice} molecule {}  worker: {f} → {t2}",
                    padded_short_mol(id)
                )
            }
        }
        WatchEvent::Propelled {
            worker,
            molecule,
            stale_seconds,
        } => {
            format!(
                "[{t}] {propel} PROPEL {worker} ← {} (stale {stale_seconds}s)",
                short_mol(molecule)
            )
        }
        WatchEvent::StaleDetected {
            worker,
            molecule,
            stale_seconds,
        } => {
            format!(
                "[{t}] {stale} STALE {worker} ← {} (stale {stale_seconds}s, no nudge)",
                short_mol(molecule)
            )
        }
    }
}

/// Render the single-line rolling heartbeat shown at the bottom of the
/// terminal.
///
/// `label` lets the caller distinguish the two commands: `"watch"` for
/// `cs watch`, `"run"` for `cs run`. Otherwise the shape is identical —
/// operators reading either command's output see the same format.
pub(crate) fn render_heartbeat(
    label: &str,
    now: DateTime<Local>,
    elapsed: Duration,
    spinner_frame: &str,
    workers: usize,
    running: usize,
) -> String {
    let plural = if workers == 1 { "" } else { "s" };
    format!(
        "{spinner_frame} {} · {label} · {} · {workers} worker{plural} · {running} running",
        now.format("%H:%M:%S"),
        format_elapsed(elapsed)
    )
}

/// Render the baseline snapshot header — printed once at the first poll
/// before the initial `+` events. Includes a one-line legend for the
/// prefix glyphs used in subsequent log lines.
pub(crate) fn render_baseline_header(
    ts: DateTime<Utc>,
    workers: usize,
    molecules: usize,
) -> String {
    format!(
        "[{}] BASELINE {workers} worker(s), {molecules} molecule(s)  [+ new  ~ changed  - removed]",
        format_ts(ts)
    )
}

// ---------------------------------------------------------------------------
// Poll pass — load state, diff against a prior snapshot.
// ---------------------------------------------------------------------------

/// Result of one state-poll pass.
#[derive(Debug, Clone)]
pub(crate) struct PollOutcome {
    /// Snapshot taken at the start of this poll (becomes the next `prev`).
    pub snapshot: Snapshot,
    /// Structural diff events.
    pub events: Vec<WatchEvent>,
}

/// Load the fleet and molecule pool, diff against `prev`, return the
/// resulting events and new snapshot. Pure state projection — no propel
/// logic here, no nudges sent.
///
/// Passing `prev = None` signals "first poll of the session"; the
/// returned events will include an addition for every worker and
/// molecule currently on disk.
pub(crate) fn poll_and_diff(
    store: &FileStore,
    prev: Option<&Snapshot>,
) -> anyhow::Result<PollOutcome> {
    let fleet = store.load_fleet()?;
    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let snap = build_snapshot(&fleet, &molecules);
    let prev_snap = prev.cloned().unwrap_or_default();
    let events = diff(&prev_snap, &snap);
    Ok(PollOutcome {
        snapshot: snap,
        events,
    })
}

// ---------------------------------------------------------------------------
// I/O helpers — stdout writers that respect the heartbeat footer.
// ---------------------------------------------------------------------------

/// Clear the current terminal line (carriage return + clear-to-end-of-line).
/// Only emitted when stdout is a TTY — piping to a file should never
/// produce escape sequences.
pub(crate) fn clear_line(out: &mut impl Write, tty: bool) -> std::io::Result<()> {
    if tty {
        write!(out, "\r\x1b[K")?;
    }
    Ok(())
}

/// Print a batch of events as append-only log lines. Clears the heartbeat
/// footer first so the scrollback never shows partial overlaps, then
/// flushes. Does nothing when `events` is empty so the footer stays put.
pub(crate) fn print_events(
    out: &mut impl Write,
    tty: bool,
    now: DateTime<Utc>,
    events: &[WatchEvent],
) -> std::io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    clear_line(out, tty)?;
    for ev in events {
        writeln!(out, "{}", render_event(now, ev))?;
    }
    out.flush()?;
    Ok(())
}

/// Print the baseline header + initial `+` events for the very first
/// poll. After this runs, the operator sees everything that was on disk
/// when the command started.
pub(crate) fn print_baseline(
    out: &mut impl Write,
    tty: bool,
    now: DateTime<Utc>,
    outcome: &PollOutcome,
) -> std::io::Result<()> {
    clear_line(out, tty)?;
    writeln!(
        out,
        "{}",
        render_baseline_header(
            now,
            outcome.snapshot.workers.len(),
            outcome.snapshot.molecules.len()
        )
        .bold()
    )?;
    for ev in &outcome.events {
        writeln!(out, "{}", render_event(now, ev))?;
    }
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{Local, TimeZone, Utc};
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
    use tempfile::TempDir;

    use super::*;

    fn make_worker(name: &str, desired: DesiredState, status: WorkerStatus) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            status,
        );
        w.desired = desired;
        w
    }

    fn make_molecule(
        id: &str,
        status: MoleculeStatus,
        worker: Option<&str>,
        step: usize,
    ) -> MoleculeData {
        MoleculeData {
            fleet_id: FleetId::new("default").unwrap(),
            id: MoleculeId::new(id).unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: worker.map(|w| WorkerId::new(w).unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: step,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn build_snapshot_captures_minimal_fields() {
        let mut fleet = Fleet::default();
        let w = make_worker("quartz", DesiredState::Running, WorkerStatus::Active);
        fleet.workers.insert(w.id.clone(), w);
        let mols = vec![make_molecule(
            "cs-20260409-aaaa",
            MoleculeStatus::Running,
            Some("quartz"),
            1,
        )];

        let snap = build_snapshot(&fleet, &mols);
        assert_eq!(snap.workers.len(), 1);
        assert_eq!(snap.molecules.len(), 1);
        let v = &snap.workers[&WorkerId::new("quartz").unwrap()];
        assert_eq!(v.desired, DesiredState::Running);
        assert_eq!(v.status, WorkerStatus::Active);
    }

    #[test]
    fn diff_empty_prev_emits_additions() {
        let mut fleet = Fleet::default();
        let w = make_worker("quartz", DesiredState::Running, WorkerStatus::Active);
        fleet.workers.insert(w.id.clone(), w);
        let mols = vec![make_molecule(
            "cs-20260409-aaaa",
            MoleculeStatus::Running,
            Some("quartz"),
            0,
        )];
        let next = build_snapshot(&fleet, &mols);

        let events = diff(&Snapshot::default(), &next);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], WatchEvent::WorkerAdded { .. }));
        assert!(matches!(events[1], WatchEvent::MoleculeAdded { .. }));
    }

    #[test]
    fn diff_detects_worker_status_change() {
        let mut fleet = Fleet::default();
        fleet.workers.insert(
            WorkerId::new("quartz").unwrap(),
            make_worker("quartz", DesiredState::Running, WorkerStatus::Active),
        );
        let prev = build_snapshot(&fleet, &[]);

        fleet.workers.insert(
            WorkerId::new("quartz").unwrap(),
            make_worker(
                "quartz",
                DesiredState::Running,
                WorkerStatus::Error("boom".to_owned()),
            ),
        );
        let next = build_snapshot(&fleet, &[]);

        let events = diff(&prev, &next);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WatchEvent::WorkerStatusChanged { from, to, .. } => {
                assert_eq!(*from, WorkerStatus::Active);
                assert_eq!(*to, WorkerStatus::Error("boom".to_owned()));
            }
            _ => panic!("expected WorkerStatusChanged"),
        }
    }

    #[test]
    fn diff_detects_worker_removal() {
        let mut fleet = Fleet::default();
        fleet.workers.insert(
            WorkerId::new("quartz").unwrap(),
            make_worker("quartz", DesiredState::Running, WorkerStatus::Active),
        );
        let prev = build_snapshot(&fleet, &[]);
        let next = build_snapshot(&Fleet::default(), &[]);

        let events = diff(&prev, &next);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::WorkerRemoved { .. }));
    }

    #[test]
    fn diff_detects_molecule_step_and_status() {
        let mut fleet = Fleet::default();
        fleet.workers.insert(
            WorkerId::new("quartz").unwrap(),
            make_worker("quartz", DesiredState::Running, WorkerStatus::Active),
        );

        let m0 = make_molecule(
            "cs-20260409-bbbb",
            MoleculeStatus::Running,
            Some("quartz"),
            0,
        );
        let prev = build_snapshot(&fleet, &[m0]);

        let m1 = make_molecule(
            "cs-20260409-bbbb",
            MoleculeStatus::Completed,
            Some("quartz"),
            2,
        );
        let next = build_snapshot(&fleet, &[m1]);

        let events = diff(&prev, &next);
        assert!(events
            .iter()
            .any(|e| matches!(e, WatchEvent::MoleculeStatusChanged { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, WatchEvent::MoleculeStepChanged { .. })));
    }

    #[test]
    fn diff_idempotent_on_identical_snapshots() {
        let mut fleet = Fleet::default();
        fleet.workers.insert(
            WorkerId::new("quartz").unwrap(),
            make_worker("quartz", DesiredState::Running, WorkerStatus::Active),
        );
        let mols = vec![make_molecule(
            "cs-20260409-cccc",
            MoleculeStatus::Running,
            Some("quartz"),
            1,
        )];
        let snap = build_snapshot(&fleet, &mols);

        let events = diff(&snap, &snap);
        assert!(events.is_empty(), "identical snapshots produce no events");
    }

    #[test]
    fn render_event_formats_worker_added() {
        let ev = WatchEvent::WorkerAdded {
            id: WorkerId::new("quartz").unwrap(),
            view: WorkerView {
                desired: DesiredState::Running,
                status: WorkerStatus::Active,
            },
        };
        let line = render_event(
            DateTime::parse_from_rfc3339("2026-04-09T12:34:56Z")
                .unwrap()
                .to_utc(),
            &ev,
        );
        assert!(line.contains("12:34:56"));
        assert!(line.contains("+ worker quartz"));
        assert!(
            line.contains("status: active"),
            "expected colon separator: {line}"
        );
        assert!(
            line.contains("desired: running"),
            "expected colon separator: {line}"
        );
    }

    #[test]
    fn render_event_molecule_added_uses_colon_separator() {
        let ev = WatchEvent::MoleculeAdded {
            id: MoleculeId::new("task-20260409-aaaa").unwrap(),
            view: MoleculeView {
                status: MoleculeStatus::Running,
                assigned_worker: Some(WorkerId::new("quartz").unwrap()),
                current_step: 1,
                total_steps: 2,
            },
        };
        let line = render_event(Utc::now(), &ev);
        assert!(
            line.contains("status:"),
            "snapshot should use colon: {line}"
        );
        assert!(
            line.contains("step: 1/2"),
            "snapshot should use colon: {line}"
        );
        assert!(
            line.contains("worker: quartz"),
            "snapshot should use colon: {line}"
        );
    }

    #[test]
    fn render_event_molecule_added_shows_none_worker() {
        let ev = WatchEvent::MoleculeAdded {
            id: MoleculeId::new("task-20260409-bbbb").unwrap(),
            view: MoleculeView {
                status: MoleculeStatus::Pending,
                assigned_worker: None,
                current_step: 0,
                total_steps: 2,
            },
        };
        let line = render_event(Utc::now(), &ev);
        assert!(
            line.contains("worker: (none)"),
            "None worker should display as (none): {line}"
        );
        assert!(
            !line.contains("worker: -"),
            "None worker must not be a bare dash: {line}"
        );
    }

    #[test]
    fn render_event_molecule_worker_assigned_from_none() {
        let ev = WatchEvent::MoleculeWorkerChanged {
            id: MoleculeId::new("task-20260409-cccc").unwrap(),
            from: None,
            to: Some(WorkerId::new("mission-e027").unwrap()),
        };
        let line = render_event(Utc::now(), &ev);
        assert!(
            line.contains("worker assigned: mission-e027"),
            "None→value should say 'worker assigned': {line}"
        );
        assert!(
            !line.contains("(none) →"),
            "should not show (none) → when assigning: {line}"
        );
    }

    #[test]
    fn render_event_molecule_worker_reassigned() {
        let ev = WatchEvent::MoleculeWorkerChanged {
            id: MoleculeId::new("task-20260409-dddd").unwrap(),
            from: Some(WorkerId::new("old-worker").unwrap()),
            to: Some(WorkerId::new("new-worker").unwrap()),
        };
        let line = render_event(Utc::now(), &ev);
        assert!(
            line.contains("worker: old-worker → new-worker"),
            "reassignment should show from → to: {line}"
        );
    }

    #[test]
    fn render_event_uses_short_molecule_id_in_diffs() {
        let ev = WatchEvent::MoleculeStatusChanged {
            id: MoleculeId::new("task-20260409-abcd").unwrap(),
            from: MoleculeStatus::Pending,
            to: MoleculeStatus::Running,
        };
        let line = render_event(Utc::now(), &ev);
        assert!(
            line.contains("molecule abcd"),
            "diff lines should use short suffix: {line}"
        );
        assert!(
            !line.contains("task-20260409-abcd"),
            "diff lines should not use full ID: {line}"
        );
    }

    #[test]
    fn render_baseline_header_includes_legend() {
        let header = render_baseline_header(Utc::now(), 2, 3);
        assert!(
            header.contains("+ new"),
            "baseline header should include legend: {header}"
        );
        assert!(
            header.contains("~ changed"),
            "baseline header should include legend: {header}"
        );
        assert!(
            header.contains("- removed"),
            "baseline header should include legend: {header}"
        );
    }

    fn fixed_local(h: u32, m: u32, s: u32) -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 4, 9, h, m, s)
            .single()
            .expect("valid local datetime")
    }

    #[test]
    fn render_heartbeat_contains_label_and_state() {
        let hb = render_heartbeat(
            "watch",
            fixed_local(14, 30, 45),
            Duration::from_secs(42),
            "⣾",
            3,
            2,
        );
        assert!(hb.contains("watch"), "heartbeat missing label: {hb}");
        assert!(hb.contains("42s"), "heartbeat missing elapsed: {hb}");
        assert!(
            hb.contains("3 worker"),
            "heartbeat missing worker count: {hb}"
        );
        assert!(hb.contains("2 running"), "heartbeat missing running: {hb}");
        assert!(hb.contains("⣾"), "heartbeat missing spinner: {hb}");
        assert!(
            hb.contains("14:30:45"),
            "heartbeat missing wall-clock time: {hb}"
        );
    }

    #[test]
    fn render_heartbeat_uses_run_label_when_requested() {
        let hb = render_heartbeat(
            "run",
            fixed_local(14, 30, 45),
            Duration::from_secs(1),
            "⣾",
            1,
            1,
        );
        assert!(hb.contains("· run ·"), "heartbeat missing run label: {hb}");
    }

    #[test]
    fn render_heartbeat_singular_worker_has_no_plural_s() {
        let hb = render_heartbeat(
            "watch",
            fixed_local(9, 0, 0),
            Duration::from_secs(5),
            "⣾",
            1,
            0,
        );
        assert!(hb.contains("1 worker "), "expected singular form: {hb}");
        assert!(!hb.contains("1 workers"), "unexpected plural: {hb}");
    }

    /// Regression guard: the heartbeat must not leak implementation
    /// details like tick counters or next-deadline countdowns.
    #[test]
    fn render_heartbeat_has_no_implementation_leakage() {
        let hb = render_heartbeat(
            "watch",
            fixed_local(1, 2, 3),
            Duration::from_secs(99),
            "⣾",
            5,
            3,
        );
        assert!(!hb.contains("tick="), "heartbeat leaked tick counter: {hb}");
        assert!(!hb.contains("next="), "heartbeat leaked countdown: {hb}");
        assert!(!hb.contains("interval"), "heartbeat leaked interval: {hb}");
        assert!(
            !hb.contains("/60s"),
            "heartbeat leaked interval window: {hb}"
        );
        let colons = hb.chars().filter(|c| *c == ':').count();
        assert!(
            colons >= 2,
            "heartbeat missing HH:MM:SS wall-clock (colons={colons}): {hb}"
        );
        assert!(
            hb.contains("01:02:03"),
            "heartbeat missing expected wall-clock time: {hb}"
        );
    }

    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_elapsed(Duration::from_secs(3599)), "59m59s");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "1h00m00s");
        assert_eq!(format_elapsed(Duration::from_secs(3725)), "1h02m05s");
    }

    #[test]
    fn poll_and_diff_on_empty_fleet_is_empty() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::default()).unwrap();

        let outcome = poll_and_diff(&store, None).unwrap();
        assert!(outcome.snapshot.workers.is_empty());
        assert!(outcome.snapshot.molecules.is_empty());
        assert!(outcome.events.is_empty());
    }

    #[test]
    fn poll_and_diff_detects_new_worker_across_polls() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::default()).unwrap();

        let first = poll_and_diff(&store, None).unwrap();

        let mut fleet = Fleet::default();
        let w = make_worker("quartz", DesiredState::Running, WorkerStatus::Active);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let second = poll_and_diff(&store, Some(&first.snapshot)).unwrap();
        assert_eq!(second.events.len(), 1);
        assert!(matches!(second.events[0], WatchEvent::WorkerAdded { .. }));
    }
}
