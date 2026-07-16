// SPDX-License-Identifier: AGPL-3.0-only

//! TOML schema for the daemon supervisor.
//!
//! The schema is the canonical one from `idea-20260419-25fd`:
//!
//! ```toml
//! [supervisor]
//! state_file = "~/.cosmon/daemon-supervisor.state.json"
//! log_file = "~/.cosmon/daemon-supervisor.log"
//! kill_switch = "~/.cosmon/stand-down.lock"
//!
//! [[daemon]]
//! name = "notification-bot"
//! binary = "/Users/you/.local/bin/notification-bot"
//! args = []
//! throttle_seconds = 30
//! env = { HOME = "/Users/you", RUST_LOG = "info" }
//! log_stdout = "/Users/you/.mailroom/logs/tg-bot.log"
//! log_stderr = "/Users/you/.mailroom/logs/tg-bot.err"
//! enabled = true
//! kill_switch = "/Users/you/.mailroom/stand-down.lock"
//! ```
//!
//! Two discipline choices, mirrored from `cosmon-scheduler::config`:
//!
//! 1. **Root is tolerant, daemons are strict.** Unknown root-level keys are
//!    silently accepted so forward-compatible schema additions never brick an
//!    older binary, but a typo inside `[[daemon]]` (e.g. `throttlee_seconds`)
//!    is a hard parse error because drifted fields silently change semantics.
//!
//! 2. **All validation errors are reported in one pass.** We collect every
//!    bad daemon into a single diagnostic so the operator doesn't play
//!    whack-a-mole.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Config — root TOML document
// ---------------------------------------------------------------------------

/// Root TOML document for the supervisor. Tolerant of unknown top-level keys.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Supervisor-wide settings.
    pub supervisor: SupervisorConfig,

    /// Declared daemons. Order is preserved for deterministic dry-run output.
    #[serde(default, rename = "daemon")]
    pub daemons: Vec<DaemonSpec>,
}

/// Supervisor-wide settings applied to every managed child.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SupervisorConfig {
    /// Path to the atomic state file
    /// (default `~/.cosmon/daemon-supervisor.state.json`).
    pub state_file: String,

    /// Path to the aggregate log file written by the supervisor itself.
    pub log_file: String,

    /// Path to the global kill-switch (`~/.cosmon/stand-down.lock`). If
    /// present the supervisor SIGTERMs every child and parks them until the
    /// lock disappears. Matches the existing `cosmon-scheduler` semantics so
    /// one `touch` disables the whole fleet.
    pub kill_switch: String,

    /// **Crash-loop escape valve.** When a single
    /// child crashes-and-restarts this many times inside
    /// `crash_loop_window_seconds`, the supervisor fires one
    /// operator-visible `PropulsionDown` alert (via [`Self::notify_command`])
    /// instead of crash-looping in silence. `0` disables the valve. Default
    /// `5`.
    ///
    /// The valve closes a silent failure mode: the
    /// `Exited → throttle → SpawnNow` policy is *correct* but invisible — a
    /// daemon whose config parses yet is semantically broken re-spawns
    /// forever with no signal. Silent give-up dressed as diligence is the
    /// bug; making the event exist is the fix.
    #[serde(default = "default_crash_loop_threshold")]
    pub crash_loop_threshold: u32,

    /// Rolling window (seconds) over which [`Self::crash_loop_threshold`]
    /// crashes trigger the `PropulsionDown` alert. Default `300` (5 min).
    #[serde(default = "default_crash_loop_window_seconds")]
    pub crash_loop_window_seconds: u64,

    /// Argv the supervisor invokes to surface a `PropulsionDown` alert on
    /// the operator-visible notify channel. The supervisor appends the
    /// alert flags + message (`--title PropulsionDown --level alert
    /// <message>`), so the default `["cs", "notify"]` resolves to
    /// `cs notify --title PropulsionDown --level alert "<daemon> crashed N
    /// times in Ws"`. An empty list disables dispatch (the decision is
    /// still logged to stderr). Default `["cs", "notify"]`.
    #[serde(default = "default_notify_command")]
    pub notify_command: Vec<String>,
}

/// A single supervised daemon entry. Strict: unknown fields are a hard error.
///
/// # Content identity
///
/// Two specs are considered **the same daemon** iff they share the same
/// `name`. They are considered **unchanged** iff they hash to the same
/// [BLAKE3] content hash — see [`crate::reload::spec_content_hash`]. Any
/// difference in binary, args, env, log paths, throttle, enabled flag, or
/// `kill_switch` causes a *changed* diagnosis on hot-reload, which in turn
/// triggers a `stop → spawn` cycle rather than a silent behavior swap.
///
/// [BLAKE3]: https://github.com/BLAKE3-team/BLAKE3
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DaemonSpec {
    /// Unique name across the file. Used for state-keying, log-prefixing,
    /// and `cs daemons status` lookup.
    pub name: String,

    /// Absolute (or PATH-resolvable) binary to execute.
    pub binary: String,

    /// Argv tail. Element 0 is the binary above, not included here.
    #[serde(default)]
    pub args: Vec<String>,

    /// Seconds to wait after an exit before the supervisor respawns the
    /// child. Fixed throttle (Q2 iteration 1 — Niel's cheapest). A value of
    /// `0` means "respawn immediately".
    #[serde(default = "default_throttle_seconds")]
    pub throttle_seconds: u64,

    /// Environment variables merged onto the inherited process environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Path that the child's stdout will be appended to. Parent directories
    /// are created by the real `tokio_process` adapter (Task 2); this crate
    /// only stores the path.
    #[serde(default)]
    pub log_stdout: Option<String>,

    /// Path that the child's stderr will be appended to.
    #[serde(default)]
    pub log_stderr: Option<String>,

    /// Per-daemon kill-switch path. If present the supervisor SIGTERMs *this*
    /// child alone; other daemons keep running. Evaluated *after* the global
    /// kill-switch (global disables everything regardless of per-daemon).
    #[serde(default)]
    pub kill_switch: Option<String>,

    /// When `false`, the daemon is never spawned (useful for staging an
    /// entry before enabling it, or for debugging).
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

const fn default_throttle_seconds() -> u64 {
    30
}

const fn default_enabled() -> bool {
    true
}

const fn default_crash_loop_threshold() -> u32 {
    5
}

const fn default_crash_loop_window_seconds() -> u64 {
    300
}

fn default_notify_command() -> Vec<String> {
    vec!["cs".to_owned(), "notify".to_owned()]
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors surfaced while loading or validating a [`Config`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read the TOML file from disk.
    #[error("failed to read config file: {0}")]
    Io(#[from] io::Error),

    /// The file was read but did not parse as TOML matching the schema.
    #[error("failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),

    /// Semantic validation failure. The inner string contains one line per
    /// offending daemon so `cosmon-daemon-supervisor --check` can surface
    /// the whole picture in a single diagnostic.
    #[error("config validation failed:\n{0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------
// Config impls
// ---------------------------------------------------------------------------

impl Config {
    /// Load and validate a config file.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] if the file cannot be read,
    /// [`ConfigError::Parse`] if the TOML is malformed, or
    /// [`ConfigError::Invalid`] if semantic validation fails.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)?;
        Self::from_str_validated(&raw)
    }

    /// Parse a TOML string and then run semantic validation.
    ///
    /// Named `from_str_validated` (not `FromStr`) because the trait would
    /// lose validation errors behind an associated error type; we want the
    /// full [`ConfigError`] enum.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Parse`] if the input is not valid TOML, or
    /// [`ConfigError::Invalid`] if the parsed schema fails semantic
    /// validation (duplicate names, empty binary).
    pub fn from_str_validated(raw: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Semantic validation: unique daemon names, non-empty binary. Collects
    /// *all* errors before returning.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Invalid`] with a multi-line diagnostic if any
    /// daemon violates one of the invariants.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();

        for (idx, d) in self.daemons.iter().enumerate() {
            if d.name.is_empty() {
                errors.push(format!("daemon[{idx}]: name is empty"));
            } else if !seen.insert(d.name.as_str()) {
                errors.push(format!(
                    "daemon[{idx}] '{name}': duplicate name",
                    name = d.name
                ));
            }
            if d.binary.trim().is_empty() {
                errors.push(format!(
                    "daemon[{idx}] '{name}': binary is empty",
                    name = d.name
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errors.join("\n")))
        }
    }

    /// Return the daemons indexed by name. Useful as input to
    /// [`crate::reload::diff`].
    #[must_use]
    pub fn by_name(&self) -> std::collections::HashMap<String, DaemonSpec> {
        self.daemons
            .iter()
            .map(|d| (d.name.clone(), d.clone()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// ~ expansion
// ---------------------------------------------------------------------------

/// Expand a leading `~` or `~/` to the given home directory.
///
/// Passing an explicit `home` keeps the function pure (no environment reads)
/// so it can be called from tests without setting `$HOME`. Callers that want
/// the live home should do the `dirs::home_dir()` lookup themselves and pass
/// it in — the supervisor binary does this once at startup.
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
/// use cosmon_daemon_supervisor::config::expand_tilde;
///
/// let home = Path::new("/Users/you");
/// assert_eq!(
///     expand_tilde("~/.cosmon/foo.log", home),
///     PathBuf::from("/Users/you/.cosmon/foo.log"),
/// );
/// assert_eq!(
///     expand_tilde("/etc/passwd", home),
///     PathBuf::from("/etc/passwd"),
/// );
/// assert_eq!(expand_tilde("~", home), PathBuf::from("/Users/you"));
/// ```
#[must_use]
pub fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_toml() -> &'static str {
        r#"
            [supervisor]
            state_file = "~/.cosmon/daemon-supervisor.state.json"
            log_file = "~/.cosmon/daemon-supervisor.log"
            kill_switch = "~/.cosmon/stand-down.lock"

            [[daemon]]
            name = "notification-bot"
            binary = "/usr/local/bin/notification-bot"
            args = []
            throttle_seconds = 30
            env = { RUST_LOG = "info" }
            log_stdout = "/tmp/tg-bot.log"
            log_stderr = "/tmp/tg-bot.err"
            enabled = true

            [[daemon]]
            name = "emacs-daemon"
            binary = "/usr/local/bin/emacs"
            args = ["--fg-daemon"]
        "#
    }

    #[test]
    fn parses_valid_toml() {
        let cfg = Config::from_str_validated(valid_toml()).expect("valid config");
        assert_eq!(cfg.daemons.len(), 2);
        assert_eq!(cfg.daemons[0].name, "notification-bot");
        assert_eq!(cfg.daemons[0].throttle_seconds, 30);
        assert_eq!(cfg.daemons[1].name, "emacs-daemon");
        // Defaults apply to the second daemon.
        assert_eq!(cfg.daemons[1].throttle_seconds, 30);
        assert!(cfg.daemons[1].enabled);
    }

    #[test]
    fn rejects_unknown_daemon_field() {
        let raw = r#"
            [supervisor]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"

            [[daemon]]
            name = "typo"
            binary = "/bin/echo"
            throttlee_seconds = 5
        "#;
        let err = Config::from_str_validated(raw).expect_err("unknown field rejected");
        assert!(
            matches!(err, ConfigError::Parse(_)),
            "expected parse error, got {err:?}"
        );
    }

    #[test]
    fn rejects_duplicate_names() {
        let raw = r#"
            [supervisor]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"

            [[daemon]]
            name = "dup"
            binary = "/bin/a"

            [[daemon]]
            name = "dup"
            binary = "/bin/b"
        "#;
        let err = Config::from_str_validated(raw).expect_err("duplicate names");
        assert!(err.to_string().contains("duplicate"), "got: {err}");
    }

    #[test]
    fn rejects_empty_binary() {
        let raw = r#"
            [supervisor]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"

            [[daemon]]
            name = "empty"
            binary = ""
        "#;
        let err = Config::from_str_validated(raw).expect_err("empty binary");
        assert!(err.to_string().contains("binary"), "got: {err}");
    }

    #[test]
    fn rejects_empty_name() {
        let raw = r#"
            [supervisor]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"

            [[daemon]]
            name = ""
            binary = "/bin/echo"
        "#;
        let err = Config::from_str_validated(raw).expect_err("empty name");
        assert!(err.to_string().contains("name"), "got: {err}");
    }

    #[test]
    fn collects_all_errors_in_one_pass() {
        let raw = r#"
            [supervisor]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"

            [[daemon]]
            name = "bad1"
            binary = ""

            [[daemon]]
            name = ""
            binary = "/bin/echo"
        "#;
        let err = Config::from_str_validated(raw).expect_err("multiple");
        let msg = err.to_string();
        assert!(msg.contains("bad1"), "got: {msg}");
        assert!(msg.contains("name is empty"), "got: {msg}");
    }

    #[test]
    fn tolerates_unknown_root_key() {
        let raw = r#"
            [supervisor]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"

            [audit]
            anything = "goes"
        "#;
        let cfg = Config::from_str_validated(raw).expect("tolerates unknown root");
        assert_eq!(cfg.daemons.len(), 0);
    }

    #[test]
    fn load_reads_file_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemons.toml");
        fs::write(&path, valid_toml()).unwrap();
        let cfg = Config::load(&path).expect("loads from disk");
        assert_eq!(cfg.daemons.len(), 2);
    }

    #[test]
    fn by_name_keys_on_spec_name() {
        let cfg = Config::from_str_validated(valid_toml()).unwrap();
        let map = cfg.by_name();
        assert!(map.contains_key("notification-bot"));
        assert!(map.contains_key("emacs-daemon"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn expand_tilde_handles_home_relative_paths() {
        let home = Path::new("/home/test");
        assert_eq!(
            expand_tilde("~/.cosmon/foo", home),
            PathBuf::from("/home/test/.cosmon/foo")
        );
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/test"));
        // No tilde → pass through.
        assert_eq!(
            expand_tilde("/etc/hosts", home),
            PathBuf::from("/etc/hosts")
        );
        // Leading-tilde-but-no-slash (e.g. `~other`) is **not** expanded: we
        // don't resolve other users' homes, matching scheduler behavior.
        assert_eq!(expand_tilde("~other", home), PathBuf::from("~other"));
    }
}
