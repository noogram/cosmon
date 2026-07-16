// SPDX-License-Identifier: AGPL-3.0-only

//! `cs quench` — **deprecated** alias for `cs freeze --reason <str>` (ADR-052 §D3).
//!
//! Quench was "graceful shutdown with state preservation" — the exact
//! definition of freeze. ADR-052 collapses the verbs: `cs freeze` is the
//! canonical perimeter, `--reason <str>` captures the intent that quench
//! carried implicitly.
//!
//! This module is retained for one release cycle so pre-existing scripts
//! and muscle memory keep working. A stderr deprecation notice is emitted
//! on every invocation. Legacy quench semantics (end state = `Stopped`
//! rather than `Paused`) are preserved for the backward-compat window;
//! callers migrating to `cs freeze --reason` get the unified freeze state
//! model (Paused, ready for `cs thaw`).

use chrono::Utc;
use cosmon_core::id::WorkerId;
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{DesiredState, WorkerStatus};
use cosmon_filestore::FileStore;
use cosmon_state::StateStore;
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the `quench` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// ID of the worker to gracefully shut down.
    pub worker: String,

    /// Grace period in seconds before force-killing (default: 30).
    #[arg(long, default_value = "30")]
    pub timeout: u64,

    /// Skip graceful exit — go straight to force-kill (same as `kill`).
    #[arg(long)]
    pub force: bool,

    /// Skip tmux interaction (state-only transition, for testing).
    #[arg(long)]
    pub no_tmux: bool,
}

/// Execute the deprecated `quench` command.
///
/// Emits a stderr deprecation notice, then runs the legacy quench logic
/// so scripts relying on the final `Stopped` state keep working. Callers
/// migrating to the canonical `cs freeze --reason <str>` will land on the
/// unified freeze state model (Paused, resumable via `cs thaw`).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    eprintln!(
        "cs quench: deprecated — use `cs freeze {} --reason quench` instead (ADR-052 §D3). \
         Note: `cs freeze` lands the worker in `Paused` (resumable via `cs thaw`) \
         rather than `Stopped`. This alias will be removed after one release cycle.",
        args.worker
    );

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    let worker_id = WorkerId::new(&args.worker)?;

    let mut fleet = store.load_fleet()?;

    let worker = fleet
        .workers
        .get_mut(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker not found: {worker_id}"))?;

    // Validate: only Running or Paused workers can be quenched.
    match worker.desired {
        DesiredState::Running | DesiredState::Paused => {}
        other @ DesiredState::Stopped => {
            return Err(anyhow::anyhow!(
                "cannot quench worker {worker_id}: desired is {other} (expected running or paused)"
            ));
        }
    }

    let previous_status = worker.desired.to_string();

    // Transition to Stopping (intermediate state in old model).
    worker.status = WorkerStatus::Stopping;
    worker.updated_at = Utc::now();
    store.save_fleet(&fleet)?;

    // Perform the actual shutdown via tmux (unless --no-tmux).
    let graceful = if args.no_tmux {
        true
    } else {
        let backend = TmuxBackend::new(super::tmux_socket_name(ctx));
        let timeout = std::time::Duration::from_secs(args.timeout);

        if args.force {
            let _ = backend.terminate(&worker_id);
            false
        } else {
            backend.graceful_exit(&worker_id, timeout).unwrap_or(false)
        }
    };

    // Transition to Stopped.
    let worker = fleet
        .workers
        .get_mut(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker vanished: {worker_id}"))?;
    worker.desired = DesiredState::Stopped;
    worker.status = WorkerStatus::Stopped;
    worker.current_molecule = None;
    worker.updated_at = Utc::now();

    store.save_fleet(&fleet)?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "quench",
            "worker_id": worker_id.as_str(),
            "previous_status": previous_status,
            "status": "stopped",
            "graceful": graceful,
        });
        println!("{out}");
    } else {
        let method = if graceful {
            "gracefully"
        } else {
            "force-killed"
        };
        println!("Quenched worker {worker_id} ({previous_status} -> stopped, {method})");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, WorkerId};
    use cosmon_core::worker::WorkerStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, StateStore, WorkerData};
    use std::path::PathBuf;
    use tempfile::TempDir;

    use super::*;

    use cosmon_core::worker::DesiredState;

    fn make_store_with_worker(name: &str, status: &WorkerStatus) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let store = FileStore::new(&path);
        let mut fleet = Fleet::default();
        let wid = WorkerId::new(name).unwrap();
        let mut data = WorkerData::new(
            wid.clone(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            status.clone(),
        );
        data.desired = match &status {
            WorkerStatus::Active => DesiredState::Running,
            WorkerStatus::Paused => DesiredState::Paused,
            _ => DesiredState::Stopped,
        };
        fleet.workers.insert(wid, data);
        store.save_fleet(&fleet).unwrap();
        (tmp, path)
    }

    #[test]
    fn test_quench_active_worker() {
        let (tmp, state_dir) = make_store_with_worker("quartz", &WorkerStatus::Active);
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "quartz".to_owned(),
            timeout: 5,
            force: false,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("quartz").unwrap();
        let worker = fleet.workers.get(&wid).unwrap();
        assert_eq!(worker.status, WorkerStatus::Stopped);
        assert!(worker.current_molecule.is_none());
    }

    #[test]
    fn test_quench_paused_worker() {
        let (tmp, state_dir) = make_store_with_worker("jasper", &WorkerStatus::Paused);
        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "jasper".to_owned(),
            timeout: 5,
            force: false,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("jasper").unwrap();
        assert_eq!(
            fleet.workers.get(&wid).unwrap().status,
            WorkerStatus::Stopped
        );
    }

    #[test]
    fn test_quench_stopped_worker_rejected() {
        let (_tmp, state_dir) = make_store_with_worker("opal", &WorkerStatus::Stopped);
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "opal".to_owned(),
            timeout: 5,
            force: false,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("cannot quench"));
        assert!(err.to_string().contains("stopped"));
    }

    #[test]
    fn test_quench_force_flag() {
        let (tmp, state_dir) = make_store_with_worker("topaz", &WorkerStatus::Active);
        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "topaz".to_owned(),
            timeout: 5,
            force: true,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("topaz").unwrap();
        assert_eq!(
            fleet.workers.get(&wid).unwrap().status,
            WorkerStatus::Stopped
        );
    }

    #[test]
    fn test_quench_nonexistent_worker() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            worker: "ghost".to_owned(),
            timeout: 5,
            force: false,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
