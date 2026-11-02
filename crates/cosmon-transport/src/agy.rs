// SPDX-License-Identifier: AGPL-3.0-only

//! `agy` (Google Antigravity CLI) session management — sibling of
//! [`crate::claude`], [`crate::aider`], [`crate::codex`] and [`crate::opencode`].
//!
//! Spawns the publicly-installable `agy` binary inside a tmux pane.
//! Like codex and opencode, it runs its own agent loop inside the pane;
//! cosmon supervises the *pane*, not the loop, through the standard
//! `pane-died` hook.
//!
//! # Architectural posture
//!
//! agy is a [`crate::registry::SupervisionMode::TmuxPane`] /
//! `LoopOwnership::External` / `RuntimeOwnership::Vendor` adapter.
//! It is an external CLI binary on PATH whose pane dies when the run completes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::event_v2::AdapterHandleState;
use cosmon_core::id::WorkerId;
use cosmon_state::events::worker_spawn::{
    emit_adapter_briefing_consumed, emit_adapter_handle_reconciled, emit_worker_spawn_attempted,
};

use crate::spawn::SpawnError;
use crate::TmuxBackend;

pub use crate::spawn::AdapterTelemetry;

/// The adapter-name token carried on every Worker-Spawn Port event the
/// agy transport emits.
pub const ADAPTER_NAME: &str = "agy";

/// The native conversation identity agy would resume if inherited.
const NATIVE_IDENTITY_VAR: &str = "ANTIGRAVITY_CONVERSATION_ID";

/// Error type for agy adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum AgyError {
    /// Failed to spawn the agy session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    /// Failed to kill the session.
    #[error("kill failed: {0}")]
    KillFailed(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<AgyError> for SpawnError {
    fn from(e: AgyError) -> Self {
        match e {
            AgyError::SpawnFailed(m) => Self::SpawnFailed(m),
            AgyError::KillFailed(m) => Self::KillFailed(m),
            AgyError::Io(m) => Self::Io(m),
        }
    }
}

/// Configuration for spawning an agy session.
#[derive(Debug, Clone)]
pub struct AgySessionConfig {
    /// Tmux socket name.
    pub socket: String,
    /// Tmux session name.
    pub session_name: String,
    /// Working directory. The tmux session is created with this as its
    /// cwd (`tmux new-session -c <work_dir>`).
    pub work_dir: String,
    /// Resolved agy binary path (PATH-relative or absolute).
    pub binary: PathBuf,
    /// Optional prompt forwarded as the positional message to `agy`.
    pub prompt: Option<String>,
    /// Optional model pin resolved by the common Incarnation selector.
    /// Passed as `--model`.
    pub model: Option<String>,
    /// Optional IFBDD telemetry context.
    pub telemetry: Option<AdapterTelemetry>,
    /// Optional pre-existing worker the spawn path detected.
    pub pre_existing_worker: Option<WorkerId>,
    /// Environment entries injected into this tmux session with
    /// `new-session -e`.
    pub environment: Vec<(String, String)>,
}

/// Spawn an agy session in a tmux window.
///
/// Builds the agy command and delegates to [`TmuxBackend::spawn_worker`].
/// The agy command is run with stdout teed to `agy-stream.jsonl` in the
/// molecule directory to leave a durable receipt while showing progress in the pane.
///
/// # Errors
/// Returns [`AgyError::SpawnFailed`] when the tmux session cannot be created.
pub fn spawn_agy_session(config: &AgySessionConfig, mol_dir: &Path) -> Result<(), AgyError> {
    spawn_agy_session_witnessed(config, mol_dir).map(|_| ())
}

/// Launch with an optional terminal incarnation captured at creation.
///
/// # Errors
/// Returns the same launch errors as [`spawn_agy_session`].
pub fn spawn_agy_session_witnessed(
    config: &AgySessionConfig,
    mol_dir: &Path,
) -> Result<Option<cosmon_core::terminal::TerminalIdentity>, AgyError> {
    let cmd = build_agy_command(config, mol_dir);

    // Drop an explicit entry too; the inherited value is cleared by
    // `env -u` in the launched command (see `build_agy_command`).
    let mut env = config.environment.clone();
    env.retain(|(k, _)| k != NATIVE_IDENTITY_VAR);

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
        .spawn_worker_with_env_witnessed(&config.session_name, &config.work_dir, &cmd, &env)
        .map_err(|e| AgyError::SpawnFailed(e.to_string()))
}

/// Build the shell command line for tmux.
///
/// The command is portable POSIX sh (dash, bash, zsh, macOS `/bin/sh`); it
/// does not use `set -o pipefail`, which dash rejects (the pane would exit 2
/// before agy starts). Instead agy's status is captured explicitly: agy runs
/// in a `{ …; }` group whose last act is `echo $?` into `agy-exit.code` in the
/// molecule directory, the group's output is piped to `tee`, and the pane
/// finally runs `exit "$(cat agy-exit.code)"`. The pane's exit status is
/// therefore agy's, not `tee`'s. A stale code file from an earlier run is
/// removed first, and a missing file (group killed before `echo`) makes
/// `exit` fail with a non-zero status rather than read as success. agy is
/// launched through `env -u ANTIGRAVITY_CONVERSATION_ID` so a value inherited
/// from the tmux server environment never reaches it.
///
/// Shape: `rm -f '<mol>/agy-exit.code'; { env -u ANTIGRAVITY_CONVERSATION_ID agy --print-timeout 0 --output-format stream-json [--model <model>] --dangerously-skip-permissions -p '<prompt>' 2>&1; echo $? > '<mol>/agy-exit.code'; } | tee -a '<mol>/agy-stream.jsonl'; exit "$(cat '<mol>/agy-exit.code')"`
#[must_use]
pub fn build_agy_command(config: &AgySessionConfig, mol_dir: &Path) -> String {
    let quote = |p: std::path::PathBuf| p.to_string_lossy().replace('\'', "'\\''");
    let exit_code = quote(mol_dir.join("agy-exit.code"));
    let receipt = quote(mol_dir.join("agy-stream.jsonl"));

    let mut cmd = format!(
        "rm -f '{exit_code}'; {{ env -u {NATIVE_IDENTITY_VAR} {}",
        shell_escape(&config.binary.to_string_lossy())
    );
    cmd.push_str(" --print-timeout 0 --output-format stream-json");

    if let Some(ref model) = config.model {
        cmd.push_str(" --model ");
        cmd.push_str(&shell_escape(model));
    }

    cmd.push_str(" --dangerously-skip-permissions");

    if let Some(ref prompt) = config.prompt {
        cmd.push_str(" -p ");
        let escaped = prompt.replace('\'', "'\\''");
        let _ = write!(cmd, "'{escaped}'");
    }

    let _ = write!(
        cmd,
        " 2>&1; echo $? > '{exit_code}'; }} | tee -a '{receipt}'; exit \"$(cat '{exit_code}')\""
    );

    cmd
}

/// Kill an agy session by tmux session name.
///
/// # Errors
/// Returns [`AgyError::KillFailed`] if the session cannot be killed.
pub fn kill_session(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<(), AgyError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| AgyError::KillFailed(e.to_string()))?;
    let outcome = backend
        .terminate(&wid)
        .map_err(|e| AgyError::KillFailed(e.to_string()));

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

/// Read `briefing.md` bytes and emit
/// [`EventV2::AdapterBriefingConsumed`](cosmon_core::event_v2::EventV2::AdapterBriefingConsumed)
/// with `adapter_name = "agy"`.
///
/// # Errors
/// Returns [`AgyError::Io`] if the briefing cannot be read.
pub fn consume_briefing(
    briefing_path: &Path,
    recorded_seal: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<Vec<u8>, AgyError> {
    let bytes = std::fs::read(briefing_path).map_err(|e| AgyError::Io(e.to_string()))?;

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

    fn cfg(binary: &str) -> AgySessionConfig {
        AgySessionConfig {
            socket: "s".into(),
            session_name: "n".into(),
            work_dir: "/w".into(),
            binary: PathBuf::from(binary),
            prompt: Some("hi".into()),
            model: None,
            telemetry: None,
            pre_existing_worker: None,
            environment: vec![],
        }
    }

    #[test]
    fn command_clears_inherited_native_identity() {
        let cmd = build_agy_command(&cfg("agy"), Path::new("/m"));
        assert!(
            cmd.contains("env -u ANTIGRAVITY_CONVERSATION_ID agy "),
            "{cmd}"
        );
    }

    #[test]
    fn command_is_posix_sh_without_pipefail() {
        let cmd = build_agy_command(&cfg("agy"), Path::new("/m"));
        assert!(!cmd.contains("pipefail"), "{cmd}");
        assert!(cmd.contains("echo $? > '/m/agy-exit.code'"), "{cmd}");
        assert!(cmd.ends_with("exit \"$(cat '/m/agy-exit.code')\""), "{cmd}");
    }

    /// Run the built pipeline under `shell` and return its exit code.
    /// `None` when the shell is not installed (prints a note).
    fn run_under(shell: &str, binary: &str, tag: &str) -> Option<i32> {
        let dir =
            std::env::temp_dir().join(format!("agy it's {tag}-{shell}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd = build_agy_command(&cfg(binary), &dir);
        let status = match std::process::Command::new(shell)
            .args(["-c", &cmd])
            .status()
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("note: skipping {shell} case: {e}");
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            }
        };
        let _ = std::fs::remove_dir_all(&dir);
        status.code()
    }

    fn exit3_script() -> String {
        let p = std::env::temp_dir().join(format!("agy-exit3-{}.sh", std::process::id()));
        std::fs::write(&p, "#!/bin/sh\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn pipeline_exit_status_is_agys_not_tees() {
        let script = exit3_script();
        for shell in ["sh", "dash"] {
            if let Some(code) = run_under(shell, "false", "f") {
                assert_eq!(code, 1, "{shell}: false must read as 1");
            }
            if let Some(code) = run_under(shell, &script, "s") {
                assert_eq!(code, 3, "{shell}: script exit 3 must read as 3");
            }
        }
        let _ = std::fs::remove_file(&script);
    }

    #[test]
    fn receipt_path_is_under_the_molecule_dir() {
        let cmd = build_agy_command(&cfg("agy"), Path::new("/state/mol-1"));
        assert!(
            cmd.contains("tee -a '/state/mol-1/agy-stream.jsonl'"),
            "{cmd}"
        );
    }
}
