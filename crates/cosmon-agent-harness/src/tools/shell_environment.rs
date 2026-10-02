// SPDX-License-Identifier: AGPL-3.0-only

//! The explicit environment of the `exec_command` shell.
//!
//! The tool shell used to inherit its parent's whole environment, so any
//! provider credential the harness process held (under the default name or a
//! custom one) was readable by model-issued commands. The shell is now built
//! from [`Command::env_clear`](std::process::Command::env_clear) plus the
//! small reviewed set returned by [`build`], on first spawn and on every
//! respawn. The parent environment is never re-imported after a restart.
//!
//! What the set contains, and why:
//!
//! - **Fixed terminal settings** (`PS1`, `PS2`, `HISTFILE`, `TERM`) that keep
//!   the sentinel protocol deterministic.
//! - **Toolchain and locale** names in [`INHERITED`]: `PATH`, `HOME`, user
//!   identity, locale, temp dir, and the Rust toolchain homes. Each is copied
//!   by exact name, never by prefix.
//! - **Lifecycle context** from [`PilotVar`] in [`LIFECYCLE`]: the molecule
//!   directory and parent id so the worker's `cs` reaches its own molecule,
//!   the spawn-depth and role guard, the artifact window, and the egress
//!   markers. The egress policy is not copied raw: it is resolved first and
//!   emitted as an explicit token, so an unset or corrupt value reaches the
//!   shell as `deny-external`.
//!
//! What is excluded, by omission rather than by a denylist: every credential
//! variable (whatever its name), shell-startup variables such as `BASH_ENV`
//! and `ENV`, loader injection variables such as `LD_PRELOAD` and
//! `DYLD_INSERT_LIBRARIES`, the agent-socket variables, and the Claude
//! account/model pins in [`PilotVar`].
//!
//! This reduces inherited secrets. The shell still runs as the same user with
//! full filesystem access, so it is not isolation; see the module docs of
//! [`crate::tools::exec_command`].

use std::ffi::OsString;

use cosmon_core::egress::EgressPolicy;
use cosmon_core::pilot_env::PilotVar;

/// Variables copied from the parent by exact name when present.
pub const INHERITED: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

/// Pilot variables the shell needs as lifecycle context. The egress policy
/// itself is handled separately (see [`build`]).
pub const LIFECYCLE: &[PilotVar] = &[
    PilotVar::MolDir,
    PilotVar::ParentMolId,
    PilotVar::Depth,
    PilotVar::SessionRole,
    PilotVar::ArtifactDir,
    PilotVar::EgressRequireNetns,
    PilotVar::ApiRequest,
];

/// Build the shell environment from a parent lookup and the already-resolved
/// egress policy.
///
/// Taking the lookup as a closure keeps the allowlist testable without
/// mutating the process environment.
#[must_use]
pub fn build(
    parent: impl Fn(&str) -> Option<OsString>,
    policy: EgressPolicy,
) -> Vec<(String, OsString)> {
    let mut env: Vec<(String, OsString)> = vec![
        ("PS1".to_owned(), OsString::new()),
        ("PS2".to_owned(), OsString::new()),
        ("HISTFILE".to_owned(), "/dev/null".into()),
        ("TERM".to_owned(), "dumb".into()),
        (EgressPolicy::ENV_VAR.to_owned(), policy.token().into()),
    ];
    env.extend(
        INHERITED
            .iter()
            .copied()
            .chain(LIFECYCLE.iter().map(|v| v.name()))
            .filter_map(|name| parent(name).map(|v| (name.to_owned(), v))),
    );
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(env: &[(String, OsString)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn only_allowlisted_names_pass() {
        let parent = |name: &str| Some(OsString::from(format!("v-{name}")));
        let env = build(parent, EgressPolicy::AllowAll);
        for (k, _) in &env {
            let ok = ["PS1", "PS2", "HISTFILE", "TERM", EgressPolicy::ENV_VAR]
                .contains(&k.as_str())
                || INHERITED.contains(&k.as_str())
                || LIFECYCLE.iter().any(|v| v.name() == k);
            assert!(ok, "unexpected variable {k} in the shell environment");
        }
        let n = names(&env);
        for denied in [
            "OPENAI_API_KEY",
            "ANTHROPIC_MODEL",
            "CLAUDE_CONFIG_DIR",
            "BASH_ENV",
            "LD_PRELOAD",
            "SSH_AUTH_SOCK",
        ] {
            assert!(!n.contains(&denied), "{denied} must not pass");
        }
    }

    #[test]
    fn policy_is_emitted_resolved_not_copied() {
        let parent =
            |name: &str| (name == EgressPolicy::ENV_VAR).then(|| OsString::from("allow-all"));
        let env = build(parent, EgressPolicy::DenyExternal);
        let policy: Vec<_> = env
            .iter()
            .filter(|(k, _)| k == EgressPolicy::ENV_VAR)
            .collect();
        assert_eq!(policy.len(), 1);
        assert_eq!(policy[0].1, OsString::from("deny-external"));
    }
}
