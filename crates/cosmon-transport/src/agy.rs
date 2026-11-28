// SPDX-License-Identifier: AGPL-3.0-only

//! `agy` (Google Antigravity CLI) session management — sibling of
//! [`crate::claude`], [`crate::aider`], [`crate::codex`] and [`crate::opencode`].
//!
//! Spawns the publicly-installable `agy` binary inside a tmux pane, under
//! cosmon's pane driver ([`crate::agy_pane`], run as the hidden `cs agy-pane`).
//! agy runs its own agent loop; cosmon supervises the *pane*, not the loop,
//! through the standard `pane-died` hook.
//!
//! # Architectural posture
//!
//! agy is a [`crate::registry::SupervisionMode::TmuxPane`] /
//! `LoopOwnership::External` / `RuntimeOwnership::Vendor` adapter.
//! It is an external CLI binary on PATH. Since issue #240 its session stays
//! open after the brief's turn, like the claude and codex panes: the operator
//! can type into the pane and `cs whisper` delivers a continuation. The pane
//! dies when agy exits (end of input, or a failed turn such as a quota stop).

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
    /// The pane driver: the `cs` executable whose hidden `agy-pane`
    /// subcommand sits between the terminal and agy (see [`crate::agy_pane`]).
    pub driver: PathBuf,
    /// Optional brief, sent by the driver as the session's first message.
    pub prompt: Option<String>,
    /// Optional model pin resolved by the common Incarnation selector.
    /// Passed as `--model`.
    pub model: Option<String>,
    /// Harness settings already rendered onto agy's flag channel
    /// (`--<key> <value>` per key, ADR-177 / issue #245), appended after
    /// `--model` in order. Carried verbatim: an unknown key fails in agy's own
    /// parser at launch.
    pub harness_args: Vec<String>,
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
/// The pane driver appends agy's raw output to `agy-stream.jsonl` in the
/// molecule directory, a durable receipt, and shows it readably in the pane.
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
/// Shape: `exec env -u ANTIGRAVITY_CONVERSATION_ID <driver> agy-pane
/// --molecule-dir <mol> [--prompt <prompt>] -- <agy> --print-timeout 0
/// --input-format stream-json --output-format stream-json [--model <model>]
/// [<harness args>…] --dangerously-skip-permissions -p ''`, every word
/// shell-quoted when it needs it.
///
/// - `--input-format stream-json` with `-p ''` is agy's multi-turn print mode:
///   one NDJSON message per stdin line, one turn each, the same `stream-json`
///   frames on stdout as a one-shot run. The driver sends the brief as the
///   first message and each terminal line after it, so the session stays open
///   and `cs whisper` reaches it (#240).
/// - The driver, not a shell pipeline, appends agy's output to
///   `<mol>/agy-stream.jsonl`, writes agy's exit status to `<mol>/agy-exit.code`
///   and exits with it, so the pane's exit status is agy's.
/// - `exec` makes the driver the pane's process, so `pane_current_command`
///   names it rather than a shell, and no shell is left in the pane to receive
///   typed text after agy ends.
/// - `env -u ANTIGRAVITY_CONVERSATION_ID` keeps a value inherited from the tmux
///   server environment from reaching agy.
/// - Harness args sit after `--model` and before the permission flag, in the
///   resolver's order, each word quoted on its own (#245).
#[must_use]
pub fn build_agy_command(config: &AgySessionConfig, mol_dir: &Path) -> String {
    let mut words: Vec<String> = vec![
        "exec".into(),
        "env".into(),
        "-u".into(),
        NATIVE_IDENTITY_VAR.into(),
        config.driver.to_string_lossy().into_owned(),
        "agy-pane".into(),
        "--molecule-dir".into(),
        mol_dir.to_string_lossy().into_owned(),
    ];
    if let Some(prompt) = &config.prompt {
        words.push("--prompt".into());
        words.push(prompt.clone());
    }
    words.push("--".into());
    words.push(config.binary.to_string_lossy().into_owned());
    words.extend(
        [
            "--print-timeout",
            "0",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]
        .map(str::to_owned),
    );
    if let Some(model) = &config.model {
        words.push("--model".into());
        words.push(model.clone());
    }
    words.extend(config.harness_args.iter().cloned());
    words.push("--dangerously-skip-permissions".into());
    words.push("-p".into());
    words.push(String::new());
    words
        .iter()
        .map(|w| shell_escape(w))
        .collect::<Vec<_>>()
        .join(" ")
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
            driver: PathBuf::from("/opt/cs"),
            prompt: Some("hi".into()),
            model: None,
            harness_args: vec![],
            telemetry: None,
            pre_existing_worker: None,
            environment: vec![],
        }
    }

    #[test]
    fn command_without_settings_is_the_documented_shape() {
        let cmd = build_agy_command(&cfg("agy"), Path::new("/m"));
        assert_eq!(
            cmd,
            "exec env -u ANTIGRAVITY_CONVERSATION_ID /opt/cs agy-pane --molecule-dir /m \
             --prompt hi -- agy --print-timeout 0 --input-format stream-json \
             --output-format stream-json --dangerously-skip-permissions -p ''"
        );
    }

    #[test]
    fn command_with_model_and_harness_settings_places_them_in_order() {
        let mut c = cfg("agy");
        c.model = Some("gemini-4-neon".into());
        c.harness_args = vec![
            "--effort".into(),
            "high".into(),
            "--mode".into(),
            "plan it".into(),
        ];
        let cmd = build_agy_command(&c, Path::new("/m"));
        assert!(
            cmd.ends_with(
                "--output-format stream-json --model gemini-4-neon --effort high \
                 --mode 'plan it' --dangerously-skip-permissions -p ''"
            ),
            "{cmd}"
        );
    }

    #[test]
    fn command_without_prompt_sends_no_prompt_flag() {
        let mut c = cfg("agy");
        c.prompt = None;
        let cmd = build_agy_command(&c, Path::new("/m"));
        assert!(!cmd.contains("--prompt"), "{cmd}");
        assert!(cmd.contains("--molecule-dir /m -- agy "), "{cmd}");
    }

    /// The command line, run by a POSIX shell, hands the driver exactly the
    /// words it was built from: quotes, spaces, `$`, backquotes and newlines in
    /// the prompt, a path with a quote, and a harness value with spaces.
    #[test]
    fn the_shell_hands_the_driver_every_word_verbatim() {
        let dir = std::env::temp_dir().join(format!("agy it's argv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("argv");
        let driver = dir.join("driver");
        std::fs::write(
            &driver,
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done > \"$AGY_ARGV_OUT\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prompt = "it's \"quoted\" $HOME `id`\nsecond line";
        let mut c = cfg("/bin/agy");
        c.driver = driver.clone();
        c.prompt = Some(prompt.into());
        c.harness_args = vec!["--effort".into(), "x y".into()];
        let cmd = build_agy_command(&c, &dir);
        let status = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .env("AGY_ARGV_OUT", &out)
            .status()
            .unwrap();
        assert!(status.success());
        let raw = std::fs::read(&out).unwrap();
        let words: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|w| !w.is_empty())
            .map(|w| String::from_utf8_lossy(w).into_owned())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        let mol = dir.to_string_lossy().into_owned();
        assert_eq!(
            words,
            [
                "agy-pane",
                "--molecule-dir",
                mol.as_str(),
                "--prompt",
                prompt,
                "--",
                "/bin/agy",
                "--print-timeout",
                "0",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--effort",
                "x y",
                "--dangerously-skip-permissions",
                "-p",
            ]
        );
        assert!(raw.ends_with(b"-p\0\0"), "the empty -p value is passed");
    }

    #[test]
    fn command_clears_inherited_native_identity_through_exec() {
        let cmd = build_agy_command(&cfg("agy"), Path::new("/m"));
        assert!(
            cmd.starts_with("exec env -u ANTIGRAVITY_CONVERSATION_ID "),
            "{cmd}"
        );
    }
}
