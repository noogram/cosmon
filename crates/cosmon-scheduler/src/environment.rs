// SPDX-License-Identifier: AGPL-3.0-only

//! Environment probe — abstracts the "outside world" for tick evaluation.
//!
//! The [`tick`](crate::tick::tick) function needs three questions answered
//! while deciding whether to fire a patrol:
//!
//! 1. *Does this path exist on the filesystem?* (kill-switch detection)
//! 2. *Is this environment variable set in the invoking shell?*
//!    (`require_env` gate)
//! 3. *What time is it now?* (cron + interval accounting)
//!
//! All three are pure queries against side-channel state. Factoring them
//! behind the [`Environment`] trait lets the logic be tested with
//! deterministic stubs and keeps `tick` a pure function of its inputs.

use std::collections::HashSet;
use std::env;
use std::path::Path;

use chrono::{DateTime, Utc};

/// Read-only probe of the invoking process's world.
pub trait Environment {
    /// Returns `true` if `path` exists on the filesystem. Called for the
    /// global kill-switch and per-patrol kill-switch paths.
    ///
    /// Implementations should expand `~` to `$HOME` before checking; the real
    /// [`EnvProbe`] does so via [`shellexpand_home`].
    fn path_exists(&self, path: &str) -> bool;

    /// Returns `true` if `var` is set and non-empty in the invoking shell.
    /// Used for each entry in a patrol's `require_env`.
    fn env_var_set(&self, var: &str) -> bool;

    /// Current instant in UTC. Used by the cadence gates (interval
    /// accounting + cron matching). Default impl returns [`Utc::now`] so
    /// existing stubs keep working without ceremony; tests that need
    /// deterministic time use [`StubEnv::with_now`].
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Production probe — `std::fs::metadata` + `std::env::var` with `~` expansion.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvProbe;

impl Environment for EnvProbe {
    fn path_exists(&self, path: &str) -> bool {
        let expanded = shellexpand_home(path);
        Path::new(expanded.as_ref()).exists()
    }

    fn env_var_set(&self, var: &str) -> bool {
        env::var_os(var).is_some_and(|v| !v.is_empty())
    }
}

/// Minimal `~`/`$HOME` expander. We avoid the `shellexpand` crate on purpose:
/// the dependency surface is larger than the work done here (one prefix check
/// and one `HOME` lookup), and patrols.toml is operator-authored — full shell
/// expansion would be a footgun, not a feature.
#[must_use]
pub fn shellexpand_home(path: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            let mut s = home.to_string_lossy().into_owned();
            if !s.ends_with('/') {
                s.push('/');
            }
            s.push_str(rest);
            return std::borrow::Cow::Owned(s);
        }
    } else if path == "~" {
        if let Some(home) = env::var_os("HOME") {
            return std::borrow::Cow::Owned(home.to_string_lossy().into_owned());
        }
    }
    std::borrow::Cow::Borrowed(path)
}

/// Deterministic stub used in unit tests: path-existence, env-var sets, and
/// an optional frozen clock are captured inline.
#[derive(Debug, Default)]
pub struct StubEnv {
    /// Paths the stub pretends exist.
    pub existing_paths: HashSet<String>,
    /// Env-var names the stub pretends are set.
    pub set_vars: HashSet<String>,
    /// Frozen instant returned by [`Environment::now`]. `None` falls back to
    /// wall-clock time (useful for smoke tests).
    pub fixed_now: Option<DateTime<Utc>>,
}

impl StubEnv {
    /// Builder: mark a path as existing.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.existing_paths.insert(path.into());
        self
    }

    /// Builder: mark an env-var as set.
    #[must_use]
    pub fn with_env(mut self, var: impl Into<String>) -> Self {
        self.set_vars.insert(var.into());
        self
    }

    /// Builder: freeze the clock at `now`. Without this, the stub falls
    /// back to [`Utc::now`] and time-based tests become flaky.
    #[must_use]
    pub fn with_now(mut self, now: DateTime<Utc>) -> Self {
        self.fixed_now = Some(now);
        self
    }
}

impl Environment for StubEnv {
    fn path_exists(&self, path: &str) -> bool {
        self.existing_paths.contains(path)
    }

    fn env_var_set(&self, var: &str) -> bool {
        self.set_vars.contains(var)
    }

    fn now(&self) -> DateTime<Utc> {
        self.fixed_now.unwrap_or_else(Utc::now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shellexpand_home_expands_tilde_slash() {
        // Set HOME to a known value for the assertion.
        // SAFETY of approach: we read our own expectation back, we don't
        // manipulate any global that other tests depend on (tempfile-style).
        let prior = env::var_os("HOME");
        env::set_var("HOME", "/tmp/fakehome");
        let expanded = shellexpand_home("~/foo.txt");
        assert_eq!(expanded.as_ref(), "/tmp/fakehome/foo.txt");
        if let Some(p) = prior {
            env::set_var("HOME", p);
        }
    }

    #[test]
    fn shellexpand_home_passes_through_non_tilde() {
        assert_eq!(
            shellexpand_home("/absolute/path").as_ref(),
            "/absolute/path"
        );
        assert_eq!(shellexpand_home("relative/path").as_ref(), "relative/path");
    }

    #[test]
    fn stub_env_matches_declared_state() {
        let env = StubEnv::default().with_path("/tmp/kill").with_env("CI");
        assert!(env.path_exists("/tmp/kill"));
        assert!(!env.path_exists("/tmp/nope"));
        assert!(env.env_var_set("CI"));
        assert!(!env.env_var_set("NOT_SET"));
    }

    #[test]
    fn env_probe_returns_false_for_nonexistent() {
        let probe = EnvProbe;
        assert!(!probe.path_exists("/definitely/does/not/exist/42"));
        assert!(!probe.env_var_set("__COSMON_SCHEDULER_UNSET_VAR__"));
    }
}
