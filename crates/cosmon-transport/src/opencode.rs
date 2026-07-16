// SPDX-License-Identifier: AGPL-3.0-only

//! `opencode` (sst/opencode) CLI session management — sibling of
//! [`crate::claude`], [`crate::aider`] and [`crate::codex`].
//!
//! Spawns the publicly-installable `opencode` binary
//! (`npm install -g opencode-ai`, or the `curl … | bash` installer; MIT)
//! inside a tmux pane — the same **outside-in safety valve** path codex
//! takes. Like codex it runs its
//! own agent loop inside the pane; cosmon supervises the *pane*, not the
//! loop, through the standard `pane-died` hook.
//!
//! # Architectural posture
//!
//! opencode is a [`crate::registry::SupervisionMode::TmuxPane`] /
//! `LoopOwnership::External` / `RuntimeOwnership::Vendor` adapter — same
//! category as claude/aider/codex (ADR-125). It is an
//! external CLI binary on PATH whose pane dies when the run completes; it is
//! **not** a Direct-API in-process adapter. The binary-name distinction is a
//! pane-signature concern (ADR-098 §C3), not a supervision-mode concern, so
//! this module introduces no new `SupervisionMode` variant — it reuses
//! `TmuxPane` verbatim.
//!
//! # Relation to [`crate::codex`]
//!
//! This module is a deliberate near-clone of [`crate::codex`], trimmed of
//! the codex-specific SF-7 version-pin machinery (which guarded a `codex
//! exec` exit-code quirk and is not load-bearing for opencode). The spawn /
//! kill / liveness / briefing-consume surface is identical so that the two
//! external-CLI adapters stay structurally aligned.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::event_v2::{AdapterHandleState, AdapterProbeKind, AdapterProbeResult};
use cosmon_core::id::WorkerId;
use cosmon_state::events::worker_spawn::{
    emit_adapter_briefing_consumed, emit_adapter_handle_reconciled, emit_adapter_liveness_probed,
    emit_worker_spawn_attempted,
};

use crate::spawn::SpawnError;
use crate::TmuxBackend;

pub use crate::spawn::AdapterTelemetry;

/// The adapter-name token carried on every Worker-Spawn Port event the
/// opencode transport emits.
pub const ADAPTER_NAME: &str = "opencode";

/// Error type for opencode adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum OpencodeError {
    /// Failed to spawn the opencode session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    /// Failed to kill the session.
    #[error("kill failed: {0}")]
    KillFailed(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<OpencodeError> for SpawnError {
    fn from(e: OpencodeError) -> Self {
        match e {
            OpencodeError::SpawnFailed(m) => Self::SpawnFailed(m),
            OpencodeError::KillFailed(m) => Self::KillFailed(m),
            OpencodeError::Io(m) => Self::Io(m),
        }
    }
}

/// Configuration for spawning an opencode session.
#[derive(Debug, Clone)]
pub struct OpencodeSessionConfig {
    /// Tmux socket name.
    pub socket: String,
    /// Tmux session name.
    pub session_name: String,
    /// Working directory. The tmux session is created with this as its
    /// cwd (`tmux new-session -c <work_dir>`), so `opencode run` inherits
    /// it without a redundant `--cwd` flag. Retained on the config because
    /// the spawn-telemetry envelope records it.
    pub work_dir: String,
    /// Resolved opencode binary path (PATH-relative or absolute).
    pub binary: PathBuf,
    /// Optional prompt forwarded as the positional message to `opencode run`.
    pub prompt: Option<String>,
    /// Optional IFBDD telemetry context.
    pub telemetry: Option<AdapterTelemetry>,
    /// Optional pre-existing worker the spawn path detected.
    pub pre_existing_worker: Option<WorkerId>,
}

/// Spawn an opencode session in a tmux window.
///
/// Builds `opencode run [prompt]` and delegates to
/// [`TmuxBackend::spawn_worker`]. `opencode run` is opencode's
/// non-interactive automation subcommand — the counterpart of `codex exec`.
/// The working directory is supplied by the tmux session itself
/// (`new-session -c <work_dir>`), so no `--cwd` flag is passed. Emits
/// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
/// *before* the backend call when telemetry is attached.
///
/// # Errors
///
/// Returns [`OpencodeError::SpawnFailed`] when the tmux session cannot be
/// created.
pub fn spawn_opencode_session(config: &OpencodeSessionConfig) -> Result<(), OpencodeError> {
    let mut cmd = shell_escape(&config.binary.to_string_lossy());
    cmd.push_str(" run");
    if let Some(ref prompt) = config.prompt {
        let escaped = prompt.replace('\'', "'\\''");
        let _ = write!(cmd, " '{escaped}'");
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
    backend
        .spawn_worker(&config.session_name, &config.work_dir, &cmd)
        .map_err(|e| OpencodeError::SpawnFailed(e.to_string()))
}

/// Kill an opencode session by tmux session name.
///
/// Emits [`EventV2::AdapterHandleReconciled`](cosmon_core::event_v2::EventV2::AdapterHandleReconciled)
/// when telemetry is attached.
///
/// # Errors
///
/// Returns [`OpencodeError::KillFailed`] if the session cannot be killed.
pub fn kill_session(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<(), OpencodeError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| OpencodeError::KillFailed(e.to_string()))?;
    let outcome = backend
        .terminate(&wid)
        .map_err(|e| OpencodeError::KillFailed(e.to_string()));

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

/// Check whether an opencode tmux session is alive.
///
/// Emits [`EventV2::AdapterLivenessProbed`](cosmon_core::event_v2::EventV2::AdapterLivenessProbed)
/// with `probe_kind = PaneSignature` when telemetry is attached.
///
/// # Errors
///
/// Returns [`OpencodeError::Io`] if the tmux command fails unexpectedly.
pub fn check_alive(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<bool, OpencodeError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| OpencodeError::Io(e.to_string()))?;
    let outcome = backend
        .is_alive(&wid)
        .map_err(|e| OpencodeError::Io(e.to_string()));

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

/// Read `briefing.md` bytes and emit
/// [`EventV2::AdapterBriefingConsumed`](cosmon_core::event_v2::EventV2::AdapterBriefingConsumed)
/// with `adapter_name = "opencode"`.
///
/// # Errors
///
/// Returns [`OpencodeError::Io`] if the briefing cannot be read.
pub fn consume_briefing(
    briefing_path: &Path,
    recorded_seal: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<Vec<u8>, OpencodeError> {
    let bytes = std::fs::read(briefing_path).map_err(|e| OpencodeError::Io(e.to_string()))?;

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

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Minimal shell-escape — wraps `s` in single quotes when it contains
/// any character outside the safe ASCII subset. Mirrors
/// [`crate::codex::shell_escape`].
fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::{Envelope, EventV2};
    use cosmon_core::id::MoleculeId;
    use std::fs;
    use tempfile::tempdir;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260615-556a").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-opencode").unwrap()
    }

    fn telemetry(state_dir: &Path) -> AdapterTelemetry {
        AdapterTelemetry::new(mol(), wkr(), state_dir.to_owned(), "uuid-opencode")
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
    fn shell_escape_passes_safe_input_untouched() {
        assert_eq!(shell_escape("opencode"), "opencode");
        assert_eq!(
            shell_escape("/usr/local/bin/opencode"),
            "/usr/local/bin/opencode"
        );
    }

    #[test]
    fn shell_escape_quotes_unsafe_input() {
        assert_eq!(shell_escape("with spaces"), "'with spaces'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn spawn_config_round_trips_fields() {
        let cfg = OpencodeSessionConfig {
            socket: "cosmon".into(),
            session_name: "polecat-opencode".into(),
            work_dir: "/tmp/wt".into(),
            binary: PathBuf::from("opencode"),
            prompt: Some("hello".into()),
            telemetry: None,
            pre_existing_worker: None,
        };
        let c = cfg.clone();
        assert_eq!(c.session_name, "polecat-opencode");
        assert_eq!(c.prompt.as_deref(), Some("hello"));
    }

    /// `consume_briefing` emits the same WS-4 envelope shape as
    /// claude/aider/codex, with `adapter_name = "opencode"`.
    #[test]
    fn consume_briefing_emits_opencode_event() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let briefing_path = state_dir.join("briefing.md");
        fs::write(&briefing_path, b"opencode hello").unwrap();

        let t = telemetry(state_dir);
        let bytes = consume_briefing(&briefing_path, "deadbeef", Some(&t)).expect("read succeeds");
        assert_eq!(bytes, b"opencode hello");

        let envelopes = read_envelopes(state_dir);
        let (adapter, observed) = envelopes
            .iter()
            .find_map(|e| match &e.event {
                EventV2::AdapterBriefingConsumed {
                    adapter_name,
                    briefing_seal_observed,
                    ..
                } => Some((adapter_name.clone(), briefing_seal_observed.clone())),
                _ => None,
            })
            .expect("AdapterBriefingConsumed event emitted");
        assert_eq!(adapter, "opencode");
        assert_eq!(
            observed,
            blake3::hash(b"opencode hello").to_hex().to_string()
        );
    }
}
