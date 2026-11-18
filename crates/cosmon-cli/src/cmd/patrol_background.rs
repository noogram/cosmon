// SPDX-License-Identifier: AGPL-3.0-only

//! The shell side of the "background work in flight" rule (noogram/cosmon#216).
//!
//! `cosmon_core::propel::decide_nudge` is I/O-free and sees one number:
//! [`NudgeView::background_work_age`]. This module reads the two places that
//! number comes from and nothing else:
//!
//! 1. **The provider's report.** `cs sessions hook run` records, on each Claude
//!    `Stop` event, how many background tasks were still running
//!    ([`cosmon_core::presence::BackgroundTasks`]). Patrol finds the worker's
//!    presence row by its molecule and reads the age of that record.
//! 2. **A detached gate run.** `scripts/run-gates-bg.sh` writes `gates.pid` in
//!    the molecule directory and, once the run ends, `gates.exit`. It is not a
//!    provider background task, so the provider never reports it; this source
//!    is adapter-independent.
//!
//! Both sources answer "is something in flight, and since when?". Anything
//! unreadable, absent or ambiguous is `None`, which the judge decides exactly
//! as it did before the rule existed. The oldest of the two ages is returned,
//! because the ceiling bounds the *longest* stretch the worker has been waiting.
//!
//! [`NudgeView::background_work_age`]: cosmon_core::propel::NudgeView::background_work_age

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use cosmon_core::id::MoleculeId;
use cosmon_core::presence::Presence;
use cosmon_filestore::gate_slots::live_gate_slot_holders_in;
use cosmon_filestore::presence_store::{pid_is_alive, PresenceStore};
use cosmon_state::StateStore;

/// File a detached gate run writes its pid to, inside the molecule directory.
const GATES_PID_FILE: &str = "gates.pid";
/// File a detached gate run writes its exit code to when it ends.
const GATES_EXIT_FILE: &str = "gates.exit";
/// File a detached gate run writes while it waits for a gate slot (ADR-190 D1):
/// the waiting process's pid on the first line, the pool name on the second.
const GATES_QUEUED_FILE: &str = "gates.queued";

/// The cosmon state root a molecule directory lives under.
///
/// A molecule directory is `<state>/fleets/<fleet>/molecules/<id>`. Returns
/// `None` when the path does not have that shape, so a flat test directory or a
/// relocated store degrades to "unknown" instead of scanning the wrong tree.
fn state_root_of(molecule_dir: &Path) -> Option<PathBuf> {
    let molecules = molecule_dir.parent()?;
    let fleet = molecules.parent()?;
    let fleets = fleet.parent()?;
    if molecules.file_name()? != "molecules" || fleets.file_name()? != "fleets" {
        return None;
    }
    fleets.parent().map(Path::to_path_buf)
}

/// Age of a detached gate run still in flight in `molecule_dir`.
///
/// In flight means `gates.pid` names a live process and `gates.exit` does not
/// exist. A pid file whose process is gone with no exit file is a run that died
/// and is not in flight. The age is that of the pid file, which the runner
/// writes once, when it detaches.
#[must_use]
pub(crate) fn gate_run_age(molecule_dir: &Path, now: DateTime<Utc>) -> Option<Duration> {
    if molecule_dir.join(GATES_EXIT_FILE).exists() {
        return None;
    }
    let pid_path = molecule_dir.join(GATES_PID_FILE);
    let pid: u32 = std::fs::read_to_string(&pid_path)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if !pid_is_alive(pid) {
        return None;
    }
    let modified: DateTime<Utc> = std::fs::metadata(&pid_path).ok()?.modified().ok()?.into();
    Some((now - modified).max(Duration::zero()))
}

/// Whether the detached gate run in `molecule_dir` is queued for a slot of a
/// busy pool (ADR-190, amendment "Queued is not a stall").
///
/// True only when **all** of these hold: `gates.queued` names a live process,
/// neither `gates.pid` nor `gates.exit` exists, and the named pool has at
/// least one live holder (a `flock` the kernel still reports as held). Any
/// other combination — a dead queued process, a pool nobody holds, an
/// unreadable or malformed file — is unknown and returns `false`, which the
/// judge decides exactly as it did before slots existed. A leftover
/// `gates.queued` can therefore never silence patrol on its own.
#[must_use]
pub(crate) fn gate_queued_at(molecule_dir: &Path, state_root: &Path) -> bool {
    if molecule_dir.join(GATES_EXIT_FILE).exists() || molecule_dir.join(GATES_PID_FILE).exists() {
        return false;
    }
    let Ok(body) = std::fs::read_to_string(molecule_dir.join(GATES_QUEUED_FILE)) else {
        return false;
    };
    let mut lines = body.lines();
    let (Some(pid), Some(pool)) = (lines.next(), lines.next()) else {
        return false;
    };
    let Ok(pid) = pid.trim().parse::<u32>() else {
        return false;
    };
    pid_is_alive(pid)
        && live_gate_slot_holders_in(state_root, pool.trim()).is_ok_and(|held| held >= 1)
}

/// [`gate_queued_at`] for molecule `mid`, over the state root its directory
/// lives under. A molecule directory that does not have the usual shape is
/// unknown (`false`), as for [`background_work_age`].
#[must_use]
pub(crate) fn gate_run_queued(store: &dyn StateStore, mid: &MoleculeId) -> bool {
    let dir = store.molecule_dir(mid);
    state_root_of(&dir).is_some_and(|root| gate_queued_at(&dir, &root))
}

/// Age of the provider-reported background work of the session working on
/// `molecule`.
///
/// Among the presence rows that name the molecule, the most recent heartbeat is
/// the live session; an older row left by a previous session of the same
/// molecule is not consulted.
#[must_use]
pub(crate) fn reported_background_age(
    presences: &[Presence],
    molecule: &MoleculeId,
    now: DateTime<Utc>,
) -> Option<Duration> {
    presences
        .iter()
        .filter(|p| p.current_molecule.as_ref() == Some(molecule))
        .max_by_key(|p| p.heartbeat_at)?
        .background_tasks
        .as_ref()
        .map(|b| b.age(now))
}

/// The oldest background work known to be in flight for `mid`, from either
/// source, or `None` when neither reports any.
#[must_use]
pub(crate) fn background_work_age(
    store: &dyn StateStore,
    mid: &MoleculeId,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let dir = store.molecule_dir(mid);
    let gate = gate_run_age(&dir, now);
    let reported = state_root_of(&dir).and_then(|root| {
        let presences = PresenceStore::new(root).scan().ok()?;
        reported_background_age(&presences, mid, now)
    });
    gate.into_iter().chain(reported).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use cosmon_core::id::SessionId;
    use cosmon_core::presence::BackgroundTasks;
    use cosmon_filestore::gate_slots::{try_acquire_gate_slot_in, GateSlotGuard};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
    }

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20261116-e643").unwrap()
    }

    fn row(
        sid: &str,
        molecule: &MoleculeId,
        beat: DateTime<Utc>,
        since: Option<DateTime<Utc>>,
    ) -> Presence {
        Presence {
            current_molecule: Some(molecule.clone()),
            heartbeat_at: beat,
            background_tasks: since.map(|since| BackgroundTasks {
                running: 1,
                kinds: vec!["shell".to_owned()],
                since,
                observed_at: beat,
            }),
            ..Presence::new(SessionId::new(sid).unwrap(), "cosmon", "/tmp/x", 1, beat)
        }
    }

    #[test]
    fn a_live_pid_without_an_exit_file_is_a_gate_run_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gates.pid"), std::process::id().to_string()).unwrap();
        let age = gate_run_age(dir.path(), Utc::now()).expect("in flight");
        assert!(age < Duration::minutes(1));
    }

    #[test]
    fn an_exit_file_means_the_run_is_over() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gates.pid"), std::process::id().to_string()).unwrap();
        std::fs::write(dir.path().join("gates.exit"), "0").unwrap();
        assert_eq!(gate_run_age(dir.path(), Utc::now()), None);
    }

    #[test]
    fn a_dead_pid_without_an_exit_file_is_not_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        // Does not fit a pid_t, so it can never name a live process.
        std::fs::write(dir.path().join("gates.pid"), "4000000000").unwrap();
        assert_eq!(gate_run_age(dir.path(), Utc::now()), None);
    }

    #[test]
    fn missing_or_unreadable_gate_files_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(gate_run_age(dir.path(), Utc::now()), None);
        std::fs::write(dir.path().join("gates.pid"), "not a pid").unwrap();
        assert_eq!(gate_run_age(dir.path(), Utc::now()), None);
    }

    #[test]
    fn the_gate_age_is_the_age_of_the_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("gates.pid");
        std::fs::write(&pid, std::process::id().to_string()).unwrap();
        let age = gate_run_age(dir.path(), Utc::now() + Duration::minutes(20)).unwrap();
        assert!(age >= Duration::minutes(19) && age <= Duration::minutes(21));
    }

    #[test]
    fn the_reported_age_comes_from_the_most_recent_session_of_the_molecule() {
        let t = now();
        let other = MoleculeId::new("task-20261116-aaaa").unwrap();
        let rows = vec![
            // A previous session of the same molecule, long dead, with a stale record.
            row(
                "old",
                &mol(),
                t - Duration::hours(5),
                Some(t - Duration::hours(5)),
            ),
            row(
                "live",
                &mol(),
                t - Duration::seconds(30),
                Some(t - Duration::minutes(7)),
            ),
            row("elsewhere", &other, t, Some(t - Duration::hours(1))),
        ];
        assert_eq!(
            reported_background_age(&rows, &mol(), t),
            Some(Duration::minutes(7))
        );
    }

    #[test]
    fn a_live_session_that_reports_nothing_is_unknown_even_if_an_old_one_did() {
        let t = now();
        let rows = vec![
            row(
                "old",
                &mol(),
                t - Duration::hours(5),
                Some(t - Duration::hours(5)),
            ),
            row("live", &mol(), t, None),
        ];
        assert_eq!(reported_background_age(&rows, &mol(), t), None);
        assert_eq!(reported_background_age(&[], &mol(), t), None);
    }

    /// A state root with one molecule directory, and the slot a holder took.
    fn queued_fixture(pool_holders: usize) -> (tempfile::TempDir, PathBuf, Vec<GateSlotGuard>) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("fleets/default/molecules/task-1");
        std::fs::create_dir_all(&dir).unwrap();
        let held = (0..pool_holders)
            .map(|_| {
                try_acquire_gate_slot_in(root.path(), "full_gate", 4)
                    .unwrap()
                    .unwrap()
            })
            .collect();
        (root, dir, held)
    }

    fn write_queued(dir: &Path, pid: &str, pool: &str) {
        std::fs::write(dir.join("gates.queued"), format!("{pid}\n{pool}\n")).unwrap();
    }

    #[test]
    fn a_live_queued_process_behind_a_live_holder_is_queued() {
        let (root, dir, _held) = queued_fixture(1);
        write_queued(&dir, &std::process::id().to_string(), "full_gate");
        assert!(gate_queued_at(&dir, root.path()));
    }

    #[test]
    fn a_pool_with_no_live_holder_is_not_a_reason_to_stay_silent() {
        let (root, dir, held) = queued_fixture(1);
        write_queued(&dir, &std::process::id().to_string(), "full_gate");
        drop(held);
        assert!(!gate_queued_at(&dir, root.path()));
    }

    #[test]
    fn a_holder_in_another_pool_does_not_count() {
        let (root, dir, _held) = queued_fixture(1);
        write_queued(&dir, &std::process::id().to_string(), "docker_build");
        assert!(!gate_queued_at(&dir, root.path()));
    }

    #[test]
    fn a_stale_queued_file_with_a_dead_process_is_not_queued() {
        let (root, dir, _held) = queued_fixture(1);
        write_queued(&dir, "4000000000", "full_gate");
        assert!(!gate_queued_at(&dir, root.path()));
    }

    #[test]
    fn a_pid_or_an_exit_file_ends_the_queued_state() {
        for file in ["gates.pid", "gates.exit"] {
            let (root, dir, _held) = queued_fixture(1);
            write_queued(&dir, &std::process::id().to_string(), "full_gate");
            std::fs::write(dir.join(file), "1").unwrap();
            assert!(!gate_queued_at(&dir, root.path()), "{file}");
        }
    }

    #[test]
    fn missing_or_malformed_queued_files_are_unknown() {
        let (root, dir, _held) = queued_fixture(1);
        assert!(!gate_queued_at(&dir, root.path()));
        for body in ["", "not a pid\nfull_gate\n", "1234\n", "\u{0}"] {
            std::fs::write(dir.join("gates.queued"), body).unwrap();
            assert!(!gate_queued_at(&dir, root.path()), "{body:?}");
        }
        // A pool name that escapes the slot directory is refused, not followed.
        write_queued(&dir, &std::process::id().to_string(), "../x");
        assert!(!gate_queued_at(&dir, root.path()));
    }

    #[test]
    fn the_state_root_is_read_from_the_molecule_directory_shape() {
        let dir = Path::new("/g/.cosmon/state/fleets/default/molecules/task-1");
        assert_eq!(state_root_of(dir), Some(PathBuf::from("/g/.cosmon/state")));
        assert_eq!(state_root_of(Path::new("/tmp/flat/task-1")), None);
    }
}
