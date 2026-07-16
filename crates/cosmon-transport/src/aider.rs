// SPDX-License-Identifier: AGPL-3.0-only

//! Aider CLI session management — plain functions for spawn/kill/alive.
//!
//! These are standalone functions that manage Aider sessions
//! (`aider-chat`, the Python TUI installed via `pip` / `uv tool`) via
//! [`TmuxBackend`]. They map [`Clearance`] to Aider's flag surface
//! (`--no-auto-commits`, `--yes-always`, `--auto-accept-architect`)
//! and delegate all tmux operations to the backend.
//!
//! # Why a second Adapter exists
//!
//! ADR-097 PR-3 (C4): the substrate that forces the future
//! Worker-Spawn `Spawn` trait (C5) to be drawn against *two* real
//! implementations rather than retro-fitted to a single one. Mirrors
//! the four-function shape of [`crate::claude`] without sharing a
//! trait yet — the trait extraction is C5's job.
//!
//! # ADR-097 Worker-Spawn Port IFBDD trail
//!
//! Each spawn / kill / liveness probe / briefing-consume call is
//! instrumented with the same five `EventV2` variants the claude
//! adapter emits, but with `adapter_name = "aider"`. The free
//! emission helpers live in [`cosmon_state::events::worker_spawn`]
//! — call-site stability is part of the IFBDD discipline
//! (forgemaster §2.4): adapters call the free functions by name,
//! they do not own the schema.
//!
//! # Per-Adapter readiness
//!
//! `crate::readiness::classify_output` inspects Claude Code's TUI
//! markers, so an Aider pane returns
//! [`SessionStatus::Unknown`](crate::readiness::SessionStatus) under
//! that path — the *loud failure* that was the forcing
//! function for the readiness-per-Adapter refactor. That refactor has
//! landed: aider's readiness now lives in
//! [`crate::readiness::AiderProbe`], an aider-specific
//! [`LiveProbe`](crate::readiness::LiveProbe) that matches aider's own
//! banner / `>` REPL prompt via
//! [`aider_output_is_live`](crate::readiness::aider_output_is_live).
//! `cs tackle`'s aider spawn path waits on that probe through the same
//! `await_live` contract the Claude path uses — replacing the bespoke
//! `2s` / `is_alive` postcondition loop (B5).

use std::fmt;
use std::fmt::Write as _;
use std::path::Path;

use chrono::Utc;
use cosmon_core::clearance::Clearance;
use cosmon_core::event_v2::{AdapterHandleState, AdapterProbeKind, AdapterProbeResult};
use cosmon_core::id::WorkerId;
use cosmon_state::events::worker_spawn::{
    emit_adapter_briefing_consumed, emit_adapter_handle_reconciled, emit_adapter_liveness_probed,
    emit_worker_spawn_attempted, emit_worker_spawn_failed,
};

use crate::spawn::SpawnError;
use crate::TmuxBackend;

// `AdapterTelemetry` was lifted into `crate::spawn` (ADR-097 / PR-4).
// Re-exported here so existing callers (`aider::AdapterTelemetry`)
// keep compiling unchanged — same gesture as `crate::claude`.
pub use crate::spawn::AdapterTelemetry;

/// The adapter-name token carried on every Worker-Spawn Port event the
/// aider transport emits.
///
/// As with [`crate::claude::ADAPTER_NAME`], the value is a free-form
/// `String` on the wire (ADR-079 §1). Registered in
/// [`crate::registry::default_registry`] alongside Aider's pane
/// signatures.
pub const ADAPTER_NAME: &str = "aider";

/// Aider flag-bundles, mapped from [`Clearance`].
///
/// Unlike Claude's single `--permission-mode` flag, Aider's safety
/// surface is a *set* of flags. Each variant materialises a stable
/// argument list at spawn time so the mapping is visible to the
/// auditor without re-deriving it from the CLI string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiderPermissionFlags {
    /// Read-only — Aider may *propose* edits but never commits.
    Plan,
    /// Read + write — Aider applies edits without prompting, no
    /// auto-commits (operator merges via cosmon's branch flow).
    AcceptEdits,
    /// Full autonomy — Aider applies edits *and* architect-mode
    /// changes without prompting. Closest analogue of Claude's
    /// `--bypass-permissions`.
    Bypass,
}

impl AiderPermissionFlags {
    /// Return the flag list this variant injects into the spawn
    /// command line. Order matches the briefing's safe-default
    /// recipe.
    #[must_use]
    pub fn as_flags(self) -> &'static [&'static str] {
        match self {
            Self::Plan => &["--no-auto-commits"],
            Self::AcceptEdits => &["--yes-always", "--no-auto-commits"],
            Self::Bypass => &["--yes-always", "--auto-accept-architect"],
        }
    }
}

impl fmt::Display for AiderPermissionFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_flags().join(" ").as_str())
    }
}

impl From<Clearance> for AiderPermissionFlags {
    fn from(c: Clearance) -> Self {
        match c {
            Clearance::Read => Self::Plan,
            Clearance::Write => Self::AcceptEdits,
            Clearance::Execute => Self::Bypass,
        }
    }
}

/// Error type for Aider session operations.
#[derive(Debug, thiserror::Error)]
pub enum AiderError {
    /// Failed to spawn the Aider session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    /// Failed to kill the session.
    #[error("kill failed: {0}")]
    KillFailed(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(String),
}

/// Configuration for spawning an Aider session.
#[derive(Debug, Clone)]
pub struct AiderSessionConfig {
    /// Tmux socket name.
    pub socket: String,
    /// Tmux session name.
    pub session_name: String,
    /// Working directory for the Aider session.
    pub work_dir: String,
    /// Flag bundle derived from agent clearance.
    pub permission_flags: AiderPermissionFlags,
    /// Model identifier passed to `aider --model <model>` — the lever
    /// that serves academy's per-role routing
    /// (e.g. `kimi-k2.6`, `gemini-3.1-pro`). Constructor-injected by
    /// design; **not** exposed at the `cs tackle` flag level in C4
    /// (torvalds §3.2 single-axis discipline).
    pub model: String,
    /// Optional initial prompt to send to Aider via `--message`.
    pub prompt: Option<String>,
    /// Extra arguments appended after the clearance + model flags.
    /// Populated by the future C6 TOML loader
    /// (`[adapters.aider].extra_args`).
    pub extra_args: Vec<String>,
    /// Optional IFBDD telemetry context (ADR-097).
    pub telemetry: Option<AdapterTelemetry>,
    /// Optional pre-existing worker the spawn path detected under the
    /// target session name.
    pub pre_existing_worker: Option<WorkerId>,
}

/// Spawn a new Aider session in a tmux window.
///
/// Builds `aider --model <model> <permission-flags> [--message ...]
/// [extra args]` and delegates the tmux session creation to
/// [`TmuxBackend::spawn_worker`] (renamed in ADR-097 / PR-4 from the
/// historical `spawn_claude` — it is, and always was, a generic
/// "spawn a command in a tmux session with cwd" primitive).
///
/// When [`AiderSessionConfig::telemetry`] is `Some`, emits
/// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
/// *before* the backend call (ADR-097 WS-1: record the *attempt*).
///
/// # Errors
///
/// Returns [`AiderError::SpawnFailed`] if the tmux session cannot be
/// created.
pub fn spawn_aider_session(config: &AiderSessionConfig) -> Result<(), AiderError> {
    let mut cmd = format!("aider --model {}", shell_escape(&config.model));
    for flag in config.permission_flags.as_flags() {
        cmd.push(' ');
        cmd.push_str(flag);
    }
    if let Some(ref prompt) = config.prompt {
        let escaped = prompt.replace('\'', "'\\''");
        let _ = write!(cmd, " --message '{escaped}'");
    }
    for extra in &config.extra_args {
        cmd.push(' ');
        cmd.push_str(&shell_escape(extra));
    }

    if let Some(t) = &config.telemetry {
        emit_worker_spawn_attempted(
            &t.state_dir,
            &t.mol_id,
            &t.worker_id,
            ADAPTER_NAME,
            &config.work_dir,
            &t.invocation_uuid,
            0,
            config.pre_existing_worker.as_ref(),
        );
    }

    let backend = TmuxBackend::new(&config.socket);
    let outcome = backend
        .spawn_worker(&config.session_name, &config.work_dir, &cmd)
        .map_err(|e| AiderError::SpawnFailed(e.to_string()));

    // ADR-097 / WS-1' (delib-20260519-e6db W3 / adversary F1.3) —
    // record the terminal partner of WorkerSpawnAttempted when the
    // backend refused the spawn. Symmetrical with claude.rs so the
    // cross-adapter cat-test catches the case for both Adapters.
    if let (Err(err), Some(t)) = (&outcome, &config.telemetry) {
        emit_worker_spawn_failed(
            &t.state_dir,
            &t.mol_id,
            &t.worker_id,
            ADAPTER_NAME,
            &err.to_string(),
        );
    }

    outcome
}

/// Kill an Aider session by tmux session name. Mirrors
/// [`crate::claude::kill_session`] semantically; emits an
/// [`EventV2::AdapterHandleReconciled`](cosmon_core::event_v2::EventV2::AdapterHandleReconciled)
/// when telemetry is attached.
///
/// # Errors
///
/// Returns [`AiderError::KillFailed`] if the session cannot be killed.
pub fn kill_session(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<(), AiderError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| AiderError::KillFailed(e.to_string()))?;
    let outcome = backend
        .terminate(&wid)
        .map_err(|e| AiderError::KillFailed(e.to_string()));

    if let Some(t) = telemetry {
        let released_at = Utc::now();
        let handle_state = if outcome.is_ok() {
            AdapterHandleState::ReleasedClean
        } else {
            AdapterHandleState::ReleasedOrphan
        };
        emit_adapter_handle_reconciled(
            &t.state_dir,
            &t.mol_id,
            &t.worker_id,
            ADAPTER_NAME,
            handle_state,
            None,
            released_at,
            0,
        );
    }

    outcome
}

/// Check whether an Aider tmux session is alive.
///
/// Emits [`EventV2::AdapterLivenessProbed`](cosmon_core::event_v2::EventV2::AdapterLivenessProbed)
/// with `probe_kind = PaneSignature` when telemetry is attached.
///
/// # Errors
///
/// Returns [`AiderError::Io`] if the tmux command fails unexpectedly.
pub fn check_alive(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<bool, AiderError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| AiderError::Io(e.to_string()))?;
    let outcome = backend
        .is_alive(&wid)
        .map_err(|e| AiderError::Io(e.to_string()));

    if let (Some(t), Ok(alive)) = (telemetry, &outcome) {
        let probe_result = if *alive {
            AdapterProbeResult::Alive {
                evidence: format!("tmux session {session_name} exists on socket {socket}"),
            }
        } else {
            AdapterProbeResult::Stuck {
                reason: format!("tmux session {session_name} absent on socket {socket}"),
            }
        };
        emit_adapter_liveness_probed(
            &t.state_dir,
            &t.mol_id,
            &t.worker_id,
            ADAPTER_NAME,
            AdapterProbeKind::PaneSignature,
            probe_result,
            0,
        );
    }

    outcome
}

/// Read a briefing file and emit
/// [`EventV2::AdapterBriefingConsumed`](cosmon_core::event_v2::EventV2::AdapterBriefingConsumed)
/// recording observed-vs-recorded seal disagreement (ADR-097 / WS-4).
///
/// Identical contract to [`crate::claude::consume_briefing`]; the
/// `adapter_name` carried on the event is `"aider"`.
///
/// # Errors
///
/// Returns [`AiderError::Io`] if the briefing cannot be read.
pub fn consume_briefing(
    briefing_path: &Path,
    recorded_seal: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<Vec<u8>, AiderError> {
    let bytes = std::fs::read(briefing_path).map_err(|e| AiderError::Io(e.to_string()))?;

    if let Some(t) = telemetry {
        let observed_seal = blake3::hash(&bytes).to_hex().to_string();
        let path_str = briefing_path.to_string_lossy().into_owned();
        emit_adapter_briefing_consumed(
            &t.state_dir,
            &t.mol_id,
            &t.worker_id,
            ADAPTER_NAME,
            &path_str,
            &observed_seal,
            recorded_seal,
            bytes.len() as u64,
            Utc::now(),
        );
    }

    Ok(bytes)
}

/// Build an [`AiderSessionConfig`] from common parameters.
///
/// Telemetry is left unset; see [`session_config_with_telemetry`] for
/// the IFBDD-instrumented variant.
#[must_use]
pub fn session_config(
    socket: impl Into<String>,
    session_name: impl Into<String>,
    work_dir: impl AsRef<Path>,
    clearance: Clearance,
    model: impl Into<String>,
    prompt: Option<String>,
) -> AiderSessionConfig {
    AiderSessionConfig {
        socket: socket.into(),
        session_name: session_name.into(),
        work_dir: work_dir.as_ref().to_string_lossy().into_owned(),
        permission_flags: clearance.into(),
        model: model.into(),
        prompt,
        extra_args: Vec::new(),
        telemetry: None,
        pre_existing_worker: None,
    }
}

/// Build an [`AiderSessionConfig`] carrying an IFBDD telemetry context.
#[must_use]
pub fn session_config_with_telemetry(
    socket: impl Into<String>,
    session_name: impl Into<String>,
    work_dir: impl AsRef<Path>,
    clearance: Clearance,
    model: impl Into<String>,
    prompt: Option<String>,
    telemetry: AdapterTelemetry,
) -> AiderSessionConfig {
    let mut config = session_config(socket, session_name, work_dir, clearance, model, prompt);
    config.telemetry = Some(telemetry);
    config
}

/// Minimal shell-escape — wraps `s` in single quotes when it contains
/// any character outside the safe ASCII subset. Mirrors the helper
/// inside [`crate::TmuxBackend`] without re-exporting it.
fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl From<AiderError> for SpawnError {
    fn from(e: AiderError) -> Self {
        match e {
            AiderError::SpawnFailed(m) => Self::SpawnFailed(m),
            AiderError::KillFailed(m) => Self::KillFailed(m),
            AiderError::Io(m) => Self::Io(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::{Envelope, EventV2};
    use cosmon_core::id::MoleculeId;
    use std::fs;
    use tempfile::tempdir;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260517-85fe").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-bbbb").unwrap()
    }

    fn telemetry(state_dir: &Path) -> AdapterTelemetry {
        AdapterTelemetry::new(mol(), wkr(), state_dir.to_owned(), "uuid-aider")
    }

    fn read_envelopes(state_dir: &Path) -> Vec<Envelope> {
        let path = state_dir.join("events.jsonl");
        let raw = fs::read_to_string(&path).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Envelope::from_line(l).expect("envelope must parse"))
            .collect()
    }

    #[test]
    fn clearance_maps_to_flags() {
        assert_eq!(
            AiderPermissionFlags::from(Clearance::Read),
            AiderPermissionFlags::Plan
        );
        assert_eq!(
            AiderPermissionFlags::from(Clearance::Write),
            AiderPermissionFlags::AcceptEdits
        );
        assert_eq!(
            AiderPermissionFlags::from(Clearance::Execute),
            AiderPermissionFlags::Bypass
        );
    }

    #[test]
    fn flag_bundles_match_briefing_recipe() {
        assert_eq!(
            AiderPermissionFlags::Plan.as_flags(),
            &["--no-auto-commits"]
        );
        assert_eq!(
            AiderPermissionFlags::AcceptEdits.as_flags(),
            &["--yes-always", "--no-auto-commits"]
        );
        assert_eq!(
            AiderPermissionFlags::Bypass.as_flags(),
            &["--yes-always", "--auto-accept-architect"]
        );
    }

    #[test]
    fn session_config_builder_sets_model_and_clears_telemetry() {
        let config = session_config(
            "cosmon",
            "aider-test",
            "/tmp/work",
            Clearance::Write,
            "kimi-k2.6",
            Some("hello".to_owned()),
        );
        assert_eq!(config.socket, "cosmon");
        assert_eq!(config.session_name, "aider-test");
        assert_eq!(config.work_dir, "/tmp/work");
        assert_eq!(config.permission_flags, AiderPermissionFlags::AcceptEdits);
        assert_eq!(config.model, "kimi-k2.6");
        assert_eq!(config.prompt.as_deref(), Some("hello"));
        assert!(config.extra_args.is_empty());
        assert!(config.telemetry.is_none());
    }

    #[test]
    fn session_config_with_telemetry_attaches_context() {
        let dir = tempdir().unwrap();
        let t = telemetry(dir.path());
        let config = session_config_with_telemetry(
            "cosmon",
            "aider-test",
            "/tmp/work",
            Clearance::Execute,
            "gemini-3.1-pro",
            None,
            t.clone(),
        );
        assert!(config.telemetry.is_some());
        assert_eq!(config.telemetry.as_ref().unwrap().mol_id, t.mol_id);
    }

    /// ADR-097 WS-4 — `consume_briefing` emits an
    /// `AdapterBriefingConsumed` with `adapter_name = "aider"` and an
    /// observed seal computed from the bytes actually read.
    #[test]
    fn consume_briefing_emits_aider_event() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let briefing_path = state_dir.join("briefing.md");
        fs::write(&briefing_path, b"aider hello").unwrap();

        let t = telemetry(state_dir);
        let bytes = consume_briefing(&briefing_path, "deadbeef", Some(&t)).expect("read succeeds");
        assert_eq!(bytes, b"aider hello");

        let envelopes = read_envelopes(state_dir);
        let (observed, recorded, adapter) = envelopes
            .iter()
            .find_map(|e| match &e.event {
                EventV2::AdapterBriefingConsumed {
                    adapter_name,
                    briefing_seal_observed,
                    briefing_seal_recorded,
                    ..
                } => Some((
                    briefing_seal_observed.clone(),
                    briefing_seal_recorded.clone(),
                    adapter_name.clone(),
                )),
                _ => None,
            })
            .expect("AdapterBriefingConsumed event emitted");
        assert_eq!(adapter, "aider");
        assert_eq!(observed, blake3::hash(b"aider hello").to_hex().to_string());
        assert_eq!(recorded, "deadbeef");
    }

    /// Galileo §2.4 WS-4 silent-failure detection: an Aider spawn
    /// that never goes through `consume_briefing` leaves the audit
    /// query for `AdapterBriefingConsumed { adapter_name: "aider" }`
    /// empty even though other Worker-Spawn Port events are present.
    #[test]
    fn missing_consume_briefing_yields_empty_aider_audit_match() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        // Emit a WorkerSpawnAttempted so the file is non-empty.
        emit_worker_spawn_attempted(
            state_dir,
            &mol(),
            &wkr(),
            ADAPTER_NAME,
            "/tmp/wt",
            "uuid-aider",
            0,
            None,
        );

        let envelopes = read_envelopes(state_dir);
        let consumed = envelopes
            .iter()
            .filter(|e| {
                matches!(
                    &e.event,
                    EventV2::AdapterBriefingConsumed { adapter_name, .. }
                        if adapter_name == "aider"
                )
            })
            .count();
        assert_eq!(
            consumed, 0,
            "WS-4 detection: missing consume_briefing → empty audit match for aider"
        );
    }

    #[test]
    fn shell_escape_passes_safe_input_untouched() {
        assert_eq!(shell_escape("kimi-k2.6"), "kimi-k2.6");
        assert_eq!(shell_escape("gpt-4o-mini"), "gpt-4o-mini");
    }

    #[test]
    fn shell_escape_quotes_unsafe_input() {
        assert_eq!(shell_escape("a b"), "'a b'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn permission_flags_display_joins_with_spaces() {
        assert_eq!(
            AiderPermissionFlags::AcceptEdits.to_string(),
            "--yes-always --no-auto-commits"
        );
    }
}
