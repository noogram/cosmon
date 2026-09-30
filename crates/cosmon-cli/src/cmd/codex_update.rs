// SPDX-License-Identifier: AGPL-3.0-only

//! Machine-wide, bounded codex update check before an interactive launch.

use std::fs::{self, OpenOptions};
use std::io;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_state::event_log;
use fs2::FileExt as _;

const CHECK_INTERVAL: Duration = Duration::from_secs(600);
const UPDATE_BUDGET: Duration = Duration::from_secs(120);
const LOCK_BUDGET: Duration = Duration::from_secs(150);

/// A version change, an unchanged install, or a bounded updater failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    Updated { from: String, to: String },
    Unchanged,
    Failed(String),
}

/// External updater boundary, implemented by the installed binary in production.
pub(crate) trait CodexUpdater: Send + Sync {
    fn version(&self) -> io::Result<String>;
    fn update(&self) -> io::Result<()>;
}

/// The operator's installed codex CLI. No test invokes this implementation.
pub(crate) struct InstalledCodex;

impl CodexUpdater for InstalledCodex {
    fn version(&self) -> io::Result<String> {
        let mut child = std::process::Command::new("codex")
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err(io::Error::other("codex --version failed"));
                }
                let mut text = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_string(&mut text)?;
                }
                return Ok(text.trim().to_owned());
            }
            if start.elapsed() >= Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "codex --version exceeded 10s",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn update(&self) -> io::Result<()> {
        let mut child = std::process::Command::new("codex")
            .arg("update")
            .env("CODEX_NON_INTERACTIVE", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("codex update exited {status}")))
                };
            }
            if start.elapsed() >= UPDATE_BUDGET {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "codex update exceeded 120s",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Resolve the operator cosmon home, independent of any galaxy checkout.
pub(crate) fn operator_cosmon_home() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cosmon"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unset"))
}

/// Serialize checks across galaxies and cache a successful check briefly.
/// A failure is returned to the caller and must never block worker launch.
pub(crate) fn update_before_launch(home: &Path, updater: &dyn CodexUpdater) -> UpdateOutcome {
    match update_under_lock(home, updater) {
        Ok(outcome) => outcome,
        Err(error) => UpdateOutcome::Failed(error.to_string()),
    }
}

/// Persist an update result on the molecule and in the galaxy event log.
pub(crate) fn record_outcome(
    state_dir: &Path,
    mol_dir: &Path,
    mol_id: &MoleculeId,
    outcome: &UpdateOutcome,
) -> io::Result<()> {
    let event = match outcome {
        UpdateOutcome::Updated { from, to } => EventV2::CodexUpdated {
            molecule_id: mol_id.clone(),
            from: from.clone(),
            to: to.clone(),
        },
        UpdateOutcome::Failed(reason) => EventV2::CodexUpdateFailed {
            molecule_id: mol_id.clone(),
            reason: reason.clone(),
        },
        UpdateOutcome::Unchanged => return Ok(()),
    };
    fs::create_dir_all(mol_dir)?;
    let line = serde_json::to_string(&event).map_err(io::Error::other)?;
    writeln!(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(mol_dir.join("codex-updates.jsonl"))?,
        "{line}"
    )?;
    event_log::emit_one(state_dir.join("events.jsonl"), event, None).map_err(io::Error::other)?;
    Ok(())
}

fn update_under_lock(home: &Path, updater: &dyn CodexUpdater) -> io::Result<UpdateOutcome> {
    fs::create_dir_all(home)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("codex-update.lock"))?;
    let start = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if start.elapsed() >= LOCK_BUDGET {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "codex update lock busy",
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
    let before = updater.version()?;
    let stamp_path = home.join("codex-update.checked");
    let failed_path = home.join("codex-update.failed");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    if let Ok(stamp) = fs::read_to_string(&stamp_path) {
        let mut parts = stamp.lines();
        if parts.next() == Some(before.as_str())
            && parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|then| now.saturating_sub(then) < CHECK_INTERVAL.as_secs())
        {
            return Ok(UpdateOutcome::Unchanged);
        }
    }
    if let Ok(stamp) = fs::read_to_string(&failed_path) {
        let mut parts = stamp.lines();
        if parts.next() == Some(before.as_str())
            && parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|then| now.saturating_sub(then) < CHECK_INTERVAL.as_secs())
        {
            return Ok(UpdateOutcome::Failed(
                parts.next().unwrap_or("previous update failed").to_owned(),
            ));
        }
    }
    if let Err(error) = updater.update() {
        let reason = error.to_string().replace(['\r', '\n'], " ");
        fs::write(failed_path, format!("{before}\n{now}\n{reason}\n"))?;
        return Ok(UpdateOutcome::Failed(reason));
    }
    let after = updater.version()?;
    fs::write(stamp_path, format!("{after}\n{now}\n"))?;
    let _ = fs::remove_file(failed_path);
    if before == after {
        Ok(UpdateOutcome::Unchanged)
    } else {
        Ok(UpdateOutcome::Updated {
            from: before,
            to: after,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct FakeUpdater {
        version: Mutex<String>,
        installs: Mutex<usize>,
        fail: bool,
    }

    impl CodexUpdater for FakeUpdater {
        fn version(&self) -> io::Result<String> {
            Ok(self.version.lock().unwrap().clone())
        }
        fn update(&self) -> io::Result<()> {
            *self.installs.lock().unwrap() += 1;
            if self.fail {
                return Err(io::Error::other("fake installer failure"));
            }
            *self.version.lock().unwrap() = "codex-cli 2".to_owned();
            Ok(())
        }
    }

    #[test]
    fn concurrent_galaxies_share_one_machine_update() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let config_bytes = b"check_for_update_on_startup = true\n";
        std::fs::write(&config_path, config_bytes).unwrap();
        let updater = Arc::new(FakeUpdater {
            version: Mutex::new("codex-cli 1".to_owned()),
            installs: Mutex::new(0),
            fail: false,
        });
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = home.path().to_path_buf();
                let updater = Arc::clone(&updater);
                std::thread::spawn(move || update_before_launch(&path, updater.as_ref()))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(*updater.installs.lock().unwrap(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|out| matches!(out, UpdateOutcome::Updated { .. }))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|out| matches!(out, UpdateOutcome::Unchanged))
                .count(),
            7
        );
        assert_eq!(std::fs::read(config_path).unwrap(), config_bytes);
    }

    #[test]
    fn failed_installer_returns_a_failure_without_a_version_change() {
        let home = tempfile::tempdir().unwrap();
        let updater = FakeUpdater {
            version: Mutex::new("codex-cli 1".to_owned()),
            installs: Mutex::new(0),
            fail: true,
        };
        assert!(matches!(
            update_before_launch(home.path(), &updater),
            UpdateOutcome::Failed(_)
        ));
        assert_eq!(updater.version().unwrap(), "codex-cli 1");
    }

    #[test]
    fn concurrent_failed_checks_share_one_attempt_and_return_failure() {
        let home = tempfile::tempdir().unwrap();
        let updater = Arc::new(FakeUpdater {
            version: Mutex::new("codex-cli 1".to_owned()),
            installs: Mutex::new(0),
            fail: true,
        });
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = home.path().to_path_buf();
                let updater = Arc::clone(&updater);
                std::thread::spawn(move || update_before_launch(&path, updater.as_ref()))
            })
            .collect();
        assert!(handles
            .into_iter()
            .all(|h| matches!(h.join().unwrap(), UpdateOutcome::Failed(_))));
        assert_eq!(*updater.installs.lock().unwrap(), 1);
    }
}
