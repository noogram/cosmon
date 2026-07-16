// SPDX-License-Identifier: AGPL-3.0-only

//! Worker-Spawn Port data types — the shared envelope every spawn site
//! threads through.
//!
//! The historical `Spawn` trait (ADR-097 / PR-4) was deleted after a
//! kill-switch grep showed it earned nothing today: the in-tree spawn
//! dispatch in `cs tackle` is a literal `match adapter.as_str()` on
//! five names, and the only consumers of the trait method
//! `.spawn(&cfg)` were the in-process OpenAI/Anthropic providers,
//! whose impls were converted to inherent methods in the same rip.
//!
//! What stays: [`AdapterTelemetry`], [`SpawnConfig`], [`WorkerHandle`],
//! [`SpawnError`] — the data envelope IFBDD-instrumented spawn sites
//! exchange (ADR-097).

use std::path::PathBuf;

use cosmon_core::clearance::Clearance;
use cosmon_core::id::{MoleculeId, WorkerId};

/// IFBDD telemetry context every Adapter threads through its spawn /
/// kill / probe path (ADR-097).
///
/// Lifted from the duplicate per-Adapter `AdapterTelemetry` structs
/// that lived in `crate::claude` and `crate::aider` (identical shape
/// — the comments in `aider.rs` named C5 as the lift). Both modules
/// `pub use` this type so existing callers (`claude::AdapterTelemetry`
/// / `aider::AdapterTelemetry`) keep compiling unchanged.
#[derive(Debug, Clone)]
pub struct AdapterTelemetry {
    /// The molecule this worker is bound to.
    pub mol_id: MoleculeId,
    /// The worker identity the adapter is registering / probing /
    /// killing.
    pub worker_id: WorkerId,
    /// Path to the cosmon `state/` directory under which
    /// `events.jsonl` lives.
    pub state_dir: PathBuf,
    /// Random per-invocation identifier for the spawn attempt.
    /// Two distinct values for the same `(mol_id, worker_id)` is
    /// the audit signal for WS-1's double-spawn pathology.
    pub invocation_uuid: String,
    /// Optional override for the `adapter_name` stamped on
    /// provider-level IFBDD events (`WorkerSpawnAttempted`,
    /// `AdapterLivenessProbed`).
    ///
    /// `None` — the default — means the emitting provider stamps its
    /// own class constant (e.g. `OpenAIProvider` → `"openai"`).
    ///
    /// **Why this exists.** The built-in `local` floor reuses
    /// [`OpenAIProvider`] against an Ollama OpenAI-compat endpoint
    /// (`spawn_local_session`). Without this override the provider
    /// stamps `adapter_name = "openai"` on `events.jsonl`, which (a)
    /// breaches the ADR-099 cat-test invariant `adapter_selected ==
    /// worker_spawned` on the *default* dispatch path, and (b) makes an
    /// egress audit read a remote endpoint (`openai`) for a run that
    /// never left the machine. Set this to the
    /// [`ValidatedAdapterName`](cosmon_core::spawn_seam::ValidatedAdapterName)
    /// wire string (`"local"`) so the log states the truth.
    pub adapter_name: Option<String>,
}

impl AdapterTelemetry {
    /// Construct a telemetry context.
    ///
    /// `adapter_name` defaults to `None`, so the emitting provider
    /// stamps its own class constant. Override it with
    /// [`Self::with_adapter_name`] when the provider is reused under a
    /// different validated adapter identity (the `local` floor reuses
    /// [`OpenAIProvider`]).
    #[must_use]
    pub fn new(
        mol_id: MoleculeId,
        worker_id: WorkerId,
        state_dir: impl Into<PathBuf>,
        invocation_uuid: impl Into<String>,
    ) -> Self {
        Self {
            mol_id,
            worker_id,
            state_dir: state_dir.into(),
            invocation_uuid: invocation_uuid.into(),
            adapter_name: None,
        }
    }

    /// Override the `adapter_name` stamped on provider-level IFBDD
    /// events with the validated adapter identity. Builder-style.
    ///
    /// See [`Self::adapter_name`] for why the `local` floor needs this.
    #[must_use]
    pub fn with_adapter_name(mut self, adapter_name: impl Into<String>) -> Self {
        self.adapter_name = Some(adapter_name.into());
        self
    }
}

/// Shared spawn-time inputs every Adapter accepts.
///
/// Strictly the *intersection* of what `claude.rs` and `aider.rs`
/// need — Adapter-specific knobs (Claude has none extra; Aider's
/// `model` and `extra_args`) live on the Adapter struct itself,
/// constructor-injected per forgemaster §3.1. Widening this struct
/// to fit one Adapter is the anti-pattern the briefing names.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Tmux socket the session is created on.
    pub socket: String,
    /// Tmux session name (matches the [`WorkerId`] string form).
    pub session_name: String,
    /// Working directory for the worker process.
    pub work_dir: String,
    /// Agent clearance → each Adapter maps this to its own flag
    /// surface (`--permission-mode` for Claude; a flag bundle for
    /// Aider).
    pub clearance: Clearance,
    /// Optional initial prompt sent to the Adapter at spawn time.
    pub prompt: Option<String>,
    /// Optional IFBDD telemetry context (ADR-097). `None` preserves
    /// today's behaviour for callers (thaw / patrol respawn / tests)
    /// that have not yet been upgraded.
    pub telemetry: Option<AdapterTelemetry>,
    /// Pre-existing worker the spawn path detected under the target
    /// session name; recorded on `WorkerSpawnAttempted` so a tmux
    /// collision becomes auditable. `None` is the normal path.
    pub pre_existing_worker: Option<WorkerId>,
}

/// Handle returned by an Adapter's `spawn` method and passed to
/// `terminate` / `is_alive`.
///
/// Carries just enough state for an Adapter to address its own
/// worker on subsequent calls. Telemetry is carried through so the
/// reconcile / probe paths emit the same `(mol_id, worker_id)`
/// envelope as the original spawn.
///
/// `#[non_exhaustive]` — keeps
/// future handle fields (process id, container id, IPC handle…)
/// non-breaking. Use [`Self::new`] from downstream crates now that the
/// struct literal `WorkerHandle { … }` is sealed.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct WorkerHandle {
    /// Tmux socket the worker lives on.
    pub socket: String,
    /// Tmux session name.
    pub session_name: String,
    /// Telemetry context carried from spawn-time, if any.
    pub telemetry: Option<AdapterTelemetry>,
}

impl WorkerHandle {
    /// Construct a [`WorkerHandle`] from its three fields.
    ///
    /// Required path for downstream crates now that the struct is
    /// `#[non_exhaustive]` — the struct literal
    /// `WorkerHandle { socket, session_name, telemetry }` no longer
    /// compiles outside `cosmon-transport`.
    #[must_use]
    pub fn new(
        socket: impl Into<String>,
        session_name: impl Into<String>,
        telemetry: Option<AdapterTelemetry>,
    ) -> Self {
        Self {
            socket: socket.into(),
            session_name: session_name.into(),
            telemetry,
        }
    }
}

/// Unified spawn error variant for in-process adapter call sites.
///
/// Per-Adapter modules keep their own typed errors
/// ([`crate::claude::ClaudeError`], [`crate::aider::AiderError`])
/// for the free-function call sites; this enum is the IFBDD envelope
/// the in-process providers (`OpenAI`, `Anthropic`) surface from their
/// `spawn` method.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// Failed to spawn the worker session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    /// Failed to terminate the worker.
    #[error("kill failed: {0}")]
    KillFailed(String),
    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260517-f227").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-cccc").unwrap()
    }

    #[test]
    fn adapter_telemetry_round_trips_fields() {
        let dir = tempdir().unwrap();
        let t = AdapterTelemetry::new(mol(), wkr(), dir.path().to_owned(), "uuid-x");
        assert_eq!(t.mol_id, mol());
        assert_eq!(t.worker_id, wkr());
        assert_eq!(t.state_dir, dir.path());
        assert_eq!(t.invocation_uuid, "uuid-x");
        // Default: no override — the emitting provider stamps its own
        // class constant.
        assert_eq!(t.adapter_name, None);
    }

    /// The `local` floor reuses `OpenAIProvider`; `with_adapter_name`
    /// lets it stamp the validated identity (`"local"`) on
    /// provider-level events instead of the `"openai"` class constant.
    #[test]
    fn with_adapter_name_overrides_the_provider_class_constant() {
        let dir = tempdir().unwrap();
        let t = AdapterTelemetry::new(mol(), wkr(), dir.path().to_owned(), "uuid-x")
            .with_adapter_name("local");
        assert_eq!(t.adapter_name.as_deref(), Some("local"));
        // The other fields survive the builder mutation untouched.
        assert_eq!(t.invocation_uuid, "uuid-x");
    }

    #[test]
    fn spawn_config_is_clone() {
        let cfg = SpawnConfig {
            socket: "cosmon".into(),
            session_name: "polecat-cccc".into(),
            work_dir: "/tmp/wt".into(),
            clearance: Clearance::Execute,
            prompt: Some("hi".into()),
            telemetry: None,
            pre_existing_worker: None,
        };
        let c = cfg.clone();
        assert_eq!(c.session_name, "polecat-cccc");
    }

    #[test]
    fn worker_handle_round_trips_fields() {
        let h = WorkerHandle {
            socket: "cosmon".into(),
            session_name: "polecat-cccc".into(),
            telemetry: None,
        };
        assert_eq!(h.socket, "cosmon");
        let _: &Path = Path::new(&h.session_name);
    }
}
