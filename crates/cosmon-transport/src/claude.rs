// SPDX-License-Identifier: AGPL-3.0-only

//! Claude Code session management — plain functions for spawn/kill/alive.
//!
//! These are standalone functions that manage Claude Code sessions via
//! [`TmuxBackend`]. They map [`Clearance`] to Claude's `--permission-mode`
//! flag and delegate all tmux operations to the backend (no duplicate
//! tmux command execution).
//!
//! # ADR-097 Worker-Spawn Port IFBDD trail
//!
//! Each spawn / kill / liveness probe is instrumented with one of the
//! five Worker-Spawn Port `EventV2` variants
//! ([`cosmon_core::event_v2::EventV2::WorkerSpawnAttempted`] /
//! [`AdapterLivenessProbed`](cosmon_core::event_v2::EventV2::AdapterLivenessProbed) /
//! [`AdapterBriefingConsumed`](cosmon_core::event_v2::EventV2::AdapterBriefingConsumed) /
//! [`AdapterHandleReconciled`](cosmon_core::event_v2::EventV2::AdapterHandleReconciled)).
//! The wiring is **opt-in**: a caller without an [`AdapterTelemetry`] context
//! sees today's behaviour (no events emitted), preserving the integration
//! tests and the thaw / patrol respawn paths that have not yet been
//! upgraded.

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
// Re-exported here so existing callers (`claude::AdapterTelemetry`)
// keep compiling unchanged — null behavioural move per the briefing.
pub use crate::spawn::AdapterTelemetry;

/// The adapter-name token carried on every Worker-Spawn Port event the
/// claude transport emits.
///
/// The value is a free-form `String` on the wire (ADR-079 §1 — values of
/// the existing `Adapter` primitive, not a new typed enum). For C2 only
/// `"claude"` is populated; future adapters add their own constant.
pub const ADAPTER_NAME: &str = "claude";

/// Claude Code permission modes, mapped from [`Clearance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    /// Plan-only mode — Claude can read and plan but not edit.
    Plan,
    /// Accept edits — Claude can read and write files.
    AcceptEdits,
    /// Bypass all permission checks — full autonomous execution.
    BypassPermissions,
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan => f.write_str("plan"),
            Self::AcceptEdits => f.write_str("acceptEdits"),
            Self::BypassPermissions => f.write_str("bypassPermissions"),
        }
    }
}

impl From<Clearance> for PermissionMode {
    fn from(c: Clearance) -> Self {
        match c {
            Clearance::Read => Self::Plan,
            Clearance::Write => Self::AcceptEdits,
            Clearance::Execute => Self::BypassPermissions,
        }
    }
}

/// Error type for Claude session operations.
#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    /// Failed to spawn the Claude session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    /// Failed to kill the session.
    #[error("kill failed: {0}")]
    KillFailed(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(String),
}

/// Configuration for spawning a Claude session.
#[derive(Debug, Clone)]
pub struct ClaudeSessionConfig {
    /// Tmux socket name.
    pub socket: String,
    /// Tmux session name.
    pub session_name: String,
    /// Working directory for the Claude session.
    pub work_dir: String,
    /// Permission mode derived from agent clearance.
    pub permission_mode: PermissionMode,
    /// Optional initial prompt to send to Claude.
    pub prompt: Option<String>,
    /// Optional IFBDD telemetry context (ADR-097). When `Some`, the
    /// spawn path emits [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
    /// immediately before invoking the backend. When `None`, no event
    /// is emitted — preserves today's behaviour for callers (thaw /
    /// patrol respawn / tests) that have not yet been upgraded.
    pub telemetry: Option<AdapterTelemetry>,
    /// Optional pre-existing worker the spawn path detected under the
    /// target session name. Recorded on
    /// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
    /// so a tmux collision becomes auditable. `None` is the normal
    /// path; only set when the caller actively probed for a collision.
    pub pre_existing_worker: Option<WorkerId>,
}

/// Spawn a new Claude Code session in a tmux window.
///
/// Creates a tmux session running `claude` with the appropriate
/// `--permission-mode` flag derived from the agent's [`Clearance`].
/// Delegates all tmux operations to [`TmuxBackend`].
///
/// When [`ClaudeSessionConfig::telemetry`] is `Some`, an
/// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
/// is emitted immediately *before* the backend call. The event lands
/// even if the spawn subsequently fails — the IFBDD trail records the
/// *attempt*, not just the success path.
///
/// # Errors
///
/// Returns [`ClaudeError::SpawnFailed`] if the tmux session cannot be created.
pub fn spawn_claude_session(config: &ClaudeSessionConfig) -> Result<(), ClaudeError> {
    let mut claude_cmd = format!("claude --permission-mode {}", config.permission_mode);

    if let Some(ref prompt) = config.prompt {
        // Shell-escape the prompt for safe embedding
        let escaped = prompt.replace('\'', "'\\''");
        let _ = write!(claude_cmd, " --prompt '{escaped}'");
    }

    // ADR-097 / WS-1 — record the spawn attempt *before* the backend
    // call so a crash mid-spawn still leaves a trail. The event uses
    // `pid: 0` because the OS-level process id is not knowable until
    // after the spawn returns; the adapter never lies about a value
    // it cannot measure.
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
        .spawn_worker(&config.session_name, &config.work_dir, &claude_cmd)
        .map_err(|e| ClaudeError::SpawnFailed(e.to_string()));

    // ADR-097 / WS-1' (delib-20260519-e6db W3 / adversary F1.3) —
    // record the terminal partner of WorkerSpawnAttempted when the
    // backend refused the spawn. Without this event, WS-1 with no
    // WS-5 is indistinguishable from "live but unprobed" and the TLA+
    // invariant I1 (ws1_implies_ws5) is falsified.
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

/// Kill a Claude session by tmux session name.
///
/// When `telemetry` is `Some`, emits an
/// [`EventV2::AdapterHandleReconciled`](cosmon_core::event_v2::EventV2::AdapterHandleReconciled)
/// recording the handle-release outcome. The event lands on both the
/// success and the failure path — a termination error is recorded as
/// [`AdapterHandleState::ReleasedOrphan`] (the underlying process
/// raced the release).
///
/// # Errors
///
/// Returns [`ClaudeError::KillFailed`] if the session cannot be killed.
pub fn kill_session(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<(), ClaudeError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| ClaudeError::KillFailed(e.to_string()))?;
    let outcome = backend
        .terminate(&wid)
        .map_err(|e| ClaudeError::KillFailed(e.to_string()));

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

/// Check whether a tmux session is alive.
///
/// Returns `true` if a session with the given name exists on the socket.
///
/// When `telemetry` is `Some`, emits an
/// [`EventV2::AdapterLivenessProbed`](cosmon_core::event_v2::EventV2::AdapterLivenessProbed)
/// with `probe_kind = PaneSignature` recording the verdict. A `true`
/// return becomes `AdapterProbeResult::Alive { evidence: "tmux session
/// exists" }`; a `false` return becomes `AdapterProbeResult::Stuck {
/// reason: "tmux session absent" }`.
///
/// # Errors
///
/// Returns [`ClaudeError::Io`] if the tmux command fails unexpectedly.
pub fn check_alive(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<bool, ClaudeError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| ClaudeError::Io(e.to_string()))?;
    let outcome = backend
        .is_alive(&wid)
        .map_err(|e| ClaudeError::Io(e.to_string()));

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
/// recording the seal the adapter observed versus the seal previously
/// recorded for the current step (ADR-097 / WS-4).
///
/// The helper is called by the spawn path when the adapter materialises
/// the worker's initial prompt from `briefing.md`. The observed seal is
/// computed over the bytes the adapter *actually* read; the recorded
/// seal is the value the caller looked up from
/// `MoleculeData::briefing_seals`. Disagreement between the two is the
/// silent-failure mode WS-4 names — a post-seal edit to `briefing.md`
/// that silently reshapes the worker's contract.
///
/// Returns the bytes read so the caller can forward them to the
/// adapter's prompt-construction logic. A read failure surfaces as
/// [`ClaudeError::Io`]; no telemetry is emitted in that case (the
/// briefing was never *consumed*, so the WS-4 event would lie).
///
/// # Errors
///
/// Returns [`ClaudeError::Io`] if the briefing cannot be read.
pub fn consume_briefing(
    briefing_path: &Path,
    recorded_seal: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<Vec<u8>, ClaudeError> {
    let bytes = std::fs::read(briefing_path).map_err(|e| ClaudeError::Io(e.to_string()))?;

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

/// Build a [`ClaudeSessionConfig`] from common parameters.
///
/// Convenience constructor that maps [`Clearance`] to [`PermissionMode`]
/// and leaves the optional IFBDD telemetry context unset. Callers that
/// want the Worker-Spawn Port events emitted should populate
/// [`ClaudeSessionConfig::telemetry`] after construction or use
/// [`session_config_with_telemetry`].
#[must_use]
pub fn session_config(
    socket: impl Into<String>,
    session_name: impl Into<String>,
    work_dir: impl AsRef<Path>,
    clearance: Clearance,
    prompt: Option<String>,
) -> ClaudeSessionConfig {
    ClaudeSessionConfig {
        socket: socket.into(),
        session_name: session_name.into(),
        work_dir: work_dir.as_ref().to_string_lossy().into_owned(),
        permission_mode: clearance.into(),
        prompt,
        telemetry: None,
        pre_existing_worker: None,
    }
}

/// Build a [`ClaudeSessionConfig`] carrying an IFBDD telemetry context.
///
/// Equivalent to [`session_config`] plus an
/// [`AdapterTelemetry`] attached so the spawn path emits
/// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
/// when invoked.
#[must_use]
pub fn session_config_with_telemetry(
    socket: impl Into<String>,
    session_name: impl Into<String>,
    work_dir: impl AsRef<Path>,
    clearance: Clearance,
    prompt: Option<String>,
    telemetry: AdapterTelemetry,
) -> ClaudeSessionConfig {
    let mut config = session_config(socket, session_name, work_dir, clearance, prompt);
    config.telemetry = Some(telemetry);
    config
}

impl From<ClaudeError> for SpawnError {
    fn from(e: ClaudeError) -> Self {
        match e {
            ClaudeError::SpawnFailed(m) => Self::SpawnFailed(m),
            ClaudeError::KillFailed(m) => Self::KillFailed(m),
            ClaudeError::Io(m) => Self::Io(m),
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
        MoleculeId::new("task-20260517-0b46").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-aaaa").unwrap()
    }

    fn telemetry(state_dir: &Path) -> AdapterTelemetry {
        AdapterTelemetry::new(mol(), wkr(), state_dir.to_owned(), "uuid-test")
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
    fn test_clearance_to_permission_mode() {
        assert_eq!(PermissionMode::from(Clearance::Read), PermissionMode::Plan);
        assert_eq!(
            PermissionMode::from(Clearance::Write),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from(Clearance::Execute),
            PermissionMode::BypassPermissions
        );
    }

    #[test]
    fn test_permission_mode_display() {
        assert_eq!(PermissionMode::Plan.to_string(), "plan");
        assert_eq!(PermissionMode::AcceptEdits.to_string(), "acceptEdits");
        assert_eq!(
            PermissionMode::BypassPermissions.to_string(),
            "bypassPermissions"
        );
    }

    #[test]
    fn test_session_config_builder() {
        let config = session_config(
            "cosmon",
            "test-session",
            "/tmp/work",
            Clearance::Execute,
            Some("hello".to_owned()),
        );
        assert_eq!(config.socket, "cosmon");
        assert_eq!(config.session_name, "test-session");
        assert_eq!(config.work_dir, "/tmp/work");
        assert_eq!(config.permission_mode, PermissionMode::BypassPermissions);
        assert_eq!(config.prompt.as_deref(), Some("hello"));
        assert!(config.telemetry.is_none());
    }

    /// `session_config_with_telemetry` attaches the IFBDD context so the
    /// spawn path emits [`EventV2::WorkerSpawnAttempted`] when invoked.
    #[test]
    fn test_session_config_with_telemetry_attaches_context() {
        let dir = tempdir().unwrap();
        let t = telemetry(dir.path());
        let config = session_config_with_telemetry(
            "cosmon",
            "test-session",
            "/tmp/work",
            Clearance::Execute,
            None,
            t.clone(),
        );
        assert!(config.telemetry.is_some());
        assert_eq!(config.telemetry.as_ref().unwrap().mol_id, t.mol_id);
    }

    /// ADR-097 WS-4 — `consume_briefing` emits an
    /// `AdapterBriefingConsumed` event with the observed seal computed
    /// from the bytes it actually read, separate from the recorded seal
    /// the caller supplied. Disagreement is what the audit looks for.
    #[test]
    fn consume_briefing_emits_event_with_observed_and_recorded_seal() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let briefing_path = state_dir.join("briefing.md");
        fs::write(&briefing_path, b"hello world").unwrap();

        let t = telemetry(state_dir);
        let bytes = consume_briefing(&briefing_path, "deadbeef", Some(&t)).expect("read succeeds");
        assert_eq!(bytes, b"hello world");

        let envelopes = read_envelopes(state_dir);
        let consumed = envelopes
            .iter()
            .find_map(|e| match &e.event {
                EventV2::AdapterBriefingConsumed {
                    briefing_seal_observed,
                    briefing_seal_recorded,
                    bytes_read,
                    ..
                } => Some((
                    briefing_seal_observed.clone(),
                    briefing_seal_recorded.clone(),
                    *bytes_read,
                )),
                _ => None,
            })
            .expect("AdapterBriefingConsumed event emitted");
        let expected = blake3::hash(b"hello world").to_hex().to_string();
        assert_eq!(consumed.0, expected);
        assert_eq!(consumed.1, "deadbeef");
        assert_eq!(consumed.2, 11);
    }

    /// `consume_briefing` without telemetry must not write any event —
    /// preserves today's behaviour for callers that have not yet
    /// adopted the IFBDD context.
    #[test]
    fn consume_briefing_without_telemetry_emits_nothing() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let briefing_path = state_dir.join("briefing.md");
        fs::write(&briefing_path, b"silent").unwrap();

        let _ = consume_briefing(&briefing_path, "deadbeef", None).expect("read succeeds");

        let envelopes = read_envelopes(state_dir);
        assert!(
            envelopes
                .iter()
                .all(|e| !matches!(e.event, EventV2::AdapterBriefingConsumed { .. })),
            "no telemetry context → no event"
        );
    }

    /// Galileo §2.1 audit-query negative test — a `claude` adapter call
    /// path that does *not* go through `consume_briefing` leaves the
    /// audit-query for `AdapterBriefingConsumed` returning empty.
    /// That empty match is the WS-4 silent-failure signal.
    #[test]
    fn missing_consume_briefing_yields_empty_audit_match() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        // Emit some other Worker-Spawn Port event to ensure the file
        // exists with at least one envelope — the audit query must
        // still return empty for the missing variant.
        emit_worker_spawn_attempted(
            state_dir,
            &mol(),
            &wkr(),
            ADAPTER_NAME,
            "/tmp/wt",
            "uuid",
            0,
            None,
        );

        let envelopes = read_envelopes(state_dir);
        let consumed_count = envelopes
            .iter()
            .filter(|e| matches!(e.event, EventV2::AdapterBriefingConsumed { .. }))
            .count();
        assert_eq!(
            consumed_count, 0,
            "WS-4 detection: missing consume_briefing → empty audit match"
        );
    }
}
