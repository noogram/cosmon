// SPDX-License-Identifier: AGPL-3.0-only

//! What a worker adapter can actually *do* — the seam that lets a formula
//! refuse a dispatch it could never be satisfied by.
//!
//! # The hole this closes (noogram/cosmon #4, clause 2)
//!
//! An external evaluator dispatched a `task-work`-shaped mission onto the
//! `local` adapter and observed the machinery run end to end while the
//! mission could not possibly satisfy its own briefing: the briefing told
//! the worker to run cargo gates, `git commit`, and walk the `cs evolve` /
//! `cs complete` lifecycle, and the ADR-100 Direct-API chat loop drives
//! none of those. The first fix made the *briefing* adapter-aware
//! (`build_prompt`, commit `d81b58a`): a local worker is no longer told to
//! do things it cannot do. That left the deeper half the reporter had
//! actually named — *"gate formulas on adapter capabilities"*. A formula
//! whose contract is shell work should not reach a chat-only model at all,
//! however carefully its prompt is worded.
//!
//! This module is the vocabulary for that refusal. A formula declares what
//! its steps require of a worker:
//!
//! ```toml
//! requires_capabilities = ["shell", "vcs"]
//! ```
//!
//! and [`missing_capabilities`] answers, for a resolved adapter name,
//! which of those are absent. `cs tackle` turns a non-empty answer into a
//! pre-dispatch refusal (see `cosmon-cli`'s `refuse_incapable_adapter_dispatch`).
//!
//! # Why the provision table is binary today, and says so
//!
//! The honest current split is the one [`crate::egress::adapter_is_local`]
//! already draws. A local adapter (`local` / `ollama` / `llama-cpp` /
//! `llama`) is an in-process chat loop over a confined tool registry: no
//! shell, no git, no `cs`. Every other adapter is an external coding-agent
//! CLI driving a real tmux pane and a real shell, so it has all three. So
//! [`adapter_provides`] is `!adapter_is_local` for every capability — it is
//! not a richer table pretending to be one.
//!
//! The *vocabulary* is nonetheless three-valued rather than a single
//! `requires_shell: bool`, because the distinctions are real in the
//! formulas even where no adapter separates them yet: `merge-conflict`
//! needs [`WorkerCapability::Vcs`] specifically, `producer-work` needs
//! [`WorkerCapability::Shell`] to execute its smoke-dispatch. When an
//! adapter lands that has one and not the other (a sandboxed executor with
//! a shell but no repository), the formulas already say which they meant
//! and only this table changes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;

/// A faculty a formula's steps need from the worker that executes them.
///
/// Distinct from [`Capability`](crate::capability::Capability), which is an
/// *authorization* grant (what an agent is permitted to do). This is an
/// *ability*: what the adapter is physically able to do at all. A grant can
/// be widened by policy; an ability cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerCapability {
    /// Run arbitrary commands in a shell — build toolchains, test runners,
    /// linters, project scripts.
    Shell,
    /// Drive version control: branch, commit, resolve a conflict.
    Vcs,
    /// Invoke the `cs` CLI, i.e. drive its own lifecycle transitions
    /// (`cs evolve`, `cs nucleate`, `cs complete`) rather than having
    /// cosmon drive them on its behalf.
    CosmonCli,
}

impl WorkerCapability {
    /// Every capability, in declaration order.
    ///
    /// Exists so callers can render the accepted vocabulary in an error
    /// message without restating it — a list that drifts from the enum is
    /// how a "did you mean" hint starts lying.
    pub const ALL: [Self; 3] = [Self::Shell, Self::Vcs, Self::CosmonCli];

    /// The TOML token for this capability.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Vcs => "vcs",
            Self::CosmonCli => "cs-cli",
        }
    }
}

impl fmt::Display for WorkerCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

impl FromStr for WorkerCapability {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shell" => Ok(Self::Shell),
            "vcs" => Ok(Self::Vcs),
            "cs-cli" => Ok(Self::CosmonCli),
            _ => Err(ParseEnumError {
                type_name: "WorkerCapability",
                value: s.to_owned(),
            }),
        }
    }
}

/// Whether the adapter named `adapter_name` can do `capability`.
///
/// See the module docs for why this is `!adapter_is_local` for every
/// capability today. An unknown adapter name is treated as non-local, i.e.
/// capable: an operator-registered external CLI adapter this build has
/// never heard of is a coding agent, and refusing it on ignorance would
/// gate dispatches that work.
#[must_use]
pub fn adapter_provides(adapter_name: &str, capability: WorkerCapability) -> bool {
    let _ = capability;
    !crate::egress::adapter_is_local(adapter_name)
}

/// Which of `required` the adapter named `adapter_name` cannot provide.
///
/// Returns them deduplicated and in [`WorkerCapability::ALL`] order, so the
/// refusal message a caller builds from this is stable regardless of the
/// order the formula listed them in.
#[must_use]
pub fn missing_capabilities(
    adapter_name: &str,
    required: &[WorkerCapability],
) -> Vec<WorkerCapability> {
    WorkerCapability::ALL
        .into_iter()
        .filter(|cap| required.contains(cap) && !adapter_provides(adapter_name, *cap))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_through_parse() {
        for cap in WorkerCapability::ALL {
            let parsed: WorkerCapability = cap.token().parse().expect("token parses");
            assert_eq!(parsed, cap, "{} must round-trip", cap.token());
        }
    }

    #[test]
    fn unknown_token_is_rejected_rather_than_silently_ignored() {
        // A typo'd capability in a formula must fail loudly at parse time:
        // silently dropping it would produce a formula that claims a
        // requirement it does not enforce.
        assert!("shel".parse::<WorkerCapability>().is_err());
        assert!("".parse::<WorkerCapability>().is_err());
    }

    #[test]
    fn local_adapters_provide_nothing_and_coding_agents_provide_everything() {
        for local in ["local", "ollama", "llama-cpp", "llama"] {
            for cap in WorkerCapability::ALL {
                assert!(
                    !adapter_provides(local, cap),
                    "{local} must not claim {cap}"
                );
            }
        }
        for remote in ["claude", "codex", "gemini"] {
            for cap in WorkerCapability::ALL {
                assert!(adapter_provides(remote, cap), "{remote} must claim {cap}");
            }
        }
    }

    #[test]
    fn missing_capabilities_is_empty_when_nothing_is_required() {
        assert!(missing_capabilities("local", &[]).is_empty());
    }

    #[test]
    fn missing_capabilities_reports_in_canonical_order() {
        let required = [
            WorkerCapability::CosmonCli,
            WorkerCapability::Shell,
            WorkerCapability::Vcs,
        ];
        assert_eq!(
            missing_capabilities("local", &required),
            vec![
                WorkerCapability::Shell,
                WorkerCapability::Vcs,
                WorkerCapability::CosmonCli
            ]
        );
        assert!(missing_capabilities("claude", &required).is_empty());
    }
}
