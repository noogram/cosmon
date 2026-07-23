// SPDX-License-Identifier: AGPL-3.0-only

//! Atomic frontier projection — the single-state view of dispatchable molecules.
//!
//! # Why this exists
//!
//! Before this module, the cosmon scheduler decided whether a molecule was
//! safe to dispatch by combining **two separate facts** on every poll tick:
//!
//! 1. **DAG readiness** — all `BlockedBy` predecessors are in a terminal
//!    state (computed from [`crate::MoleculeData::typed_links`] via the
//!    runtime's `Plan`).
//! 2. **Predecessor branch merged** — `cs done` has fast-forwarded or
//!    three-way-merged the predecessor's feature branch back onto `main`
//!    (enforced only by the temporal ordering of the runtime loop:
//!    `on_complete` was called before `next_actions`).
//!
//! Because these two facts were observed independently, the upcoming Phase 1
//! TLA+ scheduler specification had to carry a separate state variable for
//! each — roughly **one third** of the proof obligations were about
//! temporal interleavings between them.
//!
//! This module collapses the two facts into **one atomic filesystem
//! projection**: [`Frontier`]. A molecule appears in the projection's
//! `ready` set if and only if every upstream predecessor is **structurally
//! merged** — its `MoleculeData::merged_at` field is stamped. `cs done`
//! writes the new projection at the one instant both facts are true
//! (post-merge); `cs reconcile` can rebuild it at any time from the
//! authoritative state. The scheduler reads a single file instead of
//! re-deriving a two-phase check.
//!
//! # Invariants
//!
//! - **Pure projection.** [`compute`] is a deterministic function of the
//!   molecule set at the moment of the call. Running it twice in a row on
//!   an unchanged store yields the same [`Frontier`] modulo `computed_at`.
//! - **Idempotent on disk.** [`save`] writes atomically via temp file plus
//!   rename — a partial write never leaves a corrupt projection. Callers
//!   that save the same content twice get byte-identical files (the body
//!   is sorted by molecule id).
//! - **Best-effort read.** [`load`] returns `Ok(None)` for missing or
//!   corrupt files. Consumers must treat the absence of a projection as
//!   "fall back to computing it from the store" — there is no hard
//!   dependency on the file existing.
//! - **Not the source of truth.** The authoritative state is still the
//!   per-molecule JSON under `.cosmon/state/fleets/<fleet>/molecules/`.
//!   `frontier.json` is a projection, deletable and reproducible at any
//!   time by `cs reconcile`.
//!
//! # Layout
//!
//! ```text
//! .cosmon/state/frontier.json
//! ```
//!
//! See ADR-041 (atomic frontier projection).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use cosmon_core::error::CosmonError;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;

use crate::{MoleculeFilter, StateStore};

/// Current on-disk schema version for [`Frontier`]. Bumped on breaking
/// changes so readers can dispatch to the correct parser.
pub const FRONTIER_SCHEMA_VERSION: u32 = 1;

/// Relative path of the projection inside the cosmon state directory.
pub const FRONTIER_FILENAME: &str = "frontier.json";

/// The atomic projection of the dispatchable molecule set.
///
/// Collapses the historical two-phase check (`Plan::ready` + runtime loop
/// ordering) into a single persistent state observable by any reader.
///
/// The serialized form sorts `ready` by molecule id so two invocations of
/// [`compute`] on an unchanged store produce byte-identical output. This
/// makes `frontier.json` diff-friendly under git and lets callers detect
/// "the frontier did not change" by hashing the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Frontier {
    /// Schema version — see [`FRONTIER_SCHEMA_VERSION`].
    pub version: u32,
    /// Wall-clock time the projection was computed.
    pub computed_at: DateTime<Utc>,
    /// Molecule ids that are **dispatchable right now**: pending, every
    /// upstream predecessor merged (via `merged_at`) or collapsed.
    ///
    /// Sorted ascending by id for deterministic serialization.
    pub ready: Vec<MoleculeId>,
}

impl Frontier {
    /// Build an empty projection stamped with `computed_at = now`.
    ///
    /// Used as the starting point for incremental tests; production code
    /// should call [`compute`] on a live store.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: FRONTIER_SCHEMA_VERSION,
            computed_at: Utc::now(),
            ready: Vec::new(),
        }
    }
}

/// Pure reducer: compute the frontier from an already-loaded molecule slice.
///
/// This is the canonical **single-state** reducer: it inspects each
/// molecule's `status` and `merged_at` in the same pass, so there is no
/// two-phase check. Callers
/// that already hold a `crate::FleetSnapshot` should use this function
/// directly to avoid reloading the store. Persistence callers should use
/// [`compute`] which wraps this reducer with a [`StateStore::list_molecules`]
/// read and stamps `computed_at`.
///
/// A molecule is included in [`Frontier::ready`] iff:
///
/// 1. Its status is [`MoleculeStatus::Pending`] — it has never been
///    dispatched (Running, Completed, Collapsed, Queued, or Frozen are
///    all excluded).
/// 2. It has no `assigned_worker` — a dispatched-but-not-yet-running
///    worker would race with the scheduler if it surfaced twice.
/// 3. Every `BlockedBy { source }` upstream predecessor is **cleared**:
///    - `status == Completed` **and** `merged_at.is_some()`
///      (merge-before-dispatch — the dependent's worktree needs the
///      committed output), or
///    - `status == Frozen` **and** `stuck_at.is_none()` (a *delivered*
///      freeze: the predecessor delivered its work and parked for
///      visibility — e.g. a mission-plan mission that decomposed; it owns
///      no branch the children must wait to see merged). A *stuck* freeze
///      (`cs stuck`, `stuck_at.is_some()`) is **not** cleared — it means
///      "not ready, hold dependents", or
///    - `status == Collapsed` (the collapse cascade releases successors),
///      or
///    - the predecessor is **absent from the snapshot** (torn down by
///      `cs done`, which merges before removing — so its work has landed).
/// 4. Every `DecayedFrom` parent (if any) meets the same criterion.
///
/// A missing predecessor clears (rather than blocks) because the only way
/// a once-present blocker leaves the store is `cs done`, which merges
/// first; treating it as unmet permanently orphaned mission-plan children
/// behind a dead `blocked_by` edge.
///
/// The returned `ready` vector is sorted by molecule id for deterministic
/// serialization.
#[must_use]
pub fn compute_from_molecules(molecules: &[crate::MoleculeData]) -> Vec<MoleculeId> {
    let by_id: std::collections::HashMap<&MoleculeId, &crate::MoleculeData> =
        molecules.iter().map(|m| (&m.id, m)).collect();

    let predecessor_cleared = |id: &MoleculeId| -> bool {
        let Some(m) = by_id.get(id) else {
            // A predecessor absent from the snapshot has been torn down by
            // `cs done` (which merges *before* removing) — or it is a
            // dangling link inherited from an already-torn-down ancestor.
            // Either way the work it owed, if any, has already landed on the
            // dependent's branch lineage. Treat it as cleared so dependents
            // are never orphaned behind a dead edge.
            //
            // This is the load-bearing fix for the "dead `blocked_by`"
            // class: a mission-plan child left `--blocked-by` a collapsed /
            // frozen / torn-down parent used to stay PERMANENTLY blocked,
            // freezing the whole fleet DAG and forcing manual edge surgery
            // on each child's `state.json` (task-20260604-6056). Before this
            // change a missing predecessor was treated as "unmet", which is
            // safe only while every blocker is still live — but `cs run`
            // auto-`cs done`s completed blockers mid-DAG, so the invariant
            // "every blocker is live" never held in a chaining fleet.
            return true;
        };
        match m.status {
            // `Collapsed` releases its successors unconditionally (the
            // collapse cascade frees the lateral axis).
            MoleculeStatus::Collapsed => true,
            // `Frozen` is TWO disjoint species that must gate oppositely, so
            // it cannot clear unconditionally — the discriminant is
            // `stuck_at`:
            //
            // - **delivered-freeze** (`stuck_at == None`, set by
            //   `freeze_on_last_step`) has *delivered* its work and is parked
            //   for visibility — the canonical case is a mission-plan mission
            //   that finished decomposing into a child DAG. It produces no
            //   branch the children must wait to see merged onto `main`:
            //   content flows through the DAG-aligned branch lineage
            //   (`cs tackle` branches from the blocker's branch), not through
            //   `main`. Requiring `merged_at` here was the BUG-1 orphan: a
            //   frozen mission is never `cs done`'d, never stamps `merged_at`,
            //   and would gate every child forever. → **release**.
            //
            // - **stuck-freeze** (`stuck_at.is_some()`, set by `cs stuck`)
            //   means "not ready — waiting on an external decision / missing
            //   prerequisite / do-not-execute". Releasing its dependents is
            //   exactly wrong: a blocker that literally says "do not execute"
            //   would fling its successors at the runtime. This is the
            //   convoy-cascade hole (task-20260710-6174: an idea frozen
            //   "NE PAS EXÉCUTER" was read as delivered, dispatching its
            //   children 5f33+700e). → **stay blocked**. When the operator
            //   resolves it (`cs thaw` → Pending, or advances to
            //   Completed+merged) the successors gate correctly again.
            MoleculeStatus::Frozen => m.stuck_at.is_none(),
            // A completed predecessor must additionally have its branch
            // merged (merge-before-dispatch) so the dependent's worktree
            // carries its committed output.
            MoleculeStatus::Completed => m.merged_at.is_some(),
            // ADR-062 — Starved is non-cleared (waiting on external
            // refresh); any future `MoleculeStatus` variant added under
            // `#[non_exhaustive]` defaults to non-cleared until an
            // explicit decision is made.
            _ => false,
        }
    };

    let mut ready: Vec<MoleculeId> = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Pending)
        .filter(|m| m.assigned_worker.is_none())
        .filter(|m| m.blocked_by().iter().all(|p| predecessor_cleared(p)))
        .filter(|m| {
            m.typed_links.iter().all(|link| match link {
                cosmon_core::interaction::MoleculeLink::DecayedFrom { id } => {
                    predecessor_cleared(id)
                }
                _ => true,
            })
        })
        .map(|m| m.id.clone())
        .collect();

    ready.sort();
    ready
}

/// Compute the frontier projection from the authoritative state store.
///
/// Thin wrapper around [`compute_from_molecules`] that reads the molecule
/// set from `store` and stamps a fresh `computed_at`. Callers that want
/// byte-identical `save` output between calls must reuse a single
/// [`Frontier`] value — a second call to [`compute`] picks up a new
/// `computed_at` even when the ready set is unchanged.
///
/// # Errors
///
/// Propagates [`CosmonError::StateStore`] from [`StateStore::list_molecules`].
pub fn compute(store: &dyn StateStore) -> Result<Frontier, CosmonError> {
    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let ready = compute_from_molecules(&molecules);
    Ok(Frontier {
        version: FRONTIER_SCHEMA_VERSION,
        computed_at: Utc::now(),
        ready,
    })
}

/// Resolve the on-disk path of the frontier projection under a state directory.
///
/// The file is placed directly inside `state_dir` (typically
/// `.cosmon/state/`) rather than nested, so `cs reconcile` and `cs done`
/// can find it without walking subdirectories.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FRONTIER_FILENAME)
}

/// Serialize and atomically persist a [`Frontier`] under `state_dir`.
///
/// Uses the write-temp-then-rename pattern so a crash mid-write never
/// leaves a half-flushed projection on disk. Creates `state_dir` if
/// missing. Idempotent: calling [`save`] twice with the same [`Frontier`]
/// content produces the same file byte-for-byte **only if the caller
/// preserves `computed_at` across calls**. Use [`compute`] once per
/// reconcile cycle and reuse the result if you need byte equality.
///
/// # Errors
///
/// Returns [`CosmonError::StateStore`] if the directory cannot be created,
/// the temp file cannot be written, or the rename fails.
pub fn save(state_dir: &Path, frontier: &Frontier) -> Result<(), CosmonError> {
    fs::create_dir_all(state_dir).map_err(|e| CosmonError::StateStore {
        reason: format!("frontier mkdir: {e}"),
    })?;
    let final_path = path(state_dir);
    let tmp_path = final_path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(frontier).map_err(|e| CosmonError::StateStore {
        reason: format!("frontier serialize: {e}"),
    })?;
    {
        let mut f = fs::File::create(&tmp_path).map_err(|e| CosmonError::StateStore {
            reason: format!("frontier create tmp: {e}"),
        })?;
        f.write_all(&body).map_err(|e| CosmonError::StateStore {
            reason: format!("frontier write tmp: {e}"),
        })?;
        f.sync_all().map_err(|e| CosmonError::StateStore {
            reason: format!("frontier fsync tmp: {e}"),
        })?;
    }
    fs::rename(&tmp_path, &final_path).map_err(|e| CosmonError::StateStore {
        reason: format!("frontier rename: {e}"),
    })?;
    Ok(())
}

/// Best-effort load of the persisted projection.
///
/// Returns `Ok(None)` when the file is missing or cannot be parsed — the
/// caller must fall back to [`compute`] in that case, so stale or
/// corrupted projections never block the scheduler. A hard I/O failure
/// still propagates so the operator notices disk problems.
///
/// # Errors
///
/// Returns [`CosmonError::StateStore`] only for non-`NotFound` I/O errors.
pub fn load(state_dir: &Path) -> Result<Option<Frontier>, CosmonError> {
    let p = path(state_dir);
    let body = match fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(CosmonError::StateStore {
                reason: format!("frontier read: {e}"),
            })
        }
    };
    match serde_json::from_slice::<Frontier>(&body) {
        Ok(f) if f.version == FRONTIER_SCHEMA_VERSION => Ok(Some(f)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use tempfile::TempDir;

    use cosmon_core::id::{FleetId, FormulaId, WorkerId};
    use cosmon_core::interaction::MoleculeLink;

    use super::*;
    use crate::MoleculeData;

    #[derive(Default)]
    struct MemStore {
        mols: std::sync::Mutex<HashMap<MoleculeId, MoleculeData>>,
    }

    impl StateStore for MemStore {
        fn load_fleet(&self) -> Result<crate::Fleet, CosmonError> {
            Ok(crate::Fleet::default())
        }
        fn save_fleet(&self, _fleet: &crate::Fleet) -> Result<(), CosmonError> {
            Ok(())
        }
        fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
            self.mols
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| CosmonError::MoleculeNotFound(id.clone()))
        }
        fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError> {
            self.mols.lock().unwrap().insert(id.clone(), data.clone());
            Ok(())
        }
        fn list_molecules(
            &self,
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            Ok(self.mols.lock().unwrap().values().cloned().collect())
        }
    }

    fn mk(id: &str, status: MoleculeStatus, links: Vec<MoleculeLink>) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 1,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: links,
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
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
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        }
    }

    #[test]
    fn empty_store_produces_empty_frontier() {
        let store = MemStore::default();
        let f = compute(&store).unwrap();
        assert!(f.ready.is_empty());
        assert_eq!(f.version, FRONTIER_SCHEMA_VERSION);
    }

    #[test]
    fn single_pending_no_blockers_is_ready() {
        let store = MemStore::default();
        let m = mk("task-20260414-aaaa", MoleculeStatus::Pending, Vec::new());
        store.save_molecule(&m.id, &m).unwrap();

        let f = compute(&store).unwrap();
        assert_eq!(f.ready.len(), 1);
        assert_eq!(f.ready[0].as_str(), "task-20260414-aaaa");
    }

    #[test]
    fn pending_with_unmerged_predecessor_is_not_ready() {
        let store = MemStore::default();
        let a = mk("task-20260414-aaaa", MoleculeStatus::Completed, Vec::new());
        // A is completed but not merged — B must wait.
        store.save_molecule(&a.id, &a).unwrap();

        let b = mk(
            "task-20260414-bbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: a.id.clone(),
            }],
        );
        store.save_molecule(&b.id, &b).unwrap();

        let f = compute(&store).unwrap();
        assert!(f.ready.is_empty(), "B must not dispatch before A merged");
    }

    #[test]
    fn pending_with_merged_predecessor_is_ready() {
        let store = MemStore::default();
        let mut a = mk("task-20260414-aaaa", MoleculeStatus::Completed, Vec::new());
        a.merged_at = Some(Utc::now());
        store.save_molecule(&a.id, &a).unwrap();

        let b = mk(
            "task-20260414-bbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: a.id.clone(),
            }],
        );
        store.save_molecule(&b.id, &b).unwrap();

        let f = compute(&store).unwrap();
        assert_eq!(f.ready.len(), 1);
        assert_eq!(f.ready[0].as_str(), "task-20260414-bbbb");
    }

    #[test]
    fn collapsed_predecessor_releases_successor() {
        let store = MemStore::default();
        let a = mk("task-20260414-aaaa", MoleculeStatus::Collapsed, Vec::new());
        store.save_molecule(&a.id, &a).unwrap();

        let b = mk(
            "task-20260414-bbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: a.id.clone(),
            }],
        );
        store.save_molecule(&b.id, &b).unwrap();

        let f = compute(&store).unwrap();
        assert_eq!(f.ready.len(), 1);
    }

    #[test]
    fn frozen_predecessor_releases_successor_without_merged_at() {
        // BUG 1 (task-20260604-6056): a mission-plan mission freezes itself
        // post-decompose (`freeze_on_last_step`) and is never `cs done`'d, so
        // it never stamps `merged_at`. Its children must still be released —
        // gating a Frozen blocker on `merged_at` orphaned them forever.
        let store = MemStore::default();
        let mission = mk("mission-20260604-aaaa", MoleculeStatus::Frozen, Vec::new());
        assert!(
            mission.merged_at.is_none(),
            "frozen mission has no merge stamp"
        );
        // Pin the species: this is a *delivered* freeze (freeze_on_last_step
        // path), NOT a `cs stuck` freeze. `stuck_at` distinguishes the two —
        // the sibling test `stuck_frozen_predecessor_does_NOT_release_successor`
        // covers the opposite case.
        assert!(
            mission.stuck_at.is_none(),
            "delivered-freeze has no stuck stamp"
        );
        store.save_molecule(&mission.id, &mission).unwrap();

        let child = mk(
            "task-20260604-bbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: mission.id.clone(),
            }],
        );
        store.save_molecule(&child.id, &child).unwrap();

        let f = compute(&store).unwrap();
        assert_eq!(
            f.ready.len(),
            1,
            "child must dispatch behind a frozen (delivered) mission"
        );
        assert_eq!(f.ready[0].as_str(), "task-20260604-bbbb");
    }

    #[test]
    fn stuck_frozen_predecessor_does_not_release_successor() {
        // REGRESSION (task-20260710-6174 / task-20260711-9b86): a molecule
        // frozen via `cs stuck` (topic "[IDÉE — NE PAS EXÉCUTER TEL QUEL]")
        // carries `stuck_at = Some(t)` and has NOT delivered its work. The
        // resident runtime read it as delivered and flung its successors
        // (5f33 + 700e) at the fleet. A blocker that says "do not execute"
        // must HOLD its dependents, not release them. The discriminant is
        // `stuck_at`: delivered-freeze (None) releases, stuck-freeze (Some)
        // stays blocked.
        let store = MemStore::default();
        let mut blocker = mk("task-20260710-6174", MoleculeStatus::Frozen, Vec::new());
        blocker.stuck_at = Some(Utc::now());
        // Distinguishes it structurally from the delivered-freeze species.
        assert!(
            !blocker.freeze_on_last_step,
            "stuck-freeze is not a parked mission"
        );
        store.save_molecule(&blocker.id, &blocker).unwrap();

        let child = mk(
            "task-20260710-5f33",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: blocker.id.clone(),
            }],
        );
        store.save_molecule(&child.id, &child).unwrap();

        let f = compute(&store).unwrap();
        assert!(
            f.ready.is_empty(),
            "successor of a `cs stuck` (stuck_at=Some) blocker must NOT dispatch"
        );
    }

    proptest::proptest! {
        /// Property: a Frozen predecessor releases its successor **iff** it is
        /// a delivered-freeze (`stuck_at == None`). This is the exact bisection
        /// the fix introduces — the two Frozen species gate oppositely and the
        /// only distinguisher is `stuck_at`. All other molecule fields are held
        /// fixed; only the presence/absence of the stuck stamp varies.
        #[test]
        fn frozen_predecessor_release_iff_not_stuck(is_stuck in proptest::bool::ANY) {
            let store = MemStore::default();
            let mut blocker = mk("task-20260710-6174", MoleculeStatus::Frozen, Vec::new());
            blocker.stuck_at = if is_stuck { Some(Utc::now()) } else { None };
            store.save_molecule(&blocker.id, &blocker).unwrap();

            let child = mk(
                "task-20260710-5f33",
                MoleculeStatus::Pending,
                vec![MoleculeLink::BlockedBy {
                    source: blocker.id.clone(),
                }],
            );
            store.save_molecule(&child.id, &child).unwrap();

            let released = !compute(&store).unwrap().ready.is_empty();
            proptest::prop_assert_eq!(released, !is_stuck);
        }
    }

    #[test]
    fn missing_predecessor_releases_successor() {
        // BUG 2 compounding (task-20260604-6056): `cs run` auto-`cs done`s a
        // completed blocker mid-DAG (merge happens before teardown). On the
        // next tick the blocker is gone from the store; its dependent must
        // still chain rather than stay blocked behind a dead `blocked_by`
        // edge to a torn-down molecule.
        let store = MemStore::default();
        // Note: the predecessor `task-20260604-dead` is intentionally NOT
        // saved — it models a blocker already torn down by `cs done`.
        let child = mk(
            "task-20260604-cccc",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260604-dead").unwrap(),
            }],
        );
        store.save_molecule(&child.id, &child).unwrap();

        let f = compute(&store).unwrap();
        assert_eq!(
            f.ready.len(),
            1,
            "child must dispatch when its only blocker has been torn down"
        );
        assert_eq!(f.ready[0].as_str(), "task-20260604-cccc");
    }

    #[test]
    fn running_molecule_is_never_in_frontier() {
        let store = MemStore::default();
        let m = mk("task-20260414-aaaa", MoleculeStatus::Running, Vec::new());
        store.save_molecule(&m.id, &m).unwrap();

        let f = compute(&store).unwrap();
        assert!(f.ready.is_empty());
    }

    #[test]
    fn assigned_worker_excludes_from_frontier() {
        let store = MemStore::default();
        let mut m = mk("task-20260414-aaaa", MoleculeStatus::Pending, Vec::new());
        m.assigned_worker = Some(WorkerId::new("tenant-auditor").unwrap());
        store.save_molecule(&m.id, &m).unwrap();

        let f = compute(&store).unwrap();
        assert!(f.ready.is_empty());
    }

    #[test]
    fn ready_is_sorted_by_id() {
        let store = MemStore::default();
        for id in [
            "task-20260414-cccc",
            "task-20260414-aaaa",
            "task-20260414-bbbb",
        ] {
            let m = mk(id, MoleculeStatus::Pending, Vec::new());
            store.save_molecule(&m.id, &m).unwrap();
        }

        let f = compute(&store).unwrap();
        assert_eq!(
            f.ready
                .iter()
                .map(cosmon_core::id::MoleculeId::as_str)
                .collect::<Vec<_>>(),
            vec![
                "task-20260414-aaaa",
                "task-20260414-bbbb",
                "task-20260414-cccc"
            ]
        );
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let mut f = Frontier::empty();
        f.ready.push(MoleculeId::new("task-20260414-aaaa").unwrap());

        save(&state_dir, &f).unwrap();
        let loaded = load(&state_dir).unwrap().unwrap();
        assert_eq!(loaded, f);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn load_corrupt_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(path(tmp.path()), "{ not valid json").unwrap();
        assert!(load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn load_wrong_version_returns_none() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        let wrong = serde_json::json!({
            "version": 999,
            "computed_at": Utc::now().to_rfc3339(),
            "ready": [],
        });
        fs::write(path(tmp.path()), wrong.to_string()).unwrap();
        assert!(load(tmp.path()).unwrap().is_none());
    }

    /// Property: `reconcile ≡ reconcile ∘ reconcile` — computing the
    /// frontier twice on the same unchanged store yields the same `ready`
    /// set. This is the idempotence requirement from the task briefing.
    #[test]
    fn compute_is_idempotent() {
        let store = MemStore::default();
        let mut a = mk("task-20260414-aaaa", MoleculeStatus::Completed, Vec::new());
        a.merged_at = Some(Utc::now());
        store.save_molecule(&a.id, &a).unwrap();

        let b = mk(
            "task-20260414-bbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: a.id.clone(),
            }],
        );
        store.save_molecule(&b.id, &b).unwrap();

        let c = mk("task-20260414-cccc", MoleculeStatus::Pending, Vec::new());
        store.save_molecule(&c.id, &c).unwrap();

        let f1 = compute(&store).unwrap();
        let f2 = compute(&store).unwrap();
        assert_eq!(f1.ready, f2.ready);
    }

    /// Property: save/load/compute form a stable fixpoint — projecting the
    /// frozen `ready` set from disk matches a fresh in-memory compute.
    #[test]
    fn save_then_recompute_yields_same_ready() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");

        let store = MemStore::default();
        let mut a = mk("task-20260414-aaaa", MoleculeStatus::Completed, Vec::new());
        a.merged_at = Some(Utc::now());
        store.save_molecule(&a.id, &a).unwrap();
        let b = mk(
            "task-20260414-bbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: a.id.clone(),
            }],
        );
        store.save_molecule(&b.id, &b).unwrap();

        let f = compute(&store).unwrap();
        save(&state_dir, &f).unwrap();

        let loaded = load(&state_dir).unwrap().unwrap();
        let recomputed = compute(&store).unwrap();
        assert_eq!(loaded.ready, recomputed.ready);
    }
}
