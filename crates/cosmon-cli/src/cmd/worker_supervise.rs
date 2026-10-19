// SPDX-License-Identifier: AGPL-3.0-only

//! Internal waiting parent for the opt-in Codex tmux launch. Receipt evidence
//! never grants completion, harvest, signal, or process adoption authority.

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

/// Run one direct child; inherited signal dispositions intentionally permit unknown after death.
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
        &cosmon_transport::worker_supervisor::DirectWorkerLaunch,
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

/// Prepare durable identity before creating the pane, then launch with the historic authority.
#[cfg(unix)]
pub(super) fn spawn_codex(
    config: &cosmon_transport::codex::CodexSessionConfig,
    state_dir: &Path,
    molecule: &cosmon_state::MoleculeData,
    worker: &cosmon_core::id::WorkerId,
    molecule_dir: &Path,
) -> anyhow::Result<Option<cosmon_core::terminal::TerminalIdentity>> {
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
    let receipts = {
        let _guard = ledger.lock_fleet()?;
        let mut observed = ledger.load_molecule(&molecule.id)?;
        anyhow::ensure!(
            observed
                .process
                .as_ref()
                .is_some_and(|process| &process.worker_id == worker),
            "worker dispatch changed before supervisor preparation"
        );
        let receipts =
            FileWorkerExitStore::prepare(molecule_dir, request).map_err(anyhow::Error::msg)?;
        if let Some(process) = observed.process.as_mut() {
            process.worker_launch = Some(receipts.request().identity.clone());
        }
        ledger.save_molecule(&molecule.id, &observed)?;
        cosmon_state::event_log::emit_one(
            state_dir.join("events.jsonl"),
            cosmon_core::event_v2::EventV2::WorkerLaunchPrepared {
                molecule_id: molecule.id.clone(),
                launch: receipts.request().identity.clone(),
            },
            None,
        )?;
        receipts
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
    Ok(
        cosmon_transport::TmuxBackend::new(&config.socket).spawn_worker_with_env_witnessed(
            &config.session_name,
            &config.work_dir,
            &command.join(" "),
            &assignments,
        )?,
    )
}

#[cfg(unix)]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
