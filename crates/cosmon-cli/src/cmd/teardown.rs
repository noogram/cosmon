// SPDX-License-Identifier: AGPL-3.0-only

//! `cs teardown` — gracefully shut down an entire fleet.
//!
//! Reads the persisted fleet spec, quenches all active workers belonging
//! to that fleet, and removes them from state. The symmetric opposite of
//! `cs deploy`.

use cosmon_core::id::WorkerId;
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{DesiredState, WorkerStatus};
use cosmon_filestore::FileStore;
use cosmon_state::StateStore;
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the `teardown` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Name of the fleet to tear down (matches fleet.toml `fleet = "..."` name).
    pub fleet: String,

    /// Force-kill instead of graceful quench.
    #[arg(long)]
    pub force: bool,

    /// Skip tmux interaction (state-only, for testing).
    #[arg(long)]
    pub no_tmux: bool,
}

/// Execute the `teardown` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    // Load fleet spec to find which workers belong to this fleet.
    let spec_path = state_dir
        .join("fleets")
        .join(format!("{}.json", args.fleet));
    if !spec_path.exists() {
        return Err(anyhow::anyhow!("fleet not found: {}", args.fleet));
    }
    let spec_json = std::fs::read_to_string(&spec_path)?;
    let spec: serde_json::Value = serde_json::from_str(&spec_json)?;

    let fleet_agents: Vec<String> = spec["agents"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    if fleet_agents.is_empty() {
        return Err(anyhow::anyhow!("fleet '{}' has no agents", args.fleet));
    }

    let mut fleet = store.load_fleet()?;
    let backend = if args.no_tmux {
        None
    } else {
        Some(TmuxBackend::new(&args.fleet))
    };

    let timeout = std::time::Duration::from_secs(if args.force { 1 } else { 15 });
    let mut stopped: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for agent_name in &fleet_agents {
        let Ok(wid) = WorkerId::new(agent_name) else {
            skipped.push(agent_name.clone());
            continue;
        };

        if let Some(worker) = fleet.workers.get_mut(&wid) {
            // Only teardown running or paused workers.
            match worker.desired {
                DesiredState::Running | DesiredState::Paused => {
                    if let Some(ref be) = backend {
                        let _ = be.graceful_exit(&wid, timeout);
                    }
                    worker.desired = DesiredState::Stopped;
                    worker.status = WorkerStatus::Stopped;
                    worker.current_molecule = None;
                    worker.updated_at = chrono::Utc::now();
                    stopped.push(agent_name.clone());
                }
                DesiredState::Stopped => {
                    skipped.push(agent_name.clone());
                }
            }
        } else {
            skipped.push(agent_name.clone());
        }
    }

    store.save_fleet(&fleet)?;

    // Emit event.
    let _ = cosmon_filestore::event::append(
        &state_dir.join("events.jsonl"),
        &cosmon_core::event::Envelope::now(cosmon_core::event::Event::WorkerTerminated {
            worker_id: WorkerId::new(&args.fleet)
                .unwrap_or_else(|_| WorkerId::new("fleet").unwrap()),
            reason: format!("fleet teardown: {} workers stopped", stopped.len()),
        }),
    );

    if ctx.json {
        let out = serde_json::json!({
            "command": "teardown",
            "fleet": args.fleet,
            "stopped": stopped,
            "skipped": skipped,
        });
        println!("{out}");
    } else {
        println!(
            "Teardown fleet '{}': {} stopped, {} skipped",
            args.fleet,
            stopped.len(),
            skipped.len()
        );
        for name in &stopped {
            println!("  - {name}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, WorkerId};
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, StateStore, WorkerData};
    use std::path::PathBuf;
    use tempfile::TempDir;

    use super::*;

    fn setup_fleet(tmp: &TempDir) -> PathBuf {
        let state_dir = tmp.path().to_path_buf();
        let store = FileStore::new(&state_dir);

        // Create fleet state with workers.
        let mut fleet = Fleet::new();
        for name in ["alpha", "beta"] {
            let wid = WorkerId::new(name).unwrap();
            let mut wd = WorkerData::new(
                wid.clone(),
                AgentId::new(name).unwrap(),
                AgentRole::Implementation,
                Clearance::Write,
                WorkerStatus::Active,
            );
            wd.desired = DesiredState::Running;
            fleet.workers.insert(wid, wd);
        }
        store.save_fleet(&fleet).unwrap();

        // Create fleet spec.
        let fleets_dir = state_dir.join("fleets");
        std::fs::create_dir_all(&fleets_dir).unwrap();
        let spec = serde_json::json!({
            "name": "test-fleet",
            "agents": [{"name": "alpha"}, {"name": "beta"}]
        });
        std::fs::write(
            fleets_dir.join("test-fleet.json"),
            serde_json::to_string(&spec).unwrap(),
        )
        .unwrap();

        state_dir
    }

    #[test]
    fn test_teardown_stops_active_workers() {
        let tmp = TempDir::new().unwrap();
        let state_dir = setup_fleet(&tmp);

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            fleet: "test-fleet".to_owned(),
            force: false,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(&state_dir);
        let fleet = store.load_fleet().unwrap();
        for name in ["alpha", "beta"] {
            let wid = WorkerId::new(name).unwrap();
            assert_eq!(fleet.workers[&wid].status, WorkerStatus::Stopped);
        }
    }

    #[test]
    fn test_teardown_unknown_fleet_errors() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::new()).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            fleet: "ghost".to_owned(),
            force: false,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
