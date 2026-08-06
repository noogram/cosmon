// SPDX-License-Identifier: AGPL-3.0-only

//! The hermetic boundary between a worker's **pilotage** environment and the
//! environment its **verification gates** run in.
//!
//! # The defect this module exists to close
//!
//! `cs tackle` steers a worker by putting variables in its environment: which
//! Claude account to bill, how deep the spawn chain is, where the molecule
//! state lives, whether the adapter may reach the network. Those are *pilot*
//! variables — instructions addressed to the worker process.
//!
//! A worker then runs `just gates`, which runs `cargo test`, which inherits
//! that environment wholesale. Every test process is now reading variables
//! that were addressed to its parent. Three times this has produced a false
//! verdict, always with the same shape — a **pilotage** variable read as a
//! **configuration** variable by a test:
//!
//! - `COSMON_EGRESS_POLICY=deny-external` (set to confine a *local* adapter in
//!   a network namespace) reached the `cs`-spawning HTTP suites, which then
//!   ran every child through the egress delegate. On 2026-08-06 that verdict
//!   collapsed `task-20260804-2bbb`; the work was intact (preserved at
//!   `226b9b0d`) and the verdict was false.
//! - `CB_DEPTH` made every `cs tackle` a test spawns hit the depth guard: 4
//!   tackle suites plus 7 others red, `1588 passed / 0 failed` once removed.
//! - `ANTHROPIC_MODEL` reddened the credential suites through the F6
//!   incoherent-pair refusal.
//!
//! Each was diagnosed by hand, each time from scratch.
//!
//! # Why a manifest, and why this one cannot silently rot
//!
//! Three shapes were considered:
//!
//! - **(b) A reserved `COSMON_PILOT_*` prefix, stripped by pattern.** Forgetting
//!   becomes impossible, which is the property we want — but it cannot cover
//!   the surface. `ANTHROPIC_MODEL` and `CLAUDE_CONFIG_DIR` are *Claude Code's*
//!   variable names, read by a third-party binary; renaming them renames
//!   nothing, it just stops steering the worker. And `COSMON_EGRESS_POLICY` is
//!   not pilotage at all — it is a load-bearing security control that
//!   [`crate::egress::EgressPolicy::from_env_value`] fails **closed** on.
//!   Moving it into a namespace whose defining property is "safe to strip"
//!   would be mislabelling a jail as a hint.
//! - **(c) Pass the pilot state out-of-band (context file, argv).** Impossible
//!   for most of the surface: `claude` reads `ANTHROPIC_MODEL` and
//!   `CLAUDE_CONFIG_DIR` from its own environment and from nowhere else, and a
//!   worker's `cs` resolves its molecule through `COSMON_MOL_DIR`.
//! - **(a) An explicit list the gate recipe unsets.** Simple, and the honest
//!   objection is that a list you must remember to update is a regression with
//!   a delay fuse.
//!
//! This module is (a) with that fuse pulled, by making the list the
//! **producer's** list rather than a copy of it. [`PilotVar`] is the single
//! declaration of every variable `cs tackle` injects; the spawn path emits
//! through [`PilotVar::name`], and the boundary strips [`PilotVar::ALL`]. You
//! cannot inject a variable you have not declared, and everything declared is
//! stripped. That is what covers *the variable nobody has invented yet*: it
//! will be born as a variant here, because that is where the emitter reads its
//! name from.
//!
//! Two residual holes, stated rather than papered over:
//!
//! 1. A pilot variable set by something that is **not** `cs tackle` — an
//!    operator's shell, a frozen tmux server env — is outside the closure. The
//!    canary ([`detect_in`]) is the second line of defence: it names the
//!    variable in one red test instead of leaving N suites mysteriously red.
//! 2. The strip happens at the **gate** boundary, not at the process boundary.
//!    A worker who types `cargo test` by hand bypasses it — and gets the
//!    canary, which tells them exactly that.
//!
//! # What this boundary must never do
//!
//! Strip the egress policy from anything **other** than a verification gate.
//! `deny-external` confining a local adapter is a real guarantee; the fix for
//! a test poisoned by it is to keep the test out of the adapter's environment,
//! never to weaken the adapter's confinement. The boundary is applied by
//! `scripts/no-pilot-env.sh`, which is referenced from the `justfile` and CI
//! and from no runtime code path — pinned by
//! `boundary_is_never_applied_to_a_runtime_path`.

/// One environment variable that `cs tackle` injects to **steer** a worker,
/// and that therefore must not be visible to that worker's verification gates.
///
/// This enum is the single source of truth. The spawn path emits names through
/// [`PilotVar::name`] and the gate boundary strips [`PilotVar::ALL`], so the
/// two lists are the same list — see the module docs for why that, rather than
/// a hand-maintained denylist, is what protects against the variable nobody
/// has invented yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PilotVar {
    /// Which Claude account directory the worker bills to (`cb next` /
    /// operator override). Claude Code reads it from the environment only.
    ClaudeConfigDir,
    /// Model pin passed opaquely to the worker's `claude`. Poisons the
    /// credential suites through the F6 incoherent-pair refusal.
    AnthropicModel,
    /// Claude Code's own root-under-`bypassPermissions` escape valve.
    IsSandbox,
    /// `worker` for a spawned session — half of the Gödel self-reference guard.
    SessionRole,
    /// Spawn depth. The other half of the guard: a test that spawns `cs tackle`
    /// inherits the worker's depth and its child trips the refusal.
    Depth,
    /// Absolute molecule state directory the worker's `cs` resolves against.
    MolDir,
    /// The molecule that spawned this worker.
    ParentMolId,
    /// Egress posture for the in-process `exec_command` tool. A **security
    /// control**, listed here because a gate must not inherit the adapter's
    /// confinement — never because it is safe to remove at runtime.
    EgressPolicy,
    /// Operator/exposed opt-in to hard netns enforcement; travels with
    /// [`Self::EgressPolicy`] and would leave a gate half-confined alone.
    EgressRequireNetns,
    /// Marks a dispatch that arrived through the RPP API, which projects the
    /// exposed multi-tenant posture onto the egress requirement.
    ApiRequest,
    /// Delivery window for a tenant-visible result. A gate inheriting it would
    /// write test output into a live tenant's artifact directory.
    ArtifactDir,
}

impl PilotVar {
    /// Every declared pilot variable.
    ///
    /// Exhaustiveness is enforced by `all_lists_every_variant`, which matches
    /// on each variant with no wildcard arm: adding a variant stops that test
    /// compiling until it is listed here.
    pub const ALL: &'static [Self] = &[
        Self::ClaudeConfigDir,
        Self::AnthropicModel,
        Self::IsSandbox,
        Self::SessionRole,
        Self::Depth,
        Self::MolDir,
        Self::ParentMolId,
        Self::EgressPolicy,
        Self::EgressRequireNetns,
        Self::ApiRequest,
        Self::ArtifactDir,
    ];

    /// The environment variable's name on the wire.
    ///
    /// The spawn path must emit through this accessor rather than a literal,
    /// so that declaring a variable and stripping it stay the same act.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClaudeConfigDir => "CLAUDE_CONFIG_DIR",
            Self::AnthropicModel => "ANTHROPIC_MODEL",
            Self::IsSandbox => "IS_SANDBOX",
            Self::SessionRole => "CB_SESSION_ROLE",
            Self::Depth => "CB_DEPTH",
            Self::MolDir => "COSMON_MOL_DIR",
            Self::ParentMolId => "COSMON_PARENT_MOL_ID",
            Self::EgressPolicy => crate::egress::EgressPolicy::ENV_VAR,
            Self::EgressRequireNetns => crate::egress::REQUIRE_NETNS_ENV,
            Self::ApiRequest => "COSMON_API_REQUEST",
            Self::ArtifactDir => "COSMON_ARTIFACT_DIR",
        }
    }
}

/// Every pilot-variable name, in [`PilotVar::ALL`] order.
///
/// This is what the gate boundary unsets and what the canary looks for.
#[must_use]
pub fn names() -> Vec<&'static str> {
    PilotVar::ALL.iter().map(|v| v.name()).collect()
}

/// Which pilot variables are present (and non-empty) in `lookup`'s environment.
///
/// The canary behind the second line of defence: a test process that finds
/// anything here is running inside a worker's pilotage environment, and any
/// red it produces is unattributable until that is fixed. `lookup` is injected
/// so this is testable without mutating the process environment.
#[must_use]
pub fn detect_in<F>(lookup: F) -> Vec<&'static str>
where
    F: Fn(&str) -> Option<String>,
{
    PilotVar::ALL
        .iter()
        .map(|v| v.name())
        .filter(|name| lookup(name).is_some_and(|value| !value.is_empty()))
        .collect()
}

/// The operator-facing explanation for a canary hit, naming the variables and
/// the way out.
///
/// Kept here rather than at the call site because the point of the canary is
/// that the *next* person does not re-derive the diagnosis, and a message is
/// the only part of a red test anyone reads.
#[must_use]
pub fn canary_message(found: &[&str]) -> String {
    format!(
        "pilot-env boundary breached: {} set in this test process's environment.\n\
         These variables steer a `cs tackle` WORKER; a test that reads them is \
         reading its parent's instructions as its own configuration, and any \
         red it produces is a false verdict (this has collapsed a healthy \
         molecule before — task-20260804-2bbb, 2026-08-06).\n\
         Run the gates through the boundary — `just gates` / `just quick`, or \
         `./scripts/no-pilot-env.sh cargo test --workspace` for a scoped run.",
        found.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` really does list every variant.
    ///
    /// The match has no wildcard arm on purpose: a new variant breaks this
    /// test's compilation, which is the moment the author is told that
    /// declaring a pilot variable also enrols it in the boundary.
    #[test]
    fn all_lists_every_variant() {
        for var in PilotVar::ALL {
            match var {
                PilotVar::ClaudeConfigDir
                | PilotVar::AnthropicModel
                | PilotVar::IsSandbox
                | PilotVar::SessionRole
                | PilotVar::Depth
                | PilotVar::MolDir
                | PilotVar::ParentMolId
                | PilotVar::EgressPolicy
                | PilotVar::EgressRequireNetns
                | PilotVar::ApiRequest
                | PilotVar::ArtifactDir => {}
            }
        }
        assert_eq!(
            PilotVar::ALL.len(),
            11,
            "a variant was added or removed without updating ALL"
        );
    }

    /// Names are unique — a duplicate would make the strip list quietly
    /// shorter than the enum suggests.
    #[test]
    fn names_are_unique() {
        let mut seen = names();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate pilot-var name");
    }

    /// The egress names are borrowed from the security module, not retyped.
    /// A rename there must not silently drop the variable out of the boundary.
    #[test]
    fn egress_names_track_the_security_module() {
        assert_eq!(
            PilotVar::EgressPolicy.name(),
            crate::egress::EgressPolicy::ENV_VAR
        );
        assert_eq!(
            PilotVar::EgressRequireNetns.name(),
            crate::egress::REQUIRE_NETNS_ENV
        );
    }

    /// The canary reports exactly the variables that are set and non-empty.
    #[test]
    fn detect_reports_set_non_empty_vars_only() {
        let found = detect_in(|k| match k {
            "CB_DEPTH" => Some("1".to_owned()),
            "ANTHROPIC_MODEL" => Some(String::new()), // set-but-empty steers nothing
            "COSMON_EGRESS_POLICY" => Some("deny-external".to_owned()),
            _ => None,
        });
        assert_eq!(found, vec!["CB_DEPTH", "COSMON_EGRESS_POLICY"]);
    }

    /// A clean environment produces no canary hit — the boundary must be
    /// invisible when it is respected.
    #[test]
    fn detect_is_silent_on_a_clean_environment() {
        assert!(detect_in(|_| None).is_empty());
    }

    /// The message names the offenders and the way out; the diagnosis is the
    /// deliverable here, not the failure.
    #[test]
    fn canary_message_names_the_variables_and_the_remedy() {
        let msg = canary_message(&["CB_DEPTH"]);
        assert!(msg.contains("CB_DEPTH"));
        assert!(msg.contains("no-pilot-env.sh"));
    }
}
