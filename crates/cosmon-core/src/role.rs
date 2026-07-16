// SPDX-License-Identifier: AGPL-3.0-only

//! Compile-time worker roles — typestate for the cosmon *équipage*.
//!
//! This module encodes a worker's **role** and its **trunk-lock state**
//! in the *type*, not in a runtime string. A worker that does not hold a
//! capability simply has no method to misuse it: the wrong call site does
//! not type-check, so it cannot ship.
//!
//! # Where this sits in the authorisation stack
//!
//! cosmon already had three authorisation layers (ADR-008):
//!
//! - **Layer 1** — Cargo `cfg(feature)` (compile-time, not in types).
//! - **Layer 2** — [`Clearance`](crate::clearance::Clearance) +
//!   [`Capability`](crate::capability::Capability) (runtime, per-agent).
//! - **Layer 3** — [`FeatureFlags`](crate::feature_flags::FeatureFlags)
//!   (dynamic config).
//!
//! This module adds a **Layer 0**: a *compile-time* binding between a
//! role and the verbs it may invoke. Layer 2 can *deny* a wrong call at
//! runtime (returns an error); Layer 0 makes the wrong call *not exist*.
//! Layer 0 is strictly stronger and strictly cheaper — there is no
//! runtime check to forget, and the failure surfaces at `cargo build`
//! rather than in production. [`Role::capabilities`] is the bridge: it
//! maps each compile-time role to the Layer-2 [`Capability`] grants the
//! same role would carry at runtime, so the two layers cannot drift.
//!
//! # The invariant this protects: I1 WRITER-UNIQUE (ADR-110)
//!
//! [ADR-110 §I1](../../../docs/adr/110-single-writer-trunk-and-coordination-invariants.md)
//! names *single-writer-trunk*: at any instant at most one worker may
//! write the cosmon `main` branch. An earlier phase enforced it
//! mechanically with an advisory `flock` (`with_trunk_lock`). This module
//! lifts the guard into the type system, so a *misuse* of the lock becomes
//! a compile error rather than a runtime panic on lock-state inconsistency.
//!
//! Two type-level mechanisms carry I1:
//!
//! 1. **Role gating.** Only [`Stitcher`] implements [`CanWriteTrunk`].
//!    Every other role lacks the trait bound, so `acquire_trunk` does
//!    not exist for it.
//! 2. **Lock-state typestate.** A [`TypedWorker`] carries its lock state
//!    ([`Unlocked`] → [`TrunkHeld`]) as a second type parameter. The
//!    merge verb [`land`](TypedWorker::land) exists *only* on the
//!    [`TrunkHeld`] state, so you cannot land without first acquiring the
//!    lock, and you cannot acquire it twice (the [`Unlocked`] →
//!    [`TrunkHeld`] transition consumes `self` by value).
//!
//! `land` mints a [`TrunkWritePermit`] — a token an I/O layer (e.g.
//! `cosmon-filestore::with_trunk_lock`) can take by reference as *proof*
//! that the caller is a lock-holding [`Stitcher`]. The proof travels
//! across the crate boundary without re-checking a string.
//!
//! # The five roles (the *équipage*)
//!
//! | Role            | Real cosmon function                         | Capability marker        |
//! |-----------------|----------------------------------------------|--------------------------|
//! | [`Implementer`] | cognition worker running formula steps       | [`CanImplement`]         |
//! | [`Verifier`]    | runs the `DoD` gates (build/test/clippy/fmt)  | [`CanVerify`]            |
//! | [`Baker`]       | `pizzaiolo` — builds & pushes images         | [`CanBake`]              |
//! | [`Stitcher`]    | `cs stitch` / `cs land` — merges to trunk    | [`CanWriteTrunk`]        |
//! | [`Orchestrator`]| resident runtime (`cs run`) driving a DAG    | [`CanSpawn`]             |
//!
//! A [`Verifier`] therefore *cannot* spawn a sub-worker (it does not
//! implement [`CanSpawn`]) and *cannot* write the trunk (it does not
//! implement [`CanWriteTrunk`]). Both are compile errors, exercised in
//! `tests/compile_fail/role/`.
//!
//! # Roles ↔ scopes (the scope-per-verb grid)
//!
//! [`Role::scopes`] returns the `OAuth2` scopes a role's verbs require,
//! mirroring the *scope-per-verb grid* catalogued in
//! `cosmon-rpp-adapter::auth::scopes`. [`WorkerScope`] is the typed mirror
//! of that catalogue; [`WorkerScope::as_oauth_str`] returns the canonical
//! `cosmon:…` string. The adapter catalogue remains the single source of
//! truth for the wire strings — this enum is the in-core, role-facing view
//! of the same grid. Note the [`Orchestrator`] requires
//! `{MoleculeWrite, WorkerSpawn}`, which is exactly the
//! `tackle = MOLECULE_WRITE ∧ WORKER_SPAWN` rule.
//!
//! # Example — the happy path
//!
//! ```
//! use cosmon_core::id::WorkerId;
//! use cosmon_core::role::{Stitcher, TypedWorker, WorkerScope};
//!
//! let stitcher = TypedWorker::<Stitcher>::new(WorkerId::new("stitch-7f3a").unwrap());
//! assert_eq!(stitcher.role_name(), "stitcher");
//!
//! // Acquire the trunk write-token: Unlocked -> TrunkHeld (consumes self).
//! let stitcher = stitcher.acquire_trunk();
//! assert_eq!(stitcher.lock_state_name(), "trunk-held");
//!
//! // Only a TrunkHeld Stitcher can mint a write permit.
//! let permit = stitcher.land();
//! assert_eq!(permit.writer().as_str(), "stitch-7f3a");
//!
//! // Hand the lock back: TrunkHeld -> Unlocked.
//! let _stitcher = stitcher.release_trunk();
//! ```
//!
//! # Example — the guarantee (does not compile)
//!
//! A [`Verifier`] has no `spawn_child` method:
//!
//! ```compile_fail
//! use cosmon_core::id::WorkerId;
//! use cosmon_core::role::{TypedWorker, Verifier};
//!
//! let verifier = TypedWorker::<Verifier>::new(WorkerId::new("verify-1").unwrap());
//! // error[E0599]: no method named `spawn_child` found — Verifier: !CanSpawn
//! let _ = verifier.spawn_child(WorkerId::new("child-1").unwrap());
//! ```
//!
//! An [`Implementer`] cannot acquire the trunk lock:
//!
//! ```compile_fail
//! use cosmon_core::id::WorkerId;
//! use cosmon_core::role::{Implementer, TypedWorker};
//!
//! let worker = TypedWorker::<Implementer>::new(WorkerId::new("impl-1").unwrap());
//! // error[E0599]: no method named `acquire_trunk` — Implementer: !CanWriteTrunk
//! let _ = worker.acquire_trunk();
//! ```

use std::marker::PhantomData;

use crate::capability::Capability;
use crate::id::WorkerId;

// ---------------------------------------------------------------------------
// Sealing — roles and lock-states are a closed set defined here.
// ---------------------------------------------------------------------------

mod sealed {
    /// Seals [`Role`](super::Role) and [`LockState`](super::LockState):
    /// downstream crates cannot introduce a new role or lock-state, so
    /// the capability matrix stays exhaustive and auditable in one place.
    pub trait Sealed {}
}

// ---------------------------------------------------------------------------
// Role marker types — uninhabited, zero-sized, never constructed.
// ---------------------------------------------------------------------------

/// Cognition worker running a molecule's formula steps. Writes on its
/// own molecule branch (`cs evolve`); never on trunk. See [`CanImplement`].
pub enum Implementer {}

/// Runs the Definition-of-Done gates (build / test / clippy / fmt).
/// Read-only on molecule state — by I5 OBSERVATION-NEUTRE it must not
/// mutate. See [`CanVerify`].
pub enum Verifier {}

/// The `pizzaiolo` baker: builds and pushes container images / artifacts.
/// See [`CanBake`].
pub enum Baker {}

/// The single writer: merges molecule branches into trunk
/// (`cs stitch` / `cs land`). The *only* role that implements
/// [`CanWriteTrunk`] — this is ADR-110 I1 WRITER-UNIQUE at the type level.
pub enum Stitcher {}

/// The resident runtime (`cs run`) driving a DAG of molecules. The
/// *only* role that implements [`CanSpawn`]. See [`CanSpawn`].
pub enum Orchestrator {}

// ---------------------------------------------------------------------------
// Lock-state marker types — the typestate of the trunk write-token.
// ---------------------------------------------------------------------------

/// The worker does not hold the trunk write-token. Default state.
pub enum Unlocked {}

/// The worker holds the trunk write-token (advisory `flock` on
/// `trunk.lock`). Only in this state does [`land`](TypedWorker::land)
/// exist. Reachable only from [`Unlocked`] via
/// [`acquire_trunk`](TypedWorker::acquire_trunk), and only for a role
/// implementing [`CanWriteTrunk`].
pub enum TrunkHeld {}

impl sealed::Sealed for Implementer {}
impl sealed::Sealed for Verifier {}
impl sealed::Sealed for Baker {}
impl sealed::Sealed for Stitcher {}
impl sealed::Sealed for Orchestrator {}
impl sealed::Sealed for Unlocked {}
impl sealed::Sealed for TrunkHeld {}

// ---------------------------------------------------------------------------
// The Role trait + capability marker traits.
// ---------------------------------------------------------------------------

/// A compile-time worker role. Sealed: the set of roles is closed and
/// defined in this module.
pub trait Role: sealed::Sealed {
    /// Stable, lowercase, kebab-case role name (matches
    /// [`Display`](std::fmt::Display) conventions elsewhere in the crate).
    const NAME: &'static str;

    /// The scope-per-verb scopes this role's verbs require.
    #[must_use]
    fn scopes() -> &'static [WorkerScope];

    /// The Layer-2 [`Capability`] grants this role would carry at runtime
    /// (ADR-008). Bridges the compile-time role to the runtime RBAC plane
    /// so the two cannot drift.
    #[must_use]
    fn capabilities() -> &'static [Capability];
}

/// The lock-state of a [`TypedWorker`] w.r.t. the trunk write-token.
/// Sealed: only [`Unlocked`] and [`TrunkHeld`] exist.
pub trait LockState: sealed::Sealed {
    /// Stable, lowercase name of the lock-state.
    const NAME: &'static str;
}

impl LockState for Unlocked {
    const NAME: &'static str = "unlocked";
}
impl LockState for TrunkHeld {
    const NAME: &'static str = "trunk-held";
}

/// Capability: may write on its own molecule branch (`cs evolve`).
pub trait CanImplement: Role {}

/// Capability: may run the `DoD` gates. Read-only on molecule state.
pub trait CanVerify: Role {}

/// Capability: may build and push images / artifacts.
pub trait CanBake: Role {}

/// Capability: may spawn a sub-worker. Held only by [`Orchestrator`].
pub trait CanSpawn: Role {}

/// Capability: may acquire the trunk write-token and merge to trunk.
/// Held only by [`Stitcher`] — the type-level form of ADR-110 I1.
pub trait CanWriteTrunk: Role {}

// --- Role impls + capability assignments (the whole matrix, one place) ---

impl Role for Implementer {
    const NAME: &'static str = "implementer";
    fn scopes() -> &'static [WorkerScope] {
        &[WorkerScope::MoleculeWrite, WorkerScope::ArtifactWrite]
    }
    fn capabilities() -> &'static [Capability] {
        &[Capability::AccessMcp]
    }
}
impl CanImplement for Implementer {}

impl Role for Verifier {
    const NAME: &'static str = "verifier";
    fn scopes() -> &'static [WorkerScope] {
        &[WorkerScope::MoleculeRead, WorkerScope::ArtifactRead]
    }
    fn capabilities() -> &'static [Capability] {
        &[]
    }
}
impl CanVerify for Verifier {}

impl Role for Baker {
    const NAME: &'static str = "baker";
    fn scopes() -> &'static [WorkerScope] {
        &[WorkerScope::MoleculeRead, WorkerScope::ArtifactWrite]
    }
    fn capabilities() -> &'static [Capability] {
        &[]
    }
}
impl CanBake for Baker {}

impl Role for Stitcher {
    const NAME: &'static str = "stitcher";
    fn scopes() -> &'static [WorkerScope] {
        &[WorkerScope::MoleculeWrite]
    }
    fn capabilities() -> &'static [Capability] {
        &[Capability::ManageFleet]
    }
}
impl CanWriteTrunk for Stitcher {}

impl Role for Orchestrator {
    const NAME: &'static str = "orchestrator";
    // Exactly the b538 `tackle` rule: MOLECULE_WRITE ∧ WORKER_SPAWN.
    fn scopes() -> &'static [WorkerScope] {
        &[WorkerScope::MoleculeWrite, WorkerScope::WorkerSpawn]
    }
    fn capabilities() -> &'static [Capability] {
        &[
            Capability::ManageFleet,
            Capability::Patrol,
            Capability::SpawnSubagent,
        ]
    }
}
impl CanSpawn for Orchestrator {}

// ---------------------------------------------------------------------------
// WorkerScope — typed mirror of the b538 scope-per-verb grid.
// ---------------------------------------------------------------------------

/// The `OAuth2` scopes a worker role's verbs require — the in-core,
/// role-facing view of the *scope-per-verb grid*. The canonical wire
/// strings live in
/// `cosmon-rpp-adapter::auth::scopes`; this enum mirrors them so a role's
/// scope requirements can be reasoned about inside `cosmon-core` without
/// depending on the HTTP adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkerScope {
    /// `cosmon:molecule:read` — read-only molecule store access.
    MoleculeRead,
    /// `cosmon:molecule:write` — mutate cheap, reversible molecule state.
    MoleculeWrite,
    /// `cosmon:worker:spawn` — spawn a worker (burns Anthropic credit).
    WorkerSpawn,
    /// `cosmon:worker:read` — read-only listing of active workers.
    WorkerRead,
    /// `cosmon:worker:terminate` — terminate a running worker session.
    WorkerTerminate,
    /// `cosmon:artifact:read` — read per-molecule artifacts.
    ArtifactRead,
    /// `cosmon:artifact:write` — push per-molecule artifacts.
    ArtifactWrite,
    /// `cosmon:events:subscribe` — subscribe to the molecule event SSE.
    EventsSubscribe,
    /// `cosmon:logs:subscribe` — subscribe to the worker-output SSE.
    LogsSubscribe,
}

impl WorkerScope {
    /// The canonical `OAuth2` scope string, identical to the corresponding
    /// constant in `cosmon-rpp-adapter::auth::scopes`.
    #[must_use]
    pub const fn as_oauth_str(self) -> &'static str {
        match self {
            Self::MoleculeRead => "cosmon:molecule:read",
            Self::MoleculeWrite => "cosmon:molecule:write",
            Self::WorkerSpawn => "cosmon:worker:spawn",
            Self::WorkerRead => "cosmon:worker:read",
            Self::WorkerTerminate => "cosmon:worker:terminate",
            Self::ArtifactRead => "cosmon:artifact:read",
            Self::ArtifactWrite => "cosmon:artifact:write",
            Self::EventsSubscribe => "cosmon:events:subscribe",
            Self::LogsSubscribe => "cosmon:logs:subscribe",
        }
    }
}

impl std::fmt::Display for WorkerScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_oauth_str())
    }
}

// ---------------------------------------------------------------------------
// Value objects produced by role-gated verbs.
// ---------------------------------------------------------------------------

/// A record that a role-gated verb was invoked by an entitled worker.
///
/// Returned by the "act" verbs ([`evolve`](TypedWorker::evolve),
/// [`verify`](TypedWorker::verify), [`bake`](TypedWorker::bake)). It
/// carries the worker, a stable verb label, and the scopes the verb
/// exercises — enough for an audit/event layer to log the action without
/// re-deriving entitlement. Its mere existence is a *type-checked* proof
/// that the worker held the capability for `verb`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct ScopedAction {
    worker: WorkerId,
    verb: &'static str,
    scopes: &'static [WorkerScope],
}

impl ScopedAction {
    /// The worker that performed the action.
    #[must_use]
    pub fn worker(&self) -> &WorkerId {
        &self.worker
    }

    /// Stable label of the verb (`"evolve"`, `"verify"`, `"bake"`).
    #[must_use]
    pub fn verb(&self) -> &'static str {
        self.verb
    }

    /// The scopes this verb exercises.
    #[must_use]
    pub fn scopes(&self) -> &'static [WorkerScope] {
        self.scopes
    }
}

/// Authorisation to spawn a specific child worker, mintable only by a
/// role implementing [`CanSpawn`]. A spawn machinery should take a
/// `&SpawnPermit` so a worker that is not allowed to spawn cannot even
/// construct the argument.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct SpawnPermit {
    parent: WorkerId,
    child: WorkerId,
}

impl SpawnPermit {
    /// The spawning (parent) worker.
    #[must_use]
    pub fn parent(&self) -> &WorkerId {
        &self.parent
    }

    /// The worker to be spawned.
    #[must_use]
    pub fn child(&self) -> &WorkerId {
        &self.child
    }
}

/// Proof that the bearer is a lock-holding [`Stitcher`], mintable only by
/// [`TypedWorker::<Stitcher, TrunkHeld>::land`](TypedWorker::land).
///
/// This is the cross-crate carrier of ADR-110 I1. An I/O layer that
/// actually mutates the trunk (e.g. `cosmon-filestore`'s merge path) can
/// take `&TrunkWritePermit` in its signature, so it is *unconstructible*
/// to call that path without a lock-holding stitcher in hand:
///
/// ```
/// # use cosmon_core::id::WorkerId;
/// # use cosmon_core::role::{Stitcher, TrunkWritePermit, TypedWorker};
/// /// Stand-in for `cosmon_filestore::merge_to_trunk`.
/// fn merge_to_trunk(permit: &TrunkWritePermit) -> &WorkerId {
///     permit.writer()
/// }
///
/// let held = TypedWorker::<Stitcher>::new(WorkerId::new("stitch-1").unwrap())
///     .acquire_trunk();
/// let permit = held.land();
/// assert_eq!(merge_to_trunk(&permit).as_str(), "stitch-1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct TrunkWritePermit {
    writer: WorkerId,
}

impl TrunkWritePermit {
    /// The lock-holding writer this permit vouches for.
    #[must_use]
    pub fn writer(&self) -> &WorkerId {
        &self.writer
    }
}

// ---------------------------------------------------------------------------
// TypedWorker<R, L> — the worker carrying its role and lock-state in type.
// ---------------------------------------------------------------------------

/// A worker whose role `R` and trunk-lock state `L` are encoded in the
/// type. Verbs are gated by capability trait bounds (`R: Can…`) and by
/// the lock state, so an unauthorised call does not type-check.
///
/// Deliberately **not** `Clone`/`Copy`: cloning a [`TrunkHeld`] worker
/// would duplicate the single write-token and violate I1. Ownership is
/// the enforcement — a transition consumes `self` by value.
pub struct TypedWorker<R: Role, L: LockState = Unlocked> {
    id: WorkerId,
    _role: PhantomData<fn() -> R>,
    _lock: PhantomData<fn() -> L>,
}

// Manual Debug: derive would wrongly require `R: Debug`, `L: Debug`.
impl<R: Role, L: LockState> std::fmt::Debug for TypedWorker<R, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedWorker")
            .field("id", &self.id)
            .field("role", &R::NAME)
            .field("lock_state", &L::NAME)
            .finish()
    }
}

impl<R: Role> TypedWorker<R, Unlocked> {
    /// Construct a worker in the [`Unlocked`] state for role `R`.
    #[must_use]
    pub fn new(id: WorkerId) -> Self {
        Self {
            id,
            _role: PhantomData,
            _lock: PhantomData,
        }
    }
}

// --- Accessors available in any role / any lock-state ---

// `role_name`/`lock_state_name`/`scopes`/`capabilities` reflect the type
// parameters `R`/`L`, not `self` — but the instance-method form
// (`worker.scopes()`) is the intended ergonomic surface.
#[allow(clippy::unused_self)]
impl<R: Role, L: LockState> TypedWorker<R, L> {
    /// The worker id.
    #[must_use]
    pub fn id(&self) -> &WorkerId {
        &self.id
    }

    /// The role name ([`Role::NAME`]).
    #[must_use]
    pub fn role_name(&self) -> &'static str {
        R::NAME
    }

    /// The lock-state name ([`LockState::NAME`]).
    #[must_use]
    pub fn lock_state_name(&self) -> &'static str {
        L::NAME
    }

    /// The scopes this role requires ([`Role::scopes`]).
    #[must_use]
    pub fn scopes(&self) -> &'static [WorkerScope] {
        R::scopes()
    }

    /// The Layer-2 capabilities this role carries ([`Role::capabilities`]).
    #[must_use]
    pub fn capabilities(&self) -> &'static [Capability] {
        R::capabilities()
    }
}

// --- Role-gated "act" verbs ---

impl<R: CanImplement, L: LockState> TypedWorker<R, L> {
    /// Record an `evolve` (write on the worker's own molecule branch).
    /// Available only to roles implementing [`CanImplement`].
    pub fn evolve(&self) -> ScopedAction {
        ScopedAction {
            worker: self.id.clone(),
            verb: "evolve",
            scopes: R::scopes(),
        }
    }
}

impl<R: CanVerify, L: LockState> TypedWorker<R, L> {
    /// Record a `verify` (run the `DoD` gates). Available only to roles
    /// implementing [`CanVerify`]. Read-only by construction (I5).
    pub fn verify(&self) -> ScopedAction {
        ScopedAction {
            worker: self.id.clone(),
            verb: "verify",
            scopes: R::scopes(),
        }
    }
}

impl<R: CanBake, L: LockState> TypedWorker<R, L> {
    /// Record a `bake` (build & push an image/artifact). Available only
    /// to roles implementing [`CanBake`].
    pub fn bake(&self) -> ScopedAction {
        ScopedAction {
            worker: self.id.clone(),
            verb: "bake",
            scopes: R::scopes(),
        }
    }
}

impl<R: CanSpawn, L: LockState> TypedWorker<R, L> {
    /// Mint authorisation to spawn `child`. Available only to roles
    /// implementing [`CanSpawn`] (only [`Orchestrator`]). A [`Verifier`]
    /// calling this is a compile error.
    pub fn spawn_child(&self, child: WorkerId) -> SpawnPermit {
        SpawnPermit {
            parent: self.id.clone(),
            child,
        }
    }
}

// --- Trunk write-token: the lock-state typestate (I1) ---

impl<R: CanWriteTrunk> TypedWorker<R, Unlocked> {
    /// Acquire the trunk write-token: [`Unlocked`] → [`TrunkHeld`].
    ///
    /// Available only to a role implementing [`CanWriteTrunk`] (only
    /// [`Stitcher`]). Consumes `self` by value, so a worker cannot hold
    /// the token twice, and the [`Unlocked`] handle is gone for the
    /// duration of the hold.
    ///
    /// This is the *type-level* counterpart of
    /// `cosmon-filestore::acquire_trunk_lock`; the returned handle is the
    /// proof, the advisory `flock` is the mechanism.
    #[must_use]
    pub fn acquire_trunk(self) -> TypedWorker<R, TrunkHeld> {
        TypedWorker {
            id: self.id,
            _role: PhantomData,
            _lock: PhantomData,
        }
    }
}

impl<R: CanWriteTrunk> TypedWorker<R, TrunkHeld> {
    /// Mint a [`TrunkWritePermit`] — proof, for an I/O layer, that the
    /// caller is a lock-holding [`Stitcher`]. Exists *only* on
    /// [`TrunkHeld`]: you cannot land without holding the lock.
    pub fn land(&self) -> TrunkWritePermit {
        TrunkWritePermit {
            writer: self.id.clone(),
        }
    }

    /// Release the trunk write-token: [`TrunkHeld`] → [`Unlocked`].
    /// Consumes `self` so the held handle cannot be reused after release.
    #[must_use]
    pub fn release_trunk(self) -> TypedWorker<R, Unlocked> {
        TypedWorker {
            id: self.id,
            _role: PhantomData,
            _lock: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wid(s: &str) -> WorkerId {
        WorkerId::new(s).unwrap()
    }

    #[test]
    fn role_names_are_stable_and_distinct() {
        assert_eq!(Implementer::NAME, "implementer");
        assert_eq!(Verifier::NAME, "verifier");
        assert_eq!(Baker::NAME, "baker");
        assert_eq!(Stitcher::NAME, "stitcher");
        assert_eq!(Orchestrator::NAME, "orchestrator");

        let names = [
            Implementer::NAME,
            Verifier::NAME,
            Baker::NAME,
            Stitcher::NAME,
            Orchestrator::NAME,
        ];
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "role names must be distinct");
    }

    #[test]
    fn worker_scope_oauth_strings_match_b538_catalogue() {
        // Mirrors cosmon-rpp-adapter::auth::scopes constants verbatim.
        assert_eq!(
            WorkerScope::MoleculeRead.as_oauth_str(),
            "cosmon:molecule:read"
        );
        assert_eq!(
            WorkerScope::MoleculeWrite.as_oauth_str(),
            "cosmon:molecule:write"
        );
        assert_eq!(
            WorkerScope::WorkerSpawn.as_oauth_str(),
            "cosmon:worker:spawn"
        );
        assert_eq!(WorkerScope::WorkerRead.as_oauth_str(), "cosmon:worker:read");
        assert_eq!(
            WorkerScope::WorkerTerminate.as_oauth_str(),
            "cosmon:worker:terminate"
        );
        assert_eq!(
            WorkerScope::ArtifactRead.as_oauth_str(),
            "cosmon:artifact:read"
        );
        assert_eq!(
            WorkerScope::ArtifactWrite.as_oauth_str(),
            "cosmon:artifact:write"
        );
        assert_eq!(
            WorkerScope::EventsSubscribe.as_oauth_str(),
            "cosmon:events:subscribe"
        );
        assert_eq!(
            WorkerScope::LogsSubscribe.as_oauth_str(),
            "cosmon:logs:subscribe"
        );
    }

    #[test]
    fn worker_scope_display_is_oauth_string() {
        assert_eq!(
            WorkerScope::MoleculeWrite.to_string(),
            "cosmon:molecule:write"
        );
    }

    #[test]
    fn orchestrator_scopes_match_b538_tackle_rule() {
        // b538: tackle = MOLECULE_WRITE ∧ WORKER_SPAWN.
        assert_eq!(
            Orchestrator::scopes(),
            &[WorkerScope::MoleculeWrite, WorkerScope::WorkerSpawn]
        );
    }

    #[test]
    fn implementer_can_evolve() {
        let w = TypedWorker::<Implementer>::new(wid("impl-7f3a"));
        let action = w.evolve();
        assert_eq!(action.verb(), "evolve");
        assert_eq!(action.worker().as_str(), "impl-7f3a");
        assert_eq!(action.scopes(), Implementer::scopes());
    }

    #[test]
    fn verifier_can_verify_and_is_read_only() {
        let w = TypedWorker::<Verifier>::new(wid("verify-1"));
        let action = w.verify();
        assert_eq!(action.verb(), "verify");
        // Read-only: no write scope.
        assert!(!action.scopes().contains(&WorkerScope::MoleculeWrite));
        assert!(action.scopes().contains(&WorkerScope::MoleculeRead));
    }

    #[test]
    fn baker_can_bake() {
        let w = TypedWorker::<Baker>::new(wid("bake-1"));
        assert_eq!(w.bake().verb(), "bake");
    }

    #[test]
    fn orchestrator_can_spawn() {
        let w = TypedWorker::<Orchestrator>::new(wid("orch-1"));
        let permit = w.spawn_child(wid("child-9"));
        assert_eq!(permit.parent().as_str(), "orch-1");
        assert_eq!(permit.child().as_str(), "child-9");
    }

    #[test]
    fn stitcher_lock_lifecycle() {
        let unlocked = TypedWorker::<Stitcher>::new(wid("stitch-1"));
        assert_eq!(unlocked.lock_state_name(), "unlocked");

        let held = unlocked.acquire_trunk();
        assert_eq!(held.lock_state_name(), "trunk-held");

        let permit = held.land();
        assert_eq!(permit.writer().as_str(), "stitch-1");

        let back = held.release_trunk();
        assert_eq!(back.lock_state_name(), "unlocked");
    }

    #[test]
    fn capabilities_bridge_to_layer2() {
        // Orchestrator carries the spawn capability at Layer 2.
        assert!(Orchestrator::capabilities().contains(&Capability::SpawnSubagent));
        // Verifier carries no extra Layer-2 grant.
        assert!(Verifier::capabilities().is_empty());
        // Stitcher manages the fleet (the merge).
        assert!(Stitcher::capabilities().contains(&Capability::ManageFleet));
    }

    #[test]
    fn debug_shows_role_and_lock_state() {
        let w = TypedWorker::<Stitcher>::new(wid("stitch-1"));
        let dbg = format!("{w:?}");
        assert!(dbg.contains("stitcher"));
        assert!(dbg.contains("unlocked"));
    }

    // A Stitcher is single-purpose: it writes the trunk but is not an
    // implementer. This compiles because we only call trunk verbs.
    #[test]
    fn stitcher_is_the_only_trunk_writer() {
        fn writes_trunk<R: CanWriteTrunk>(w: TypedWorker<R, Unlocked>) -> TrunkWritePermit {
            w.acquire_trunk().land()
        }
        let permit = writes_trunk(TypedWorker::<Stitcher>::new(wid("stitch-1")));
        assert_eq!(permit.writer().as_str(), "stitch-1");
    }
}
