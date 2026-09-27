// SPDX-License-Identifier: AGPL-3.0-only

//! `cs freeze` — suspend a worker with state preservation (Slurm-style preemption).
//!
//! Gracefully stops the worker's Claude session and moves it to `Paused` status,
//! preserving its current molecule assignment for later resumption via `cs thaw`.
//! This is the "requeue" preemption mode from Slurm: the worker exits cleanly,
//! its context is saved, and it can be restarted later where it left off.
//!
//! State transition: `Active → Paused` (with `frozen_molecule` and `frozen_at` set).
//!
//! The session itself is not part of what freeze preserves. Durable state
//! (fleet record, molecule directory, worktree) survives; the tmux session is
//! ephemeral and is torn down, name included, because `cs thaw` re-creates it
//! under the same name. A pane left behind — even a dead one kept by
//! `remain-on-exit` — makes that respawn fail with `duplicate session`.

use chrono::Utc;
use cosmon_core::id::WorkerId;
use cosmon_core::transport::{TransportBackend, TransportError};
use cosmon_core::worker::{DesiredState, WorkerStatus};
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the `freeze` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// ID of the worker to freeze.
    pub worker: String,

    /// ID of the worker that is preempting this one (optional metadata).
    #[arg(long)]
    pub by: Option<String>,

    /// Operator-supplied reason for the freeze, recorded on the event.
    ///
    /// Under ADR-052 §D3, `cs freeze --reason <str>` subsumes the former
    /// `cs quench` verb: "graceful shutdown with state preservation" is
    /// what freeze already does, and the reason is the missing metadata
    /// that distinguished quench's intent.
    #[arg(long)]
    pub reason: Option<String>,

    /// Grace period in seconds before force-killing (default: 15).
    #[arg(long, default_value = "15")]
    pub timeout: u64,

    /// Skip tmux interaction (state-only transition, for testing).
    #[arg(long)]
    pub no_tmux: bool,
}

/// Stop the worker's session and free its name for a later `cs thaw`.
///
/// `graceful_exit` alone does not free the name: with `remain-on-exit` (which
/// every `cs tackle` sets to arm its `pane-died` witness) the adapter exits and
/// tmux keeps the dead pane under the session name. A worker whose adapter had
/// already exited is not even visible to `graceful_exit`, which then answers
/// `NotFound`. The name-addressed [`TransportBackend::terminate_session`]
/// reaches both cases and is idempotent when the name is already free.
///
/// Returns whether the adapter exited gracefully; `false` means it was
/// force-stopped or was not running.
///
/// # Errors
/// Returns the transport error when the session name cannot be freed.
fn stop_session(
    backend: &dyn TransportBackend,
    worker_id: &WorkerId,
    timeout: std::time::Duration,
) -> Result<bool, TransportError> {
    let graceful = backend.graceful_exit(worker_id, timeout).unwrap_or(false);
    backend.terminate_session(worker_id.as_str())?;
    Ok(graceful)
}

/// Execute the `freeze` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if args.no_tmux {
        return run_with_backend(ctx, args, None);
    }
    let backend = TmuxBackend::new(super::tmux_socket_name(ctx));
    run_with_backend(ctx, args, Some(&backend))
}

/// Execute `freeze` against an explicit transport (`None` = state-only).
///
/// Separated from [`run`] so tests can drive the whole command, state write
/// included, against a fake transport instead of a tmux server.
#[allow(clippy::too_many_lines)]
fn run_with_backend(
    ctx: &Context,
    args: &Args,
    backend: Option<&dyn TransportBackend>,
) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let worker_id = WorkerId::new(&args.worker)?;

    let preemptor_id = args
        .by
        .as_ref()
        .map(|s| WorkerId::new(s).map_err(|e| anyhow::anyhow!(e)))
        .transpose()?;

    let mut fleet = store.load_fleet()?;

    let worker = fleet
        .workers
        .get_mut(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker not found: {worker_id}"))?;

    // Only Running workers can be frozen.
    if worker.desired != DesiredState::Running {
        return Err(anyhow::anyhow!(
            "cannot freeze worker {worker_id}: desired is {} (expected running)",
            worker.desired
        ));
    }

    // Stop the session and free its name before recording the pause. If the
    // name cannot be freed the worker is not recorded as paused: a `Paused`
    // record over an occupied name is exactly the state `cs thaw` cannot
    // leave. `unclean` records that the adapter did not exit on request.
    let mut unclean = false;
    if let Some(backend) = backend {
        let timeout = std::time::Duration::from_secs(args.timeout);
        let graceful = stop_session(backend, &worker_id, timeout).map_err(|e| {
            anyhow::anyhow!("cannot freeze worker {worker_id}: failed to stop its session: {e}")
        })?;
        unclean = !graceful;
    }

    let now = Utc::now();

    // Freeze: save molecule state, transition to Paused.
    let worker = fleet
        .workers
        .get_mut(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker vanished: {worker_id}"))?;

    worker.frozen_molecule = worker.current_molecule.clone();
    worker.frozen_at = Some(now);
    worker.preempted_by.clone_from(&preemptor_id);
    worker.current_molecule = None;
    worker.desired = DesiredState::Paused;
    worker.status = WorkerStatus::Paused;
    worker.updated_at = now;

    store.save_fleet(&fleet)?;

    // Emit events.
    let events_path = state_dir.join("events.jsonl");
    let _ = cosmon_filestore::event::append(
        &events_path,
        &cosmon_core::event::Envelope::now(cosmon_core::event::Event::WorkerFrozen {
            worker_id: worker_id.clone(),
            preempted_by: preemptor_id.clone(),
        }),
    );
    // Emit molecule_frozen if a molecule was associated with the worker.
    if let Some(ref mol_id) = fleet.workers[&worker_id].frozen_molecule {
        let _ = cosmon_filestore::event::append(
            &events_path,
            &cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeFrozen {
                molecule_id: mol_id.clone(),
            }),
        );

        // ADR-030 M3 — archive the frozen molecule's state so a reclone
        // or thaw observer can reconstruct the pause point. Non-fatal:
        // any failure is logged and the freeze still succeeds. The
        // `mol.archived` idempotence gate ensures a repeated freeze on
        // the same molecule never re-archives — the first write wins.
        let config_path = super::resolve_config_from_context(ctx);
        let project_config =
            cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
        if project_config.archive.enabled {
            if let Ok(mut mol) = store.load_molecule(mol_id) {
                if !mol.archived {
                    let mol_dir = cosmon_state::archive::resolve_molecule_dir(&state_dir, mol_id)
                        .unwrap_or_else(|| store.molecule_dir(mol_id));
                    if cosmon_state::archive::write_non_fatal(
                        &state_dir,
                        &mol_dir,
                        &mol,
                        cosmon_state::archive::Trigger::Freeze,
                        now,
                    )
                    .is_some()
                    {
                        mol.archived = true;
                        let _ = store.save_molecule(mol_id, &mol);
                    }
                }
            }
        }
    }

    if ctx.json {
        let mut out = serde_json::json!({
            "command": "freeze",
            "worker_id": worker_id.as_str(),
            "status": "paused",
            "frozen_at": now.to_rfc3339(),
            "unclean": unclean,
        });
        if let Some(ref mol) = fleet.workers[&worker_id].frozen_molecule {
            out["frozen_molecule"] = serde_json::json!(mol.as_str());
        }
        if let Some(ref by) = preemptor_id {
            out["preempted_by"] = serde_json::json!(by.as_str());
        }
        if let Some(ref reason) = args.reason {
            out["reason"] = serde_json::json!(reason);
        }
        println!("{out}");
    } else {
        let mut msg = format!("Frozen worker {worker_id} (active -> paused)");
        if unclean {
            msg.push_str(" [WARNING: session did not exit on request; force-stopped]");
        }
        if let Some(ref by) = preemptor_id {
            use std::fmt::Write;
            let _ = write!(msg, ", preempted by {by}");
        }
        if let Some(ref reason) = args.reason {
            use std::fmt::Write;
            let _ = write!(msg, ", reason: {reason}");
        }
        println!("{msg}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, MoleculeId, WorkerId};
    use cosmon_core::worker::WorkerStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, StateStore, WorkerData};
    use std::path::PathBuf;
    use tempfile::TempDir;

    use super::*;

    use cosmon_core::worker::DesiredState;

    /// A transport that behaves like tmux under `remain-on-exit`: the pane
    /// can die while its session name stays taken, and only a name-addressed
    /// teardown frees it.
    struct RemainOnExitTmux {
        /// Whether the adapter process behind the pane is running.
        pane_alive: std::cell::Cell<bool>,
        /// Whether the session name is still allocated.
        name_taken: std::cell::Cell<bool>,
        /// Make the name-addressed teardown fail, to exercise the error path.
        refuse_teardown: bool,
    }

    impl RemainOnExitTmux {
        fn live() -> Self {
            Self {
                pane_alive: std::cell::Cell::new(true),
                name_taken: std::cell::Cell::new(true),
                refuse_teardown: false,
            }
        }
    }

    impl TransportBackend for RemainOnExitTmux {
        fn spawn(
            &self,
            _agent: &cosmon_core::transport::AgentDefinition,
            _config: &cosmon_core::transport::RuntimeConfig,
        ) -> Result<cosmon_core::transport::SpawnHandle, TransportError> {
            Err(TransportError::Io("freeze never spawns".to_owned()))
        }

        fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
            // Like tmux `terminate`, it only sees live panes.
            if !self.pane_alive.get() {
                return Err(TransportError::NotFound(id.clone()));
            }
            self.pane_alive.set(false);
            self.name_taken.set(false);
            Ok(())
        }

        fn is_alive(&self, _id: &WorkerId) -> Result<bool, TransportError> {
            Ok(self.pane_alive.get())
        }

        fn send_input(&self, _id: &WorkerId, _input: &str) -> Result<(), TransportError> {
            Ok(())
        }

        fn capture_output(&self, _id: &WorkerId, _lines: usize) -> Result<String, TransportError> {
            Ok(String::new())
        }

        fn list_sessions(
            &self,
        ) -> Result<Vec<cosmon_core::transport::SessionInfo>, TransportError> {
            Ok(Vec::new())
        }

        fn graceful_exit(
            &self,
            id: &WorkerId,
            _timeout: std::time::Duration,
        ) -> Result<bool, TransportError> {
            // `/exit` ends the adapter; the dead pane keeps the name.
            if !self.pane_alive.get() {
                return Err(TransportError::NotFound(id.clone()));
            }
            self.pane_alive.set(false);
            Ok(true)
        }

        fn session_exists(&self, _session_name: &str) -> Result<bool, TransportError> {
            Ok(self.name_taken.get())
        }

        fn terminate_session(&self, _session_name: &str) -> Result<(), TransportError> {
            if self.refuse_teardown {
                return Err(TransportError::Io("kill-session failed".to_owned()));
            }
            self.pane_alive.set(false);
            self.name_taken.set(false);
            Ok(())
        }
    }

    fn freeze_args(worker: &str) -> Args {
        Args {
            worker: worker.to_owned(),
            by: None,
            reason: Some("x".to_owned()),
            timeout: 1,
            no_tmux: false,
        }
    }

    fn ctx_at(state_dir: PathBuf) -> Context {
        Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        }
    }

    /// The C4 reproduction: freeze a live worker, and the session name must be
    /// free afterwards, since `cs thaw` creates a new session under it. Before
    /// the fix freeze only asked the adapter to exit, the dead pane kept the
    /// name, and thaw failed with `duplicate session`.
    #[test]
    fn freeze_frees_the_session_name_thaw_will_respawn_under() {
        let (tmp, state_dir) =
            make_store_with_worker("quartz", &WorkerStatus::Active, Some("cs-20260406-abcd"));
        let tmux = RemainOnExitTmux::live();

        run_with_backend(&ctx_at(state_dir), &freeze_args("quartz"), Some(&tmux)).unwrap();

        assert!(
            !tmux.session_exists("quartz").unwrap(),
            "freeze left the session name taken; thaw would hit `duplicate session`"
        );
        let fleet = FileStore::new(tmp.path()).load_fleet().unwrap();
        let worker = &fleet.workers[&WorkerId::new("quartz").unwrap()];
        assert_eq!(worker.status, WorkerStatus::Paused);
    }

    /// A worker whose adapter already exited (the pane of a `completed`
    /// molecule) is invisible to `graceful_exit`. Freeze must still free the
    /// name.
    #[test]
    fn freeze_frees_the_name_of_an_already_dead_pane() {
        let (_tmp, state_dir) = make_store_with_worker("opal", &WorkerStatus::Active, None);
        let tmux = RemainOnExitTmux::live();
        tmux.pane_alive.set(false);

        run_with_backend(&ctx_at(state_dir), &freeze_args("opal"), Some(&tmux)).unwrap();

        assert!(!tmux.session_exists("opal").unwrap());
    }

    /// If the name cannot be freed, the worker is not recorded as paused:
    /// that record would promise a thaw that cannot succeed.
    #[test]
    fn freeze_refuses_to_record_a_pause_it_cannot_thaw() {
        let (tmp, state_dir) = make_store_with_worker("ruby", &WorkerStatus::Active, None);
        let tmux = RemainOnExitTmux {
            refuse_teardown: true,
            ..RemainOnExitTmux::live()
        };

        let err =
            run_with_backend(&ctx_at(state_dir), &freeze_args("ruby"), Some(&tmux)).unwrap_err();

        assert!(
            err.to_string().contains("failed to stop its session"),
            "{err}"
        );
        let fleet = FileStore::new(tmp.path()).load_fleet().unwrap();
        let worker = &fleet.workers[&WorkerId::new("ruby").unwrap()];
        assert_eq!(worker.desired, DesiredState::Running);
    }

    fn make_store_with_worker(
        name: &str,
        status: &WorkerStatus,
        molecule: Option<&str>,
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
            status.clone(),
        );
        worker.desired = match &status {
            WorkerStatus::Active => DesiredState::Running,
            WorkerStatus::Paused => DesiredState::Paused,
            _ => DesiredState::Stopped,
        };
        if let Some(m) = molecule {
            worker = worker.with_molecule(MoleculeId::new(m).unwrap());
        }
        fleet.workers.insert(wid, worker);
        store.save_fleet(&fleet).unwrap();
        (tmp, path)
    }

    #[test]
    fn test_freeze_active_worker() {
        let (tmp, state_dir) =
            make_store_with_worker("quartz", &WorkerStatus::Active, Some("cs-20260406-abcd"));
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "quartz".to_owned(),
            by: None,
            reason: None,
            timeout: 5,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("quartz").unwrap();
        let worker = fleet.workers.get(&wid).unwrap();
        assert_eq!(worker.desired, DesiredState::Paused);
        assert_eq!(worker.status, WorkerStatus::Paused);
        assert!(worker.current_molecule.is_none());
        assert_eq!(
            worker.frozen_molecule.as_ref().unwrap().as_str(),
            "cs-20260406-abcd"
        );
        assert!(worker.frozen_at.is_some());
        assert!(worker.preempted_by.is_none());
    }

    #[test]
    fn test_freeze_with_preemptor() {
        let (tmp, state_dir) = make_store_with_worker("ruby", &WorkerStatus::Active, None);
        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "ruby".to_owned(),
            by: Some("topaz".to_owned()),
            reason: None,
            timeout: 5,
            no_tmux: true,
        };

        run(&ctx, &args).unwrap();

        let store = FileStore::new(tmp.path());
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("ruby").unwrap();
        let worker = fleet.workers.get(&wid).unwrap();
        assert_eq!(worker.desired, DesiredState::Paused);
        assert_eq!(worker.status, WorkerStatus::Paused);
        assert_eq!(worker.preempted_by.as_ref().unwrap().as_str(), "topaz");
    }

    #[test]
    fn test_freeze_stopped_worker_rejected() {
        // Worker with desired=Stopped cannot be frozen.
        let (_tmp, state_dir) = make_store_with_worker("opal", &WorkerStatus::Stopped, None);
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            worker: "opal".to_owned(),
            by: None,
            reason: None,
            timeout: 5,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("cannot freeze"));
    }

    #[test]
    fn test_freeze_nonexistent_worker() {
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
            by: None,
            reason: None,
            timeout: 5,
            no_tmux: true,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
