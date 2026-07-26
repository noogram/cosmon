// SPDX-License-Identifier: AGPL-3.0-only

//! Runtime host probe for egress-jail capability — the impure-shell half of
//! the C1-F3 fix (task-20260712-8d2d).
//!
//! [`cosmon_core::egress`] is I/O-free: it owns the *decision* (given whether
//! the host can create an unprivileged user+network namespace, what
//! enforcement mode / preflight outcome applies) but must not read `/proc`, run
//! a subprocess, or read the environment. This module is where all of that
//! happens, and it hands core a typed [`cosmon_core::egress::NetnsProbe`] whose
//! classification logic
//! ([`cosmon_core::egress::sysctl_userns_blocker`],
//! [`cosmon_core::egress::sandbox_policy_evidence`],
//! [`cosmon_core::egress::classify_netns_attempt_failure`]) stays pure and
//! testable on any host.
//!
//! Both `cs tackle` (the spawner) and `exec_command` (the per-subprocess jail)
//! consult the probe to compute the *truthful* enforcement mode via
//! [`cosmon_core::egress::EgressJail::enforcement_mode_for`], rather than the
//! optimistic `cfg!`-only ceiling that lied on a host which refuses the
//! namespace.
//!
//! # Why the probe is typed and not a bool (task-20260726-eabf)
//!
//! It used to return `bool`. The decision survived the seam; the *cause* did
//! not — so the operator-facing message on the far side had to invent one, and
//! it invented two sysctls it had never read. An external tester copied those
//! key names out of our message into a public reproduction recipe as if they
//! were measurements. Carrying [`cosmon_core::egress::NetnsBlocker`] across the
//! seam is what lets the message state only what was observed, up to and
//! including *"this probe cannot say why"*.

use cosmon_core::egress::{
    classify_netns_attempt_failure, sandbox_policy_evidence, sysctl_userns_blocker, NetnsBlocker,
    NetnsProbe, EXPOSED_MULTITENANT_ENV, REQUIRE_NETNS_ENV,
};

/// The RPP subprocess envelope (ADR-080 §3.5) stamps every hosted-tenant
/// invocation of `cs` with this marker. It is owned by
/// `cosmon_rpp_adapter::subprocess::env::COSMON_API_REQUEST`; the string is
/// re-stated here (not imported) so the harness does not take a dependency on
/// the adapter crate. Re-naming it there requires a successor ADR-080
/// amendment, at which point this constant must follow.
const RPP_API_REQUEST_ENV: &str = "COSMON_API_REQUEST";

/// Probe whether this host can actually create the unprivileged user+network
/// namespace that `EnforcementMode::Netns` relies on — and, when it cannot,
/// **which observation says so**.
///
/// The order is deliberate and is the whole correction of task-20260726-eabf:
///
/// 1. **Read the sysctls.** A restrictive one is a cheap, *certain* cause, and
///    reporting it quotes a measurement (`sysctl_userns_blocker` returns the
///    key and value it saw).
/// 2. **Attempt the operation.** Permissive sysctls prove nothing: an external
///    tester's container read `unprivileged_userns_clone = 1` and
///    `max_user_namespaces = 79654` and `unshare -Ur true` still returned
///    `Operation not permitted`, because the engine's default seccomp profile
///    refuses the syscall. The attempt is the only positive capability claim
///    this function is willing to make.
/// 3. **Classify a failure from what is observable**, not from what is
///    plausible. If a sandbox policy layer is active, that is the attribution
///    (without naming *which* layer — see
///    [`NetnsBlocker::SandboxPolicyBlocksSyscall`]). If none is,
///    [`NetnsBlocker::Undetermined`] is the answer, and it says so out loud.
///
/// Reporting the wrong cause here is worse than reporting none: the previous
/// version named two sysctls whenever anything failed, and those two key names
/// were copied out of our message into a public reproduction recipe as though
/// they had been measured.
///
/// Without any probe at all — the pre-C1-F3 state — `enforcement_mode()`
/// reported `Netns` from `cfg!(target_os = "linux")` alone, and a
/// `deny-external` worker on a host that refuses the namespace became *totally
/// unusable*: every `unshare` failed, bash never `exec`'d, and every
/// `exec_command` died opaquely with `"shell died during init"`.
#[must_use]
pub fn netns_probe() -> NetnsProbe {
    #[cfg(target_os = "linux")]
    {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        if let Some(blocker) = sysctl_userns_blocker(
            read("/proc/sys/kernel/unprivileged_userns_clone").as_deref(),
            read("/proc/sys/user/max_user_namespaces").as_deref(),
            read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").as_deref(),
        ) {
            return NetnsProbe::Unavailable(blocker);
        }
        // Ask the host the only truthful way: attempt the exact namespace setup
        // the jail wrapper uses, and keep the stderr — swallowing it is how a
        // real refusal becomes an invented cause.
        let attempt = std::process::Command::new("unshare")
            .args(["--net", "--user", "--map-root-user", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .output();
        let (failed_stderr, spawn_error) = match attempt {
            Ok(out) if out.status.success() => return NetnsProbe::Available,
            Ok(out) => (String::from_utf8_lossy(&out.stderr).into_owned(), None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return NetnsProbe::Unavailable(NetnsBlocker::ToolMissing {
                    tool: "unshare".to_owned(),
                });
            }
            Err(e) => (String::new(), Some(format!("could not run `unshare`: {e}"))),
        };
        let policy = sandbox_policy_evidence(
            read("/proc/self/status").as_deref(),
            read("/proc/self/attr/current").as_deref(),
            read("/sys/fs/selinux/enforce").as_deref(),
        );
        let observed = spawn_error.unwrap_or(failed_stderr);
        NetnsProbe::Unavailable(classify_netns_attempt_failure(&observed, policy.as_deref()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Silence unused-import warnings on non-Linux targets — the pure
        // helpers are only reached through the `/proc` reads above.
        let _ = (
            sysctl_userns_blocker,
            sandbox_policy_evidence,
            classify_netns_attempt_failure,
        );
        let _ = NetnsBlocker::ToolMissing {
            tool: String::new(),
        };
        NetnsProbe::not_linux()
    }
}

/// `true` when this host can build the real netns jail.
///
/// Convenience over [`netns_probe`] for the call sites that only need the
/// enforcement *decision* and never render a cause. Anything that produces an
/// operator-facing message must use [`netns_probe`] instead — a bare bool is
/// exactly what forced the message downstream to guess.
#[must_use]
pub fn netns_available() -> bool {
    netns_probe().is_available()
}

/// `true` when the operator demanded *hard* netns enforcement via
/// [`REQUIRE_NETNS_ENV`] (`1` / `true` / `yes`, case-insensitive).
///
/// Unset selects the default degrade-to-advisory behaviour; set forces
/// [`cosmon_core::egress::EgressJail::preflight`] to refuse a `deny-external`
/// dispatch that cannot be kernel-enforced on this host.
#[must_use]
pub fn require_netns_from_env() -> bool {
    std::env::var(REQUIRE_NETNS_ENV)
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// `true` when this dispatch serves an **exposed multi-tenant** deployment, so
/// a `deny-external` policy that cannot be kernel-enforced must be *refused*
/// rather than degraded to advisory (task-20260713-8acc, architectural-
/// invariants.md §8u).
///
/// Two independent signals, either of which is sufficient:
///
/// - the dedicated operator knob [`EXPOSED_MULTITENANT_ENV`]
///   (`COSMON_EGRESS_EXPOSED`) set to a truthy value; or
/// - the RPP subprocess envelope marker `COSMON_API_REQUEST` (ADR-080 §3.5),
///   which the hosted endpoint stamps on *every* tenant-originated `cs`
///   invocation — so the hosted path is fail-closed with zero extra
///   configuration.
///
/// The dedicated knob accepts `1` / `true` / `yes` (case-insensitive); the RPP
/// marker is treated as exposed whenever it is present and non-empty (the
/// adapter always sets it to `"1"`).
#[must_use]
pub fn exposed_multitenant_from_env() -> bool {
    let dedicated = std::env::var(EXPOSED_MULTITENANT_ENV)
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let rpp_marker = std::env::var(RPP_API_REQUEST_ENV).is_ok_and(|v| !v.trim().is_empty());
    dedicated || rpp_marker
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netns_available_is_false_on_non_linux() {
        // On the darwin dev host this must be false so `deny-external` degrades
        // to advisory rather than attempting an impossible `unshare`.
        if !cfg!(target_os = "linux") {
            assert!(!netns_available());
        }
    }

    #[test]
    fn probe_and_bool_never_disagree() {
        // The bool is derived from the probe, so a caller that renders a cause
        // and a caller that only needs the decision cannot drift apart.
        assert_eq!(netns_probe().is_available(), netns_available());
    }

    #[test]
    fn non_linux_probe_reports_the_structural_cause_not_a_sysctl() {
        // The darwin dev host is unavailable for a *structural* reason. Before
        // task-20260726-eabf every unavailability rendered the same
        // sysctl-flavoured sentence; here the blocker must be NotLinux and its
        // text must not mention a kernel knob nobody read.
        if cfg!(target_os = "linux") {
            return;
        }
        let probe = netns_probe();
        let blocker = probe
            .blocker()
            .expect("non-Linux host cannot build a netns");
        assert_eq!(blocker.as_token(), "netns-unavailable:not-linux");
        let text = blocker.describe();
        assert!(!text.contains("sysctl"), "{text}");
        assert!(text.contains(std::env::consts::OS), "{text}");
    }

    /// On a netns-capable Linux host the probe must report `Available` — and it
    /// may only do so after actually creating the namespace. Skipped elsewhere:
    /// a container with a restrictive seccomp profile legitimately reports a
    /// blocker, which is the whole point of the type.
    #[test]
    fn linux_probe_when_available_is_a_functional_result() {
        if !cfg!(target_os = "linux") {
            return;
        }
        match netns_probe() {
            cosmon_core::egress::NetnsProbe::Available => {
                // The attempt succeeded; nothing further to assert.
            }
            cosmon_core::egress::NetnsProbe::Unavailable(blocker) => {
                // Whatever the cause, it must carry an attributable token and a
                // description that is not the old blanket sysctl sentence.
                assert!(blocker.as_token().starts_with("netns-unavailable:"));
                let text = blocker.describe();
                let named_sysctl = text.contains("kernel.unprivileged_userns_clone")
                    || text.contains("user.max_user_namespaces")
                    || text.contains("kernel.apparmor_restrict_unprivileged_userns");
                if named_sysctl {
                    assert_eq!(
                        blocker.as_token(),
                        "netns-unavailable:sysctl-restricted",
                        "a sysctl may only be named when it was the key actually read: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn require_netns_from_env_parses_truthy_tokens() {
        // The pure parse is exercised via a scoped env mutation; keep the two
        // reads serialised by doing them in one test (no cross-test env races).
        // SAFETY: single-threaded within this test; restored before returning.
        std::env::remove_var(REQUIRE_NETNS_ENV);
        assert!(!require_netns_from_env());
        std::env::set_var(REQUIRE_NETNS_ENV, "1");
        assert!(require_netns_from_env());
        std::env::set_var(REQUIRE_NETNS_ENV, "TRUE");
        assert!(require_netns_from_env());
        std::env::set_var(REQUIRE_NETNS_ENV, "no");
        assert!(!require_netns_from_env());
        std::env::remove_var(REQUIRE_NETNS_ENV);
    }

    #[test]
    fn exposed_multitenant_reads_both_signals() {
        // Serialised env reads in one test to avoid cross-test env races.
        // SAFETY: single-threaded within this test; restored before returning.
        std::env::remove_var(EXPOSED_MULTITENANT_ENV);
        std::env::remove_var(RPP_API_REQUEST_ENV);
        assert!(!exposed_multitenant_from_env());

        // Dedicated operator knob.
        std::env::set_var(EXPOSED_MULTITENANT_ENV, "1");
        assert!(exposed_multitenant_from_env());
        std::env::set_var(EXPOSED_MULTITENANT_ENV, "no");
        assert!(!exposed_multitenant_from_env());
        std::env::remove_var(EXPOSED_MULTITENANT_ENV);

        // RPP hosted-tenant marker — fail-closed with zero extra config.
        std::env::set_var(RPP_API_REQUEST_ENV, "1");
        assert!(exposed_multitenant_from_env());
        std::env::set_var(RPP_API_REQUEST_ENV, "   ");
        assert!(!exposed_multitenant_from_env());
        std::env::remove_var(RPP_API_REQUEST_ENV);
    }
}
