// SPDX-License-Identifier: AGPL-3.0-only

//! `cs kill` — **deprecated** alias for `cs purge <worker> --force` (ADR-052 §D3).
//!
//! `cs kill` and `cs purge` were both "infrastructure teardown" — the only
//! difference was the force-kill tmux step. ADR-052 collapses them onto a
//! single verb: `cs purge <worker> --force` replicates the old `cs kill`
//! behaviour, while `cs purge` (no arg) keeps the sweep semantics.
//!
//! This module is retained as a thin wrapper for one release cycle so
//! existing scripts and muscle memory keep working. A stderr deprecation
//! notice is emitted on every invocation.

use super::Context;

/// Arguments for the deprecated `kill` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// ID of the worker to terminate.
    pub worker: String,
}

/// Execute the deprecated `kill` command.
///
/// Emits a stderr deprecation notice, then delegates to the canonical
/// `cs purge <worker> --force` handler so output is byte-compatible with
/// the new command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    eprintln!(
        "cs kill: deprecated — use `cs purge {} --force` instead (ADR-052 §D3). \
         This alias will be removed after one release cycle.",
        args.worker
    );
    super::purge::run(
        ctx,
        &super::purge::Args {
            worker: Some(args.worker.clone()),
            force: true,
            status: None,
            role: None,
        },
    )
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

    fn make_store_with_worker(name: &str, status: WorkerStatus) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let store = FileStore::new(&path);
        let mut fleet = Fleet::default();
        let wid = WorkerId::new(name).unwrap();
        fleet.workers.insert(
            wid.clone(),
            WorkerData::new(
                wid,
                AgentId::new("polecat").unwrap(),
                AgentRole::Implementation,
                Clearance::Write,
                status,
            ),
        );
        store.save_fleet(&fleet).unwrap();
        (tmp, path)
    }

    #[test]
    fn test_kill_delegates_to_purge_force() {
        // Post-ADR-052: `cs kill` is a deprecated alias for `cs purge
        // --force`, which REMOVES the worker from fleet (instead of
        // setting status to Stopped in place). The alias must delegate
        // to the canonical path, so the worker should be gone.
        let (tmp, state_dir) = make_store_with_worker("quartz", WorkerStatus::Active);
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "quartz".to_owned(),
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("quartz").unwrap();
        assert!(
            !fleet.workers.contains_key(&wid),
            "worker must be removed by the purge --force alias"
        );
    }

    #[test]
    fn test_kill_nonexistent_worker_errors() {
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
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
