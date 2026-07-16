// SPDX-License-Identifier: AGPL-3.0-only

//! **Linux-only, env-gated** end-to-end coverage for the `exec_command`
//! egress jail under real kernel enforcement. This closes the verification
//! gap left by the macOS-only advisory tests: it asserts that a strict
//! egress policy actually blocks network access inside the spawned
//! namespace on a netns-capable kernel.
//!
//! # Why this test is gated, not always-on
//!
//! `cosmon-core::egress` unit-tests the *construction* of the `unshare`
//! wrapper on every host, and `exec_command_egress.rs` pins the *env wiring*
//! on macOS where the mode is only [`EnforcementMode::Advisory`]. Neither
//! actually proves the load-bearing claim: that under
//! [`EgressPolicy::DenyExternal`] a subprocess **physically cannot reach the
//! network**. That claim is only true — and only testable — inside a
//! netns-capable Linux kernel (colima), because on macOS the jail is advisory
//! and the command runs unchanged.
//!
//! So this test is deliberately a **no-op** unless two conditions hold:
//!
//! 1. `cfg!(target_os = "linux")` — the kernel can create an unprivileged
//!    network namespace; and
//! 2. the env gate `COSMON_NETNS_E2E=1` is set — which only the container
//!    harness (`scripts/cs-pilot-netns-egress-test.sh` +
//!    `docker/cs-pilot-netns/`) does.
//!
//! On the macOS dev host and in ordinary `cargo test --workspace`, the body
//! still **compiles** (so type/API drift is caught everywhere) but returns
//! early after printing a skip line. It never spawns `unshare`, never touches
//! the network. The real assertions run only inside the ephemeral Linux
//! container, where reaching for `1.1.1.1:443` is a controlled, expected probe.
//!
//! # What it proves (inside the container)
//!
//! Driving the **real** [`ExecCommand`] production path
//! (`ExecCommand::execute` → `EgressJail::wrap` → `unshare --net`):
//!
//! - **Not vacuous:** with the policy *unset* (`AllowAll`), an outbound TCP
//!   probe to a routable external address **succeeds** — proving the container
//!   itself has network, so a later block is meaningful and not just "there
//!   was never any network".
//! - **Denied by construction:** with `COSMON_EGRESS_POLICY=deny-external`,
//!   the same probe **fails** — the worker is dropped into an egress-denied
//!   netns with loopback only, so `claude -p` (or any remote oracle shellout)
//!   finds the API a *refused syscall*, not a *detected anomaly*.
//! - **Local commands still run:** under the same strict policy a CPU-local
//!   command (`echo`) still produces its output and exits 0 — the jail denies
//!   *egress*, not *execution*.

use std::path::Path;

use cosmon_agent_harness::tool::Tool;
use cosmon_agent_harness::{ExecCommand, ExecResult};
use cosmon_core::egress::{EgressJail, EgressPolicy, EnforcementMode};

/// Drive the real tool and decode its envelope (same shape as
/// `exec_command_egress.rs`).
fn exec(tool: &ExecCommand, work_dir: &Path, cmd: &str) -> ExecResult {
    let args = serde_json::json!({ "command": cmd }).to_string();
    let raw = tool
        .execute(&args, work_dir)
        .expect("exec must return an envelope");
    serde_json::from_str(&raw).expect("result is valid JSON")
}

/// A self-contained outbound-TCP probe that needs no extra binary: bash's
/// `/dev/tcp` redirection. It prints `NET_OK` when the connect succeeds and
/// `NET_BLOCKED` otherwise, and **always exits 0** so the envelope's
/// `exit_code` distinguishes "jail spawned and probe ran" (0) from "the
/// `unshare` wrapper itself failed to spawn" (non-zero — a loud failure, never
/// a silent bypass). `timeout` bounds a hung connect.
const TCP_PROBE: &str = "if timeout 5 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; \
     then echo NET_OK; else echo NET_BLOCKED; fi";

/// The whole suite lives in **one** `#[test]` so the `COSMON_EGRESS_POLICY`
/// env mutation cannot race a sibling test thread (cargo runs tests in a file
/// concurrently). The baseline and the jailed probes are sequential phases of
/// the same proof.
#[test]
fn netns_denies_external_egress_while_local_commands_run() {
    // ---- gate 1: explicit opt-in (only the container harness sets this) ----
    if std::env::var("COSMON_NETNS_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skip: netns e2e is opt-in — set COSMON_NETNS_E2E=1 inside a \
             netns-capable Linux container (see scripts/cs-pilot-netns-egress-test.sh)"
        );
        return;
    }
    // ---- gate 2: the kernel must support an unprivileged net namespace -----
    if !cfg!(target_os = "linux") {
        eprintln!("skip: netns enforcement requires Linux; macOS gets Advisory mode only");
        return;
    }

    // On Linux the host must self-report Netns, else the wrap is advisory and
    // the whole proof is vacuous — fail loudly rather than pass on a no-op.
    assert_eq!(
        EgressJail::enforcement_mode(),
        EnforcementMode::Netns,
        "expected Netns enforcement on Linux; got Advisory — kernel cannot create the namespace"
    );

    let dir = tempfile::tempdir().expect("tempdir");

    // ---- phase 1: baseline — AllowAll reaches the network ------------------
    // Proves the container has egress, so a later block is a real denial and
    // not an artefact of an air-gapped builder. `allow-all` must be set
    // **explicitly**: since the security-review 5008 fix an *unset* policy
    // fail-closes to deny-external, which would block this baseline and make
    // the whole proof vacuous.
    std::env::set_var(EgressPolicy::ENV_VAR, EgressPolicy::AllowAll.token());
    let baseline = exec(&ExecCommand::new(), dir.path(), TCP_PROBE);
    assert_eq!(
        baseline.exit_code, 0,
        "baseline probe must run: {baseline:?}"
    );
    assert!(
        baseline.output.contains("NET_OK"),
        "baseline (AllowAll) must reach the network, else the block test is vacuous: {baseline:?}"
    );

    // ---- phase 2: deny-external blocks the same probe ----------------------
    std::env::set_var(EgressPolicy::ENV_VAR, EgressPolicy::DenyExternal.token());
    let jailed_net = exec(&ExecCommand::new(), dir.path(), TCP_PROBE);
    // exit_code 0 ⇒ the unshare wrapper spawned and the probe ran; a failure
    // to create the namespace would surface as a non-zero envelope (loud).
    assert_eq!(
        jailed_net.exit_code, 0,
        "the netns jail must spawn and run the probe (non-zero ⇒ unshare failed to create the \
         namespace; enable unprivileged user namespaces in the kernel): {jailed_net:?}"
    );
    assert!(
        jailed_net.output.contains("NET_BLOCKED"),
        "deny-external must make external egress unreachable (no route in the netns): {jailed_net:?}"
    );

    // ---- phase 3: local commands still run under the strict policy ---------
    let jailed_local = exec(&ExecCommand::new(), dir.path(), "echo local-still-runs");
    std::env::remove_var(EgressPolicy::ENV_VAR);
    assert_eq!(
        jailed_local.exit_code, 0,
        "a CPU-local command must still run under deny-external: {jailed_local:?}"
    );
    assert!(
        jailed_local.output.contains("local-still-runs"),
        "deny-external denies egress, not execution: {jailed_local:?}"
    );
}
