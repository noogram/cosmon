// SPDX-License-Identifier: AGPL-3.0-only

//! The bounded tenant drain, in-process (issue #54 / U6).
//!
//! `POST /v1/molecules/{id}/run` used to shell `cs --json run <root>
//! --max-actions … --max-depth … --max-molecules … --timeout …` through
//! the ADR-080 §3.5 clause (e) subprocess envelope and read the loop's
//! named exit code back from the child. This module is the same drain
//! as a library call: the exact loop `cs run <root>` executes —
//! [`cosmon_runtime::compile_plan`] → the B1/B2 pre-checks →
//! [`cosmon_runtime::DagPolicy`] → [`cosmon_runtime::Runtime`] — with
//! the [`cosmon_runtime::LibraryExecutor`] as the dispatch seam, so no
//! `cs` binary is involved anywhere on the path.
//!
//! # What is bounded, and by whom
//!
//! Unchanged from ADR-124: the client DEMANDS, the server DECIDES. The
//! request carries only the root molecule id; the B1 (depth) / B2
//! (width) / B3 (budget) bounds come from the tenant's sealed binding
//! and land here as [`cosmon_runtime::RunBounds`] plus the pre-loop
//! depth check. The B3 budget is passed unconditionally — a tenant
//! drain is NEVER unbounded (godel Q3).
//!
//! # What the library drain does NOT do (recorded parity gap)
//!
//! `cs run`'s step 9 tore completed molecules down through `cs done` —
//! the sealed harvest transaction. That transaction has no library
//! form yet (its one implementation is `cmd/done.rs`; see ADR-176
//! §11), so the in-process drain dispatches and drains but does **not**
//! integrate: completed molecules keep their branches until the
//! operator (or a future library harvest) lands them. This is the same
//! enumerated follow-up as the `land` route's effect half; the ADR-080
//! §3.5 amendment records it. The termination token is honest about
//! it: a drain that finishes with completed-but-unintegrated molecules
//! reports [`token::DRAINED`] — the *drain* did complete; harvest was
//! never attempted, not refused — while `teardown_failed` remains
//! reserved for an attempted harvest that a sealed `cs done` refused.

use std::path::Path;
use std::time::Duration;

use cosmon_core::id::MoleculeId;
use cosmon_runtime::{
    compile_plan, dag_depth, load_parallel_limits, DagPolicy, LibraryExecutor, RunBounds, Runtime,
    RuntimeConfig, ShutdownReason,
};
use cosmon_state::StateStore;

use crate::nucleon_map::DrainBounds;

/// Stable drain-termination reason tokens published in
/// `drain.terminated` events. Byte-identical to the B1 moussage
/// reject-reason labels — one vocabulary for the bound, whether the
/// client meets it as an HTTP refusal or as an event token.
pub mod token {
    /// The DAG drained: no pending and no running molecule left.
    pub const DRAINED: &str = "drained";
    /// B3 — the binding's action budget ran out (was exit 90).
    pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";
    /// B2 — the fleet grew past the binding's molecule quota (was 91).
    pub const MOLECULE_QUOTA_EXCEEDED: &str = "molecule_quota_exceeded";
    /// B1 — the compiled plan is deeper than the binding allows (was 92).
    pub const MAX_DEPTH_EXCEEDED: &str = "max_depth_exceeded";
    /// The wall-clock deadline fired (was exit 124) — a NAMED exit, I4.
    pub const TIMEOUT: &str = "timeout";
    /// A ready molecule's current formula step is an execution kind the
    /// library executor does not cover (gate / native / query / llm) — the
    /// ADR-080 §3.5.3 parity gap, met mid-drain instead of at admission.
    /// A permanent refusal, reported on the first tick that observes it
    /// (never run to `timeout`); the event body names the molecule and the
    /// step kind. The tackle route's sibling is `501
    /// tackle_unsupported_step`.
    pub const UNSUPPORTED_STEP: &str = "unsupported_step";
    /// Anything else: the loop could not run (store fault, bad root).
    pub const ERROR: &str = "error";
}

/// How a drain ended: the stable wire token, plus the refusal detail when
/// the token alone would hide the cause.
///
/// [`token::UNSUPPORTED_STEP`] is the one token that names a *specific*
/// molecule rather than a property of the whole drain, so it carries the
/// refused molecule id and step kind for the `drain.terminated` event body.
#[derive(Debug, Clone)]
pub struct DrainOutcome {
    /// The stable termination token (see [`token`]).
    pub token: &'static str,
    /// Human-readable refusal detail (molecule id + step kind) when the
    /// token is [`token::UNSUPPORTED_STEP`]; `None` otherwise.
    pub detail: Option<String>,
}

impl DrainOutcome {
    /// A detail-less outcome for the tokens that describe the whole drain.
    #[must_use]
    fn bare(token: &'static str) -> Self {
        Self {
            token,
            detail: None,
        }
    }
}

/// Run the bounded drain over the tenant's own store and return the
/// stable termination token (plus refusal detail) for the
/// `drain.terminated` event.
///
/// Blocking (the runtime loop sleeps between ticks) — the route runs it
/// inside `spawn_blocking`, detached, exactly as it detached the old
/// subprocess. Total by construction: the B3 budget is always finite
/// and `max_runtime` is always set, so every path reaches a token.
#[must_use]
pub fn run_drain<B>(
    tenant_root: &Path,
    root_id: &MoleculeId,
    bounds: &DrainBounds,
    timeout: Duration,
    executor: LibraryExecutor<B>,
) -> DrainOutcome
where
    B: cosmon_core::transport::TransportBackend + 'static,
{
    // One spelling of the tenant paths, shared with the envelope's
    // `COSMON_STATE_DIR` pin ([`crate::worker_env::tenant_state_dir`]) so
    // the drain and the workers it dispatches cannot disagree about which
    // store they read. Deliberately deterministic, not the ambient
    // `cosmon_filestore` resolvers — see that helper's docs for why an
    // env-first resolver is wrong on a multi-tenant path.
    let state_dir = crate::worker_env::tenant_state_dir(tenant_root);
    let store = cosmon_filestore::FileStore::new(&state_dir);

    // Compile the DAG once — the same one-shot walk `cs run` performs.
    let Ok((plan, edges)) = compile_plan(&store, std::slice::from_ref(root_id)) else {
        return DrainOutcome::bare(token::ERROR);
    };
    let mut dag_ids: std::collections::HashSet<MoleculeId> =
        std::collections::HashSet::from([root_id.clone()]);
    for (a, b) in &edges {
        dag_ids.insert(a.clone());
        dag_ids.insert(b.clone());
    }

    // B1 — depth bound: a plan deeper than the binding is REFUSED
    // before the loop starts. Named failure, never a stall (I4).
    let depth = dag_depth(&edges).max(1);
    if depth > usize::try_from(bounds.max_depth).unwrap_or(usize::MAX) {
        return DrainOutcome::bare(token::MAX_DEPTH_EXCEEDED);
    }
    // B2 at compile time — the in-loop tick check covers mid-run growth.
    if dag_ids.len() > usize::try_from(bounds.max_molecules).unwrap_or(usize::MAX) {
        return DrainOutcome::bare(token::MOLECULE_QUOTA_EXCEEDED);
    }

    // ADR-043 parallel limits from every formula the DAG references.
    let formulas_dir = crate::worker_env::tenant_formulas_dir(tenant_root);
    let mut formula_ids: Vec<cosmon_core::id::FormulaId> = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for id in &dag_ids {
            if let Ok(m) = store.load_molecule(id) {
                if seen.insert(m.formula_id.clone()) {
                    formula_ids.push(m.formula_id);
                }
            }
        }
    }
    let limits = load_parallel_limits(&formulas_dir, &formula_ids);

    // Terminal-root pre-seed (phantom-workers fix #2): a drain requested
    // on an already-terminal root continues past it rather than waiting
    // for the root to be re-observed.
    let pre_completed: Vec<MoleculeId> = match store.load_molecule(root_id) {
        Ok(root) if root.status.is_terminal() => vec![root_id.clone()],
        _ => Vec::new(),
    };
    let policy = DagPolicy::new(plan, edges)
        .with_limits(limits)
        .with_pre_completed(pre_completed);

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(500),
        max_runtime: Some(timeout),
        ..RuntimeConfig::default()
    };
    // B3 is obligatory on the tenant path: the budget is passed as-is,
    // never mapped through the CLI's `0 = unbounded` convention.
    let run_bounds = RunBounds {
        max_actions: Some(bounds.budget),
        max_molecules: Some(usize::try_from(bounds.max_molecules).unwrap_or(usize::MAX)),
    };

    let store_box: Box<dyn StateStore> = Box::new(cosmon_filestore::FileStore::new(&state_dir));
    let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(executor), config)
        .with_run_bounds(run_bounds);

    match runtime.run() {
        Ok(report) => DrainOutcome {
            token: exit_token(report.reason),
            // The one reason with a subject: name the refused molecule and
            // step kind in the event body, so the tenant reads the cause
            // instead of a bare token.
            detail: report.refusal.map(|r| format!("{}: {}", r.molecule, r.reason)),
        },
        Err(_) => DrainOutcome::bare(token::ERROR),
    }
}

/// Map the runtime's named shutdown reason onto the stable wire token.
#[must_use]
pub fn exit_token(reason: ShutdownReason) -> &'static str {
    match reason {
        ShutdownReason::PolicyDrained => token::DRAINED,
        ShutdownReason::BudgetExhausted => token::BUDGET_EXHAUSTED,
        ShutdownReason::MoleculeQuotaExceeded => token::MOLECULE_QUOTA_EXCEEDED,
        ShutdownReason::Deadline => token::TIMEOUT,
        ShutdownReason::DispatchRefused => token::UNSUPPORTED_STEP,
        ShutdownReason::SignalTripped => token::ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RppRejectReason;

    /// The B1/B2/B3 drain-termination tokens must be byte-identical to the
    /// reject-reason labels — one vocabulary for the bound, whether the
    /// client meets it as an HTTP refusal or as an event token.
    ///
    /// [`token::UNSUPPORTED_STEP`] is deliberately NOT in this mirror: it is
    /// not a bound but the §3.5.3 parity gap met mid-drain, so its HTTP
    /// sibling is the tackle route's `501 tackle_unsupported_step` (the
    /// route-prefixed spelling), not a B1 reject label. It shares the
    /// `unsupported_step` stem with that label so the two surfaces read as
    /// one condition.
    #[test]
    fn tokens_mirror_reject_labels() {
        assert_eq!(
            token::BUDGET_EXHAUSTED,
            RppRejectReason::DrainBudgetExhausted.label()
        );
        assert_eq!(
            token::MOLECULE_QUOTA_EXCEEDED,
            RppRejectReason::DrainMoleculeQuotaExceeded.label()
        );
        assert_eq!(
            token::MAX_DEPTH_EXCEEDED,
            RppRejectReason::DrainMaxDepthExceeded.label()
        );
        assert_eq!(
            format!("tackle_{}", token::UNSUPPORTED_STEP),
            "tackle_unsupported_step",
            "the drain token and the tackle route's 501 label share the stem"
        );
    }

    #[test]
    fn every_shutdown_reason_has_a_stable_token() {
        assert_eq!(exit_token(ShutdownReason::PolicyDrained), "drained");
        assert_eq!(
            exit_token(ShutdownReason::BudgetExhausted),
            "budget_exhausted"
        );
        assert_eq!(
            exit_token(ShutdownReason::MoleculeQuotaExceeded),
            "molecule_quota_exceeded"
        );
        assert_eq!(exit_token(ShutdownReason::Deadline), "timeout");
        assert_eq!(
            exit_token(ShutdownReason::DispatchRefused),
            "unsupported_step"
        );
        assert_eq!(exit_token(ShutdownReason::SignalTripped), "error");
    }
}
