// SPDX-License-Identifier: AGPL-3.0-only

//! `cs thaw` — resume a frozen worker by re-creating its Claude session.
//!
//! Reverses `cs freeze`: re-spawns the worker's tmux session, restores
//! its molecule assignment, and transitions back to `Active`. If a molecule
//! was frozen, sends a resume prompt to the new Claude session.
//!
//! Use `--continue <message>` to send a custom resume message (e.g. after
//! a hot-restart to pick up a new MCP server).
//!
//! State transition: `Paused → Active` (`frozen_molecule` restored, preemption metadata cleared).

use chrono::Utc;
use cosmon_core::id::WorkerId;
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{DesiredState, WorkerStatus};
use cosmon_transport::claude::{session_config, spawn_claude_session};
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the `thaw` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// ID of the worker to thaw.
    pub worker: String,

    /// Custom message to send after respawn instead of the default resume prompt.
    ///
    /// Useful for hot-restart scenarios: e.g. "A new MCP server X is now
    /// available. Continue your work on molecule Y."
    #[arg(long = "continue", short = 'c')]
    pub continue_msg: Option<String>,

    /// Skip tmux interaction (state-only transition, for testing).
    #[arg(long)]
    pub no_tmux: bool,
}

/// Execute the `thaw` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let worker_id = WorkerId::new(&args.worker)?;

    let mut fleet = store.load_fleet()?;

    let worker = fleet
        .workers
        .get(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker not found: {worker_id}"))?;

    // Only Paused workers can be thawed.
    if worker.desired != DesiredState::Paused {
        return Err(anyhow::anyhow!(
            "cannot thaw worker {worker_id}: desired is {} (expected paused)",
            worker.desired
        ));
    }

    // Re-create the Claude session in tmux.
    if !args.no_tmux {
        let workdir = super::resolve_worker_workdir(worker, store.project_root().as_deref());

        let socket = super::tmux_socket_name(ctx);
        let config = session_config(
            &socket,
            worker_id.as_str(),
            &workdir,
            worker.clearance,
            None,
        );

        spawn_claude_session(&config)
            .map_err(|e| anyhow::anyhow!("failed to respawn Claude session: {e}"))?;

        // Wait for Claude to be ready (handles trust prompt automatically).
        let backend = TmuxBackend::new(&socket);
        let _ = cosmon_transport::readiness::wait_ready(
            &backend,
            &worker_id,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(500),
        );

        // Send either the custom --continue message or the default resume prompt.
        let prompt = if let Some(ref msg) = args.continue_msg {
            msg.clone()
        } else if let Some(ref mol_id) = worker.frozen_molecule {
            format!(
                "Resume work on molecule {}. Pick up where the previous session left off.",
                mol_id.as_str()
            )
        } else {
            String::new()
        };

        if !prompt.is_empty() {
            let _ = backend.send_input(&worker_id, &prompt);
        }
    }

    // Update state: restore molecule, clear preemption metadata, set Active.
    let now = Utc::now();
    let worker = fleet
        .workers
        .get_mut(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker vanished: {worker_id}"))?;

    let restored_molecule = worker.frozen_molecule.clone();
    let was_preempted_by = worker.preempted_by.clone();

    worker.current_molecule = worker.frozen_molecule.take();
    worker.frozen_at = None;
    worker.preempted_by = None;
    worker.restart_count = 0; // Reset on intentional thaw.
    worker.desired = DesiredState::Running;
    worker.status = WorkerStatus::Active;
    worker.updated_at = now;

    store.save_fleet(&fleet)?;

    // Emit events.
    let events_path = state_dir.join("events.jsonl");
    let _ = cosmon_filestore::event::append(
        &events_path,
        &cosmon_core::event::Envelope::now(cosmon_core::event::Event::WorkerThawed {
            worker_id: worker_id.clone(),
        }),
    );
    // Emit molecule_thawed if a molecule was restored.
    if let Some(ref mol_id) = restored_molecule {
        let _ = cosmon_filestore::event::append(
            &events_path,
            &cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeThawed {
                molecule_id: mol_id.clone(),
            }),
        );
    }

    if ctx.json {
        let mut out = serde_json::json!({
            "command": "thaw",
            "worker_id": worker_id.as_str(),
            "status": "active",
        });
        if let Some(ref mol) = restored_molecule {
            out["restored_molecule"] = serde_json::json!(mol.as_str());
        }
        if let Some(ref by) = was_preempted_by {
            out["was_preempted_by"] = serde_json::json!(by.as_str());
        }
        if args.continue_msg.is_some() {
            out["continue_sent"] = serde_json::json!(true);
        }
        println!("{out}");
    } else {
        let mut msg = format!("Thawed worker {worker_id} (paused -> active)");
        if let Some(ref mol) = restored_molecule {
            use std::fmt::Write;
            let _ = write!(msg, ", restored molecule {mol}");
        }
        if args.continue_msg.is_some() {
            msg.push_str(", continue message sent");
        }
        println!("{msg}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, MoleculeId, WorkerId};
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, StateStore, WorkerData};
    use std::path::PathBuf;
    use tempfile::TempDir;

    use super::*;

    fn make_frozen_worker(
        name: &str,
        frozen_mol: Option<&str>,
        preempted_by: Option<&str>,
    ) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let store = FileStore::new(&path);
        let mut fleet = Fleet::default();
        let wid = WorkerId::new(name).unwrap();
        let mut worker = WorkerData::new(
            wid.clone(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Paused,
        );
        worker.desired = DesiredState::Paused;
        worker = worker.with_frozen_at(Utc::now());
        if let Some(m) = frozen_mol {
            worker = worker.with_frozen_molecule(MoleculeId::new(m).unwrap());
        }
        if let Some(s) = preempted_by {
            worker = worker.with_preempted_by(WorkerId::new(s).unwrap());
        }
        fleet.workers.insert(wid, worker);
        store.save_fleet(&fleet).unwrap();
        (tmp, path)
    }

    #[test]
    fn test_thaw_restores_active_state() {
        let (tmp, state_dir) = make_frozen_worker("quartz", None, None);
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "quartz".to_owned(),
            continue_msg: None,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("quartz").unwrap();
        let worker = fleet.workers.get(&wid).unwrap();
        assert_eq!(worker.desired, DesiredState::Running);
        assert_eq!(worker.status, WorkerStatus::Active);
        assert!(worker.frozen_molecule.is_none());
        assert!(worker.frozen_at.is_none());
        assert!(worker.preempted_by.is_none());
    }

    #[test]
    fn test_thaw_restores_molecule() {
        let (tmp, state_dir) = make_frozen_worker("ruby", Some("cs-20260406-abcd"), Some("topaz"));
        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "ruby".to_owned(),
            continue_msg: None,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("ruby").unwrap();
        let worker = fleet.workers.get(&wid).unwrap();
        assert_eq!(worker.status, WorkerStatus::Active);
        assert_eq!(
            worker.current_molecule.as_ref().unwrap().as_str(),
            "cs-20260406-abcd"
        );
        assert!(worker.frozen_molecule.is_none());
        assert!(worker.preempted_by.is_none());
    }

    #[test]
    fn test_thaw_with_continue_message() {
        let (tmp, state_dir) = make_frozen_worker("emerald", Some("cs-20260406-work"), None);
        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "emerald".to_owned(),
            continue_msg: Some("New MCP server topon is available. Continue your work.".to_owned()),
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("emerald").unwrap();
        let worker = fleet.workers.get(&wid).unwrap();
        assert_eq!(worker.status, WorkerStatus::Active);
        assert_eq!(
            worker.current_molecule.as_ref().unwrap().as_str(),
            "cs-20260406-work"
        );
    }

    #[test]
    fn test_thaw_active_worker_rejected() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::default();
        let wid = WorkerId::new("opal").unwrap();
        let mut w = WorkerData::new(
            wid.clone(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        w.desired = DesiredState::Running;
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            worker: "opal".to_owned(),
            continue_msg: None,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("cannot thaw"));
    }

    #[test]
    fn test_thaw_nonexistent_worker() {
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
            continue_msg: None,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_freeze_thaw_roundtrip() {
        // Full roundtrip: Active → Freeze → Thaw → Active with molecule restored.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::default();
        let wid = WorkerId::new("diamond").unwrap();
        let mut w = WorkerData::new(
            wid.clone(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        )
        .with_molecule(MoleculeId::new("cs-20260406-rtrp").unwrap());
        w.desired = DesiredState::Running;
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        // Freeze
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let freeze_args = super::super::freeze::Args {
            worker: "diamond".to_owned(),
            by: Some("emperor".to_owned()),
            reason: None,
            timeout: 5,
            no_tmux: true,
        };
        super::super::freeze::run(&ctx, &freeze_args).unwrap();

        // Verify frozen state
        let fleet = store.load_fleet().unwrap();
        let w = fleet.workers.get(&wid).unwrap();
        assert_eq!(w.desired, DesiredState::Paused);
        assert_eq!(w.status, WorkerStatus::Paused);
        assert_eq!(
            w.frozen_molecule.as_ref().unwrap().as_str(),
            "cs-20260406-rtrp"
        );
        assert_eq!(w.preempted_by.as_ref().unwrap().as_str(), "emperor");
        assert!(w.current_molecule.is_none());

        // Thaw with a continue message
        let thaw_args = Args {
            worker: "diamond".to_owned(),
            continue_msg: Some("Config updated, continue your work.".to_owned()),
            no_tmux: true,
        };
        run(&ctx, &thaw_args).unwrap();

        // Verify restored state
        let fleet = store.load_fleet().unwrap();
        let w = fleet.workers.get(&wid).unwrap();
        assert_eq!(w.desired, DesiredState::Running);
        assert_eq!(w.status, WorkerStatus::Active);
        assert_eq!(
            w.current_molecule.as_ref().unwrap().as_str(),
            "cs-20260406-rtrp"
        );
        assert!(w.frozen_molecule.is_none());
        assert!(w.preempted_by.is_none());
        assert!(w.frozen_at.is_none());
    }
}
