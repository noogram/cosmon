// SPDX-License-Identifier: AGPL-3.0-only

//! Runtime host probe for egress-jail capability — the impure-shell half of
//! the C1-F3 fix (task-20260712-8d2d).
//!
//! [`cosmon_core::egress`] is I/O-free: it owns the *decision* (given whether
//! the host can create an unprivileged user+network namespace, what
//! enforcement mode / preflight outcome applies) but must not read `/proc` or
//! the environment. This module is where that reading happens, so the netns
//! probe can be tested against the pure
//! [`cosmon_core::egress::userns_permitted`] decision on any host.
//!
//! Both `cs tackle` (the spawner) and `exec_command` (the per-subprocess jail)
//! call [`netns_available`] to compute the *truthful* enforcement mode via
//! [`cosmon_core::egress::EgressJail::enforcement_mode_for`], rather than the
//! optimistic `cfg!`-only ceiling that lied on a hardened kernel.

use cosmon_core::egress::{userns_permitted, EXPOSED_MULTITENANT_ENV, REQUIRE_NETNS_ENV};

/// The RPP subprocess envelope (ADR-080 §3.5) stamps every hosted-tenant
/// invocation of `cs` with this marker. It is owned by
/// `cosmon_rpp_adapter::subprocess::env::COSMON_API_REQUEST`; the string is
/// re-stated here (not imported) so the harness does not take a dependency on
/// the adapter crate. Re-naming it there requires a successor ADR-080
/// amendment, at which point this constant must follow.
const RPP_API_REQUEST_ENV: &str = "COSMON_API_REQUEST";

/// Probe whether this host can actually create the unprivileged user+network
/// namespace that `EnforcementMode::Netns` relies on.
///
/// `false` on any non-Linux host (netns is a Linux facility) and on a Linux
/// host whose kernel hardening disables unprivileged user namespaces
/// (`/proc/sys/kernel/unprivileged_userns_clone` = `0` or
/// `/proc/sys/user/max_user_namespaces` = `0`). This is the robustness probe
/// C1-F3 added: without it `enforcement_mode()` reported `Netns` from
/// `cfg!(target_os = "linux")` alone, and a `deny-external` worker on a
/// hardened kernel became *totally unusable* — every `unshare` failed to create
/// the namespace, bash never `exec`'d, and every `exec_command` died opaquely
/// with `"shell died during init"`.
///
/// The probe is conservative in the permissive direction: a missing knob (older
/// kernel) or an unparseable value is treated as *available*, because the
/// runtime `unshare` remains the real arbiter and a false-negative here would
/// needlessly degrade a capable host. A false-positive is harmless — the spawn
/// still fails loud (now with a correctly-named program in the error), it just
/// does not benefit from the pre-spawn advisory degradation.
#[must_use]
pub fn netns_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        // The two /proc knobs miss newer hardenings: Ubuntu ≥23.10 restricts
        // unprivileged userns through AppArmor
        // (kernel.apparmor_restrict_unprivileged_userns), where the clone
        // itself succeeds and the uid_map write then fails EPERM — exactly
        // what GitHub runners exhibit. Keep the knob check as the cheap
        // fast-negative, then ask the kernel the only truthful way: attempt
        // the exact namespace setup the jail wrapper uses.
        let unpriv = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone").ok();
        let max_ns = std::fs::read_to_string("/proc/sys/user/max_user_namespaces").ok();
        if !userns_permitted(unpriv.as_deref(), max_ns.as_deref()) {
            return false;
        }
        std::process::Command::new("unshare")
            .args(["--net", "--user", "--map-root-user", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Silence the unused-import warning on non-Linux targets — the pure
        // decision helper is only reached through the `/proc` read above.
        let _ = userns_permitted;
        false
    }
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
