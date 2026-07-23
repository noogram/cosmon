// SPDX-License-Identifier: AGPL-3.0-only

//! Claude Code session management — plain functions for spawn/kill/alive.
//!
//! These are standalone functions that manage Claude Code sessions via
//! [`TmuxBackend`]. They map [`Clearance`] to Claude's `--permission-mode`
//! flag and delegate all tmux operations to the backend (no duplicate
//! tmux command execution).
//!
//! # Headless spawn contract (Jesse Thaler / issue #6)
//!
//! This is the *headless* claude spawn path — distinct from the TUI
//! send-keys path in `cosmon-cli`'s `tackle_env`. It is reached by the
//! worker-respawn / re-engage callers (`cs thaw`, patrol restart). Three
//! things that Claude Code v2.x made mandatory and that this module now
//! honours:
//!
//! 1. **`-p`, not `--prompt`.** Claude Code v2.x renamed the headless flag
//!    to `-p` / `--print` (the prompt is now positional or read from
//!    stdin); passing the removed `--prompt` dies with
//!    `error: unknown option '--prompt'` before the model ever runs.
//! 2. **Briefing on stdin, never inline.** A multi-KB multi-line briefing
//!    shell-escaped through `new-session -> sh -c -> claude -p '<blob>'`
//!    leaves claude waiting on stdin forever (the escaping across three
//!    layers desynchronises). We write the briefing to a file and redirect
//!    it onto claude's stdin (`claude -p < <file>`) — zero escaping layers.
//!    The file is created **atomically with an unpredictable name and mode
//!    `0600`** (tempfile crate: `O_CREAT|O_EXCL`, owner-only), never a
//!    guessable `/tmp/…-<pid>.txt`, and the spawn command **self-deletes**
//!    it the instant the worker has it open on stdin — so the briefing
//!    (which routinely carries the operator's private context) never lingers
//!    world-readable in shared temp. See `write_briefing_file` and
//!    `build_headless_command` (issue #6 security follow-up,
//!    task-20260721-7a68 review of `claude.rs:206-227`).
//! 3. **Root escape valve.** Under euid 0 a bypass permission mode is
//!    refused unless `IS_SANDBOX=1` is exported (many container
//!    deployments run as root); the spawn path forces it in that exact
//!    intersection.
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
use cosmon_core::root_spawn_policy::{
    decide_root_spawn, demotion_command_prefix, resolve_demote_target, RootSpawnDecision,
};
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

    /// Refused to spawn a cognitive worker as root because demotion to a
    /// non-root uid was disabled (COSMON-DEV #20 / contract-20A outcome 2).
    /// No live worker was created — the typed alternative to the forbidden
    /// root-bypass spawn.
    #[error("root-spawn refused: {0}")]
    RootSpawnRefused(String),
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
    /// Optional initial prompt (the worker briefing) to hand to Claude.
    ///
    /// When `Some`, it is delivered to a headless `claude -p` via **stdin
    /// from a temp file** — never as an inline `-p '<blob>'` argument (see
    /// the module docs, point 2). When `None`, a bare TUI session is
    /// spawned and the caller is expected to deliver work via send-keys.
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

/// Shell-quote a string for safe embedding in the spawn command.
///
/// Mirrors the quoting used on the TUI path (`tackle_env::shell_quote` /
/// `TmuxBackend::shell_quote`): safe characters pass verbatim, anything else
/// is single-quoted with embedded quotes escaped as `'\''`. Used only for
/// the *briefing file path* — the briefing content itself never touches the
/// command string (it travels on stdin), so no multi-KB blob is escaped here.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@'))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Write the worker briefing to a private temp file and return its path.
///
/// The briefing is delivered to a headless `claude -p` on **stdin** (see
/// [`build_headless_command`]) rather than as an inline `-p '<blob>'`
/// argument — a multi-KB multi-line briefing escaped through
/// `new-session -> sh -c -> claude` desynchronises and leaves claude waiting
/// on stdin forever (issue #6.3). A file redirect has no escaping layers to
/// get wrong.
///
/// # Security (task-20260721-7a68 review of the earlier `claude.rs:206-227`)
///
/// The file is created **atomically** by the `tempfile` crate
/// (`O_CREAT | O_EXCL` with mode `0600`, an unpredictable random name), not
/// written to a guessable `/tmp/cosmon-briefing-<session>-<pid>.txt`. That
/// removes the two defects the earlier version carried:
///
/// - **Predictable path / TOCTOU.** A local attacker could pre-create the
///   guessable path (or plant a symlink) and turn cosmon's `std::fs::write`
///   into an arbitrary-file overwrite. `O_EXCL` refuses a pre-existing name
///   and the random component makes it unguessable.
/// - **World-readable confidentiality leak.** The full briefing routinely
///   carries the operator's private context; the earlier version left it
///   `0644` in shared `/tmp` and deliberately never removed it. Mode `0600`
///   makes it owner-only, and the spawn command (see
///   [`build_headless_command`]) unlinks it the instant the worker has it
///   open on stdin, so it never lingers.
///
/// The handle is `keep()`-ed so the file survives this call: the `claude`
/// that reads it is a detached grand-child, and the spawn command itself is
/// responsible for the delete (with a best-effort Rust fallback in
/// [`spawn_claude_session`] if the spawn never launches the shell).
///
/// # Errors
///
/// Returns [`ClaudeError::Io`] if the file cannot be created or written.
fn write_briefing_file(briefing: &str) -> Result<String, ClaudeError> {
    use std::io::Write as _;

    let mut tmp = tempfile::Builder::new()
        .prefix("cosmon-briefing-")
        .suffix(".txt")
        .tempfile()
        .map_err(|e| ClaudeError::Io(e.to_string()))?;
    tmp.write_all(briefing.as_bytes())
        .map_err(|e| ClaudeError::Io(e.to_string()))?;
    tmp.flush().map_err(|e| ClaudeError::Io(e.to_string()))?;
    // Disable delete-on-drop: the detached worker consumes the file on stdin
    // after this call returns, and the spawn command self-deletes it then.
    let (_file, path) = tmp.keep().map_err(|e| ClaudeError::Io(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Build the shell command string for a headless claude spawn.
///
/// Pure and side-effect-free so the byte shape is unit-testable without
/// tmux, root, or a real `claude`. The three issue-#6 fixes live here:
///
/// - `-p` (not the removed `--prompt`) when a briefing is present;
/// - the briefing delivered via stdin redirect from `briefing_file`, so no
///   escaped blob rides the command string;
/// - the briefing file **self-deleted** the instant the worker has it open
///   on stdin — the redirect opens the fd for the whole brace group, `rm`
///   unlinks the name, and `claude` reads the briefing from the surviving
///   open fd, so the private briefing never lingers in temp (issue #6
///   security follow-up, task-20260721-7a68);
/// - a privilege-drop wrapper before the binary when the root-spawn policy
///   resolves a [`RootSpawnDecision::Demote`] (COSMON-DEV #20 / contract-20A).
///
/// # Root-spawn policy (COSMON-DEV #20)
///
/// The pre-#20 headless path forced `IS_SANDBOX=1` under root so Claude Code's
/// own root guard would let a *root worker* run — the forbidden third outcome
/// (a live cognitive worker with root's blast radius). This replaces that
/// bypass: [`RootSpawnDecision::Demote`] splices [`demotion_command_prefix`]
/// before the binary so the worker `exec`s as a non-root uid (F8: a non-root
/// worker runs cleanly regardless of `IS_SANDBOX`), and
/// [`RootSpawnDecision::Refuse`] is handled by the caller *before* this
/// function is reached (no live worker is created). A non-root dispatcher
/// ([`RootSpawnDecision::SpawnAsIs`]) yields a command byte-identical to
/// pre-#20.
///
/// `briefing_file` is `None` for a bare TUI spawn (caller delivers via
/// send-keys) and `Some(path)` for a headless briefing delivery.
fn build_headless_command(
    permission_mode: PermissionMode,
    briefing_file: Option<&str>,
    decision: &RootSpawnDecision,
) -> String {
    // Privilege-drop prefix, spliced immediately before the binary. Empty for
    // the non-root path and (defensively) for a `Refuse` the caller should
    // have intercepted; non-empty only for `Demote`.
    let demote = match decision {
        RootSpawnDecision::Demote { to_uid } => demotion_command_prefix(*to_uid),
        RootSpawnDecision::SpawnAsIs | RootSpawnDecision::Refuse { .. } => String::new(),
    };
    match briefing_file {
        // issue #6.1 + #6.3 + security follow-up: `-p` with the briefing on
        // stdin from a file that is unlinked while still open. POSIX applies
        // the group's `< path` redirect before running the list, so `rm`
        // removes only the directory entry — claude keeps reading the open
        // fd. The briefing is gone from temp the moment the worker starts.
        Some(path) => {
            let q = shell_quote(path);
            let mut cmd = String::new();
            let _ = write!(
                cmd,
                "{{ rm -f {q}; {demote}claude --permission-mode {permission_mode} -p; }} < {q}"
            );
            cmd
        }
        None => format!("{demote}claude --permission-mode {permission_mode}"),
    }
}

/// Spawn a new Claude Code session in a tmux window.
///
/// Creates a tmux session running `claude` with the appropriate
/// `--permission-mode` flag derived from the agent's [`Clearance`].
/// Delegates all tmux operations to [`TmuxBackend`].
///
/// When [`ClaudeSessionConfig::prompt`] is `Some`, the briefing is written to
/// a temp file and delivered to a headless `claude -p` on **stdin** (issue #6
/// — see the module docs). When `None`, a bare TUI session is spawned.
///
/// When [`ClaudeSessionConfig::telemetry`] is `Some`, an
/// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
/// is emitted immediately *before* the backend call. The event lands
/// even if the spawn subsequently fails — the IFBDD trail records the
/// *attempt*, not just the success path.
///
/// # Errors
///
/// Returns [`ClaudeError::Io`] if the briefing temp file cannot be written,
/// or [`ClaudeError::SpawnFailed`] if the tmux session cannot be created.
pub fn spawn_claude_session(config: &ClaudeSessionConfig) -> Result<(), ClaudeError> {
    let briefing_file = match config.prompt {
        Some(ref prompt) => Some(write_briefing_file(prompt)?),
        None => None,
    };
    // Root-spawn policy (COSMON-DEV #20 / contract-20A). Production callers
    // read the real effective uid; when root, demote the worker to a non-root
    // uid before exec (or refuse before a live worker exists if demotion is
    // disabled), never re-arm the old `IS_SANDBOX=1` root bypass.
    let running_uid = nix::unistd::Uid::effective().as_raw();
    let demote_target = resolve_demote_target(|k| std::env::var(k).ok());
    let decision = decide_root_spawn(running_uid, demote_target);
    if let RootSpawnDecision::Refuse { reason } = &decision {
        // Outcome 2: refuse before creating a live worker. Reap the briefing
        // temp file (no spawn will consume+unlink it) so the private briefing
        // never lingers, then surface a typed root-refusal.
        if let Some(ref path) = briefing_file {
            let _ = std::fs::remove_file(path);
        }
        return Err(ClaudeError::RootSpawnRefused(reason.to_string()));
    }
    let claude_cmd =
        build_headless_command(config.permission_mode, briefing_file.as_deref(), &decision);

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

    // Security follow-up (task-20260721-7a68): the spawn command self-deletes
    // the briefing once the worker opens it on stdin, but a *failed* spawn may
    // never launch that shell — the private briefing would then persist in
    // temp. Reap it best-effort so the confidentiality guarantee holds even on
    // the failure path.
    if outcome.is_err() {
        if let Some(ref path) = briefing_file {
            let _ = std::fs::remove_file(path);
        }
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
            // A root-spawn refusal is a spawn that never happened; it shares
            // the generic spawn-error envelope, which carries the typed reason
            // string.
            ClaudeError::SpawnFailed(m) | ClaudeError::RootSpawnRefused(m) => Self::SpawnFailed(m),
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

    // -- issue #6 headless spawn: command shape (Jesse Thaler) --

    /// #6.1: the headless command uses `-p` with the briefing on stdin, and
    /// NEVER the removed `--prompt` flag (which dies with
    /// `error: unknown option '--prompt'` on Claude Code v2.x).
    #[test]
    fn headless_command_uses_dash_p_stdin_not_prompt() {
        let cmd = build_headless_command(
            PermissionMode::BypassPermissions,
            Some("/tmp/cosmon-briefing-polecat-42.txt"),
            &RootSpawnDecision::SpawnAsIs,
        );
        assert_eq!(
            cmd,
            "{ rm -f /tmp/cosmon-briefing-polecat-42.txt; \
             claude --permission-mode bypassPermissions -p; } \
             < /tmp/cosmon-briefing-polecat-42.txt"
        );
        assert!(
            !cmd.contains("--prompt"),
            "the removed --prompt flag must never appear: {cmd}"
        );
    }

    /// Security follow-up (task-20260721-7a68): the briefing is self-deleted
    /// while the worker holds it open on stdin. The generated command must
    /// `rm` the briefing path *before* claude and wrap both in a brace group
    /// whose stdin redirect opens the fd first, so the unlink is race-free.
    #[test]
    fn headless_command_self_deletes_briefing_after_open() {
        let cmd = build_headless_command(
            PermissionMode::BypassPermissions,
            Some("/tmp/cosmon-briefing-xyz.txt"),
            &RootSpawnDecision::SpawnAsIs,
        );
        assert!(
            cmd.starts_with("{ rm -f /tmp/cosmon-briefing-xyz.txt;"),
            "must unlink the briefing inside the group: {cmd}"
        );
        assert!(
            cmd.ends_with("} < /tmp/cosmon-briefing-xyz.txt"),
            "the group's stdin must redirect from the briefing so the fd is \
             opened before the rm: {cmd}"
        );
        // The rm must precede the claude invocation in source order.
        let rm_at = cmd.find("rm -f").expect("rm present");
        let claude_at = cmd.find("claude").expect("claude present");
        assert!(rm_at < claude_at, "rm must come before claude: {cmd}");
    }

    /// A bare TUI spawn (no briefing) omits `-p` entirely — the caller
    /// delivers work via send-keys. This is the current live shape for the
    /// thaw / patrol respawn callers, which pass `prompt: None`.
    #[test]
    fn headless_command_without_briefing_is_bare_tui() {
        let cmd = build_headless_command(
            PermissionMode::AcceptEdits,
            None,
            &RootSpawnDecision::SpawnAsIs,
        );
        assert_eq!(cmd, "claude --permission-mode acceptEdits");
        assert!(!cmd.contains(" -p"), "no briefing → no -p: {cmd}");
    }

    /// COSMON-DEV #20 / contract-20A: a root dispatcher demotes the worker to
    /// a non-root uid before exec, and NEVER re-arms the old `IS_SANDBOX=1`
    /// root bypass. Reverting the fix (dropping the demotion) re-reds this.
    #[test]
    fn headless_command_demotes_the_worker_under_root() {
        let decision = decide_root_spawn(0, Some(10001));
        let cmd = build_headless_command(PermissionMode::BypassPermissions, None, &decision);
        assert_eq!(
            cmd,
            "setpriv --reuid 10001 --regid 10001 --clear-groups -- \
             claude --permission-mode bypassPermissions",
            "root must demote to a non-root uid: {cmd}"
        );
        assert!(
            !cmd.contains("IS_SANDBOX"),
            "the root path must not re-arm the IS_SANDBOX bypass: {cmd}"
        );
    }

    /// A root dispatcher with a briefing demotes the `claude` invocation
    /// *inside* the self-deleting brace group, not the `rm`.
    #[test]
    fn headless_command_demotes_only_the_binary_with_briefing() {
        let decision = decide_root_spawn(0, Some(10001));
        let cmd = build_headless_command(
            PermissionMode::BypassPermissions,
            Some("/tmp/brief.txt"),
            &decision,
        );
        assert!(
            cmd.contains("rm -f /tmp/brief.txt; setpriv --reuid 10001"),
            "the rm runs as root; only the worker binary is demoted: {cmd}"
        );
        assert!(
            cmd.contains("--clear-groups -- claude --permission-mode"),
            "the privilege-drop must wrap the claude binary: {cmd}"
        );
    }

    /// A non-root worker's command is byte-identical to the pre-#20 shape
    /// even under a bypass mode — the common fleet path is untouched.
    #[test]
    fn headless_command_non_root_has_no_sandbox_prefix() {
        let cmd = build_headless_command(
            PermissionMode::BypassPermissions,
            None,
            &RootSpawnDecision::SpawnAsIs,
        );
        assert!(
            !cmd.contains("IS_SANDBOX"),
            "non-root must not gain an IS_SANDBOX prefix: {cmd}"
        );
        assert!(!cmd.contains("setpriv"), "non-root must not demote: {cmd}");
    }

    /// #6.3: the briefing content rides a file on stdin, never the command
    /// string — so a multi-KB multi-line briefing is never shell-escaped
    /// through the `new-session -> sh -c -> claude` layers. `write_briefing_file`
    /// round-trips the bytes and the built command redirects that exact path.
    #[test]
    fn briefing_travels_on_stdin_not_inline() {
        let briefing = "line one\nline two with 'quotes' and $VARS\n".repeat(200);
        let path = write_briefing_file(&briefing).expect("write succeeds");
        let read_back = std::fs::read_to_string(&path).expect("briefing file readable");
        assert_eq!(read_back, briefing, "briefing bytes must round-trip");

        let cmd = build_headless_command(
            PermissionMode::BypassPermissions,
            Some(&path),
            &RootSpawnDecision::SpawnAsIs,
        );
        assert!(
            cmd.contains("-p; } < "),
            "must redirect stdin from the file: {cmd}"
        );
        assert!(
            !cmd.contains("line two"),
            "briefing content must never appear in the command string: {cmd}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A briefing-file path with shell-special characters is quoted so both
    /// the `rm` and the redirect survive the outer shell round-trip.
    #[test]
    fn briefing_path_with_spaces_is_quoted() {
        let cmd = build_headless_command(
            PermissionMode::BypassPermissions,
            Some("/tmp/My Dir/brief.txt"),
            &RootSpawnDecision::SpawnAsIs,
        );
        assert!(
            cmd.starts_with("{ rm -f '/tmp/My Dir/brief.txt';"),
            "a path with spaces must be single-quoted in the rm: {cmd}"
        );
        assert!(
            cmd.ends_with("} < '/tmp/My Dir/brief.txt'"),
            "a path with spaces must be single-quoted in the redirect: {cmd}"
        );
    }

    /// Security (task-20260721-7a68 finding, `claude.rs:206-227`): the briefing
    /// is created owner-only (`0600`) and with an unpredictable name — never a
    /// guessable `/tmp/cosmon-briefing-<session>-<pid>.txt` an attacker could
    /// pre-create or symlink (TOCTOU), and never world-readable.
    #[cfg(unix)]
    #[test]
    fn briefing_file_is_mode_0600_and_unpredictable() {
        use std::os::unix::fs::PermissionsExt;

        let p1 = write_briefing_file("secret operator context one").expect("write one");
        let p2 = write_briefing_file("secret operator context two").expect("write two");

        let mode = fs::metadata(&p1).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "briefing must be owner-only, got {mode:o}");

        assert_ne!(
            p1, p2,
            "successive briefings must get unpredictable distinct paths, not a \
             guessable pid-keyed name: {p1} == {p2}"
        );

        let _ = fs::remove_file(&p1);
        let _ = fs::remove_file(&p2);
    }

    /// Security (task-20260721-7a68 finding, `claude.rs:206-227`): the private
    /// briefing must NOT survive after the worker consumes it. This exercises
    /// the exact shell mechanism the spawn command uses — the group's redirect
    /// opens the fd, `rm` unlinks the name, and the consumer (here `cat`
    /// standing in for `claude`) still reads the full briefing from the open
    /// fd — then asserts the file is gone from temp.
    #[cfg(unix)]
    #[test]
    fn briefing_file_does_not_survive_consumption() {
        let briefing = "SECRET operator context — do not circulate\n".repeat(64);
        let path = write_briefing_file(&briefing).expect("write succeeds");
        assert!(
            Path::new(&path).exists(),
            "briefing exists before consumption"
        );

        // `cat` stands in for the real `claude` binary: identical fd semantics.
        let q = shell_quote(&path);
        let consume = format!("{{ rm -f {q}; cat; }} < {q}");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&consume)
            .output()
            .expect("sh runs");

        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            briefing,
            "consumer must still read the full briefing after the unlink"
        );
        assert!(
            !Path::new(&path).exists(),
            "briefing must not survive consumption: {path}"
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
