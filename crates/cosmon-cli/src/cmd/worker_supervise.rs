// SPDX-License-Identifier: AGPL-3.0-only

//! Internal waiting parent for the opt-in Codex tmux launch. Receipt evidence
//! never grants completion, harvest, signal, or process adoption authority.
//! The parent alone may stop its child, through the identity-bound channel of
//! ADR-188.

use std::path::{Path, PathBuf};

/// Transport-internal launch inputs, passed in memory rather than persisted as a command.
#[derive(clap::Args)]
pub struct Args {
    /// Canonical molecule state directory, independent of the worktree.
    #[arg(long)]
    pub molecule_dir: PathBuf,
    /// Serialized expected identity; contains no argv or environment values.
    #[arg(long)]
    pub identity: String,
    /// Final executable to launch directly.
    #[arg(long)]
    pub program: String,
    /// Literal arguments for the executable, after `--`.
    #[arg(last = true)]
    pub arguments: Vec<String>,
}

/// Run one direct child; inherited signal dispositions intentionally permit unknown after
/// death of this parent. A stop through its control channel yields the child's real status.
#[cfg(unix)]
pub fn run(args: &Args) -> anyhow::Result<()> {
    use cosmon_core::worker_exit::LaunchIdentity;
    use cosmon_state::worker_exit::FileWorkerExitStore;
    let expected: LaunchIdentity = serde_json::from_str(&args.identity)?;
    let store = FileWorkerExitStore::current(&args.molecule_dir)
        .map_err(anyhow::Error::msg)?
        .ok_or_else(|| anyhow::anyhow!("no expected worker launch"))?;
    anyhow::ensure!(
        store.request().identity == expected,
        "worker launch identity changed"
    );
    anyhow::ensure!(
        store.request().matches(
            &args.program,
            &args.arguments,
            &std::env::current_dir()?,
            |name| std::env::var(name).ok()
        ),
        "worker launch inputs changed; refusing spawn"
    );
    cosmon_transport::worker_supervisor::supervise(
        &expected,
        std::process::id(),
        &args.program,
        &args.arguments,
        &cosmon_transport::worker_supervisor::DirectWorkerLaunch::default(),
        // ADR-188: the stop channel is bound after the claim, before spawn.
        &cosmon_transport::worker_control::UnixWorkerControl {
            root: cosmon_transport::worker_control::default_root().map_err(anyhow::Error::msg)?,
        },
        &store,
        &|_| {},
    )
    .map_err(anyhow::Error::msg)
}

/// Refuse unsupported platforms without starting a child.
#[cfg(not(unix))]
pub fn run(_args: &Args) -> anyhow::Result<()> {
    anyhow::bail!("worker exit receipts require POSIX")
}

/// Refuse unsupported combinations and unresolved launches before dispatch effects.
pub(super) fn check_configuration(
    adapter: &str,
    entry: Option<&cosmon_core::config::AdapterEntry>,
    molecule_dir: &Path,
) -> anyhow::Result<bool> {
    let enabled = entry
        .and_then(|entry| entry.worker_exit_receipts)
        .unwrap_or(false);
    anyhow::ensure!(
        !enabled || (cfg!(unix) && adapter == "codex"),
        "worker_exit_receipts supports only POSIX Codex tmux launches"
    );
    if enabled {
        anyhow::ensure!(
            entry
                .and_then(|entry| entry.mode.as_deref())
                .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("exec")),
            "worker_exit_receipts requires mode = 'exec'; Interactive uses legacy supervision"
        );
        anyhow::ensure!(entry.and_then(|entry| entry.update).unwrap_or_default() != cosmon_core::config::CodexUpdatePolicy::Auto, "worker_exit_receipts requires update = 'skip' or 'operator'; automatic relaunch is not permitted");
    }
    #[cfg(unix)]
    if let Some(previous) = cosmon_state::worker_exit::FileWorkerExitStore::current(molecule_dir)
        .map_err(anyhow::Error::msg)?
    {
        use cosmon_core::worker_exit::WorkerExitStore as _;
        anyhow::ensure!(
            enabled,
            "a supervised molecule cannot fall back to pane exit status"
        );
        anyhow::ensure!(previous.read().is_terminal(), "previous supervised launch is unresolved; explicit resolution is required, never PID adoption or automatic retry");
    }
    Ok(enabled)
}

/// Creation evidence and the exact dispatch snapshot that may receive it.
#[cfg(unix)]
pub(super) struct SpawnWitness {
    /// Terminal incarnation captured while creating this worker's pane.
    pub terminal: Option<cosmon_core::terminal::TerminalIdentity>,
    /// Prepared launch A, never a later process reloaded for the terminal CAS.
    pub expected_process: cosmon_core::process::MoleculeProcess,
}

/// Prepare durable identity before creating the pane, then launch with the historic authority.
#[cfg(unix)]
pub(super) fn spawn_codex(
    config: &cosmon_transport::codex::CodexSessionConfig,
    backend: &dyn cosmon_core::transport::TransportBackend,
    state_dir: &Path,
    molecule: &cosmon_state::MoleculeData,
    worker: &cosmon_core::id::WorkerId,
    molecule_dir: &Path,
) -> anyhow::Result<SpawnWitness> {
    use cosmon_core::worker_exit::{LaunchRequest, LaunchToken};
    use cosmon_state::{worker_exit::FileWorkerExitStore, StateStore};
    use cosmon_transport::codex;
    codex::prepare_codex_session(config)?;
    let args = codex::supervised_worker_args(config);
    let environment = codex::supervised_worker_environment(config);
    let program = config
        .binary
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 worker executable"))?;
    let request = LaunchRequest::bind(
        molecule.id.clone(),
        LaunchToken::from_bytes(rand::random()),
        LaunchToken::from_bytes(rand::random()),
        program,
        &args,
        &Path::new(&config.work_dir).canonicalize()?,
        &environment,
    )
    .map_err(anyhow::Error::msg)?;
    let ledger = cosmon_filestore::FileStore::new(state_dir);
    let (receipts, expected_process) = {
        let _guard = ledger.lock_fleet()?;
        let mut observed = ledger.load_molecule(&molecule.id)?;
        anyhow::ensure!(
            observed.process == molecule.process
                && observed
                    .process
                    .as_ref()
                    .is_some_and(|process| &process.worker_id == worker),
            "worker dispatch changed before supervisor preparation"
        );
        let receipts =
            FileWorkerExitStore::prepare(molecule_dir, request).map_err(anyhow::Error::msg)?;
        let process = observed
            .process
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("worker dispatch disappeared before preparation"))?;
        process.worker_launch = Some(receipts.request().identity.clone());
        let expected_process = process.clone();
        ledger.save_molecule(&molecule.id, &observed)?;
        cosmon_state::event_log::emit_one(
            state_dir.join("events.jsonl"),
            cosmon_core::event_v2::EventV2::WorkerLaunchPrepared {
                molecule_id: molecule.id.clone(),
                launch: receipts.request().identity.clone(),
            },
            None,
        )?;
        (receipts, expected_process)
    };
    let identity = serde_json::to_string(&receipts.request().identity)?;
    let mut command = vec!["exec".to_owned(), "env".to_owned()];
    for (name, value) in &environment {
        if value.is_none() {
            command.extend(["-u".to_owned(), quote(name)]);
        }
    }
    command.extend([
        quote(&std::env::current_exe()?.to_string_lossy()),
        "worker-supervise".to_owned(),
        "--molecule-dir".to_owned(),
        quote(&molecule_dir.to_string_lossy()),
        "--identity".to_owned(),
        quote(&identity),
        "--program".to_owned(),
        quote(program),
        "--".to_owned(),
    ]);
    command.extend(args.iter().map(|arg| quote(arg)));
    let assignments: Vec<_> = environment
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();
    let terminal = backend.spawn_command(
        &config.session_name,
        &config.work_dir,
        &command.join(" "),
        &assignments,
    )?;
    Ok(SpawnWitness {
        terminal,
        expected_process,
    })
}

#[cfg(unix)]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::check_configuration;
    use cosmon_core::config::{AdapterEntry, CodexUpdatePolicy};

    #[test]
    fn receipts_require_explicit_exec_and_leave_legacy_interactive_unchanged(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        assert!(!check_configuration("codex", None, directory.path())?);
        for mode in [None, Some("interactive"), Some("typo"), Some("")] {
            let mut entry = AdapterEntry {
                worker_exit_receipts: Some(true),
                mode: mode.map(str::to_owned),
                update: Some(CodexUpdatePolicy::Skip),
                ..AdapterEntry::default()
            };
            assert!(check_configuration("codex", Some(&entry), directory.path()).is_err());
            assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
            entry.worker_exit_receipts = Some(false);
            assert!(!check_configuration(
                "codex",
                Some(&entry),
                directory.path()
            )?);
        }
        for mode in ["exec", " EXEC "] {
            let entry = AdapterEntry {
                worker_exit_receipts: Some(true),
                mode: Some(mode.to_owned()),
                update: Some(CodexUpdatePolicy::Skip),
                ..AdapterEntry::default()
            };
            assert!(check_configuration(
                "codex",
                Some(&entry),
                directory.path()
            )?);
        }
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }
}
