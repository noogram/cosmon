// SPDX-License-Identifier: AGPL-3.0-only

//! CLI type-tightening guards — make illegal nucleation / dispatch states
//! unrepresentable at the `cs` surface.
//!
//! Three historical incidents motivated this module:
//!
//! - **Orphaned children** — children nucleated without `--blocked-by`,
//!   invisible to `cs deps --transitive`. Fixed by
//!   [`ensure_parent_link_when_required`]: when a formula step sets
//!   `requires_parent_link = true`, any `cs nucleate` dispatched from a
//!   worker under that step must carry an explicit `--blocks` or
//!   `--blocked-by` edge.
//! - **Homogeneous decay** — `cs decay --count N` used to clone one
//!   topic N times; the CLI has no per-product variable flag, so every
//!   product is a byte-identical duplicate of the source. Fixed by
//!   [`ensure_decay_count_is_heterogeneous`].
//! - **convoy-cascade (2026-04-12)** — greedy dispatch resurrected stale
//!   untagged pendings. Fixed by [`warn_if_stale_untagged`] which nags
//!   at `cs tackle` time. This is warn-level; does not refuse.
//!
//! Each refusal carries a distinct non-zero exit code via [`GuardError`]
//! so scripts and external schedulers can branch on the specific rule.

use cosmon_core::id::MoleculeId;
use cosmon_state::MoleculeData;
use thiserror::Error;

/// Distinct exit codes for type-tightening refusals.
///
/// These codes are part of the CLI's public contract. Adding a new guard
/// should reserve a new code rather than reusing one.
pub(crate) mod exit_code {
    /// [`GuardError::MissingParentLink`] — b22c.
    pub const MISSING_PARENT_LINK: i32 = 10;
    /// [`GuardError::DecayHomogeneousCount`] — f4e1.
    pub const DECAY_HOMOGENEOUS_COUNT: i32 = 11;
    /// [`GuardError::DirtyBacklogRuntimeRefusal`] — ADR-048.
    pub const DIRTY_BACKLOG_REFUSAL: i32 = 12;
    /// [`GuardError::BrokerSpawnRefusal`] — Gödel self-reference.
    pub const BROKER_SPAWN_REFUSAL: i32 = 13;
    /// [`GuardError::DepthLimitExceeded`] — Gödel depth guard.
    pub const DEPTH_LIMIT_EXCEEDED: i32 = 14;
    /// [`GuardError::TierDoesNotDescend`] — ordinal stratification
    /// (smithy ADR-0021).
    pub const TIER_NO_DESCENT: i32 = 15;
    /// [`GuardError::BrieflessDispatch`] — briefless-molecule guard
    /// (task-20260711-919a). Aliased to the shared cross-crate contract in
    /// [`cosmon_core::dispatch_refusal::BRIEFLESS_DISPATCH`] so the resident
    /// runtime (which reads this exit code to park a briefless molecule
    /// instead of busy-looping its dispatch, task-20260711-4310) and this
    /// CLI emitter cannot drift.
    pub const BRIEFLESS_DISPATCH: i32 = cosmon_core::dispatch_refusal::BRIEFLESS_DISPATCH;
}

/// Errors raised by the CLI type-tightening guards.
///
/// Each variant maps to a specific exit code (see [`GuardError::exit_code`])
/// so callers can distinguish refusal kinds programmatically. The `Display`
/// impl doubles as the human-readable error line printed by `main`.
#[derive(Debug, Error)]
pub enum GuardError {
    /// b22c guard — parent-link contract violated by a worker nucleation.
    #[error(
        "formula step `{step_id}` requires child molecules to carry an \
         explicit --blocks or --blocked-by edge (parent: {parent_id}). \
         Re-run with --blocks {parent_id} to make the child visible to \
         `cs deps --transitive`, or pass --no-parent for a legitimate \
         orphan nucleation. See delib-20260409-b22c."
    )]
    MissingParentLink {
        /// The parent molecule the worker was executing when it tried to
        /// nucleate a child.
        parent_id: MoleculeId,
        /// ID of the formula step that declared `requires_parent_link`.
        step_id: String,
    },

    /// f4e1 guard — `cs decay --count N>1` has no mechanism to vary
    /// product variables, so every product is a byte-identical clone.
    #[error(
        "cs decay copies source.variables verbatim to every product, so \
         --count {count} would create {count} byte-identical children of \
         {parent}. Use N separate `cs nucleate <formula> --var topic=... \
         --blocks {parent}` invocations to produce heterogeneous \
         decomposition products. See delib-20260409-f4e1."
    )]
    DecayHomogeneousCount {
        /// Source molecule being decayed.
        parent: MoleculeId,
        /// Requested product count.
        count: usize,
    },

    /// Gödel guard — a broker session tried to spawn a child via
    /// `cs tackle`. Brokers orchestrate; they must not recursively
    /// create worker sessions.
    #[error(
        "cs tackle: refusing dispatch — current session is a broker \
         (CB_SESSION_ROLE=broker). Brokers orchestrate molecules but must \
         not spawn child workers. Use `cs run` or dispatch from a \
         non-broker session."
    )]
    BrokerSpawnRefusal,

    /// Gödel guard — spawn depth would exceed the configured maximum.
    #[error(
        "cs tackle: refusing dispatch — spawn depth {depth} would exceed \
         max_depth={max_depth} (Gödel self-reference guard). The spawn \
         chain is too deep; review the orchestration topology or increase \
         [self_reference_guard] max_depth in .cosmon/config.toml."
    )]
    DepthLimitExceeded {
        /// Current depth that triggered the guard.
        depth: u32,
        /// Configured maximum.
        max_depth: u32,
    },

    /// ADR-048 guard — runtime bootstrap refused because the backlog
    /// contains a sedimented set of stale untagged pendings that the
    /// resident DAG walker could resurrect (convoy cascade, 2026-04-12).
    ///
    /// The `Display` impl produces the canonical refusal UX specified in
    /// ADR-048 §5: names the pathology, cites the prior incident, and
    /// lists the three remediations in preferred order.
    #[error(
        "cs tackle: backlog contains {count} pending molecules older than \
         48 h without a temp:* tag ({}). Running the resident \
         runtime would risk resurrecting them (convoy cascade, 2026-04-12).\n\n\
         Fix with:\n  \
         cs nucleate temp-review && cs tackle <id>     # curate the backlog\n  \
         cs tag <mol_id> --add temp:frozen             # tag individually\n  \
         cs tackle <id> --force-runtime                # override (audited)\n\n\
         See docs/adr/048-backlog-sanity-invariant.md.",
        format_sample(sample, *count)
    )]
    DirtyBacklogRuntimeRefusal {
        /// Observed sediment cardinality.
        count: usize,
        /// Up to 5 sediment molecule IDs for operator context.
        sample: Vec<MoleculeId>,
    },

    /// godel ordinal stratification — a worker tried to nucleate a child
    /// whose formula tier does not strictly descend below the parent's.
    ///
    /// The ordinal of a molecule *is* the `Tier` of its formula (T0 leaf,
    /// T1 well-founded nucleator, T2 signature-gated). A worker may
    /// nucleate a *decomposing* formula (tier ≥ 1) only at a strictly
    /// lower tier than its own; creating a *leaf* (tier 0) is always
    /// permitted (well-founded base case). This forbids the unbounded
    /// mission-nests-mission tower (Gödel-2 self-reference). `DecayProduct`
    /// continuations are exempt — see smithy ADR-0021 and the v1 spec.
    #[error(
        "cs nucleate: refusing — ordinal tier does not descend (parent \
         {parent_id} is T{parent_level}; child formula is T{child_level}). \
         A worker may nucleate a decomposing formula (tier >= 1) only at a \
         strictly lower tier. Nucleate a leaf (tier 0) instead, or descend \
         the tier. See smithy ADR-0021 / delib-20260523-a682 (godel \
         ordinal stratification)."
    )]
    TierDoesNotDescend {
        /// Parent molecule whose worker attempted the nucleation.
        parent_id: MoleculeId,
        /// Tier level of the parent's formula (α).
        parent_level: u8,
        /// Tier level of the child formula being nucleated (β).
        child_level: u8,
    },

    /// Briefless-molecule guard (task-20260711-919a) — refusing to dispatch
    /// a molecule whose formula declares required, default-free variables
    /// that are now missing or blank. The molecule carries no operator
    /// intent: `cs tackle` would spawn a worker with an empty Mission
    /// section.
    ///
    /// The observed pathology was empty-topic `task-work` molecules
    /// dispatched by the runtime after a `cs reconcile` cleared
    /// `state.json` variables. The DAG frontier reports such a molecule as
    /// ready; this guard is the dispatch-time corollary of the frontier
    /// stuck-frozen fix (task-20260711-9b86) — ready is necessary, not
    /// sufficient. Recover the brief from `prompt.md` frontmatter and
    /// re-tag/re-nucleate, or collapse the molecule.
    #[error(
        "cs tackle: refusing dispatch — molecule {mol_id} (formula \
         `{formula_id}`) is briefless: required variable(s) [{}] are \
         missing or blank. A worker would spawn with no Mission. Recover \
         the brief from the molecule's prompt.md frontmatter and restore \
         the variable(s), or collapse the molecule. \
         See task-20260711-919a (briefless-molecule guard).",
        missing.join(", ")
    )]
    BrieflessDispatch {
        /// The molecule that would have been dispatched briefless. Stored as
        /// a `String` (not `MoleculeId`) so the variant stays small — the
        /// inline `MoleculeId` buffer would push `GuardError` over clippy's
        /// `result_large_err` threshold and enlarge every `Result<(),
        /// GuardError>` on the hot dispatch path.
        mol_id: String,
        /// The formula whose required variables are unsatisfied.
        formula_id: String,
        /// Sorted names of the missing-or-blank required variables.
        missing: Vec<String>,
    },
}

/// Format the sample list for the refusal message.
///
/// Up to 5 IDs are listed comma-separated; an overflow suffix
/// `"..., +N more"` is appended when the full set exceeds `SAMPLE_LIMIT`.
fn format_sample(sample: &[MoleculeId], total: usize) -> String {
    if sample.is_empty() {
        return "none".to_owned();
    }
    let joined = sample
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    if total > sample.len() {
        format!("{joined}, +{} more", total - sample.len())
    } else {
        joined
    }
}

impl GuardError {
    /// Exit code this refusal should produce when it bubbles to `main`.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            GuardError::MissingParentLink { .. } => exit_code::MISSING_PARENT_LINK,
            GuardError::DecayHomogeneousCount { .. } => exit_code::DECAY_HOMOGENEOUS_COUNT,
            GuardError::DirtyBacklogRuntimeRefusal { .. } => exit_code::DIRTY_BACKLOG_REFUSAL,
            GuardError::BrokerSpawnRefusal => exit_code::BROKER_SPAWN_REFUSAL,
            GuardError::DepthLimitExceeded { .. } => exit_code::DEPTH_LIMIT_EXCEEDED,
            GuardError::TierDoesNotDescend { .. } => exit_code::TIER_NO_DESCENT,
            GuardError::BrieflessDispatch { .. } => exit_code::BRIEFLESS_DISPATCH,
        }
    }
}

/// Run the ADR-048 backlog-sanity guard before a runtime bootstrap.
///
/// - On a clean backlog (or with `force=true`), returns the
///   [`SedimentReport`](cosmon_runtime::SedimentReport) for the caller to
///   log. When `force=true` and the report is dirty, the caller should
///   emit a `runtime_guard_override` audit event.
/// - On a dirty backlog without `force`, returns
///   [`GuardError::DirtyBacklogRuntimeRefusal`], which surfaces exit code
///   [`exit_code::DIRTY_BACKLOG_REFUSAL`] (`12`).
///
/// Store errors propagate as an anyhow error (not a typed [`GuardError`])
/// so the CLI's generic error path handles them — a transient I/O hiccup
/// should not produce a refusal message.
///
/// # Errors
///
/// - [`GuardError::DirtyBacklogRuntimeRefusal`] when the backlog is dirty
///   and `force` is false.
/// - An `anyhow::Error` wrapping the store I/O failure otherwise.
pub fn check_runtime_backlog_or_refuse(
    store: &dyn cosmon_state::StateStore,
    force: bool,
) -> anyhow::Result<cosmon_runtime::SedimentReport> {
    match cosmon_runtime::check_backlog(store, force) {
        Ok(report) => Ok(report),
        Err(cosmon_runtime::BacklogGuardError::DirtyBacklog(report)) => {
            Err(GuardError::DirtyBacklogRuntimeRefusal {
                count: report.count,
                sample: report.sample,
            }
            .into())
        }
        Err(cosmon_runtime::BacklogGuardError::State(e)) => {
            Err(anyhow::anyhow!("backlog-sanity guard: {e}"))
        }
    }
}

/// How old (in hours) a pending molecule must be before [`warn_if_stale_untagged`]
/// emits its stderr nag. Chosen to match the temp-review rhythm without
/// drowning users in noise at normal dispatch rates.
pub const STALE_PENDING_HOURS: i64 = 2;

/// Emit a stderr warning (not a refusal) when the target molecule of
/// `cs tackle` is pending, older than [`STALE_PENDING_HOURS`], and has no
/// `temp:*` tag. Convoy-cascade prophylaxis.
///
/// Returning `()` is intentional: tackle is still allowed to proceed.
/// The warning merely forces the operator to see what they're resurrecting.
pub fn warn_if_stale_untagged(mol: &MoleculeData) {
    if !matches!(
        mol.status,
        cosmon_core::molecule::MoleculeStatus::Pending
            | cosmon_core::molecule::MoleculeStatus::Queued
    ) {
        return;
    }
    let age = chrono::Utc::now() - mol.updated_at;
    if age < chrono::Duration::hours(STALE_PENDING_HOURS) {
        return;
    }
    let has_temp_tag = mol.tags.iter().any(|t| t.key() == "temp");
    if has_temp_tag {
        return;
    }
    let hours = age.num_hours();
    eprintln!(
        "⚠️  cs tackle: molecule {} is pending+untagged for {}h — \
         classify with `cs tag {} --add temp:hot|warm|cold|frozen` \
         or collapse it (convoy-cascade prophylaxis, ADR-temp-curation)",
        mol.id, hours, mol.id
    );
}

/// Refuse to dispatch a briefless molecule (task-20260711-919a).
///
/// A molecule is *briefless* when its formula declares one or more
/// effectively-required variables (`required` and default-free) that the
/// molecule's current `variables` leave missing or blank. Dispatching such a
/// molecule spawns a worker with an empty Mission section — no operator
/// intent to act on.
///
/// This is the dispatch-time half of the guard; the nucleation half lives in
/// [`cosmon_core::nucleate::nucleate`] (which rejects `--var topic=""` at
/// birth). The dispatch half is load-bearing for the *observed* pathology:
/// molecules whose brief was lost **after** nucleation — e.g. a `cs
/// reconcile` that cleared `state.json` variables — which the runtime then
/// dispatched because the DAG frontier reported them ready. Ready is
/// necessary, not sufficient (corollary of the frontier stuck-frozen fix,
/// task-20260711-9b86).
///
/// Leniency mirrors the sibling guards: when the formula could not be loaded
/// (`None`), the check is skipped rather than blocking — we only refuse when
/// we can see enough context to prove the molecule is briefless. A formula
/// with no required-and-default-free variables (e.g. `temp-review`) never
/// trips this guard.
///
/// # Errors
/// Returns [`GuardError::BrieflessDispatch`] listing the missing-or-blank
/// required variable names when the molecule carries no brief.
pub fn refuse_briefless_dispatch(
    mol: &MoleculeData,
    formula: Option<&cosmon_core::formula::Formula>,
) -> Result<(), GuardError> {
    let Some(formula) = formula else {
        return Ok(());
    };
    let missing = formula.missing_required_vars(&mol.variables);
    if missing.is_empty() {
        return Ok(());
    }
    Err(GuardError::BrieflessDispatch {
        mol_id: mol.id.as_str().to_owned(),
        formula_id: mol.formula_id.as_str().to_owned(),
        missing,
    })
}

/// Check the b22c parent-link contract when a worker calls `cs nucleate`.
///
/// Invariants:
/// - The check is a no-op unless `COSMON_PARENT_MOL_ID` (or equivalent
///   resolved parent id) is set.
/// - The parent's current step must carry `requires_parent_link = true`
///   in its formula for the guard to fire.
/// - If the guard fires and neither `blocks` nor `blocked_by` is
///   non-empty, the nucleation is refused.
///
/// The formula is re-parsed from disk instead of inspecting a cached
/// copy so that operators can hot-patch a `.formula.toml` between
/// tackles without a restart.
pub fn ensure_parent_link_when_required(
    parent_id: &MoleculeId,
    parent_mol: &MoleculeData,
    parent_formula: Option<&cosmon_core::formula::Formula>,
    blocks_len: usize,
    blocked_by_len: usize,
) -> Result<(), GuardError> {
    let Some(formula) = parent_formula else {
        return Ok(());
    };
    let Some(step) = formula.steps.get(parent_mol.current_step) else {
        return Ok(());
    };
    if !step.requires_parent_link {
        return Ok(());
    }
    if blocks_len > 0 || blocked_by_len > 0 {
        return Ok(());
    }
    Err(GuardError::MissingParentLink {
        parent_id: parent_id.clone(),
        step_id: step.id.clone(),
    })
}

/// Ordinal stratification — enforce a strictly descending nucleation
/// tier (smithy ADR-0021; cosmon ADR-110 §I4).
///
/// The ordinal of a molecule *is* the [`Tier`] of its formula. A worker
/// executing a parent of tier `α` may nucleate a *decomposing* child
/// (tier `β ≥ 1`) only when `β < α`. Creating a *leaf* (`β = 0`) is always
/// permitted — it is the well-founded base case. The refusal therefore
/// fires exactly when `β ≥ 1 ∧ β ≥ α`, which is the unbounded
/// mission-nests-mission tower (Gödel-2 self-reference).
///
/// This is the *decomposition* contract only. `DecayProduct` continuations
/// (revision → re-review, auto-chaining) are peer-level by construction and
/// are filtered out by the caller before this guard is consulted; they are
/// bounded by the operator frontier (I5) and the flat depth guard
/// ([`refuse_excessive_depth`]), not by the ordinal.
///
/// # Errors
///
/// Returns [`GuardError::TierDoesNotDescend`] when the descent rule is
/// violated.
pub fn ensure_tier_descends(
    parent_id: &MoleculeId,
    parent_tier: &cosmon_core::formula::Tier,
    child_tier: &cosmon_core::formula::Tier,
) -> Result<(), GuardError> {
    let alpha = parent_tier.level();
    let beta = child_tier.level();
    // Leaf creation (β = 0) is the well-founded base case — always allowed.
    // Creating a nucleating child (β ≥ 1) demands strict ordinal descent.
    if beta >= 1 && beta >= alpha {
        return Err(GuardError::TierDoesNotDescend {
            parent_id: parent_id.clone(),
            parent_level: alpha,
            child_level: beta,
        });
    }
    Ok(())
}

/// Emit the ADR-048 `runtime_guard_override` audit event when a dirty-backlog
/// refusal was bypassed by `--force-runtime`.
///
/// No-op when `report.is_dirty()` is false — an override event only makes
/// sense when the guard would otherwise have fired. Failures to write the
/// event are swallowed (the runtime bootstrap must proceed regardless);
/// the event log is best-effort, not transactional.
pub fn emit_runtime_guard_override(
    caller: &str,
    molecule_id: &MoleculeId,
    report: &cosmon_runtime::SedimentReport,
) {
    if !report.is_dirty() {
        return;
    }
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");
    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        cosmon_core::event_v2::EventV2::RuntimeGuardOverride {
            caller: caller.to_owned(),
            molecule_id: molecule_id.clone(),
            sediment_count: report.count,
            threshold: report.threshold,
            sample: report.sample.clone(),
        },
        None,
    );
}

/// Check the f4e1 decay-count contract.
///
/// `cs decay` clones the source's variables to every product, so any
/// `--count > 1` produces byte-identical children. Reject with a
/// specific exit code and point the operator at the N-separate-nucleates
/// workaround.
pub fn ensure_decay_count_is_heterogeneous(
    parent: &MoleculeId,
    count: usize,
) -> Result<(), GuardError> {
    if count > 1 {
        return Err(GuardError::DecayHomogeneousCount {
            parent: parent.clone(),
            count,
        });
    }
    Ok(())
}

/// Gödel guard — refuse `cs tackle` when the calling session is a broker.
///
/// A broker (`CB_SESSION_ROLE=broker`) orchestrates other molecules but
/// must not recursively spawn child workers. This prevents infinite
/// self-reference loops where a broker tackles a molecule that in turn
/// tries to act as a broker.
pub fn refuse_broker_spawn<F>(env_lookup: &F) -> Result<(), GuardError>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(cosmon_cli::tackle_env::SessionRole::Broker) =
        cosmon_cli::tackle_env::resolve_session_role(env_lookup)
    {
        return Err(GuardError::BrokerSpawnRefusal);
    }
    Ok(())
}

/// Gödel guard — refuse `cs tackle` when spawn depth exceeds the limit.
///
/// `CB_DEPTH` tracks how many `cs tackle` layers deep the current
/// session is. When depth >= `max_depth`, dispatch is refused to
/// prevent unbounded recursive spawn chains.
pub fn refuse_excessive_depth<F>(env_lookup: &F, max_depth: u32) -> Result<(), GuardError>
where
    F: Fn(&str) -> Option<String>,
{
    let depth = cosmon_cli::tackle_env::resolve_depth(env_lookup);
    if depth >= max_depth {
        return Err(GuardError::DepthLimitExceeded { depth, max_depth });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cosmon_core::formula::Formula;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::tag::Tag;
    use std::collections::{BTreeSet, HashMap};

    fn sample_mol(status: MoleculeStatus, age_hours: i64, tags: Vec<&str>) -> MoleculeData {
        let mut t = BTreeSet::new();
        for raw in tags {
            t.insert(Tag::new(raw.to_owned()).unwrap());
        }
        MoleculeData {
            id: MoleculeId::new("task-20260414-aaaa").unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::default(),
            assigned_worker: None,
            created_at: Utc::now() - chrono::Duration::hours(age_hours + 1),
            updated_at: Utc::now() - chrono::Duration::hours(age_hours),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: t,
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    fn formula_with_gate(requires: bool) -> Formula {
        let toml = format!(
            r#"
formula = "test-parent-link"
version = 1
description = "test"

[[steps]]
id = "decompose"
title = "Decompose"
description = "Decompose."
requires_parent_link = {requires}

[[steps]]
id = "integrate"
title = "Integrate"
description = "Integrate."
needs = ["decompose"]
"#
        );
        Formula::parse(&toml).unwrap()
    }

    /// Formula that declares `topic` as required (default-free) — the
    /// shape of `task-work` after the briefless-molecule guard landed.
    fn formula_requiring_topic() -> Formula {
        Formula::parse(
            r#"
formula = "task-work"
version = 1
description = "test"

[vars.topic]
description = "The task."
required = true

[[steps]]
id = "implement"
title = "Implement"
description = "Do it."
"#,
        )
        .unwrap()
    }

    #[test]
    fn briefless_guard_fires_on_missing_required_var() {
        // No `topic` variable at all — the post-state-clear pathology.
        let f = formula_requiring_topic();
        let mol = sample_mol(MoleculeStatus::Pending, 0, vec![]);
        let err = refuse_briefless_dispatch(&mol, Some(&f)).unwrap_err();
        match &err {
            GuardError::BrieflessDispatch {
                mol_id,
                formula_id,
                missing,
            } => {
                assert_eq!(mol_id.as_str(), "task-20260414-aaaa");
                assert_eq!(formula_id, "task-work");
                assert_eq!(missing, &vec!["topic".to_string()]);
            }
            other => panic!("expected BrieflessDispatch, got {other:?}"),
        }
        assert_eq!(err.exit_code(), exit_code::BRIEFLESS_DISPATCH);
    }

    #[test]
    fn briefless_guard_fires_on_blank_required_var() {
        // `topic` present but whitespace-only.
        let f = formula_requiring_topic();
        let mut mol = sample_mol(MoleculeStatus::Pending, 0, vec![]);
        mol.variables.insert("topic".to_string(), "   ".to_string());
        let err = refuse_briefless_dispatch(&mol, Some(&f)).unwrap_err();
        assert!(matches!(err, GuardError::BrieflessDispatch { .. }));
    }

    #[test]
    fn briefless_guard_silent_when_brief_present() {
        let f = formula_requiring_topic();
        let mut mol = sample_mol(MoleculeStatus::Pending, 0, vec![]);
        mol.variables
            .insert("topic".to_string(), "fix the parser".to_string());
        refuse_briefless_dispatch(&mol, Some(&f)).unwrap();
    }

    #[test]
    fn briefless_guard_silent_when_formula_missing() {
        // Leniency: an unloadable formula must not block dispatch.
        let mol = sample_mol(MoleculeStatus::Pending, 0, vec![]);
        refuse_briefless_dispatch(&mol, None).unwrap();
    }

    #[test]
    fn briefless_guard_silent_when_no_required_vars() {
        // A formula with no required-and-default-free variable (e.g.
        // temp-review) never trips the guard, even with empty variables.
        let f = formula_with_gate(false); // declares no [vars]
        let mol = sample_mol(MoleculeStatus::Pending, 0, vec![]);
        refuse_briefless_dispatch(&mol, Some(&f)).unwrap();
    }

    #[test]
    fn b22c_guard_fires_when_step_requires_and_no_link_passed() {
        let f = formula_with_gate(true);
        let mol = sample_mol(MoleculeStatus::Running, 0, vec![]);
        let parent = MoleculeId::new("delib-20260409-b22c").unwrap();
        let err = ensure_parent_link_when_required(&parent, &mol, Some(&f), 0, 0).unwrap_err();
        assert!(matches!(err, GuardError::MissingParentLink { .. }));
        assert_eq!(err.exit_code(), exit_code::MISSING_PARENT_LINK);
    }

    #[test]
    fn b22c_guard_silent_when_blocks_passed() {
        let f = formula_with_gate(true);
        let mol = sample_mol(MoleculeStatus::Running, 0, vec![]);
        let parent = MoleculeId::new("delib-20260409-b22c").unwrap();
        ensure_parent_link_when_required(&parent, &mol, Some(&f), 1, 0).unwrap();
        ensure_parent_link_when_required(&parent, &mol, Some(&f), 0, 1).unwrap();
    }

    #[test]
    fn b22c_guard_silent_when_step_does_not_require() {
        let f = formula_with_gate(false);
        let mol = sample_mol(MoleculeStatus::Running, 0, vec![]);
        let parent = MoleculeId::new("delib-20260409-b22c").unwrap();
        ensure_parent_link_when_required(&parent, &mol, Some(&f), 0, 0).unwrap();
    }

    #[test]
    fn b22c_guard_silent_when_formula_missing() {
        let mol = sample_mol(MoleculeStatus::Running, 0, vec![]);
        let parent = MoleculeId::new("delib-20260409-b22c").unwrap();
        ensure_parent_link_when_required(&parent, &mol, None, 0, 0).unwrap();
    }

    #[test]
    fn f4e1_guard_rejects_count_above_one() {
        let source = MoleculeId::new("idea-20260409-f4e1").unwrap();
        let err = ensure_decay_count_is_heterogeneous(&source, 3).unwrap_err();
        assert!(matches!(err, GuardError::DecayHomogeneousCount { .. }));
        assert_eq!(err.exit_code(), exit_code::DECAY_HOMOGENEOUS_COUNT);
    }

    #[test]
    fn f4e1_guard_allows_count_one() {
        let source = MoleculeId::new("idea-20260409-f4e1").unwrap();
        ensure_decay_count_is_heterogeneous(&source, 1).unwrap();
    }

    #[test]
    fn convoy_warn_is_noop_for_running_molecule() {
        // Exercise the function on the non-pending path to ensure no panic.
        let mol = sample_mol(MoleculeStatus::Running, 48, vec![]);
        warn_if_stale_untagged(&mol);
    }

    #[test]
    fn convoy_warn_is_noop_for_tagged_pending() {
        let mol = sample_mol(MoleculeStatus::Pending, 48, vec!["temp:hot"]);
        warn_if_stale_untagged(&mol);
    }

    #[test]
    fn convoy_warn_is_noop_for_fresh_pending() {
        let mol = sample_mol(MoleculeStatus::Pending, 0, vec![]);
        warn_if_stale_untagged(&mol);
    }

    #[test]
    fn convoy_warn_fires_for_stale_untagged_pending() {
        // No good way to capture stderr in a unit test without global hook;
        // just ensure the function runs without panicking on the fire path.
        let mol = sample_mol(MoleculeStatus::Pending, 48, vec![]);
        warn_if_stale_untagged(&mol);
    }

    #[test]
    fn dirty_backlog_refusal_has_exit_code_twelve() {
        let err = GuardError::DirtyBacklogRuntimeRefusal {
            count: 7,
            sample: vec![MoleculeId::new("task-20260414-d001").unwrap()],
        };
        assert_eq!(err.exit_code(), exit_code::DIRTY_BACKLOG_REFUSAL);
        assert_eq!(err.exit_code(), 12);
    }

    #[test]
    fn dirty_backlog_refusal_message_names_the_fix() {
        // The refusal's Display is an operator-facing pedagogy surface
        // (ADR-048 §5). Freeze the three remediations + citation so a
        // future editorial refactor cannot silently drop them.
        let err = GuardError::DirtyBacklogRuntimeRefusal {
            count: 5,
            sample: vec![
                MoleculeId::new("task-20260414-a001").unwrap(),
                MoleculeId::new("task-20260414-a002").unwrap(),
            ],
        };
        let msg = err.to_string();
        assert!(msg.contains("48 h"), "mentions the age threshold");
        assert!(msg.contains("temp:*"), "names the curation tag family");
        assert!(msg.contains("convoy cascade"), "cites the pathology");
        assert!(msg.contains("cs nucleate temp-review"), "remediation 1");
        assert!(msg.contains("cs tag"), "remediation 2");
        assert!(msg.contains("--force-runtime"), "remediation 3");
        assert!(
            msg.contains("048-backlog-sanity-invariant"),
            "cites the ADR"
        );
        assert!(msg.contains("task-20260414-a001"), "includes sample id");
    }

    #[test]
    fn dirty_backlog_refusal_message_handles_sample_overflow() {
        // When `count > sample.len()` the UX must say how many more
        // exist, so the operator knows the sample is partial. Without
        // this, five visible IDs look exhaustive and a larger sediment
        // set is misreported.
        let err = GuardError::DirtyBacklogRuntimeRefusal {
            count: 14,
            sample: (0..5)
                .map(|i| MoleculeId::new(format!("task-20260414-b{i:03}")).unwrap())
                .collect(),
        };
        let msg = err.to_string();
        assert!(msg.contains("+9 more"), "signals overflow: {msg}");
    }

    // -- Gödel self-reference guard tests --

    #[test]
    fn broker_spawn_refused_when_role_is_broker() {
        let err = refuse_broker_spawn(&|k| (k == "CB_SESSION_ROLE").then(|| "broker".to_owned()))
            .unwrap_err();
        assert!(matches!(err, GuardError::BrokerSpawnRefusal));
        assert_eq!(err.exit_code(), exit_code::BROKER_SPAWN_REFUSAL);
        assert_eq!(err.exit_code(), 13);
    }

    #[test]
    fn broker_spawn_allowed_when_role_is_worker() {
        refuse_broker_spawn(&|k| (k == "CB_SESSION_ROLE").then(|| "worker".to_owned())).unwrap();
    }

    #[test]
    fn broker_spawn_allowed_when_role_is_absent() {
        refuse_broker_spawn(&|_| None).unwrap();
    }

    #[test]
    fn depth_guard_refuses_at_limit() {
        let err =
            refuse_excessive_depth(&|k| (k == "CB_DEPTH").then(|| "2".to_owned()), 2).unwrap_err();
        assert!(matches!(
            err,
            GuardError::DepthLimitExceeded {
                depth: 2,
                max_depth: 2
            }
        ));
        assert_eq!(err.exit_code(), exit_code::DEPTH_LIMIT_EXCEEDED);
        assert_eq!(err.exit_code(), 14);
    }

    #[test]
    fn depth_guard_refuses_above_limit() {
        refuse_excessive_depth(&|k| (k == "CB_DEPTH").then(|| "5".to_owned()), 2).unwrap_err();
    }

    #[test]
    fn depth_guard_allows_below_limit() {
        refuse_excessive_depth(&|k| (k == "CB_DEPTH").then(|| "1".to_owned()), 2).unwrap();
    }

    #[test]
    fn depth_guard_allows_root_session() {
        refuse_excessive_depth(&|_| None, 2).unwrap();
    }

    #[test]
    fn broker_refusal_message_mentions_role() {
        let err = GuardError::BrokerSpawnRefusal;
        let msg = err.to_string();
        assert!(msg.contains("CB_SESSION_ROLE=broker"));
        assert!(msg.contains("cs run"));
    }

    #[test]
    fn depth_refusal_message_shows_values() {
        let err = GuardError::DepthLimitExceeded {
            depth: 3,
            max_depth: 2,
        };
        let msg = err.to_string();
        assert!(msg.contains("depth 3"));
        assert!(msg.contains("max_depth=2"));
        assert!(msg.contains("self_reference_guard"));
    }

    // -- godel ordinal stratification (ensure_tier_descends) --
    //
    // These six cases ARE the normative truth table of smithy
    // mission-ordinal-stratification-v1 spec §3.4. T2 tiers are built
    // directly (bypassing `validate_tier`, which refuses T2 until
    // cosmon-sign lands) to exercise the rule's *future* behaviour.

    use cosmon_core::formula::Tier;

    fn parent_id() -> MoleculeId {
        MoleculeId::new("delib-20260523-a682").unwrap()
    }
    fn t1() -> Tier {
        Tier::One {
            measure: "count".to_owned(),
        }
    }
    fn t2() -> Tier {
        Tier::Two {
            signature: std::path::PathBuf::from("sig.toml"),
        }
    }

    #[test]
    fn t1_nucleates_t0_ok() {
        // mission → task: the current, legitimate flow.
        assert!(ensure_tier_descends(&parent_id(), &t1(), &Tier::Zero).is_ok());
    }

    #[test]
    fn t1_nucleates_t1_refused() {
        // The targeted danger: a mission nucleating a same-tier sub-mission.
        let err = ensure_tier_descends(&parent_id(), &t1(), &t1()).unwrap_err();
        assert!(matches!(
            err,
            GuardError::TierDoesNotDescend {
                parent_level: 1,
                child_level: 1,
                ..
            }
        ));
    }

    #[test]
    fn t0_is_leaf_cannot_birth_mission() {
        // A leaf must not be able to spawn a decomposer (anti tower-climb).
        let err = ensure_tier_descends(&parent_id(), &Tier::Zero, &t1()).unwrap_err();
        assert!(matches!(
            err,
            GuardError::TierDoesNotDescend {
                parent_level: 0,
                child_level: 1,
                ..
            }
        ));
    }

    #[test]
    fn t0_nucleates_t0_ok() {
        // Regression guard: the revision → re-review continuation loop
        // (task-work → task-work) must survive the ordinal rule.
        assert!(ensure_tier_descends(&parent_id(), &Tier::Zero, &Tier::Zero).is_ok());
    }

    #[test]
    fn t2_nucleates_t1_ok() {
        // Strict descent across the top of the lattice.
        assert!(ensure_tier_descends(&parent_id(), &t2(), &t1()).is_ok());
    }

    #[test]
    fn t2_nucleates_t2_refused() {
        let err = ensure_tier_descends(&parent_id(), &t2(), &t2()).unwrap_err();
        assert!(matches!(
            err,
            GuardError::TierDoesNotDescend {
                parent_level: 2,
                child_level: 2,
                ..
            }
        ));
    }

    #[test]
    fn tier_no_descent_exit_code_is_15() {
        let err = GuardError::TierDoesNotDescend {
            parent_id: parent_id(),
            parent_level: 1,
            child_level: 1,
        };
        assert_eq!(err.exit_code(), exit_code::TIER_NO_DESCENT);
        assert_eq!(err.exit_code(), 15);
    }

    #[test]
    fn tier_no_descent_message_is_actionable() {
        let err = GuardError::TierDoesNotDescend {
            parent_id: parent_id(),
            parent_level: 1,
            child_level: 1,
        };
        let msg = err.to_string();
        assert!(msg.contains("does not descend"));
        assert!(msg.contains("ADR-0021"));
        assert!(msg.contains("leaf"));
    }
}
