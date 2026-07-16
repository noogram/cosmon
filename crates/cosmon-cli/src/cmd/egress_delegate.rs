// SPDX-License-Identifier: AGPL-3.0-only

//! Shared egress-jail discipline for **repo-supplied delegated shell** — the
//! inc-2 Defect-1 fix (task-20260715-ff5b).
//!
//! Two cosmon surfaces run *repo-supplied* shell verbatim against the working
//! tree:
//!
//! - the `cs done` post-merge integrity cascade (rung 1 `integrity_command`
//!   and rung 3 `build_command`, [`super::done`]); and
//! - the `cs validate` tier-2 stages ([`super::validate`]).
//!
//! Both are trust-gated ([`cosmon_cli::trust`]) — but trust hashes only
//! `.cosmon/config.toml` + the formula TOMLs, **not** the scripts those commands
//! invoke. A merged branch can therefore modify a *trusted* `integrity_command`
//! script (`./ci/integrity.sh`) without staling the trust grant, and — before
//! this jail — that script executed as a plain `sh -c` with only `current_dir`
//! set: full host filesystem and network access from the combined tree, i.e.
//! arbitrary code execution and credential exfiltration (codex-sol Defect 1).
//!
//! Ordinary agent subprocesses do **not** have this hole: they are wrapped
//! through [`cosmon_core::egress::EgressJail`] +
//! [`cosmon_agent_harness::egress_probe`] in
//! `cosmon_agent_harness::tools::exec_command`. This module routes the delegated
//! gate/validate commands through the **same** discipline:
//!
//! 1. resolve the egress policy from `COSMON_EGRESS_POLICY` (identical to
//!    `exec_command`);
//! 2. probe the host's real netns capability (the C1-F3 runtime probe, not the
//!    optimistic `cfg!` ceiling);
//! 3. run the pre-spawn [`EgressJail::preflight`] — on a host that cannot
//!    kernel-enforce a required `deny-external` policy, an **exposed
//!    multi-tenant** dispatch is **refused fail-closed** (never run unconfined),
//!    mirroring the `cs tackle` pre-worktree refusal.
//!
//! When the policy permits egress ([`EgressPolicy::AllowAll`], the trusted
//! single-operator default with `COSMON_EGRESS_POLICY` unset) the wrapped
//! command is byte-identical to the pre-fix `sh -c <command>` — so
//! cosmon-on-cosmon behaviour is unchanged.

use cosmon_core::egress::{EgressJail, EgressPolicy, EgressPreflight, EnforcementMode};

/// The decision of applying the egress jail to one delegated command.
///
/// A pure function of `(policy, netns_available, require_netns,
/// exposed_multi_tenant, program, args)` ([`jail_decision`]) so the security
/// logic is host-independently testable — the `/proc` read and env reads live
/// only in [`jail_delegated_sh`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JailDecision {
    /// Spawn `program` with `args`. Under [`EnforcementMode::Netns`] +
    /// `deny-external` these are the `unshare …` wrapper; otherwise the original
    /// `sh -c <command>`. `advisory_reason`, when `Some`, is the loud audit line
    /// a `DegradedAdvisory` preflight asks the caller to emit before spawning
    /// (the policy is recorded, not kernel-enforced).
    Ready {
        /// Program to spawn (`unshare` under netns, `sh` under advisory/allow).
        program: String,
        /// Argument vector for `program`.
        args: Vec<String>,
        /// How (or whether) egress is enforced for this spawn.
        mode: EnforcementMode,
        /// Loud audit line to emit before spawning (advisory degradation).
        advisory_reason: Option<String>,
    },
    /// Do **not** spawn: a required `deny-external` policy cannot be
    /// kernel-enforced on this host and this dispatch is exposed / hard-required.
    /// The caller must fail closed (a gate *error* → rollback for `cs done`, a
    /// bail for `cs validate`), never run the repo-supplied shell unconfined.
    Refused {
        /// Actionable operator-facing refusal message.
        message: String,
    },
}

/// Pure egress-jail decision for a delegated command — no I/O, no env reads.
///
/// Mirrors `exec_command`'s wrap logic: run the preflight, then (unless refused)
/// wrap the command under the host's truthful enforcement mode. Split from
/// [`jail_delegated_sh`] so the fail-closed security path (an exposed
/// multi-tenant, non-enforceable `deny-external` ⇒ [`JailDecision::Refused`]) is
/// testable on any host, including the darwin dev box where `netns_available` is
/// always `false`.
pub(crate) fn jail_decision(
    policy: EgressPolicy,
    netns_available: bool,
    require_netns: bool,
    exposed_multi_tenant: bool,
    program: &str,
    args: &[String],
) -> JailDecision {
    let advisory_reason =
        match EgressJail::preflight(policy, netns_available, require_netns, exposed_multi_tenant) {
            EgressPreflight::Refused { message } => return JailDecision::Refused { message },
            EgressPreflight::DegradedAdvisory { reason } => Some(reason),
            EgressPreflight::Ready => None,
        };
    let mode = EgressJail::enforcement_mode_for(netns_available);
    let jailed = EgressJail::wrap_with_mode(mode, policy, program, args);
    JailDecision::Ready {
        program: jailed.program,
        args: jailed.args,
        mode: jailed.mode,
        advisory_reason,
    }
}

/// Resolve the egress posture from the process environment + host probe and
/// return the jail decision for `sh -c <command>`.
///
/// Reads exactly what `exec_command` reads: `COSMON_EGRESS_POLICY` (policy),
/// the C1-F3 netns runtime probe, and the two fail-closed axes
/// (`COSMON_EGRESS_REQUIRE_NETNS`, `COSMON_EGRESS_EXPOSED` / the RPP
/// `COSMON_API_REQUEST` marker). With `COSMON_EGRESS_POLICY` unset the policy is
/// `AllowAll` and the returned command is the byte-identical pre-fix
/// `sh -c <command>`.
pub(crate) fn jail_delegated_sh(command: &str) -> JailDecision {
    let policy = EgressPolicy::from_env_value(std::env::var(EgressPolicy::ENV_VAR).ok().as_deref());
    let netns_available = cosmon_agent_harness::egress_probe::netns_available();
    let require_netns = cosmon_agent_harness::egress_probe::require_netns_from_env();
    let exposed = cosmon_agent_harness::egress_probe::exposed_multitenant_from_env();
    let args = ["-c".to_owned(), command.to_owned()];
    jail_decision(policy, netns_available, require_netns, exposed, "sh", &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The four env helpers `jail_delegated_sh` reads are exercised in
    // `cosmon_agent_harness::egress_probe`; here we pin the *pure* decision so
    // the fail-closed security core is falsifiable on every host.

    /// The trusted single-operator default: `COSMON_EGRESS_POLICY` unset ⇒
    /// `AllowAll` ⇒ the command is the byte-identical, unjailed `sh -c`. This is
    /// what keeps cosmon-on-cosmon `cs done` / `cs validate` unchanged.
    #[test]
    fn allow_all_is_unjailed_sh_c() {
        let args = ["-c".to_owned(), "echo hi".to_owned()];
        let decision = jail_decision(EgressPolicy::AllowAll, false, false, false, "sh", &args);
        assert_eq!(
            decision,
            JailDecision::Ready {
                program: "sh".to_owned(),
                args: vec!["-c".to_owned(), "echo hi".to_owned()],
                mode: EnforcementMode::Advisory,
                advisory_reason: None,
            }
        );
    }

    /// THE LOAD-BEARING SECURITY FALSIFIER (Defect 1): an **exposed
    /// multi-tenant** dispatch with `deny-external` on a host that cannot create
    /// the netns jail (`netns_available == false`, e.g. macOS) must be
    /// **Refused** — never degraded to an unconfined passthrough shell. A tenant
    /// could otherwise exfiltrate a neighbour's state from the combined tree.
    ///
    /// Reverting `run_delegated_command` / the validate stage runner to a plain
    /// `sh -c` (bypassing this decision) removes the refusal entirely.
    #[test]
    fn deny_external_exposed_unenforceable_is_refused() {
        let args = ["-c".to_owned(), "./ci/integrity.sh".to_owned()];
        let decision = jail_decision(
            EgressPolicy::DenyExternal,
            /* netns_available */ false,
            /* require_netns */ false,
            /* exposed_multi_tenant */ true,
            "sh",
            &args,
        );
        assert!(
            matches!(decision, JailDecision::Refused { .. }),
            "exposed deny-external that cannot be kernel-enforced must fail closed; got {decision:?}"
        );
    }

    /// The operator's hard-enforcement knob (`COSMON_EGRESS_REQUIRE_NETNS`)
    /// refuses a non-enforceable `deny-external` even for a single-operator
    /// (non-exposed) dispatch.
    #[test]
    fn deny_external_require_netns_unenforceable_is_refused() {
        let args = ["-c".to_owned(), "make check".to_owned()];
        let decision = jail_decision(
            EgressPolicy::DenyExternal,
            /* netns_available */ false,
            /* require_netns */ true,
            /* exposed_multi_tenant */ false,
            "sh",
            &args,
        );
        assert!(
            matches!(decision, JailDecision::Refused { .. }),
            "got {decision:?}"
        );
    }

    /// A non-exposed `deny-external` on a host that cannot enforce it, with no
    /// hard requirement, degrades to advisory (spawn, but carry the loud reason)
    /// — the benign macOS single-operator convenience, not a refusal.
    #[test]
    fn deny_external_unenforceable_degrades_advisory_with_reason() {
        let args = ["-c".to_owned(), "make check".to_owned()];
        let decision = jail_decision(
            EgressPolicy::DenyExternal,
            /* netns_available */ false,
            /* require_netns */ false,
            /* exposed_multi_tenant */ false,
            "sh",
            &args,
        );
        match decision {
            JailDecision::Ready {
                program,
                mode,
                advisory_reason,
                ..
            } => {
                assert_eq!(program, "sh");
                assert_eq!(mode, EnforcementMode::Advisory);
                assert!(
                    advisory_reason.is_some(),
                    "advisory degradation must carry a loud reason for the audit line"
                );
            }
            JailDecision::Refused { message } => {
                panic!(
                    "must degrade, not refuse, when not exposed and not require-netns: {message}"
                )
            }
        }
    }

    /// When the host *can* enforce (`netns_available == true`) a `deny-external`
    /// command is wrapped under `unshare` (netns mode), not run bare — proof the
    /// jail is actually applied on a capable host.
    #[test]
    fn deny_external_enforceable_wraps_under_unshare() {
        let args = ["-c".to_owned(), "make check".to_owned()];
        let decision = jail_decision(
            EgressPolicy::DenyExternal,
            /* netns_available */ true,
            false,
            false,
            "sh",
            &args,
        );
        match decision {
            JailDecision::Ready { program, mode, .. } => {
                assert_eq!(
                    program, "unshare",
                    "netns mode must wrap the command under unshare"
                );
                assert_eq!(mode, EnforcementMode::Netns);
            }
            JailDecision::Refused { message } => {
                panic!("must be ready on an enforceable host: {message}")
            }
        }
    }
}
