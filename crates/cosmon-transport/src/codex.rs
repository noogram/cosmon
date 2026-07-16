// SPDX-License-Identifier: AGPL-3.0-only

//! `Codex` (`OpenAI`) CLI session management — sibling of [`crate::claude`]
//! and [`crate::aider`].
//!
//! Spawns the publicly-installable `codex` binary
//! (`npm install -g @openai/codex`, Apache-2.0) inside a tmux pane —
//! the **outside-in safety valve** path. Perpendicular to the in-process
//! `cosmon-agent-harness` work: empirical capability today without
//! surrendering the structural contract.
//!
//! # Architectural posture
//!
//! This module deliberately does **not** introduce a
//! [`crate::registry::SupervisionMode::Subprocess`] variant. The
//! existing [`crate::registry::SupervisionMode::TmuxPane`] satisfies
//! the codex spawn path verbatim — the binary runs in a tmux pane and
//! cosmon's standard `pane-died` hook is the supervisor. The
//! binary-name distinction is a pane-signature concern (ADR-098 §C3),
//! not a supervision-mode concern.
//!
//! If during operational use a non-tmux supervision path proves
//! necessary, surface the finding in the molecule's `log.md` — adding
//! a new `SupervisionMode` variant is an ADR-grade decision (it widens
//! ADR-101's typestate), not a backdoor patch.
//!
//! # Version pin — three load-bearing pillars (tolnay §Q3)
//!
//! The pin is **decorative** unless all three of these are wired:
//! 1. **Config** — `.cosmon/adapters/codex.toml` carries
//!    `codex.version = "=0.49.2"` (the Cargo "=X.Y.Z" form is the
//!    precedent).
//! 2. **Runtime check** — [`CodexAdapter::new`] parses `codex --version`
//!    and refuses construction if it does not match.
//! 3. **Telemetry** — the mismatch is emitted as an
//!    [`EventV2::AdapterLivenessProbed`](cosmon_core::event_v2::EventV2::AdapterLivenessProbed)
//!    Stuck event with reason prefix `SF-7 binary_version_mismatch
//!    expected=<X> found=<Y>` so the operator sees it on the same
//!    cat-test surface as openai's SF-1..SF-5 trauma.
//!
//! The fake-binary test (`adapter_construction_fails_on_version_mismatch`)
//! exercises all three: a `/tmp/fake-codex/codex` script that emits
//! a wrong version string, a `PATH` override, and an assertion that
//! `CodexAdapter::new` both errors *and* writes the SF-7 envelope to
//! a test telemetry sink.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::event_v2::{AdapterHandleState, AdapterProbeKind, AdapterProbeResult};
use cosmon_core::id::WorkerId;
use cosmon_state::events::worker_spawn::{
    emit_adapter_briefing_consumed, emit_adapter_handle_reconciled, emit_adapter_liveness_probed,
    emit_worker_spawn_attempted,
};
use serde::{Deserialize, Serialize};

use crate::spawn::SpawnError;
use crate::TmuxBackend;

pub use crate::spawn::AdapterTelemetry;

/// The adapter-name token carried on every Worker-Spawn Port event the
/// codex transport emits.
pub const ADAPTER_NAME: &str = "codex";

/// Default config-file path for the version pin (relative to the
/// cosmon project root). Operator may override per-call via
/// [`CodexAdapter::new_with_config_path`].
pub const DEFAULT_PIN_PATH: &str = ".cosmon/adapters/codex.toml";

/// Parsed shape of `.cosmon/adapters/codex.toml`.
///
/// Minimal by design — the only load-bearing field is `codex.version`.
/// A future C6-style extension (extra args, pane signatures override)
/// hangs new fields off this struct without breaking the construction
/// contract.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct CodexConfigFile {
    /// The `[codex]` table.
    #[serde(default)]
    pub codex: CodexConfigSection,
}

/// Body of the `[codex]` table.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct CodexConfigSection {
    /// Pinned codex binary version string. The form mirrors Cargo's
    /// `=X.Y.Z` discipline — the leading `=` is optional and stripped
    /// for the equality check, so both `version = "=0.49.2"` and
    /// `version = "0.49.2"` resolve to the same pin.
    #[serde(default)]
    pub version: Option<String>,
}

/// Error type for codex adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    /// Failed to spawn the codex session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    /// Failed to kill the session.
    #[error("kill failed: {0}")]
    KillFailed(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(String),

    /// The installed codex binary does not match the pinned version
    /// (SF-7 binary-version-mismatch silent-failure class).
    ///
    /// Carries both `expected` (from `.cosmon/adapters/codex.toml`) and
    /// `found` (parsed from `codex --version`) so the operator sees
    /// both on stderr without a second tool invocation.
    #[error("SF-7 binary_version_mismatch expected={expected} found={found}")]
    BinaryVersionMismatch {
        /// Pinned version from `.cosmon/adapters/codex.toml`.
        expected: String,
        /// Observed version from `codex --version`.
        found: String,
    },

    /// The pinned config file declared no version. The pin is not
    /// optional — a missing version is a load-bearing-config error,
    /// not a "fall back to whatever" signal.
    #[error("codex config at {path} declares no version pin")]
    MissingVersionPin {
        /// Path the constructor consulted.
        path: PathBuf,
    },

    /// The codex binary could not be invoked (not on PATH, exec
    /// failure, killed by signal).
    #[error("codex binary invocation failed: {0}")]
    BinaryUnavailable(String),

    /// Reading or parsing the version-pin config file failed.
    #[error("codex config at {path} unreadable: {reason}")]
    ConfigUnreadable {
        /// Path the constructor consulted.
        path: PathBuf,
        /// Underlying I/O / parse error message.
        reason: String,
    },
}

impl From<CodexError> for SpawnError {
    fn from(e: CodexError) -> Self {
        match e {
            CodexError::SpawnFailed(m) => Self::SpawnFailed(m),
            CodexError::KillFailed(m) => Self::KillFailed(m),
            CodexError::Io(m) => Self::Io(m),
            other => Self::SpawnFailed(other.to_string()),
        }
    }
}

/// Launch mode for a codex worker pane.
///
/// The two modes mirror codex's own top-level shape (`codex [OPTIONS]
/// [PROMPT]` vs `codex exec [PROMPT]`):
///
/// - [`CodexMode::Interactive`] — the **default** since task-20260711-246d.
///   Spawns codex's steerable TUI (`codex <flags>`, no `exec` subcommand),
///   the same shape the `claude` adapter uses: the pane stays open after
///   the task, the worker is driveable by `cs whisper`, and completion is
///   the worker calling `cs evolve`/`cs complete` — not the pane dying.
///   This is the parity mode that makes a codex worker pilotable exactly
///   like a claude worker.
/// - [`CodexMode::Exec`] — the legacy `codex exec '<prompt>'`
///   fire-and-forget batch mode. Non-interactive, non-steerable; kept
///   reachable for batch use via `[adapters.codex].mode = "exec"`.
///
/// The `Default` is [`CodexMode::Interactive`] — selecting `--adapter codex`
/// with no further config gives the steerable pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexMode {
    /// Steerable interactive TUI (parity with the claude adapter).
    #[default]
    Interactive,
    /// Non-interactive `codex exec` batch mode (fire-and-forget).
    Exec,
}

impl CodexMode {
    /// Parse the `[adapters.codex].mode` config value.
    ///
    /// `"exec"` (case-insensitive) selects [`CodexMode::Exec`]; every other
    /// value — including `"interactive"`, the empty string, and any typo —
    /// resolves to the [`CodexMode::Interactive`] default. Fail-*open* to
    /// the steerable mode by design: an unrecognised knob must never
    /// silently drop a worker back into the non-steerable batch path.
    #[must_use]
    pub fn from_config_str(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("exec") {
            Self::Exec
        } else {
            Self::Interactive
        }
    }
}

/// Default `RUST_LOG` level for an interactive codex worker.
///
/// codex emits OTEL telemetry at `INFO` by default, which floods the
/// `cs peek` pane with noise that drowns the actual conversation (the
/// "weird" pane the operator reported 2026-07-11). Pinning `RUST_LOG=error`
/// on the interactive spawn keeps the pane readable without suppressing
/// genuine failures. Applied only in [`CodexMode::Interactive`]; `codex
/// exec` is left untouched so batch telemetry capture is unchanged.
pub const INTERACTIVE_LOG_LEVEL: &str = "error";

/// Default flags for an interactive codex worker.
///
/// - `--dangerously-bypass-approvals-and-sandbox` — the flag equivalent of
///   codex's `trust_level = "trusted"`: no approval prompts, no internal
///   sandbox. A cosmon worker already runs in an externally-supervised
///   git worktree + tmux pane (exactly the "externally sandboxed
///   environment" the flag's own help names), so the worker can run
///   `cargo` / `git` autonomously to completion.
/// - `--no-alt-screen` — render the TUI inline into normal scrollback
///   instead of the alternate screen. This keeps `tmux capture-pane`
///   (both the readiness probe and `cs peek`) showing the real
///   conversation, and is what makes the pane clean and driveable.
///
/// Overridable per-installation via `[adapters.codex].extra_args`; when
/// that row is non-empty it replaces this default set verbatim.
pub const DEFAULT_INTERACTIVE_ARGS: &[&str] = &[
    "--dangerously-bypass-approvals-and-sandbox",
    "--no-alt-screen",
];

/// Configuration for spawning a codex session.
#[derive(Debug, Clone)]
pub struct CodexSessionConfig {
    /// Tmux socket name.
    pub socket: String,
    /// Tmux session name.
    pub session_name: String,
    /// Working directory. The tmux session is created with this as its
    /// cwd (`tmux new-session -c <work_dir>`), so codex inherits it
    /// without a redundant `--cd`/`--workdir` flag. Retained on the
    /// config because the spawn-telemetry envelope records it.
    pub work_dir: String,
    /// Resolved codex binary path (PATH-relative or absolute).
    pub binary: PathBuf,
    /// Optional prompt.
    ///
    /// In [`CodexMode::Exec`] this is forwarded as the positional argument
    /// to `codex exec`. In [`CodexMode::Interactive`] it is **not** placed
    /// on the command line — the prompt is injected into the TUI composer
    /// after readiness (mirroring the claude adapter), so the assembled
    /// command carries no positional prompt.
    pub prompt: Option<String>,
    /// Launch mode (interactive TUI vs `codex exec` batch). Defaults to
    /// [`CodexMode::Interactive`].
    pub mode: CodexMode,
    /// Optional model pin resolved by the common Incarnation selector.
    /// Carried as codex's native `--model` flag in both launch modes.
    pub model: Option<String>,
    /// Extra flags for the interactive spawn. Empty means "use
    /// [`DEFAULT_INTERACTIVE_ARGS`]"; a non-empty list replaces the
    /// defaults verbatim. Ignored in [`CodexMode::Exec`].
    pub extra_args: Vec<String>,
    /// Optional IFBDD telemetry context.
    pub telemetry: Option<AdapterTelemetry>,
    /// Optional pre-existing worker the spawn path detected.
    pub pre_existing_worker: Option<WorkerId>,
}

/// Assemble the shell command string handed to the tmux backend for a
/// codex worker — the pure, unit-testable seam (mirror of
/// [`cosmon_cli::tackle_env::build_claude_command`]).
///
/// - [`CodexMode::Exec`] → `codex exec '<prompt>'` (byte-identical to the
///   pre-interactive behaviour; the prompt is single-quote escaped).
/// - [`CodexMode::Interactive`] → `RUST_LOG=error codex <flags>` with **no**
///   positional prompt (the caller injects it into the composer after
///   readiness). `<flags>` is [`DEFAULT_INTERACTIVE_ARGS`] unless
///   `config.extra_args` overrides it.
#[must_use]
pub fn build_codex_command(config: &CodexSessionConfig) -> String {
    let bin = shell_escape(&config.binary.to_string_lossy());
    match config.mode {
        CodexMode::Exec => {
            let mut cmd = bin;
            cmd.push_str(" exec");
            if let Some(ref model) = config.model {
                cmd.push_str(" --model ");
                cmd.push_str(&shell_escape(model));
            }
            if let Some(ref prompt) = config.prompt {
                let escaped = prompt.replace('\'', "'\\''");
                let _ = write!(cmd, " '{escaped}'");
            }
            cmd
        }
        CodexMode::Interactive => {
            // Quiet OTEL telemetry so the pane is readable, then the bare
            // interactive binary with its autonomy + inline-scrollback flags.
            // No positional prompt — it is injected post-readiness (the
            // claude-mirror that also fixes the submission bug).
            let mut cmd = format!("RUST_LOG={INTERACTIVE_LOG_LEVEL} {bin}");
            if let Some(ref model) = config.model {
                cmd.push_str(" --model ");
                cmd.push_str(&shell_escape(model));
            }
            let flags: Vec<String> = if config.extra_args.is_empty() {
                DEFAULT_INTERACTIVE_ARGS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            } else {
                config.extra_args.clone()
            };
            for flag in &flags {
                cmd.push(' ');
                cmd.push_str(&shell_escape(flag));
            }
            cmd
        }
    }
}

/// Spawn a codex session in a tmux window.
///
/// Delegates command assembly to [`build_codex_command`] and the spawn to
/// [`TmuxBackend::spawn_worker`]. In [`CodexMode::Interactive`] (the
/// default) the command is the steerable TUI with no positional prompt —
/// the caller injects the prompt after readiness (mirror of the claude
/// adapter). In [`CodexMode::Exec`] it is `codex exec '<prompt>'`, codex's
/// non-interactive automation subcommand whose indistinct exit-1 the
/// classifier in [`cosmon_core::adapter_exit`] already accounts for. The
/// working directory is supplied by the tmux session itself
/// (`new-session -c <work_dir>`), so no `--cd`/`--workdir` flag is passed.
/// Emits
/// [`EventV2::WorkerSpawnAttempted`](cosmon_core::event_v2::EventV2::WorkerSpawnAttempted)
/// *before* the backend call when telemetry is attached.
///
/// # Errors
///
/// Returns [`CodexError::SpawnFailed`] when the tmux session cannot be
/// created.
pub fn spawn_codex_session(config: &CodexSessionConfig) -> Result<(), CodexError> {
    let cmd = build_codex_command(config);

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
        .map_err(|e| CodexError::SpawnFailed(e.to_string()))
}

/// Kill a codex session by tmux session name.
///
/// Emits [`EventV2::AdapterHandleReconciled`](cosmon_core::event_v2::EventV2::AdapterHandleReconciled)
/// when telemetry is attached.
///
/// # Errors
///
/// Returns [`CodexError::KillFailed`] if the session cannot be killed.
pub fn kill_session(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<(), CodexError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| CodexError::KillFailed(e.to_string()))?;
    let outcome = backend
        .terminate(&wid)
        .map_err(|e| CodexError::KillFailed(e.to_string()));

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

/// Check whether a codex tmux session is alive.
///
/// Emits [`EventV2::AdapterLivenessProbed`](cosmon_core::event_v2::EventV2::AdapterLivenessProbed)
/// with `probe_kind = PaneSignature` when telemetry is attached.
///
/// # Errors
///
/// Returns [`CodexError::Io`] if the tmux command fails unexpectedly.
pub fn check_alive(
    socket: &str,
    session_name: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<bool, CodexError> {
    use cosmon_core::transport::TransportBackend;

    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name).map_err(|e| CodexError::Io(e.to_string()))?;
    let outcome = backend
        .is_alive(&wid)
        .map_err(|e| CodexError::Io(e.to_string()));

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
/// with `adapter_name = "codex"`.
///
/// # Errors
///
/// Returns [`CodexError::Io`] if the briefing cannot be read.
pub fn consume_briefing(
    briefing_path: &Path,
    recorded_seal: &str,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<Vec<u8>, CodexError> {
    let bytes = std::fs::read(briefing_path).map_err(|e| CodexError::Io(e.to_string()))?;

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

/// Read the pin file and extract `codex.version`. Both the leading `=`
/// (Cargo "=X.Y.Z" form) and any surrounding whitespace are tolerated;
/// the comparison performed by [`versions_match`] is whitespace-and-
/// leading-`=` insensitive on both sides.
///
/// Reserved for the eventual codex dispatch wiring; the `CodexAdapter`
/// orchestrator that consumed it was deleted.
#[allow(dead_code)]
fn read_pinned_version(config_path: &Path) -> Result<String, CodexError> {
    let raw = std::fs::read_to_string(config_path).map_err(|e| CodexError::ConfigUnreadable {
        path: config_path.to_owned(),
        reason: e.to_string(),
    })?;
    let parsed: CodexConfigFile =
        toml::from_str(&raw).map_err(|e| CodexError::ConfigUnreadable {
            path: config_path.to_owned(),
            reason: e.to_string(),
        })?;
    parsed
        .codex
        .version
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| CodexError::MissingVersionPin {
            path: config_path.to_owned(),
        })
}

/// Walk up from `work_dir` looking for `.cosmon/adapters/codex.toml`.
/// Returns the resolved path or `None` if no ancestor carries one.
///
/// Mirrors git's repository-discovery gesture (walk upward until a
/// marker is found). The deferred dispatch path uses this to locate
/// the pin file without the registry knowing the project root.
///
/// Reserved for the eventual codex dispatch wiring; the `CodexAdapter`
/// orchestrator that consumed it was deleted.
#[allow(dead_code)]
fn locate_pin_for_workdir(work_dir: &Path) -> Option<PathBuf> {
    let mut cursor: Option<&Path> = Some(work_dir);
    while let Some(dir) = cursor {
        let candidate = dir.join(DEFAULT_PIN_PATH);
        if candidate.is_file() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

/// Compare two version strings tolerating the Cargo `=X.Y.Z` prefix on
/// either side and surrounding whitespace. Equality is strict-string
/// once both prefixes are stripped — codex does not use semver-range
/// matching in this adapter.
///
/// Reserved for the eventual codex dispatch wiring; the `CodexAdapter`
/// orchestrator that consumed it was deleted.
#[allow(dead_code)]
fn versions_match(expected: &str, found: &str) -> bool {
    fn normalise(v: &str) -> &str {
        v.trim().trim_start_matches('=').trim()
    }
    normalise(expected) == normalise(found)
}

/// Minimal shell-escape — wraps `s` in single quotes when it contains
/// any character outside the safe ASCII subset. Mirrors
/// [`crate::aider::shell_escape`].
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
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260518-9be4").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-codex").unwrap()
    }

    fn telemetry(state_dir: &Path) -> AdapterTelemetry {
        AdapterTelemetry::new(mol(), wkr(), state_dir.to_owned(), "uuid-codex")
    }

    fn read_envelopes(state_dir: &Path) -> Vec<Envelope> {
        let path = state_dir.join("events.jsonl");
        let raw = fs::read_to_string(&path).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Envelope::from_line(l).expect("envelope must parse"))
            .collect()
    }

    /// Write `script` as an executable shim at `dir/codex` and prepend
    /// `dir` to `PATH` (returning the previous value for restoration).
    fn _install_fake_codex(dir: &Path, script: &str) {
        let path = dir.join("codex");
        fs::write(&path, script).expect("write fake codex shim");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }

    fn _prepend_path(extra: &Path) -> String {
        let previous = std::env::var("PATH").unwrap_or_default();
        let new = format!("{}:{}", extra.display(), previous);
        std::env::set_var("PATH", &new);
        previous
    }

    fn _restore_path(previous: String) {
        std::env::set_var("PATH", previous);
    }

    fn write_pin(dir: &Path, version: &str) -> PathBuf {
        let path = dir.join("codex.toml");
        fs::write(&path, format!("[codex]\nversion = \"{version}\"\n")).expect("write pin file");
        path
    }

    /// Locking guard so the four tests that mutate `PATH` cannot race
    /// each other when `cargo test` runs them on the same process.
    /// `std::sync::Mutex` is fine — these are short-lived sequential
    /// ops; no async crossings.
    fn _path_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn versions_match_strips_equality_and_whitespace() {
        assert!(versions_match("=0.49.2", "0.49.2"));
        assert!(versions_match("0.49.2", "=0.49.2"));
        assert!(versions_match("  =0.49.2  ", "0.49.2"));
        assert!(!versions_match("0.49.2", "0.49.3"));
    }

    #[test]
    fn shell_escape_passes_safe_input_untouched() {
        assert_eq!(shell_escape("codex"), "codex");
        assert_eq!(shell_escape("/usr/local/bin/codex"), "/usr/local/bin/codex");
    }

    #[test]
    fn shell_escape_quotes_unsafe_input() {
        assert_eq!(shell_escape("with spaces"), "'with spaces'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn read_pinned_version_extracts_codex_version() {
        let dir = tempdir().unwrap();
        let path = write_pin(dir.path(), "=0.49.2");
        let v = read_pinned_version(&path).expect("version read");
        assert_eq!(v, "=0.49.2");
    }

    #[test]
    fn read_pinned_version_errors_on_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("absent.toml");
        let err = read_pinned_version(&path).expect_err("missing file");
        assert!(matches!(err, CodexError::ConfigUnreadable { .. }));
    }

    #[test]
    fn read_pinned_version_errors_on_missing_version_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("codex.toml");
        fs::write(&path, "[codex]\n").unwrap();
        let err = read_pinned_version(&path).expect_err("missing version");
        assert!(matches!(err, CodexError::MissingVersionPin { .. }));
    }

    fn cfg(mode: CodexMode, prompt: Option<&str>, extra_args: Vec<String>) -> CodexSessionConfig {
        CodexSessionConfig {
            socket: "cosmon".into(),
            session_name: "polecat-codex".into(),
            work_dir: "/tmp/wt".into(),
            binary: PathBuf::from("codex"),
            prompt: prompt.map(str::to_owned),
            mode,
            model: None,
            extra_args,
            telemetry: None,
            pre_existing_worker: None,
        }
    }

    #[test]
    fn spawn_config_round_trips_fields() {
        let c = cfg(CodexMode::Exec, Some("hello"), vec![]).clone();
        assert_eq!(c.session_name, "polecat-codex");
        assert_eq!(c.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn codex_mode_defaults_to_interactive() {
        assert_eq!(CodexMode::default(), CodexMode::Interactive);
    }

    #[test]
    fn codex_mode_parses_exec_case_insensitively() {
        assert_eq!(CodexMode::from_config_str("exec"), CodexMode::Exec);
        assert_eq!(CodexMode::from_config_str("EXEC"), CodexMode::Exec);
        assert_eq!(CodexMode::from_config_str("  Exec "), CodexMode::Exec);
    }

    #[test]
    fn codex_mode_fails_open_to_interactive() {
        // Any unrecognised value — including a typo — must resolve to the
        // steerable default, never silently drop to the batch path.
        assert_eq!(
            CodexMode::from_config_str("interactive"),
            CodexMode::Interactive
        );
        assert_eq!(CodexMode::from_config_str(""), CodexMode::Interactive);
        assert_eq!(CodexMode::from_config_str("batch"), CodexMode::Interactive);
    }

    /// Exec mode keeps the byte-identical `codex exec '<prompt>'` shape the
    /// adapter shipped before interactive mode existed — the fire-and-forget
    /// contract must not regress.
    #[test]
    fn build_command_exec_mode_is_unchanged() {
        let c = cfg(CodexMode::Exec, Some("do the thing"), vec![]);
        assert_eq!(build_codex_command(&c), "codex exec 'do the thing'");
    }

    #[test]
    fn build_command_exec_mode_escapes_single_quotes() {
        let c = cfg(CodexMode::Exec, Some("it's done"), vec![]);
        assert_eq!(build_codex_command(&c), "codex exec 'it'\\''s done'");
    }

    #[test]
    fn model_pin_has_carrier_parity_in_both_modes() {
        for mode in [CodexMode::Interactive, CodexMode::Exec] {
            let mut c = cfg(mode, Some("work"), vec![]);
            c.model = Some("gpt-5.2-codex".to_owned());
            assert!(build_codex_command(&c).contains("--model gpt-5.2-codex"));
        }
    }

    /// Interactive mode: quiet telemetry prefix + bare binary + the autonomy
    /// / inline-scrollback default flags, and crucially **no** positional
    /// prompt (it is injected into the composer post-readiness).
    #[test]
    fn build_command_interactive_mode_is_steerable_and_quiet() {
        let c = cfg(CodexMode::Interactive, Some("ignored on cmdline"), vec![]);
        let cmd = build_codex_command(&c);
        assert_eq!(
            cmd,
            "RUST_LOG=error codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen"
        );
        // The prompt must NOT leak onto the command line in interactive mode.
        assert!(!cmd.contains("ignored on cmdline"));
        // No `exec` subcommand — this is the steerable TUI, not batch.
        assert!(!cmd.contains("exec"));
    }

    #[test]
    fn build_command_interactive_mode_honours_extra_args_override() {
        let c = cfg(
            CodexMode::Interactive,
            None,
            vec!["-m".into(), "gpt-5".into(), "--no-alt-screen".into()],
        );
        assert_eq!(
            build_codex_command(&c),
            "RUST_LOG=error codex -m gpt-5 --no-alt-screen"
        );
    }

    /// `consume_briefing` emits the same WS-4 envelope shape as
    /// claude/aider, with `adapter_name = "codex"`.
    #[test]
    fn consume_briefing_emits_codex_event() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let briefing_path = state_dir.join("briefing.md");
        fs::write(&briefing_path, b"codex hello").unwrap();

        let t = telemetry(state_dir);
        let bytes = consume_briefing(&briefing_path, "deadbeef", Some(&t)).expect("read succeeds");
        assert_eq!(bytes, b"codex hello");

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
        assert_eq!(adapter, "codex");
        assert_eq!(observed, blake3::hash(b"codex hello").to_hex().to_string());
    }

    /// `locate_pin_for_workdir` walks upward like git does — a pin
    /// file at the project root is found from a deep `work_dir`.
    #[test]
    fn locate_pin_for_workdir_walks_upward() {
        let project = tempdir().unwrap();
        let pin_dir = project.path().join(".cosmon/adapters");
        fs::create_dir_all(&pin_dir).unwrap();
        fs::write(
            pin_dir.join("codex.toml"),
            "[codex]\nversion = \"=0.49.2\"\n",
        )
        .unwrap();

        let deep = project.path().join("crates/foo/src");
        fs::create_dir_all(&deep).unwrap();

        let resolved = locate_pin_for_workdir(&deep).expect("pin must be found");
        assert!(resolved.starts_with(project.path()));
        assert!(resolved.ends_with(".cosmon/adapters/codex.toml"));
    }

    /// No pin file anywhere on the ancestor chain → `None`.
    #[test]
    fn locate_pin_for_workdir_returns_none_when_absent() {
        let dir = tempdir().unwrap();
        // Probe a child directory that doesn't exist on disk; the
        // walk-up still works because `parent()` doesn't stat.
        let probe = dir.path().join("nowhere/in/particular");
        assert!(locate_pin_for_workdir(&probe).is_none());
    }
}
