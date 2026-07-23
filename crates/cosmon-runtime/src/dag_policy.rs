// SPDX-License-Identifier: AGPL-3.0-only

//! `DagPolicy` — the native DAG scheduler policy for the resident runtime.
//!
//! # Role in the architecture
//!
//! [`DagPolicy`] is the first concrete [`Policy`] implementation (ADR-016 §1,
//! ADR-022 Native DAG Scheduler). It treats a set of molecules as nodes in a
//! DAG whose edges are derived from `Blocks` / `BlockedBy` typed links, and
//! it schedules their execution by:
//!
//! 1. Reading a [`FleetSnapshot`] every tick.
//! 2. Noticing which tracked molecules just transitioned to
//!    [`MoleculeStatus::Completed`].
//! 3. For each newly-completed molecule, detecting `DecayProduct` typed links
//!    and splicing the resulting children into the DAG mid-run via
//!    [`cosmon_graph::insert_subgraph`]. This is the ADR-016 §5 / ADR-022
//!    "dynamic DAG" rewiring hook — plans evolve as molecules decay.
//! 4. Pruning completed molecules out of the edge list with
//!    [`cosmon_graph::prune_completed`] so the critical-path computation and
//!    ready-frontier stay small.
//! 5. Emitting [`RuntimeAction::Evolve`] for every currently-ready molecule
//!    whose snapshot status is still `Pending` (the runtime advances the
//!    state machine, not the policy).
//!
//! The policy keeps the critical path sharp: ready molecules sitting on the
//! longest-weighted path are dispatched first (see
//! [`cosmon_graph::critical_path_weighted`]), because their latency directly
//! bounds total run time.
//!
//! # Purity
//!
//! `DagPolicy` obeys the `Policy` purity contract: it reads the snapshot it
//! is handed, updates its own internal book-keeping, and returns actions.
//! It performs no I/O, spawns no threads, and does not touch the store. The
//! runtime is responsible for actually applying the emitted actions.
//!
//! # Compile-time input
//!
//! The [`compile_plan`] helper takes a [`StateStore`] and a set of root
//! molecule IDs, transitively walks their `BlockedBy` links to collect the
//! full dependency closure, and builds a [`Plan<MoleculeId>`] ready to hand
//! to a `DagPolicy`. This is the "how do I bootstrap the policy from live
//! on-disk state?" entry point — the `DagPolicy` itself is pure once built.
//!
//! [`Plan<MoleculeId>`]: cosmon_graph::Plan

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use cosmon_core::error::CosmonError;
use cosmon_core::id::{FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_graph::{
    affinity_order, critical_path_weighted, insert_subgraph, model_switch_count, prune_completed,
    CycleError, Plan,
};

/// Convenience alias for a `(dep, dependent)` edge whose endpoints are
/// [`MoleculeId`]s. Reduces the type-complexity of [`compile_plan`]'s
/// signature and keeps call sites readable.
pub type MoleculeEdge = (MoleculeId, MoleculeId);

/// Compiled output of [`compile_plan`]: a ready-to-use [`Plan`] and the
/// flat edge list it was built from. The edge list is also returned so
/// [`DagPolicy`] can keep its own mutable copy for splicing.
pub type CompiledDag = (Plan<MoleculeId>, Vec<MoleculeEdge>);
use cosmon_state::StateStore;

use crate::{FleetSnapshot, Policy, RuntimeAction};

// ---------------------------------------------------------------------------
// ModelResolver — pre-resolution of the ADR-142 Incarnation model
// ---------------------------------------------------------------------------

/// Resolves a molecule's launch-time model (its ADR-142 `Incarnation` model
/// slot) at frontier-ordering time, for the affinity reorder (ADR-145).
///
/// # Why a resolver is needed at all
///
/// [`affinity_order`](cosmon_graph::affinity_order) clusters the ready
/// frontier by *bound model* — but the frontier is made of **pending**
/// molecules that have not been tackled yet. ADR-142 fixes the Incarnation
/// (adapter · model · effort) **once, at spawn**, so there is *no*
/// `ModelSelected` event to read at ordering time: the model must be
/// **pre-resolved** by re-deriving what `cs tackle` *will* pick. This closure
/// is exactly that pre-resolution — given a molecule's persisted state
/// (`formula_id`, `current_step`), it returns the model the imminent step
/// will run under, or `None` for "no model preference" (an unbound bucket,
/// dispatched last so it never *causes* a swap).
///
/// # Why a closure injected from the CLI
///
/// The pure policy performs no I/O (see the module `# Purity` note). Model
/// pre-resolution needs to read formula files (and, in principle, config),
/// which is the CLI layer's job. `cs run` builds the closure from the DAG's
/// formulas via [`load_step_models`] and injects it with
/// [`DagPolicy::with_affinity`]; the policy stays filesystem-free.
#[derive(Clone)]
pub struct ModelResolver(ResolveFn);

/// The boxed, thread-safe pre-resolution closure wrapped by [`ModelResolver`].
/// Aliased so the `Arc<dyn Fn …>` shape lives in one place.
type ResolveFn = Arc<dyn Fn(&cosmon_state::MoleculeData) -> Option<String> + Send + Sync>;

impl ModelResolver {
    /// Wrap a pre-resolution closure.
    ///
    /// The closure maps a molecule's persisted state to its launch-time
    /// model key, or `None` when the molecule pins no model.
    #[must_use]
    pub fn new(
        f: impl Fn(&cosmon_state::MoleculeData) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(f))
    }

    /// Resolve a molecule's model key.
    fn resolve(&self, mol: &cosmon_state::MoleculeData) -> Option<String> {
        (self.0)(mol)
    }
}

impl std::fmt::Debug for ModelResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The wrapped closure is opaque; name the type without pretending to
        // print an unprintable `dyn Fn`.
        f.write_str("ModelResolver(<closure>)")
    }
}

// ---------------------------------------------------------------------------
// DagPolicy
// ---------------------------------------------------------------------------

/// A [`Policy`] that schedules molecules according to a DAG of `Blocks` /
/// `BlockedBy` typed links, with critical-path prioritization and
/// decay-aware mid-run rewiring.
///
/// # Invariants
///
/// - `edges` is always acyclic. Every mutation goes through
///   [`insert_subgraph`], which refuses to produce a cyclic result.
/// - `completed` is monotone: once an id enters, it never leaves.
/// - `plan` is a [`Plan<MoleculeId>`] whose skip set equals `completed`.
///   When the policy splices new edges in, it rebuilds `plan` so the ready
///   frontier reflects the new topology.
/// - `known_molecules` contains every node the policy tracks — both the
///   original compile-time set and any decay products spliced in later.
///
/// [`Plan<MoleculeId>`]: cosmon_graph::Plan
#[derive(Debug, Clone)]
pub struct DagPolicy {
    /// Current dependency edges `(dep, dependent)`. Acyclic at all times.
    edges: Vec<(MoleculeId, MoleculeId)>,
    /// Pure reducer that tracks ready/running/done frontiers. Rebuilt
    /// whenever `edges` changes (decay splicing or prune).
    plan: Plan<MoleculeId>,
    /// Every molecule id the policy has committed to scheduling. Includes
    /// both compile-time nodes and decay products added mid-run.
    known_molecules: HashSet<MoleculeId>,
    /// Molecules observed in a **terminal** state — `Completed`, `Frozen`,
    /// or `Collapsed`. Acts as the skip-set for rebuilt plans so terminal
    /// predecessors automatically satisfy their dependents.
    ///
    /// A `Collapsed` molecule is deliberately treated the same as a clean
    /// completion here: `blocked-by` releases on **done, not on verdict**
    /// (task-20260706-4d1e). The DAG edge carries one bit — done / not-done;
    /// the verdict (a refuted reproduce, a rejected mission) is content on
    /// disk that the dependent's worker reads. This aligns the policy with
    /// [`cosmon_state::frontier::compute_from_molecules`], which has cleared
    /// `Collapsed` predecessors since task-20260604-6056. (Supersedes the
    /// forward-`Blocks` half of option B in `DIAGNOSIS-mission-collapse.md`;
    /// the lateral `DecayProduct` half is unchanged.)
    completed: HashSet<MoleculeId>,
    /// Evidence string stamped on every `Evolve` action emitted by this
    /// policy. Kept as a field so tests and CLI adapters can override it
    /// without subclassing.
    evidence: String,
    /// Per-`(formula, step_order)` static concurrency caps (ADR-043).
    ///
    /// Populated from `Step::parallel_limit` declarations loaded at
    /// [`compile_plan`] time. Empty map = unbounded (the pre-ADR-043
    /// default). When non-empty, [`Policy::next_actions`] counts Running
    /// molecules per `(formula, step_order)` and caps dispatch so the
    /// declared `max` is never exceeded. `Smart`-mode limits are
    /// intentionally absent from this map (parsed but not enforced today,
    /// per ADR-044).
    limits: HashMap<(FormulaId, usize), u32>,
    /// Set when a splice happens in [`DagPolicy::absorb_terminal`].
    /// The runtime observes this flag after [`Policy::next_actions`] and
    /// reloads the full edge set from the store via [`compile_plan`] — a
    /// splice built purely from the completing molecule's `typed_links`
    /// only carries parent→child edges, so inter-child `BlockedBy` links
    /// between siblings never make it into the policy's edge list. Without
    /// a disk reload the rebuilt plan would mark every child ready
    /// simultaneously, launching N workers in parallel instead of
    /// honoring the DAG.
    needs_recompile: bool,
    /// Model-affinity ordering (ADR-145, `delib-20260705-7288` C3). When
    /// `Some`, each tick reorders the eligible frontier with
    /// [`affinity_order`](cosmon_graph::affinity_order) so molecules bound to
    /// the same model dispatch contiguously and the resident model drains
    /// first — minimizing the ~40 GB VRAM model swaps that kill the local
    /// single-resident-model oracle on latency. `None` (the default) leaves
    /// the dispatch order at pure critical-path priority: cloud dispatch is
    /// byte-identical to the pre-affinity path.
    affinity: Option<ModelResolver>,
    /// The model currently loaded in the oracle's VRAM, tracked across ticks
    /// so [`affinity_order`](cosmon_graph::affinity_order) can drain the
    /// resident bucket first (contract rule 3 — no reload). Updated to the
    /// model of the **last** molecule emitted for dispatch each tick, since
    /// that is what will be resident when the next tick's batch begins.
    /// Meaningful only when `affinity.is_some()`.
    resident: Option<String>,
    /// Cumulative count of model swaps the affinity reorder has **saved**
    /// versus naive critical-path order, summed over every tick. This is the
    /// live [`model_switch_count`](cosmon_graph::model_switch_count) caller:
    /// each tick measures naive vs clustered switch counts and folds the
    /// delta here. Zero when affinity is off; read by tests and surfaced on
    /// stderr when non-zero.
    affinity_switches_saved: usize,
}

impl DagPolicy {
    /// Build a `DagPolicy` from an existing [`Plan`] and its edge list.
    ///
    /// The caller is responsible for producing `plan` from `edges` —
    /// typically via [`compile_plan`] or [`Plan::new`]. The two arguments
    /// must agree: every edge endpoint should appear in the plan and no
    /// node should live in the plan that is not on an edge (the policy
    /// treats the edge list as the authoritative node set for isolated
    /// molecules, since [`Plan`] does not expose its node universe).
    ///
    /// # Initial state
    ///
    /// `completed` is empty, so the first call to [`Policy::next_actions`]
    /// sees the ready frontier defined by `plan` alone.
    #[must_use]
    pub fn new(plan: Plan<MoleculeId>, edges: Vec<(MoleculeId, MoleculeId)>) -> Self {
        let mut known_molecules: HashSet<MoleculeId> = HashSet::new();
        // Include all nodes from edges.
        for (a, b) in &edges {
            known_molecules.insert(a.clone());
            known_molecules.insert(b.clone());
        }
        // Include standalone roots from the plan's ready frontier.
        // Without this, a single-node DAG (mission with no children yet)
        // has 0 edges → 0 known_molecules → absorb_terminal never fires
        // when the mission completes → children never spliced.
        for id in plan.ready() {
            known_molecules.insert(id.clone());
        }
        Self {
            edges,
            plan,
            known_molecules,
            completed: HashSet::new(),
            evidence: "dag-policy: critical-path dispatch".to_owned(),
            limits: HashMap::new(),
            needs_recompile: false,
            affinity: None,
            resident: None,
            affinity_switches_saved: 0,
        }
    }

    /// Enable model-affinity ordering of the ready frontier (ADR-145).
    ///
    /// `resolver` pre-resolves each pending molecule's launch-time model (its
    /// ADR-142 `Incarnation` model slot) — see [`ModelResolver`]. Once
    /// installed, every tick clusters same-model molecules contiguously
    /// *within* the critical-path order (the reorder is a stable partition,
    /// so critical-path priority survives as the intra-bucket tie-break) and
    /// drains the resident model first.
    ///
    /// Off by default; `cs run --affinity` is the only production caller.
    /// This is a **dispatch-order** change only — the reorder is a
    /// permutation, so the DAG semantics and the set of molecules dispatched
    /// are untouched (ADR-145 §Decision).
    #[must_use]
    pub fn with_affinity(mut self, resolver: ModelResolver) -> Self {
        self.affinity = Some(resolver);
        self
    }

    /// Seed the resident model — the model already warm in the oracle's VRAM
    /// at runtime start (`cs run --resident-model <id>`).
    ///
    /// Optional: a cold start (`None`) simply pays one extra load for the
    /// first bucket. A no-op when affinity is disabled. `Some("")` is
    /// normalised to `None` by the caller.
    #[must_use]
    pub fn with_resident_model(mut self, model: Option<String>) -> Self {
        self.resident = model;
        self
    }

    /// Cumulative model swaps saved by the affinity reorder (ADR-145).
    ///
    /// Zero when affinity is disabled or the frontier never presented an
    /// out-of-cluster order. Exposed for observability and tests.
    #[must_use]
    pub fn affinity_switches_saved(&self) -> usize {
        self.affinity_switches_saved
    }

    /// ADR-145 — reorder this tick's admitted eligible batch by model
    /// affinity, minimizing VRAM model swaps on a single-resident-model local
    /// oracle.
    ///
    /// On a single-GPU oracle (`ollama-g5`: 48 GB ≈ one 120 B model in VRAM),
    /// an alternating frontier reloads the model (~40 GB off disk) on every
    /// turn. [`affinity_order`](cosmon_graph::affinity_order) clusters
    /// same-model molecules and drains the resident bucket first. It is a
    /// **permutation** — every molecule the caps admitted is still dispatched;
    /// only the order changes, and critical-path order is preserved as the
    /// intra-bucket tie-break (stable partition).
    ///
    /// `model_of` PRE-RESOLVES each pending molecule's Incarnation model
    /// (ADR-142) through the injected [`ModelResolver`], because no
    /// `ModelSelected` event exists before the molecule is tackled.
    /// [`Self::resident`](Self) is tracked across ticks and updated to the
    /// model of the last molecule dispatched this tick. The saved model swaps
    /// are measured with
    /// [`model_switch_count`](cosmon_graph::model_switch_count) and folded
    /// into [`Self::affinity_switches_saved`].
    ///
    /// When affinity is disabled (the default), the batch is returned
    /// untouched — cloud dispatch is byte-identical to the pre-ADR-145 path.
    fn affinity_reorder(
        &mut self,
        eligible: Vec<MoleculeId>,
        snap_by_id: &HashMap<&MoleculeId, &cosmon_state::MoleculeData>,
    ) -> Vec<MoleculeId> {
        // Clone the Arc so the resolver (and `model_of`) borrows a local, not
        // `self` — leaving `self.resident` / `self.affinity_switches_saved`
        // free to mutate below.
        let Some(resolver) = self.affinity.clone() else {
            return eligible;
        };
        let resident = self.resident.clone();
        // Captures only shared references (`snap_by_id`, `resolver`), so the
        // closure is `Copy` and can be handed by value to each consumer.
        let model_of = |id: &MoleculeId| -> Option<String> {
            snap_by_id.get(id).and_then(|m| resolver.resolve(m))
        };
        let naive = model_switch_count(&eligible, model_of, resident.as_ref());
        let ordered = affinity_order(&eligible, model_of, resident.as_ref());
        let clustered = model_switch_count(&ordered, model_of, resident.as_ref());
        let saved = naive.saturating_sub(clustered);
        self.affinity_switches_saved += saved;
        if saved > 0 {
            eprintln!(
                "ℹ affinity: {} ready molecule(s) reordered — {saved} model swap(s) \
                 saved this tick ({naive} → {clustered})",
                ordered.len(),
            );
        }
        // The model resident after this batch drains is the last dispatched
        // molecule's model; seed the next tick from it. An unbound tail
        // (`None`) leaves the resident model untouched — it ran on whatever
        // was already loaded.
        if let Some(m) = ordered.last().and_then(model_of) {
            self.resident = Some(m);
        }
        ordered
    }

    /// Install per-`(formula, step_order)` static concurrency caps.
    ///
    /// Call after construction to activate ADR-043 enforcement. Keys absent
    /// from the map remain unbounded. A zero-entry map (the default) is a
    /// no-op — the policy dispatches every eligible molecule regardless of
    /// fan-out.
    ///
    /// The map is typically built by walking the formulas referenced in
    /// the DAG and collecting each [`cosmon_core::formula::Step`] whose
    /// `parallel_limit` is [`cosmon_core::formula::ParallelLimit::Static`].
    #[must_use]
    pub fn with_limits(mut self, limits: HashMap<(FormulaId, usize), u32>) -> Self {
        self.limits = limits;
        self
    }

    /// Pre-seed the policy's `completed` skip-set with the given molecules
    /// and rebuild the plan accordingly.
    ///
    /// This is the explicit-operator-override entry point used by
    /// `cs run <terminal-root>`: when an operator types `cs run` on a
    /// molecule that is already `Collapsed` / `Completed` / `Frozen`,
    /// they are explicitly asking the runtime to *continue past it*.
    /// It pre-seeds the named root into the skip-set before the first
    /// tick so its descendants are eligible immediately, rather than
    /// waiting for the root to be re-observed as terminal and absorbed.
    ///
    /// Since task-20260706-4d1e a collapsed root that *is* re-absorbed
    /// releases its forward `Blocks` dependents on its own (blocked-by
    /// releases on done, not on verdict), so this hook is now belt-and-
    /// suspenders for the tick-0 case rather than the sole unblock path.
    ///
    /// Pre-seeding is contained — only the explicitly-named roots are
    /// promoted. Idempotent: pre-seeding the same id twice is a no-op.
    #[must_use]
    pub fn with_pre_completed<I>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = MoleculeId>,
    {
        let mut changed = false;
        for id in ids {
            self.known_molecules.insert(id.clone());
            if self.completed.insert(id.clone()) {
                self.plan.mark_done(&id);
                changed = true;
            }
        }
        if changed {
            self.rebuild_plan();
        }
        self
    }

    /// Override the evidence string recorded on emitted `Evolve` actions.
    ///
    /// Useful for CLI adapters that want a more specific label in the
    /// molecule audit trail.
    #[must_use]
    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = evidence.into();
        self
    }

    /// Return a read-only view of the molecules this policy considers done.
    ///
    /// Exposed for tests and for the CLI adapter in sub-task 3, which will
    /// surface the set to operators via `cs run --status`.
    #[must_use]
    pub fn completed(&self) -> &HashSet<MoleculeId> {
        &self.completed
    }

    /// Return the current edge list. Exposed for diagnostics and tests.
    #[must_use]
    pub fn edges(&self) -> &[(MoleculeId, MoleculeId)] {
        &self.edges
    }

    /// Absorb one newly-**terminal** molecule: mark it done in the plan,
    /// record it in `completed`, and splice its `DecayProduct` and `Blocks`
    /// children into the edge list. Returns `true` if the edge list was
    /// mutated (triggering a plan rebuild after the whole batch is
    /// processed).
    ///
    /// **`Completed`, `Frozen`, and `Collapsed` are handled identically.**
    /// This is the load-bearing decision of task-20260706-4d1e:
    /// **`blocked-by` releases on *done*, not on *verdict*.** A `reproduce`
    /// molecule that concludes "refuted" (bug not reproducible → collapse)
    /// still releases the `fix` molecule that was `blocked-by` it — the
    /// downstream worker reads the "no repro" verdict from disk and decides.
    /// The DAG edge carries one bit (done / not-done); the verdict is
    /// content on the data plane, never a second bit on the control plane.
    ///
    /// This aligns the policy with
    /// [`cosmon_state::frontier::compute_from_molecules`], which already
    /// clears `Collapsed | Frozen` predecessors. Before this change the two
    /// readiness surfaces disagreed: the frontier reducer surfaced the fix
    /// as ready while the DAG plan held it blocked, and since dispatch is
    /// their intersection the fix never ran. (Supersedes the forward-`Blocks`
    /// half of "option B", `DIAGNOSIS-mission-collapse.md`; the lateral
    /// `DecayProduct` drain it introduced is preserved verbatim.)
    fn absorb_terminal(&mut self, mol_id: &MoleculeId, typed_links: &[MoleculeLink]) -> bool {
        self.completed.insert(mol_id.clone());

        let products: Vec<MoleculeId> = typed_links
            .iter()
            .filter_map(|link| match link {
                MoleculeLink::DecayProduct { id } => Some(id.clone()),
                MoleculeLink::Blocks { target } => Some(target.clone()),
                _ => None,
            })
            .collect();

        if products.is_empty() {
            self.plan.mark_done(mol_id);
            return false;
        }

        let new_edges: Vec<(MoleculeId, MoleculeId)> = products
            .iter()
            .map(|p| (mol_id.clone(), p.clone()))
            .collect();

        if let Ok(merged) = insert_subgraph(&self.edges, &new_edges) {
            self.edges = merged;
            for p in &products {
                self.known_molecules.insert(p.clone());
            }
            true
        } else {
            self.plan.mark_done(mol_id);
            false
        }
    }

    /// Rebuild `plan` from `edges` treating `completed` as the skip set.
    ///
    /// Called after decay splicing changes the edge topology. Preserves
    /// determinism: [`Plan::new`] surfaces a sorted initial frontier, and
    /// the `completed` skip-set ensures previously-completed molecules
    /// stay out of scheduling without removing them from the edge list —
    /// which would break the fresh plan's ability to discover spliced
    /// children whose only dependency is the just-completed parent.
    ///
    /// Pruning dead edges is handled by the separate [`DagPolicy::gc`]
    /// entry point, which a CLI adapter or long-running `cs run` loop can
    /// invoke periodically to keep the edge list compact. We deliberately
    /// do not prune inside `rebuild_plan`: `prune_completed` would delete
    /// the `(parent, child)` splice edges on the very tick they were
    /// added, silently orphaning the new children.
    fn rebuild_plan(&mut self) {
        // Rebuild plan with standalone roots. After splicing, some molecules
        // may have no edges in the plan (e.g. a researcher whose only
        // blocker — the mission — is completed and pruned). Without
        // new_with_roots, these become invisible isolated nodes and the
        // plan reports "drained" even though work exists.
        if let Ok(new_plan) = Plan::new_with_roots(
            self.edges.clone(),
            self.completed.clone(),
            self.known_molecules.clone(),
        ) {
            self.plan = new_plan;
        }
    }

    /// Garbage-collect edges whose source and dependent are both completed.
    ///
    /// This is a pure compaction step that shrinks the policy's working
    /// edge list without altering the observable ready frontier. It uses
    /// [`prune_completed`] to drop edges whose source is in `completed`,
    /// then retains only those pruned-away edges whose *other* endpoint
    /// is also completed — preserving any freshly-spliced decay children
    /// whose only dependency is a just-completed parent.
    ///
    /// Intended to be called outside the hot path by an operator or by
    /// the `cs run` loop once per N ticks. The current tests exercise
    /// this explicitly to prove the compaction is observable and safe.
    pub fn gc(&mut self) {
        // Candidate edges to potentially drop (source in completed).
        let (_dropped_src_only, _removed_nodes) = prune_completed(&self.edges, &self.completed);
        // Actually retain edges unless *both* endpoints are completed,
        // which is the only case where the edge is truly dead.
        let filtered: Vec<(MoleculeId, MoleculeId)> = self
            .edges
            .iter()
            .filter(|(src, dst)| !(self.completed.contains(src) && self.completed.contains(dst)))
            .cloned()
            .collect();
        self.edges = filtered;
        self.rebuild_plan();
    }
}

impl Policy for DagPolicy {
    fn next_actions(&mut self, snapshot: &FleetSnapshot) -> Vec<RuntimeAction> {
        // 1. Index the snapshot for O(1) status lookups.
        let snap_by_id: HashMap<&MoleculeId, &cosmon_state::MoleculeData> =
            snapshot.molecules.iter().map(|m| (&m.id, m)).collect();

        // 1b. Self-heal the plan's `running` set against the store. A
        //     molecule the plan believes is running but the store reports as
        //     `Pending` was dispatched and then rolled back — a transient
        //     `cs tackle` failure (`apply_evolve` resets it to Pending) or a
        //     liveness-recheck orphan reset. `Plan::mark_running` is one-way,
        //     so without this the molecule leaks in `running` and `ready()`
        //     never re-surfaces it: the runtime survives the dispatch error
        //     (see lib.rs) but the molecule is never retried. Returning it to
        //     the ready frontier closes that gap — the next eligibility pass
        //     re-dispatches it. (gridgame 2026-05-02 cs-run-policy=dag incident.)
        for mol in &snapshot.molecules {
            if mol.status == MoleculeStatus::Pending {
                self.plan.mark_ready(&mol.id);
            }
        }

        // 2. Identify newly-terminal tracked molecules (stable order so
        //    splicing is deterministic across runs). `Completed`, `Frozen`,
        //    and `Collapsed` are all absorption events and are handled
        //    identically: each splices both its `Blocks` and `DecayProduct`
        //    children and enters the skip-set. `blocked-by` releases on
        //    *done*, not on *verdict* (task-20260706-4d1e) — so a collapsed
        //    blocker unblocks its forward dependents just like a clean
        //    completion, and the lateral `DecayProduct` axis drains too
        //    (the surviving half of `DIAGNOSIS-mission-collapse.md`).
        let mut newly_terminal: Vec<(&MoleculeId, &[MoleculeLink])> = Vec::new();
        let mut seen: Vec<&MoleculeId> = self.known_molecules.iter().collect();
        seen.sort();
        for id in seen {
            if self.completed.contains(id) {
                continue;
            }
            let Some(mol) = snap_by_id.get(id) else {
                continue;
            };
            match mol.status {
                MoleculeStatus::Completed | MoleculeStatus::Frozen | MoleculeStatus::Collapsed => {
                    newly_terminal.push((&mol.id, mol.typed_links.as_slice()));
                }
                _ => {}
            }
        }

        // 3. Absorb each terminal event. Track whether any splice mutated
        //    the edge list — if so, we need a single rebuild at the end
        //    rather than one per splice.
        let mut needs_rebuild = false;
        for (id, links) in newly_terminal {
            let spliced = self.absorb_terminal(&id.clone(), links);
            needs_rebuild |= spliced;
        }

        if needs_rebuild {
            // Ask the runtime to reload edges from disk on this tick —
            // the splice above only knows about parent→child edges from
            // the completing molecule's typed_links, so inter-child
            // BlockedBy edges stored on disk are still invisible to us.
            self.needs_recompile = true;
            self.rebuild_plan();
        }

        // 4. Pick ready molecules and sort by critical-path priority.
        //    Critical path is recomputed every tick — it's O(|edges|) and
        //    the edge list is typically tiny.
        let ready_now: Vec<MoleculeId> = self.plan.ready().iter().cloned().collect();
        let critical: HashSet<MoleculeId> = critical_path_weighted(&self.edges, |_| 1)
            .map_or_else(|_| HashSet::new(), |path| path.into_iter().collect());

        // 5. Collapse the historical two-phase check into a single
        //    atomic-state read (ADR-041). The pure reducer in
        //    [`cosmon_state::frontier::compute_from_molecules`] folds
        //    both conditions into one pass:
        //
        //      (a) cosmon status is `Pending` (the DAG-readiness half), and
        //      (b) every upstream predecessor has `merged_at.is_some()`
        //          or is `Collapsed` (the branch-merged half).
        //
        //    Before this refactor the policy emitted a molecule whose
        //    predecessors were cosmon-Completed but whose branches had
        //    not yet landed on main, trusting the runtime loop to call
        //    `on_complete` (which runs `cs done`) before `next_actions`.
        //    That was a temporal invariant, not a structural one, and
        //    the Phase 1 TLA+ spec had to model it as a separate state
        //    variable. With the frontier reducer the invariant is
        //    structural: if a molecule is in `frontier`, both facts
        //    are true now.
        //
        //    The DAG plan still drives absorption, critical-path
        //    ordering, and splice rebuilds — it is the operational
        //    graph. The frontier reducer is an additional filter that
        //    gates dispatch on the atomic projection. We intersect the
        //    two so a molecule surfaced by the plan but blocked by an
        //    unmerged predecessor stays held.
        let frontier_set: HashSet<MoleculeId> =
            cosmon_state::frontier::compute_from_molecules(&snapshot.molecules)
                .into_iter()
                .collect();
        let mut eligible: Vec<MoleculeId> = ready_now
            .into_iter()
            .filter(|id| frontier_set.contains(id))
            .collect();

        // Sort: critical-path nodes first (deterministic tie-break by id).
        eligible.sort_by(|a, b| {
            let a_crit = critical.contains(a);
            let b_crit = critical.contains(b);
            b_crit.cmp(&a_crit).then_with(|| a.cmp(b))
        });

        // 6. ADR-043: apply per-`(formula, step_order)` concurrency caps.
        //    Seed a running counter from the snapshot, then admit eligibles
        //    in critical-path order, dropping any that would exceed their
        //    declared cap. Molecules dropped this tick stay in the ready
        //    frontier — the next tick re-evaluates as slots free up.
        let eligible: Vec<MoleculeId> = if self.limits.is_empty() {
            eligible
        } else {
            let mut running_per_key: HashMap<(FormulaId, usize), u32> = HashMap::new();
            for mol in &snapshot.molecules {
                if mol.status == MoleculeStatus::Running {
                    let key = (mol.formula_id.clone(), mol.current_step);
                    *running_per_key.entry(key).or_insert(0) += 1;
                }
            }
            let mut admitted = Vec::with_capacity(eligible.len());
            for id in eligible {
                // Phantom-molecule guard: if the id is not in the current
                // snapshot, skip it rather than admitting it. Dispatching
                // a molecule absent from the store would cause
                // `apply_evolve` to return `MoleculeNotFound`, terminating
                // the runtime. The ready frontier will re-surface it on
                // a later tick once the store catches up.
                let Some(mol) = snap_by_id.get(&id) else {
                    continue;
                };
                let key = (mol.formula_id.clone(), mol.current_step);
                let Some(&cap) = self.limits.get(&key) else {
                    admitted.push(id);
                    continue;
                };
                let running_now = *running_per_key.get(&key).unwrap_or(&0);
                if running_now < cap {
                    running_per_key.insert(key, running_now + 1);
                    admitted.push(id);
                }
                // else: drop this tick; the molecule stays in self.plan.ready()
                // and will be re-considered next tick without being marked running.
            }
            admitted
        };

        // 6b. ADR-145 — model-affinity reorder of this tick's admitted batch.
        //     A no-op when affinity is disabled (the default), so cloud
        //     dispatch is byte-identical to the pre-ADR-145 path. See
        //     [`Self::affinity_reorder`].
        let eligible = self.affinity_reorder(eligible, &snap_by_id);

        // 7. Mark each emitted molecule as running in the plan so the next
        //    tick's ready() doesn't surface it again until it completes.
        let mut actions: Vec<RuntimeAction> = Vec::with_capacity(eligible.len());
        for id in eligible {
            self.plan.mark_running(&id);
            actions.push(RuntimeAction::Evolve {
                id,
                evidence: self.evidence.clone(),
            });
        }

        actions
    }

    /// Return `true` if a splice in `Self::absorb_terminal` asked the
    /// runtime to reload the edge set from disk. The runtime clears the
    /// flag by calling [`Self::recompile`].
    fn needs_recompile(&self) -> bool {
        self.needs_recompile
    }

    /// Reload the edge list from the store and rebuild the plan.
    ///
    /// After a decay splice the policy only knows the parent→child edges
    /// carried on the completing molecule's `typed_links`. Inter-child
    /// `BlockedBy` links between siblings exist on disk but have never been
    /// touched by the policy, so the rebuilt plan would incorrectly mark
    /// every sibling ready at once. This method re-walks the store from
    /// `Self::known_molecules` via [`compile_plan`], replaces the edge
    /// list with the fresh closure, and rebuilds the plan with the
    /// preserved `completed` skip-set. The `needs_recompile` flag is
    /// cleared on success.
    ///
    /// # Errors
    ///
    /// Propagates [`CosmonError`] from the store. On failure the policy's
    /// existing state is left untouched so the runtime can retry.
    fn recompile(&mut self, store: &dyn StateStore) -> Result<(), CosmonError> {
        let roots: Vec<MoleculeId> = self.known_molecules.iter().cloned().collect();
        let (_plan, edges) = compile_plan(store, &roots)?;
        for (src, dst) in &edges {
            self.known_molecules.insert(src.clone());
            self.known_molecules.insert(dst.clone());
        }
        self.edges = edges;
        self.rebuild_plan();
        self.needs_recompile = false;
        Ok(())
    }

    /// A molecule is in `DagPolicy` scope iff it is in `Self::known_molecules`
    /// — the compile-time DAG closure plus any decay products spliced in
    /// mid-run. This is what scopes the runtime's `cs done` / drain / liveness
    /// passes to the root's connected component, instead of letting them storm
    /// the whole store (the unscoped-dispatch stall).
    fn tracks_molecule(&self, id: &MoleculeId) -> bool {
        self.known_molecules.contains(id)
    }

    /// ADR-038 Limit 1: periodic scope refresh. Detects pending molecules
    /// whose typed links point into `known_molecules` (either direction:
    /// `BlockedBy { source }` where `source` is tracked, or any other
    /// descendant reachable by BFS) and pulls them into the plan.
    ///
    /// Unlike [`Self::recompile`], this also scans *pending* molecules
    /// that have not yet been discovered by BFS from the roots — a
    /// worker may have nucleated a new child whose link points back to
    /// a tracked parent (`BlockedBy { source = known_mol }`) without the
    /// parent's `typed_links` being updated symmetrically. BFS from the
    /// root would miss such children because it only follows links out
    /// of already-tracked molecules. The sweep closes that gap.
    fn refresh_scope(&mut self, store: &dyn StateStore) -> Result<(), CosmonError> {
        // Scan every pending molecule in the store and seed
        // known_molecules with any whose BlockedBy / DecayProduct links
        // reference a molecule we already track.
        let all = store.list_molecules(&cosmon_state::MoleculeFilter::default())?;
        for mol in &all {
            if !matches!(
                mol.status,
                MoleculeStatus::Pending | MoleculeStatus::Running
            ) {
                continue;
            }
            let references_known = mol.typed_links.iter().any(|link| match link {
                MoleculeLink::BlockedBy { source } => self.known_molecules.contains(source),
                MoleculeLink::DecayProduct { id } => self.known_molecules.contains(id),
                _ => false,
            });
            if references_known {
                self.known_molecules.insert(mol.id.clone());
            }
        }
        // Now re-walk the store from the expanded known_molecules set so
        // compile_plan's BFS picks up the newly-seeded descendants and
        // their siblings.
        self.recompile(store)
    }
}

// ---------------------------------------------------------------------------
// compile_plan — bootstrap helper
// ---------------------------------------------------------------------------

/// Build a [`Plan<MoleculeId>`] + edge list from live store state.
///
/// Starting from `roots`, the helper walks the transitive closure of
/// `Blocks` / `BlockedBy` / `DecayProduct` typed links: every dependency
/// of a root, every dependent of that dependency, and every decay child
/// is loaded from the store until the graph stabilizes. The returned edge
/// list is the set of `(dep, dependent)` pairs derived from the typed
/// links; the returned [`Plan`] is built from that edge list with an
/// empty skip-set.
///
/// # Determinism
///
/// The returned edge list is produced by iterating molecules in insertion
/// order from a [`VecDeque`] BFS, and [`Plan::new`] internally sorts the
/// ready frontier by `Ord`, so the function is deterministic for a given
/// store snapshot.
///
/// # Errors
///
/// Propagates [`CosmonError`] from the underlying store on any load failure,
/// and `CosmonError::InvalidState` if the derived graph contains a cycle
/// (returned as a string-wrapped [`CycleError`] for caller readability).
///
/// [`Plan<MoleculeId>`]: cosmon_graph::Plan
pub fn compile_plan(
    store: &dyn StateStore,
    roots: &[MoleculeId],
) -> Result<CompiledDag, CosmonError> {
    // 1. BFS over the store, collecting every reachable molecule by walking
    //    both directions of the Blocks/BlockedBy relation.
    let mut visited: HashSet<MoleculeId> = HashSet::new();
    let mut queue: VecDeque<MoleculeId> = VecDeque::new();

    // Seed with roots that actually exist; callers may pass ids that turn
    // out not to have been nucleated yet, and we'd rather ignore them than
    // fail the whole compile.
    for root in roots {
        if visited.insert(root.clone()) {
            queue.push_back(root.clone());
        }
    }

    // Note: we intentionally do NOT seed the queue with *all* completed /
    // frozen molecules that carry `Blocks` links. An earlier iteration did
    // so to pull in cross-subgraph blockers, but it over-corrected: every
    // `cs run <root>` would then walk the full project history, dispatching
    // disconnected historical subgraphs. The bidirectional BFS below already
    // captures any reachable completed ancestor: from a child visited via a
    // root's `Blocks` edge, `BlockedBy` walks back into the completed
    // predecessor. What is *not* captured is a completely disconnected
    // subgraph — which is the desired scoping for `cs run <root>`.

    let mut loaded: HashMap<MoleculeId, Vec<MoleculeLink>> = HashMap::new();

    while let Some(id) = queue.pop_front() {
        let data = match store.load_molecule(&id) {
            Ok(d) => d,
            Err(CosmonError::MoleculeNotFound(_)) => {
                // Tolerate missing nodes — they contribute no edges and
                // their absence shouldn't poison the rest of the plan.
                continue;
            }
            Err(e) => return Err(e),
        };

        // Record this molecule's typed links so we can compute edges below.
        loaded.insert(id.clone(), data.typed_links.clone());

        // Walk both directions of the blocks relation so we capture the
        // full connected component of the root in the DAG. `blocked_by`
        // yields upstream dependencies; `blocks` yields downstream ones.
        for dep in data.blocked_by() {
            if visited.insert(dep.clone()) {
                queue.push_back(dep.clone());
            }
        }
        for dependent in data.blocks() {
            if visited.insert(dependent.clone()) {
                queue.push_back(dependent.clone());
            }
        }

        // Walk DecayProduct links so children created during a previous
        // run's glide phase (via `cs decay`) are included on resume.
        for child in data.decay_products() {
            if visited.insert(child.clone()) {
                queue.push_back(child.clone());
            }
        }
    }

    // 2. Materialize edges from the collected links. For each molecule M
    //    with BlockedBy(S), emit (S, M). This direction is unambiguous and
    //    matches the `(dep, dependent)` convention used by `cosmon_graph`.
    //    We do *not* also emit edges from `Blocks` links to avoid duplicates
    //    when both sides of a symmetric pair are present.
    let mut edges: Vec<(MoleculeId, MoleculeId)> = Vec::new();
    // Iterate the loaded map in sorted order so edge insertion is
    // deterministic regardless of HashMap iteration order.
    let mut ids: Vec<&MoleculeId> = loaded.keys().collect();
    ids.sort();
    for id in ids {
        let links = &loaded[id];
        for link in links {
            match link {
                MoleculeLink::BlockedBy { source } => {
                    // Only include edges whose endpoints are both tracked;
                    // a dangling BlockedBy pointing outside the closure is
                    // likely a data error, but it shouldn't panic compile_plan.
                    if loaded.contains_key(source) {
                        edges.push((source.clone(), id.clone()));
                    }
                }
                MoleculeLink::Blocks { target } => {
                    // Emit (id, target) only if the symmetric BlockedBy
                    // side isn't also present — this dedupes the pair.
                    if loaded.contains_key(target) {
                        let already = loaded.get(target).is_some_and(|ls| {
                            ls.iter().any(
                                |l| matches!(l, MoleculeLink::BlockedBy { source } if source == id),
                            )
                        });
                        if !already {
                            edges.push((id.clone(), target.clone()));
                        }
                    }
                }
                MoleculeLink::DecayProduct { id: child }
                    // Decay products are children of this molecule — emit
                    // (parent, child) so the child waits for the parent in
                    // the DAG. Only if both endpoints are in the closure.
                    if loaded.contains_key(child) => {
                        edges.push((id.clone(), child.clone()));
                    }
                _ => {}
            }
        }
    }

    // 3. Build the plan. Inject root nodes as standalone seeds so that a
    //    single-node DAG (e.g. a mission with no children yet) appears in
    //    the ready frontier. Without this, Plan::new with 0 edges has 0
    //    nodes and ready() returns empty — the runtime exits immediately.
    let mut standalone_roots: HashSet<MoleculeId> = HashSet::new();
    for root in roots {
        if loaded.contains_key(root) {
            standalone_roots.insert(root.clone());
        }
    }
    let plan = Plan::new_with_roots(edges.clone(), HashSet::new(), standalone_roots).map_err(
        |CycleError(node)| CosmonError::StateStore {
            reason: format!(
                "dependency cycle in compiled DAG at molecule {}",
                node.as_str()
            ),
        },
    )?;

    Ok((plan, edges))
}

/// Depth of a compiled DAG — the number of molecules on the longest
/// dependency chain (a single molecule with no edges has depth 1; an
/// empty edge set with no molecules has depth 0).
///
/// B1 of the moussage bounds: the depth
/// is a *compile-time* property of the plan, so the caller checks it
/// against the binding's `max_depth` BEFORE starting the loop — a plan
/// too deep is refused with a named error, never started. Mid-run
/// growth (`DecayProduct` children) is bounded by B2/B3 inside the loop;
/// B1 exists for the diagnosability of the refusal, not for
/// termination.
///
/// `edges` are `(dependency, dependent)` pairs as returned by
/// [`compile_plan`], which has already rejected cycles — the memoized
/// DFS below therefore terminates. A defensive in-stack guard still
/// breaks would-be cycles (returns the partial depth) so a corrupted
/// edge list cannot hang the caller.
#[must_use]
pub fn dag_depth(edges: &[(MoleculeId, MoleculeId)]) -> usize {
    use std::collections::{HashMap, HashSet};

    fn depth_of<'a>(
        node: &'a MoleculeId,
        children: &HashMap<&'a MoleculeId, Vec<&'a MoleculeId>>,
        memo: &mut HashMap<&'a MoleculeId, usize>,
        in_stack: &mut HashSet<&'a MoleculeId>,
    ) -> usize {
        if let Some(&d) = memo.get(node) {
            return d;
        }
        if !in_stack.insert(node) {
            // Defensive: compile_plan already rejects cycles.
            return 1;
        }
        let best_child = children.get(node).map_or(0, |kids| {
            kids.iter()
                .map(|k| depth_of(k, children, memo, in_stack))
                .max()
                .unwrap_or(0)
        });
        in_stack.remove(node);
        let d = 1 + best_child;
        memo.insert(node, d);
        d
    }

    let mut children: HashMap<&MoleculeId, Vec<&MoleculeId>> = HashMap::new();
    let mut nodes: HashSet<&MoleculeId> = HashSet::new();
    for (dep, dependent) in edges {
        children.entry(dep).or_default().push(dependent);
        nodes.insert(dep);
        nodes.insert(dependent);
    }

    let mut memo = HashMap::new();
    let mut in_stack = HashSet::new();
    nodes
        .iter()
        .map(|n| depth_of(n, &children, &mut memo, &mut in_stack))
        .max()
        .unwrap_or(0)
}

/// Load ADR-043 parallel-limit declarations from `.formula.toml` files.
///
/// Scans `formulas_dir` for each unique formula id in `formula_ids`,
/// parses the formula, and collects every step that declares
/// [`cosmon_core::formula::ParallelLimit::Static`] into a
/// `(formula, step_order) → max` map suitable for
/// [`DagPolicy::with_limits`]. `Smart`-mode limits are deliberately
/// skipped — they are parsed but not enforced today (see ADR-044).
///
/// Formulas that cannot be read or parsed are silently skipped: a missing
/// formula file shouldn't poison the whole map. The caller is expected to
/// have already validated the formula set via [`compile_plan`].
///
/// # Errors
///
/// This function never fails — I/O or parse errors are swallowed per the
/// above rationale. Return type is `HashMap` (not `Result<_>`) for the
/// same reason.
#[must_use]
pub fn load_parallel_limits(
    formulas_dir: &std::path::Path,
    formula_ids: &[FormulaId],
) -> HashMap<(FormulaId, usize), u32> {
    use cosmon_core::formula::{Formula, ParallelLimit};

    let mut out: HashMap<(FormulaId, usize), u32> = HashMap::new();
    let mut seen: HashSet<&FormulaId> = HashSet::new();
    for fid in formula_ids {
        if !seen.insert(fid) {
            continue;
        }
        let path = formulas_dir.join(format!("{}.formula.toml", fid.as_str()));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(formula) = Formula::parse(&text) else {
            continue;
        };
        for step in &formula.steps {
            if let Some(ParallelLimit::Static { max }) = step.parallel_limit {
                out.insert((fid.clone(), step.order), max);
            }
        }
    }
    out
}

/// Build the `(formula, step_order) → model-pin` map that backs the runtime's
/// affinity reorder (ADR-145). Mirrors [`load_parallel_limits`]: read each
/// referenced formula once, record every step that carries a
/// `model = "<id>"` pin. A step with no pin is absent from the map — its
/// molecule pre-resolves to `None` (unbound) and is dispatched last.
///
/// This is the **pre-resolution of the ADR-142 Incarnation model** at
/// frontier-ordering time. Only the formula-step pin is consulted, and that
/// is deliberate: it is the sole *per-molecule* model source. A tiered model
/// is reachable only from `cs tackle --model` or a formula-step `model =`
/// pin (ADR-142; strong is never inherited from a config/env default —
/// tackle's C4 safe-default guard). `--model` is a tackle-time human input
/// that does not exist for a still-pending frontier molecule, so it is out
/// of scope here by construction; config/env defaults are galaxy-global and
/// collapse every molecule into one bucket, where affinity is a no-op. The
/// formula-step pin is therefore the *only* source that produces the
/// per-molecule model variation affinity ordering exists to exploit.
///
/// Keyed by `step.order` to match [`load_parallel_limits`] and the
/// `(formula_id, current_step)` lookup the dispatch caps use — the same
/// order↔index correspondence the ADR-043 path already relies on.
#[must_use]
pub fn load_step_models(
    formulas_dir: &std::path::Path,
    formula_ids: &[FormulaId],
) -> HashMap<(FormulaId, usize), String> {
    use cosmon_core::formula::Formula;

    let mut out: HashMap<(FormulaId, usize), String> = HashMap::new();
    let mut seen: HashSet<&FormulaId> = HashSet::new();
    for fid in formula_ids {
        if !seen.insert(fid) {
            continue;
        }
        let path = formulas_dir.join(format!("{}.formula.toml", fid.as_str()));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(formula) = Formula::parse(&text) else {
            continue;
        };
        for step in &formula.steps {
            if let Some(model) = step.model.as_deref().filter(|s| !s.is_empty()) {
                out.insert((fid.clone(), step.order), model.to_owned());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_state::MoleculeData;

    fn mol_id(raw: &str) -> MoleculeId {
        MoleculeId::new(raw).expect("test molecule id")
    }

    fn make_mol(id: &MoleculeId, status: MoleculeStatus, links: Vec<MoleculeLink>) -> MoleculeData {
        MoleculeData {
            id: id.clone(),
            fleet_id: FleetId::new("default").expect("fleet id"),
            formula_id: FormulaId::new("task-work").expect("formula id"),
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
            tags: std::collections::BTreeSet::new(),
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

    fn snapshot(molecules: Vec<MoleculeData>) -> FleetSnapshot {
        FleetSnapshot { molecules }
    }

    fn evolve_ids(actions: &[RuntimeAction]) -> Vec<MoleculeId> {
        actions
            .iter()
            .filter_map(|a| match a {
                RuntimeAction::Evolve { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    // -- test (a): trivial linear chain A → B → C --

    #[test]
    fn test_dag_policy_linear_chain_dispatches_in_order() {
        let a = mol_id("task-20260410-aaaa");
        let b = mol_id("task-20260410-bbbb");
        let c = mol_id("task-20260410-cccc");

        let edges = vec![(a.clone(), b.clone()), (b.clone(), c.clone())];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        // Initial: A is ready, nothing else.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Pending, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert_eq!(
            evolve_ids(&actions),
            vec![a.clone()],
            "first tick should dispatch only root A"
        );

        // A completes → B is ready.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert_eq!(
            evolve_ids(&actions),
            vec![b.clone()],
            "A done should unlock B"
        );
        assert!(policy.completed().contains(&a));

        // B completes → C is ready.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Completed, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert_eq!(
            evolve_ids(&actions),
            vec![c.clone()],
            "B done should unlock C"
        );

        // C completes → nothing more to do.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Completed, Vec::new()),
            make_mol(&c, MoleculeStatus::Completed, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert!(
            actions.is_empty(),
            "fully drained plan should emit no actions, got {actions:?}"
        );
    }

    // -- test (b): diamond A → B, A → C, B → D, C → D --

    #[test]
    fn test_dag_policy_diamond_parallelizes_after_root() {
        let a = mol_id("task-20260410-aaa1");
        let b = mol_id("task-20260410-bbb1");
        let c = mol_id("task-20260410-ccc1");
        let d = mol_id("task-20260410-ddd1");

        let edges = vec![
            (a.clone(), b.clone()),
            (a.clone(), c.clone()),
            (b.clone(), d.clone()),
            (c.clone(), d.clone()),
        ];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        // Tick 1: only A is ready.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Pending, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert_eq!(evolve_ids(&actions), vec![a.clone()]);

        // Tick 2: A completes → both B and C are ready in parallel.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        let ids = evolve_ids(&actions);
        assert_eq!(
            ids.len(),
            2,
            "diamond should fan out to 2 parallel actions, got {ids:?}"
        );
        assert!(ids.contains(&b), "expected B in parallel fan-out");
        assert!(ids.contains(&c), "expected C in parallel fan-out");

        // Tick 3: B completes but C still running — no new ready.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Completed, Vec::new()),
            make_mol(&c, MoleculeStatus::Running, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert!(
            evolve_ids(&actions).is_empty(),
            "D must wait for C before becoming ready, got {actions:?}"
        );

        // Tick 4: C completes → D becomes ready.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Completed, Vec::new()),
            make_mol(&c, MoleculeStatus::Completed, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert_eq!(evolve_ids(&actions), vec![d.clone()]);
    }

    // -- ADR-145: model-affinity ordering of the ready frontier --

    /// Build a root→{b,c,d} fan-out DAG, dispatch the root, then return the
    /// policy positioned so the next `next_actions` call sees b, c, d ready
    /// in one batch. Shared setup for the affinity tests below.
    fn fanout_ready_after_root(
        policy_tweak: impl FnOnce(DagPolicy) -> DagPolicy,
    ) -> (DagPolicy, MoleculeId, MoleculeId, MoleculeId, FleetSnapshot) {
        let root = mol_id("task-20260410-0000");
        let b = mol_id("task-20260410-1111");
        let c = mol_id("task-20260410-2222");
        let d = mol_id("task-20260410-3333");

        let edges = vec![
            (root.clone(), b.clone()),
            (root.clone(), c.clone()),
            (root.clone(), d.clone()),
        ];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let policy = policy_tweak(DagPolicy::new(plan, edges));
        let mut policy = policy;

        // Tick 1: only the root is ready.
        let snap = snapshot(vec![
            make_mol(&root, MoleculeStatus::Pending, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
        ]);
        assert_eq!(evolve_ids(&policy.next_actions(&snap)), vec![root.clone()]);

        // Root completes → b, c, d fan out together next tick.
        let batch = snapshot(vec![
            make_mol(&root, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
        ]);
        (policy, b, c, d, batch)
    }

    /// A resolver keyed on molecule id (terse for unit tests): b, d → "qwen",
    /// c → "gptoss". "gptoss" < "qwen" by `Ord`, so a cold affinity order
    /// puts the gptoss bucket first.
    fn two_model_resolver() -> ModelResolver {
        ModelResolver::new(|mol: &MoleculeData| match mol.id.as_str() {
            "task-20260410-2222" => Some("gptoss".to_owned()),
            "task-20260410-1111" | "task-20260410-3333" => Some("qwen".to_owned()),
            _ => None,
        })
    }

    #[test]
    fn affinity_off_by_default_keeps_critical_path_order() {
        // No `with_affinity`: the fan-out batch stays in critical-path /
        // id-sorted order — byte-identical to the pre-ADR-145 path.
        let (mut policy, b, c, d, batch) = fanout_ready_after_root(|p| p);
        let ids = evolve_ids(&policy.next_actions(&batch));
        assert_eq!(
            ids,
            vec![b, c, d],
            "affinity disabled → deterministic id-sorted fan-out"
        );
        assert_eq!(policy.affinity_switches_saved(), 0);
    }

    #[test]
    fn affinity_clusters_same_model_contiguously() {
        // With affinity on and a cold start, the eligible batch [b, c, d]
        // (qwen, gptoss, qwen) is reordered to cluster models: gptoss bucket
        // first (Ord), then the qwen bucket, preserving intra-bucket order.
        let (mut policy, b, c, d, batch) =
            fanout_ready_after_root(|p| p.with_affinity(two_model_resolver()));
        let ids = evolve_ids(&policy.next_actions(&batch));
        assert_eq!(
            ids,
            vec![c, b, d],
            "gptoss (c) drains before the qwen bucket (b, d)"
        );
        // Naive [qwen, gptoss, qwen] cold = 3 loads; clustered = 2. Saved 1.
        assert_eq!(policy.affinity_switches_saved(), 1);
    }

    #[test]
    fn affinity_drains_resident_model_first() {
        // Warm on "qwen": its bucket (b, d) must drain first with no reload,
        // ahead of gptoss (c), despite gptoss sorting first by Ord.
        let (mut policy, b, c, d, batch) = fanout_ready_after_root(|p| {
            p.with_affinity(two_model_resolver())
                .with_resident_model(Some("qwen".to_owned()))
        });
        let ids = evolve_ids(&policy.next_actions(&batch));
        assert_eq!(
            ids,
            vec![b, d, c],
            "resident qwen bucket drains first, then gptoss"
        );
        // Warm on qwen: naive [qwen(no-switch), gptoss(switch), qwen(switch)]
        // = 2; clustered [qwen, qwen, gptoss] = 1. Saved 1.
        assert_eq!(policy.affinity_switches_saved(), 1);
    }

    #[test]
    fn affinity_permutation_never_drops_a_molecule() {
        // The load-bearing invariant: the reorder is a permutation — every
        // molecule the frontier admitted is still dispatched exactly once.
        let (mut policy, b, c, d, batch) =
            fanout_ready_after_root(|p| p.with_affinity(two_model_resolver()));
        let mut ids = evolve_ids(&policy.next_actions(&batch));
        ids.sort_unstable();
        let mut expect = vec![b, c, d];
        expect.sort_unstable();
        assert_eq!(ids, expect, "reorder must not drop or duplicate a molecule");
    }

    #[test]
    fn load_step_models_reads_the_model_pin() {
        // A formula with a per-step model pin is pre-resolved by
        // `load_step_models` into the `(formula, step_order) → model` map.
        let dir = std::env::temp_dir().join(format!("cosmon-affinity-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let toml = r#"
formula = "pinned"
description = "two steps, one pinned"

[[steps]]
id = "think"
title = "Think"
description = "think"
model = "gptoss:120b"

[[steps]]
id = "write"
title = "Write"
description = "write"
"#;
        std::fs::write(dir.join("pinned.formula.toml"), toml).expect("write");
        let fid = FormulaId::new("pinned").expect("fid");
        let map = load_step_models(&dir, std::slice::from_ref(&fid));
        assert_eq!(
            map.get(&(fid.clone(), 0)).map(String::as_str),
            Some("gptoss:120b"),
            "step 0 carries the model pin"
        );
        assert!(
            !map.contains_key(&(fid, 1)),
            "step 1 has no pin → absent (unbound bucket)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- test (c): mid-run decay splicing --

    #[test]
    fn test_dag_policy_decay_splices_children_into_plan() {
        // Start with a seed chain M1 → M_sink so Plan has a well-formed
        // initial edge list. M1 decays into M2 + M3 mid-run; DagPolicy
        // should pick up the DecayProduct links, splice (M1, M2) and
        // (M1, M3) into the edge list, rebuild the plan with M1 in the
        // skip set, and emit Evolve for M2, M3, and M_sink in parallel.
        let m1 = mol_id("task-20260410-m001");
        let m2 = mol_id("task-20260410-m002");
        let m3 = mol_id("task-20260410-m003");
        let sink = mol_id("task-20260410-snk1");

        let edges = vec![(m1.clone(), sink.clone())];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        // Tick 1: only M1 is ready (M2 and M3 don't exist yet).
        let snap = snapshot(vec![
            make_mol(&m1, MoleculeStatus::Pending, Vec::new()),
            make_mol(&sink, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        assert_eq!(evolve_ids(&actions), vec![m1.clone()]);

        // Tick 2: M1 completes with decay_product links to M2 and M3.
        // The children M2 and M3 exist in the snapshot as Pending.
        let m1_completed = make_mol(
            &m1,
            MoleculeStatus::Completed,
            vec![
                MoleculeLink::DecayProduct { id: m2.clone() },
                MoleculeLink::DecayProduct { id: m3.clone() },
            ],
        );
        let snap = snapshot(vec![
            m1_completed,
            make_mol(&m2, MoleculeStatus::Pending, Vec::new()),
            make_mol(&m3, MoleculeStatus::Pending, Vec::new()),
            make_mol(&sink, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        let ids = evolve_ids(&actions);

        // After splice + rebuild, the ready frontier is {M2, M3, sink}:
        // M2 and M3 are new nodes whose only dep (M1) is in the skip set,
        // and sink was already a direct descendant of M1.
        assert_eq!(
            ids.len(),
            3,
            "decay splice should unlock M2, M3, and the original sink simultaneously, got {ids:?}"
        );
        assert!(ids.contains(&m2), "M2 must be in the spliced frontier");
        assert!(ids.contains(&m3), "M3 must be in the spliced frontier");
        assert!(ids.contains(&sink), "sink must remain in the frontier");

        // Policy's edge list must now contain the new (M1, M2) and (M1, M3)
        // spliced edges — proof that insert_subgraph actually ran.
        let current_edges: HashSet<(MoleculeId, MoleculeId)> =
            policy.edges().iter().cloned().collect();
        assert!(
            current_edges.contains(&(m1.clone(), m2.clone())),
            "expected spliced edge (M1, M2) in {current_edges:?}"
        );
        assert!(
            current_edges.contains(&(m1.clone(), m3.clone())),
            "expected spliced edge (M1, M3) in {current_edges:?}"
        );

        // M1 must be recorded as completed.
        assert!(policy.completed().contains(&m1));
    }

    // -- critical-path prioritization --

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn test_dag_policy_critical_path_orders_ready_frontier() {
        // `a` is the common root; it unlocks `b` on a long chain and `x`
        // on a short chain. After `a` completes, both `b` and `x` are
        // ready. Critical path is a→b→c→d; the policy must dispatch `b`
        // before `x`.
        let root = mol_id("task-20260410-croo");
        let b = mol_id("task-20260410-crbb");
        let c = mol_id("task-20260410-crcc");
        let d = mol_id("task-20260410-crdd");
        let x = mol_id("task-20260410-crxx");

        let edges = vec![
            (root.clone(), b.clone()),
            (b.clone(), c.clone()),
            (c.clone(), d.clone()),
            (root.clone(), x.clone()),
        ];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        // Drive the policy through root → Completed.
        let snap = snapshot(vec![
            make_mol(&root, MoleculeStatus::Pending, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
            make_mol(&x, MoleculeStatus::Pending, Vec::new()),
        ]);
        let _ = policy.next_actions(&snap);

        let snap = snapshot(vec![
            make_mol(&root, MoleculeStatus::Completed, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c, MoleculeStatus::Pending, Vec::new()),
            make_mol(&d, MoleculeStatus::Pending, Vec::new()),
            make_mol(&x, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        let ids = evolve_ids(&actions);
        assert_eq!(ids.len(), 2);
        // `b` sits on the critical path (root→b→c→d), `x` is a short
        // branch. The policy must emit `b` first.
        assert_eq!(
            ids[0], b,
            "critical-path molecule `b` should be dispatched before short-branch `x`"
        );
        assert_eq!(ids[1], x);
    }

    // -- ADR-043: parallel_limit caps concurrent dispatch --

    /// Five children ready at once; a static cap of 2 on the children's
    /// `(formula, step)` coordinate should surface only 2 Evolve actions
    /// on the first tick. As children complete, the remaining ones
    /// progressively enter the frontier.
    #[test]
    fn test_dag_policy_parallel_limit_caps_fanout() {
        // Root M decomposes into 5 leaves L1..L5 at (leaf-formula, step 0).
        let root = mol_id("task-20260415-root");
        let leaves: Vec<MoleculeId> = (1..=5)
            .map(|i| mol_id(&format!("task-20260415-l00{i}")))
            .collect();

        let mut edges: Vec<(MoleculeId, MoleculeId)> = Vec::new();
        for l in &leaves {
            edges.push((root.clone(), l.clone()));
        }
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");

        // Cap the leaves' step at max=2. Leaves use a distinct formula so
        // the root's dispatch isn't also capped.
        let leaf_formula = FormulaId::new("leaf-work").expect("leaf fid");
        let mut limits: HashMap<(FormulaId, usize), u32> = HashMap::new();
        limits.insert((leaf_formula.clone(), 0), 2);
        let mut policy = DagPolicy::new(plan, edges).with_limits(limits);

        // Tick 1: root is ready; it uses task-work (no cap). Dispatches root.
        let mut initial = vec![make_mol(&root, MoleculeStatus::Pending, Vec::new())];
        for l in &leaves {
            let mut m = make_mol(l, MoleculeStatus::Pending, Vec::new());
            m.formula_id = leaf_formula.clone();
            initial.push(m);
        }
        let actions = policy.next_actions(&snapshot(initial));
        assert_eq!(evolve_ids(&actions), vec![root.clone()]);

        // Tick 2: root completes → all 5 leaves become ready, but cap=2
        // must limit dispatch to 2 leaves.
        let mut snap2 = vec![make_mol(&root, MoleculeStatus::Completed, Vec::new())];
        for l in &leaves {
            let mut m = make_mol(l, MoleculeStatus::Pending, Vec::new());
            m.formula_id = leaf_formula.clone();
            snap2.push(m);
        }
        let actions = policy.next_actions(&snapshot(snap2));
        let ids = evolve_ids(&actions);
        assert_eq!(
            ids.len(),
            2,
            "parallel_limit max=2 must cap fan-out to 2, got {ids:?}"
        );

        // Tick 3: two leaves Running (inside the cap), no additional dispatch.
        let mut snap3 = vec![make_mol(&root, MoleculeStatus::Completed, Vec::new())];
        for (i, l) in leaves.iter().enumerate() {
            let status = if i < 2 {
                MoleculeStatus::Running
            } else {
                MoleculeStatus::Pending
            };
            let mut m = make_mol(l, status, Vec::new());
            m.formula_id = leaf_formula.clone();
            snap3.push(m);
        }
        let actions = policy.next_actions(&snapshot(snap3));
        assert!(
            evolve_ids(&actions).is_empty(),
            "cap is full; no further dispatch expected, got {actions:?}"
        );

        // Tick 4: one leaf completes → one slot frees → one more dispatch.
        let mut snap4 = vec![make_mol(&root, MoleculeStatus::Completed, Vec::new())];
        for (i, l) in leaves.iter().enumerate() {
            let status = match i {
                0 => MoleculeStatus::Completed,
                1 => MoleculeStatus::Running,
                _ => MoleculeStatus::Pending,
            };
            let mut m = make_mol(l, status, Vec::new());
            m.formula_id = leaf_formula.clone();
            snap4.push(m);
        }
        let actions = policy.next_actions(&snapshot(snap4));
        assert_eq!(
            evolve_ids(&actions).len(),
            1,
            "one slot freed → one new dispatch expected"
        );
    }

    /// Without `with_limits`, the policy behaves exactly as before — the
    /// opt-in guarantee of ADR-043.
    #[test]
    fn test_dag_policy_no_limits_unbounded() {
        let root = mol_id("task-20260415-rt02");
        let leaves: Vec<MoleculeId> = (1..=5)
            .map(|i| mol_id(&format!("task-20260415-x00{i}")))
            .collect();
        let mut edges: Vec<(MoleculeId, MoleculeId)> = Vec::new();
        for l in &leaves {
            edges.push((root.clone(), l.clone()));
        }
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        // Drive root to completion.
        let mut initial = vec![make_mol(&root, MoleculeStatus::Pending, Vec::new())];
        for l in &leaves {
            initial.push(make_mol(l, MoleculeStatus::Pending, Vec::new()));
        }
        let _ = policy.next_actions(&snapshot(initial));

        let mut snap2 = vec![make_mol(&root, MoleculeStatus::Completed, Vec::new())];
        for l in &leaves {
            snap2.push(make_mol(l, MoleculeStatus::Pending, Vec::new()));
        }
        let actions = policy.next_actions(&snapshot(snap2));
        assert_eq!(
            evolve_ids(&actions).len(),
            5,
            "no limits → all 5 dispatched simultaneously"
        );
    }

    // -- idempotence: re-ticking with the same snapshot doesn't double-dispatch --

    #[test]
    fn test_dag_policy_idempotent_on_identical_snapshot() {
        let a = mol_id("task-20260410-idem");
        let b = mol_id("task-20260410-idmb");
        let edges = vec![(a.clone(), b.clone())];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Pending, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
        ]);
        let first = policy.next_actions(&snap);
        assert_eq!(evolve_ids(&first), vec![a.clone()]);

        // Now the runtime has presumably picked up the Evolve and moved A
        // into Running — reflect that in the snapshot.
        let snap = snapshot(vec![
            make_mol(&a, MoleculeStatus::Running, Vec::new()),
            make_mol(&b, MoleculeStatus::Pending, Vec::new()),
        ]);
        let second = policy.next_actions(&snap);
        assert!(
            second.is_empty(),
            "a molecule already moved to Running should not be re-dispatched, got {second:?}"
        );
    }

    // -- collapsed parent releases BOTH its DecayProduct and Blocks children --

    /// A `reproduce` molecule `M` decomposes into two lateral children `C1`
    /// and `C2` and has a forward-`Blocks` dependent `P` (the `fix`). `M`
    /// then collapses with a **refuted** verdict (bug not reproducible).
    ///
    /// `blocked-by` releases on *done*, not on *verdict* (task-20260706-4d1e):
    /// the collapse must splice **both** the lateral `DecayProduct` children
    /// `C1`/`C2` **and** the forward `Blocks` target `P`, and enter `M` into
    /// the skip-set. All three dependents become dispatchable — the `fix`
    /// worker reads the "no repro" verdict from disk and decides. This
    /// matches [`cosmon_state::frontier::compute_from_molecules`], which
    /// already clears a `Collapsed` predecessor.
    #[test]
    fn test_dag_policy_collapsed_parent_releases_decay_and_blocks() {
        let m = mol_id("task-20260410-mcol");
        let c1 = mol_id("task-20260410-cc01");
        let c2 = mol_id("task-20260410-cc02");
        let p = mol_id("task-20260410-pdfg");

        // Seed edge list: M → P (the forward gate we want to verify IS
        // released after M collapses).
        let edges = vec![(m.clone(), p.clone())];
        let plan = Plan::new(edges.clone(), HashSet::new()).expect("plan");
        let mut policy = DagPolicy::new(plan, edges);

        // Tick 1: only M is ready.
        let snap = snapshot(vec![
            make_mol(&m, MoleculeStatus::Pending, Vec::new()),
            make_mol(&p, MoleculeStatus::Pending, Vec::new()),
        ]);
        let _ = policy.next_actions(&snap);

        // Tick 2: M collapses having already materialized DecayProduct
        // children C1 and C2 (the mid-step decomposition case).
        let m_collapsed = make_mol(
            &m,
            MoleculeStatus::Collapsed,
            vec![
                MoleculeLink::DecayProduct { id: c1.clone() },
                MoleculeLink::DecayProduct { id: c2.clone() },
                MoleculeLink::Blocks { target: p.clone() },
            ],
        );
        let snap = snapshot(vec![
            m_collapsed,
            make_mol(&c1, MoleculeStatus::Pending, Vec::new()),
            make_mol(&c2, MoleculeStatus::Pending, Vec::new()),
            make_mol(&p, MoleculeStatus::Pending, Vec::new()),
        ]);
        let actions = policy.next_actions(&snap);
        let ids = evolve_ids(&actions);

        assert!(
            ids.contains(&c1),
            "C1 must be dispatched after M collapses (lateral drain), got {ids:?}"
        );
        assert!(
            ids.contains(&c2),
            "C2 must be dispatched after M collapses (lateral drain), got {ids:?}"
        );
        assert!(
            ids.contains(&p),
            "P (the fix) MUST be dispatched — blocked-by releases on done, \
             not on verdict; a refuted reproduce still unblocks the fix, got {ids:?}"
        );

        // A collapse is a terminal event: M enters the skip-set exactly
        // like a clean completion, so the forward Blocks edge (M, P) is
        // cleared and P is released.
        assert!(
            policy.completed().contains(&m),
            "collapsed M must enter the skip-set so its forward Blocks dependents release"
        );
    }

    // -- ADR-038 Limit 1: refresh_scope absorbs dynamic descendants --

    /// When a worker nucleates a child mid-run and links it to a
    /// molecule already tracked by the policy (here: the parent `P` is
    /// in `known_molecules`), `refresh_scope` re-walks the store and
    /// pulls the child into the policy's edge list. Without the sweep
    /// the child would be invisible until an explicit `recompile` (which
    /// only fires on splice). With the sweep, the child becomes eligible
    /// for dispatch on the tick after it is nucleated.
    #[test]
    fn test_refresh_scope_absorbs_new_descendant() {
        use cosmon_filestore::FileStore;
        use cosmon_state::MoleculeData as MolData;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

        let p = mol_id("task-20260414-prnt");
        let p_data = MolData {
            id: p.clone(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
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
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
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
        };
        store.save_molecule(&p, &p_data).unwrap();

        // Bootstrap the policy from P alone — no children yet.
        let (plan, edges) = compile_plan(&store, std::slice::from_ref(&p)).unwrap();
        let mut policy = DagPolicy::new(plan, edges);
        assert!(policy.known_molecules.contains(&p));

        // Worker nucleates a child C and links P via BlockedBy so the
        // child waits on P. This mirrors a `mission-controller decompose`
        // step that creates a downstream sibling mid-flight.
        let c = mol_id("task-20260414-chld");
        let mut c_data = p_data.clone();
        c_data.id = c.clone();
        c_data.status = MoleculeStatus::Pending;
        c_data.typed_links = vec![MoleculeLink::BlockedBy { source: p.clone() }];
        store.save_molecule(&c, &c_data).unwrap();

        // Before sweep: C is invisible to the policy's edge set.
        assert!(
            !policy.known_molecules.contains(&c),
            "sanity: C must not yet be tracked"
        );

        // Sweep.
        policy.refresh_scope(&store).unwrap();

        // After sweep: C is in known_molecules and the (P, C) edge exists.
        assert!(
            policy.known_molecules.contains(&c),
            "refresh_scope must absorb the new descendant"
        );
        let edges: HashSet<(MoleculeId, MoleculeId)> = policy.edges().iter().cloned().collect();
        assert!(
            edges.contains(&(p.clone(), c.clone())),
            "refresh_scope must splice the (P, C) edge, got {edges:?}"
        );
    }

    /// Sweeping the same stable store twice is a no-op on observable
    /// state: edge set unchanged, `known_molecules` unchanged. Proves
    /// idempotence of the sweep — important because the runtime calls
    /// it on a tick-count cadence.
    #[test]
    fn test_refresh_scope_is_idempotent() {
        use cosmon_filestore::FileStore;
        use cosmon_state::MoleculeData as MolData;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

        let a = mol_id("task-20260414-aaaa");
        let b = mol_id("task-20260414-bbbb");
        let make = |id: &MoleculeId, links: Vec<MoleculeLink>| MolData {
            id: id.clone(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
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
            tags: std::collections::BTreeSet::new(),
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
        };
        store
            .save_molecule(
                &a,
                &make(&a, vec![MoleculeLink::Blocks { target: b.clone() }]),
            )
            .unwrap();
        store
            .save_molecule(
                &b,
                &make(&b, vec![MoleculeLink::BlockedBy { source: a.clone() }]),
            )
            .unwrap();

        let (plan, edges) = compile_plan(&store, std::slice::from_ref(&a)).unwrap();
        let mut policy = DagPolicy::new(plan, edges);
        let edges_before: HashSet<(MoleculeId, MoleculeId)> =
            policy.edges().iter().cloned().collect();
        let known_before = policy.known_molecules.clone();

        policy.refresh_scope(&store).unwrap();
        policy.refresh_scope(&store).unwrap();

        let edges_after: HashSet<(MoleculeId, MoleculeId)> =
            policy.edges().iter().cloned().collect();
        assert_eq!(edges_before, edges_after, "edge set must be stable");
        assert_eq!(
            known_before, policy.known_molecules,
            "known_molecules must be stable"
        );
    }
}
