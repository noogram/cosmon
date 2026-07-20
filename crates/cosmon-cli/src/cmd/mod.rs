// SPDX-License-Identifier: AGPL-3.0-only

//! Subcommand implementations for the cosmon CLI.
//!
//! Each submodule corresponds to one CLI verb. All handlers receive a
//! [`Context`] carrying the global flags.

use std::path::PathBuf;

pub mod apps;
pub mod archive;
pub mod artifacts;
pub mod ask;
pub mod await_operator;
pub mod claim;
pub mod cluster;
pub mod collapse;
pub mod complete;
pub mod config;
pub mod cross_galaxy;
pub mod daemons;
pub mod demo;
pub mod deps;
pub mod diverge;
pub mod doctor;
pub mod done;
pub mod drop;
pub(crate) mod egress_delegate;
pub mod ensemble;
pub mod errors;
pub mod events;
pub mod evolve;
pub mod examples;
pub mod fleet;
pub mod freeze;
pub mod galaxies;
pub mod guard;
pub mod harvest;
pub mod health;
pub mod heartbeat;
pub mod help;
pub mod inbox;
pub mod init;
pub mod inspect;
pub mod interaction;
pub mod key;
pub mod kill;
pub mod lineage;
pub mod listen;
pub mod livelock;
pub mod markdown_help;
pub mod migrate;
pub mod mission;
pub mod motion;
pub mod mur;
pub mod notarize;
pub mod note;
pub mod notify;
pub mod nucleate;
pub mod observe;
pub mod opt_in_share;
pub mod panel;
pub mod paths;
pub mod patrol;
pub mod patrol_abandon;
pub mod patrol_heal;
pub mod peek;
pub mod peek_tui;
pub mod pilot;
pub mod preflight;
pub mod presence;
pub mod prime;
pub mod pulse;
pub mod purge;
pub mod quench;
pub mod realized_watch;
pub mod reconcile;
pub mod release_audit;
pub mod replay;
pub mod resume;
pub mod resurrect;
pub mod review;
pub mod route;
pub mod run;
pub mod scheduler;
pub mod security;
pub mod sensorium;
pub mod session;
pub mod spark;
pub mod spec_audit;
pub mod spore;
pub mod status;
pub mod stitch;
pub mod stress_test_lint;
pub mod stuck;
pub mod sync;
pub mod tackle;
pub mod tag;
pub mod tail;
pub mod teardown;
pub mod test;
pub mod thaw;
pub mod tokens;
pub mod topology;
pub mod trust;
pub mod validate;
pub mod verify;
pub mod verify_graph;
pub mod verify_trace;
pub mod vllm_mlx;
pub mod wait;
pub mod whisper;
pub mod witness;

/// Shared context derived from global CLI flags.
///
/// Threaded to every subcommand handler so they can respect `--verbose`,
/// `--json`, and `--config` without re-parsing.
#[allow(dead_code)]
pub struct Context {
    /// Whether `--verbose` was passed.
    pub verbose: bool,
    /// Whether `--json` was passed (NDJSON output mode).
    pub json: bool,
    /// Optional path to a configuration file.
    pub config: Option<PathBuf>,
}

/// Resolve the state directory from the global `--config` flag.
///
/// Delegates to [`cosmon_filestore::resolve_state_dir`] which uses
/// walk-up discovery (like `git` finding `.git/`). See that function
/// for the full precedence chain.
pub(crate) fn default_state_dir() -> PathBuf {
    cosmon_filestore::resolve_state_dir(None)
}

impl Context {
    /// Resolve the state directory honored by this invocation.
    ///
    /// The global `--config` flag (`ctx.config`) overrides walk-up
    /// discovery; otherwise [`default_state_dir`] is used. This is the
    /// canonical resolution shared by every handler.
    pub(crate) fn state_dir(&self) -> PathBuf {
        self.config.clone().unwrap_or_else(default_state_dir)
    }

    /// Obtain the hexagonal state-store adapter rooted at the resolved
    /// state directory.
    ///
    /// **This is the single seam where the persistence backend is chosen.**
    /// Handlers depend on `dyn StateStore`, not on the concrete JSON
    /// adapter — swapping to a SQLite/Dolt backend means changing this one
    /// method, not the ~30 call sites that imported
    /// `cosmon_filestore::FileStore` directly (the decorative-port pathology
    /// flagged in delib-20260622-187a F-ARCH-6). The
    /// `cosmon_filestore::FileStore` name must not reappear in a command's
    /// *production* path; tests may still construct a concrete adapter for
    /// fixture setup.
    pub(crate) fn store(&self) -> Box<dyn cosmon_state::StateStore> {
        self.store_at(self.state_dir())
    }

    /// Obtain the state-store adapter rooted at an explicit state
    /// directory.
    ///
    /// Worker-callable commands (`cs evolve`, `cs complete`, `cs collapse`)
    /// accept a per-invocation `--ops-dir` override that may differ from the
    /// global `--config`. They resolve that path themselves and then ask the
    /// Context to build the adapter, keeping the `FileStore` construction in
    /// the same single seam as [`Context::store`].
    ///
    /// `&self` is intentionally retained even though the current JSON
    /// adapter ignores it: keeping backend construction a method of
    /// `Context` means a future adapter that *selects* its backend from
    /// `self.config` / env (the SQLite/Dolt path) extends this one method
    /// rather than reintroducing scattered construction sites.
    #[allow(clippy::unused_self)]
    pub(crate) fn store_at(
        &self,
        state_dir: impl Into<PathBuf>,
    ) -> Box<dyn cosmon_state::StateStore> {
        open_store(state_dir)
    }
}

/// The single point where the concrete persistence adapter is constructed.
///
/// [`Context::store`] / [`Context::store_at`] funnel through here, and so do
/// the handful of Context-free reads that address a state directory *directly*
/// rather than selecting this invocation's own backend: a foreign galaxy's
/// store (`cs deps`, cross-galaxy ref resolution) and a captured session's
/// store (`cs diverge`). Routing them through this helper keeps the
/// `cosmon_filestore::FileStore` name out of every command's production path
/// (delib-20260622-187a F-ARCH-6) — swapping the JSON backend for SQLite/Dolt
/// stays a one-function change even for path-addressed foreign reads.
///
/// A future backend that *selects* its implementation from env/config extends
/// this single function; [`Context::store_at`] keeps `&self` so the
/// Context-driven selection path can additionally consult `self.config`.
pub(crate) fn open_store(state_dir: impl Into<PathBuf>) -> Box<dyn cosmon_state::StateStore> {
    Box::new(cosmon_filestore::FileStore::new(state_dir.into()))
}

/// Guard: require a valid `project_id` in `.cosmon/config.toml`.
///
/// Transport-touching commands (tackle, done, watch, patrol) must call this
/// before proceeding. If `.cosmon/` exists but `config.toml` has no
/// `[project]` section with `project_id`, the user must run
/// `cs init --upgrade` to establish project identity.
pub(crate) fn require_project_identity(
    ctx: &Context,
) -> anyhow::Result<cosmon_core::id::ProjectId> {
    let config_path = resolve_config_from_context(ctx);
    cosmon_filestore::resolve_project_id(&config_path).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Derive the `config.toml` path from context.
///
/// When `--config` provides the state dir (`.cosmon/state/`), the config
/// file lives at `.cosmon/config.toml` (= parent of state dir). Falls back
/// to checking the state dir itself (for test environments where the state
/// dir is a flat temp directory), then to CWD walk-up discovery.
pub(crate) fn resolve_config_from_context(ctx: &Context) -> PathBuf {
    if let Some(ref state_dir) = ctx.config {
        // Production: state_dir = .cosmon/state/ → parent = .cosmon/
        if let Some(parent) = state_dir.parent() {
            let candidate = parent.join("config.toml");
            if candidate.exists() {
                return candidate;
            }
        }
        // Flat layout: config.toml next to fleet.json in the same dir.
        let sibling = state_dir.join("config.toml");
        if sibling.exists() {
            return sibling;
        }
    }
    cosmon_filestore::resolve_config_path(None)
}

/// Resolve the tmux socket name from project config.
///
/// Delegates to [`cosmon_filestore::resolve_tmux_socket_name`], which uses
/// `project_id` when set and otherwise derives a globally unique name from
/// the project root path. Every cosmon invocation passes this through
/// `tmux -L <socket>`, so two fleets on the same host never share a tmux
/// server (sibling-isolation invariant).
pub(crate) fn tmux_socket_name(ctx: &Context) -> String {
    let config_path = resolve_config_from_context(ctx);
    cosmon_filestore::resolve_tmux_socket_name(&config_path)
}

/// Resolve the working directory for a worker from its `repo` field.
///
/// Worker repo paths are stored relative to the project root (portability).
/// Legacy fleet.json files may contain absolute paths — those are used as-is.
/// Falls back to `$HOME` if no repo is set and no project root is available.
pub(crate) fn resolve_worker_workdir(
    worker: &cosmon_state::WorkerData,
    project_root: Option<&std::path::Path>,
) -> String {
    match (&worker.repo, project_root) {
        (Some(repo), Some(root)) => cosmon_filestore::resolve_repo_path(repo, root)
            .to_string_lossy()
            .into_owned(),
        (Some(repo), None) => repo.clone(),
        (None, _) => std::env::var("HOME").unwrap_or_else(|_| ".".to_owned()),
    }
}
