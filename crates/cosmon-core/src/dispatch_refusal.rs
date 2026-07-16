// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-crate exit-code contract for the **briefless-dispatch guard**.
//!
//! # Why this constant does not live in the CLI
//!
//! The briefless-molecule guard has two seams that must agree on one integer:
//!
//! - **`cs tackle`** ([`cosmon_cli`]'s dispatch guard, task-20260711-919a)
//!   *emits* exit [`BRIEFLESS_DISPATCH`] when it refuses to spawn a worker for
//!   a molecule whose formula declares required, default-free variables that
//!   are now missing or blank — the molecule carries no operator intent.
//! - **`cs run`** (the resident runtime, [`cosmon_runtime`]) *reads* that exit
//!   code. The runtime dispatches by shelling out `cs tackle`; when that
//!   shell-out fails, the runtime must decide whether the failure is
//!   *transient* (retry next tick) or *permanent* (park the molecule). A
//!   briefless refusal is permanent: `cs tackle` will refuse identically on
//!   every retry until an operator restores the brief or collapses the
//!   molecule.
//!
//! Before this contract existed, the runtime treated *every* non-zero
//! `cs tackle` exit as transient and retracted its optimistic dispatch mark,
//! so a briefless molecule was re-emitted **every tick** — a busy-loop that
//! spawned `cs tackle` forever, flooded the trace, and (because every tick
//! "produced decisions") perpetually reset the phantom-running stall gate,
//! starving the reap sweep. The runtime needs the *number* to break that loop,
//! and the number is a contract between two crates — so it lives in the crate
//! both depend on, not in either peer.
//!
//! [`cosmon_cli`]: https://docs.rs/cosmon-cli
//! [`cosmon_runtime`]: https://docs.rs/cosmon-runtime

/// Exit code `cs tackle` returns when it refuses to dispatch a **briefless**
/// molecule (task-20260711-919a — the briefless-molecule guard).
///
/// A molecule is *briefless* when its formula declares one or more
/// effectively-required variables (`required` **and** default-free) that the
/// molecule's current `variables` leave missing or blank. Dispatching such a
/// molecule spawns a worker with an empty Mission section.
///
/// This value sits in the CLI type-tightening guard band (10–16, see
/// `cosmon_cli::cmd::guard::exit_code`); it is exposed here — the shared
/// crate — only because the resident runtime must recognise it to
/// distinguish a *permanent* dispatch refusal from a *transient* one. The CLI
/// aliases its `exit_code::BRIEFLESS_DISPATCH` to this constant so there is a
/// single source of truth.
pub const BRIEFLESS_DISPATCH: i32 = 16;

/// Whether a captured process exit code is the briefless-dispatch refusal.
///
/// `code` is the child's exit status: `Some(n)` for a normal exit, `None`
/// when the process was killed by a signal (which is never a briefless
/// refusal — a signal kill is transient/shutdown, not a guard verdict).
///
/// The resident runtime uses this to park a molecule instead of re-arming the
/// dispatch busy-loop; see the module docs.
#[must_use]
pub fn is_briefless_refusal(code: Option<i32>) -> bool {
    code == Some(BRIEFLESS_DISPATCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn briefless_dispatch_code_is_pinned() {
        // The exact integer is the cross-crate contract: a silent renumbering
        // here would desync the CLI emitter from the runtime reader. If this
        // ever changes, `cosmon_cli::cmd::guard::exit_code::BRIEFLESS_DISPATCH`
        // (which aliases this constant) and its pinning test must change too.
        assert_eq!(BRIEFLESS_DISPATCH, 16);
    }

    #[test]
    fn is_briefless_refusal_matches_only_the_guard_code() {
        assert!(is_briefless_refusal(Some(BRIEFLESS_DISPATCH)));
    }

    #[test]
    fn is_briefless_refusal_rejects_other_outcomes() {
        // Signal kill (None), clean exit (0), a sibling guard code (15), and a
        // generic failure (1) are all *not* briefless refusals — the runtime
        // must keep treating them as transient/other.
        assert!(!is_briefless_refusal(None));
        assert!(!is_briefless_refusal(Some(0)));
        assert!(!is_briefless_refusal(Some(1)));
        assert!(!is_briefless_refusal(Some(15)));
    }
}
