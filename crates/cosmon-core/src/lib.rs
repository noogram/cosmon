// SPDX-License-Identifier: AGPL-3.0-only

//! cosmon-core: Pure domain types, state machines, and trait definitions.
//!
//! I/O-free domain logic: the state machines, molecule transitions, dispatch,
//! and routing are pure functions over typed state. Every external interaction
//! is mediated through an injectable trait — `StateStore`, `CommandRunner`,
//! `PresenceSensor`, `Clock`. The in-memory implementations the domain is
//! tested against touch nothing. A handful of reference `Real*` implementations
//! that back those seams (e.g. `harness::RealCommandRunner`,
//! `attestor_event_v1::read_all`, the `presence_sensor` backends) do call
//! `std::fs`/`std::process`/`SystemTime` directly and ship in this crate; they
//! are the seams, not the core, and are tracked for relocation into the impure
//! shell by `task-20260622-3144`.
//!
//! # Public API surface — the kernel doctrine (task-20260622-da94)
//!
//! cosmon-core is published (`publish = true`) and the instant an external
//! crate depends on it, **every `pub` path becomes a frozen semver contract**.
//! Historically this crate exposed all ~85 domain modules as `pub mod`, which
//! would have frozen the entire internal domain — you could not rename
//! `spawn_seam`, privatise `convoy`, or reorganise `evolve` without a major
//! version bump. tolnay's review of the pre-publication architecture
//! (delib-20260622-187a, F-TOLNAY-1/2) named the fix: **ship the kernel, not
//! the workshop.**
//!
//! The surface is therefore partitioned three ways:
//!
//! 1. **Kernel — documented public API (`pub mod`).** The ~10 domain modules
//!    the THESIS names: [`molecule`], [`id`], [`error`], [`fleet`], [`worker`],
//!    [`formula`], [`event`], [`tag`], [`kind`], [`role`]. These — plus the
//!    curated root re-exports below — are the contract `cargo-semver-checks`
//!    governs. Changing them is a deliberate semver event.
//!
//! 2. **Workspace-internal (`#[doc(hidden)] pub mod`).** The remaining domain
//!    modules are consumed cross-crate by cosmon's own sibling crates
//!    (`cosmon-state`, `cosmon-cli`, `cosmon-transport`, …) but are **not**
//!    part of the public contract. Rust visibility cannot express
//!    "pub to my workspace, private to the world", so we use the ecosystem
//!    idiom (serde's `__private`, tokio's hidden modules): the modules stay
//!    `pub` for the compiler — zero breakage for siblings — but `#[doc(hidden)]`
//!    removes them from rustdoc and from the API surface `cargo-semver-checks`
//!    enforces. Renaming or reorganising a hidden module is **not** a breaking
//!    change. A downstream that reaches into one voids the warranty.
//!    The long-term clean split (a separate `cosmon-core-internal` crate) is
//!    tracked as a follow-up bead; doc(hidden) is the zero-risk move that lands
//!    the semver guarantee before the public flip.
//!
//! 3. **Crate-private (`pub(crate) mod`).** Modules with zero consumers outside
//!    cosmon-core: `context`, `dispatch`, `emoji`, `mcp_port`,
//!    `prompt_destination`. These are genuinely private; `#[allow(dead_code)]`
//!    covers items kept for in-tree future use.
//!
//! ## chrono as a permanent public dependency (F-TOLNAY-7)
//!
//! `chrono::DateTime<Utc>` leaks through 28+ public signatures in the kernel
//! (timestamps on molecules, events, workers). This is an **accepted**,
//! deliberate public dependency: cosmon's domain is time-stamped to the wall
//! clock and a newtype wrapper would buy nothing but churn. A major bump of
//! `chrono` is therefore a major bump of cosmon-core. Recorded here so the
//! coupling is a decision, not an accident.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

// ---------------------------------------------------------------------------
// Kernel — the documented, semver-governed public API. (Partition 1)
// ---------------------------------------------------------------------------
pub mod error;
pub mod event;
pub mod fleet;
pub mod formula;
pub mod id;
pub mod kind;
pub mod molecule;
pub mod role;
pub mod spore;
pub mod tag;
pub mod worker;

// ---------------------------------------------------------------------------
// Workspace-internal modules — `pub` for sibling crates, `#[doc(hidden)]` to
// keep them OUT of the public/semver contract. (Partition 2) Not for external
// consumers: their paths, names, and shapes may change without a major bump.
// ---------------------------------------------------------------------------
#[doc(hidden)]
pub mod adapter_attribution;
#[doc(hidden)]
pub mod adapter_exit;
#[doc(hidden)]
pub mod agent;
#[doc(hidden)]
pub mod artifact_map;
#[doc(hidden)]
pub mod atlas;
#[doc(hidden)]
pub mod attention;
#[doc(hidden)]
pub mod attestor_audit;
#[doc(hidden)]
pub mod attestor_event_v1;
#[doc(hidden)]
pub mod audit;
#[doc(hidden)]
pub mod auth;
#[doc(hidden)]
pub mod avatar;
#[doc(hidden)]
pub mod bead;
#[doc(hidden)]
pub mod calibration;
#[doc(hidden)]
pub mod capability;
#[doc(hidden)]
pub mod cas;
#[doc(hidden)]
pub mod chamber;
#[doc(hidden)]
pub mod clearance;
#[doc(hidden)]
pub mod cluster;
#[doc(hidden)]
pub mod codex_energy;
#[doc(hidden)]
pub mod committee;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod convoy;
#[doc(hidden)]
pub mod creativity;
#[doc(hidden)]
pub mod criticality;
#[doc(hidden)]
pub mod declaration;
#[doc(hidden)]
pub mod det_cache;
#[doc(hidden)]
pub mod dialogue;
#[doc(hidden)]
pub mod dispatch_refusal;
#[doc(hidden)]
pub mod egress;
#[doc(hidden)]
pub mod energy;
#[doc(hidden)]
pub mod ensemble;
#[doc(hidden)]
pub mod entropy;
#[doc(hidden)]
pub mod event_v2;
#[doc(hidden)]
pub mod evolve;
#[doc(hidden)]
pub mod expiry;
#[doc(hidden)]
pub mod feature_flags;
#[doc(hidden)]
pub mod federation;
#[doc(hidden)]
pub mod gate;
#[doc(hidden)]
pub mod governance;
#[doc(hidden)]
pub mod hook;
#[doc(hidden)]
pub mod interaction;
#[doc(hidden)]
pub mod interaction_mode;
#[doc(hidden)]
pub mod llm;
#[doc(hidden)]
pub mod message;
#[doc(hidden)]
pub mod modality;
#[doc(hidden)]
pub mod model_budget;
#[doc(hidden)]
pub mod model_chain;
#[doc(hidden)]
pub mod model_realization;
#[doc(hidden)]
pub mod model_spec;
#[doc(hidden)]
pub mod molecule_class;
#[doc(hidden)]
pub mod note;
#[doc(hidden)]
pub mod nucleate;
#[doc(hidden)]
pub mod nucleon;
#[doc(hidden)]
pub mod operator_block;
#[doc(hidden)]
pub mod ops;
#[doc(hidden)]
pub mod oracle_boundary;
#[doc(hidden)]
pub mod oracle_canary;
#[doc(hidden)]
pub mod panel;
#[doc(hidden)]
pub mod paths;
#[doc(hidden)]
pub mod patrol;
#[doc(hidden)]
pub mod pope;
#[doc(hidden)]
pub mod preempt;
#[doc(hidden)]
pub mod presence;
#[doc(hidden)]
pub mod presence_sensor;
#[doc(hidden)]
pub mod process;
#[doc(hidden)]
pub mod propel;
#[doc(hidden)]
pub mod provider_diversity;
#[doc(hidden)]
pub mod quality_band;
#[doc(hidden)]
pub mod query;
#[doc(hidden)]
pub mod reconcile;
#[doc(hidden)]
pub mod reproducibility;
#[doc(hidden)]
pub mod rig;
#[doc(hidden)]
pub mod run_state;
#[doc(hidden)]
pub mod scope_guard;
#[doc(hidden)]
pub mod session;
#[doc(hidden)]
pub mod signal;
#[doc(hidden)]
pub mod slugify;
#[doc(hidden)]
pub mod sor;
#[doc(hidden)]
pub mod spawn_seam;
#[doc(hidden)]
pub mod spec;
#[doc(hidden)]
pub mod tackle;
#[doc(hidden)]
pub mod toposort;
#[doc(hidden)]
pub mod transport;
#[doc(hidden)]
pub mod visual;
#[doc(hidden)]
pub mod vitality;

// ---------------------------------------------------------------------------
// Crate-private modules — zero external consumers. (Partition 3)
// `#[allow(dead_code)]` covers items retained for in-tree future use.
// ---------------------------------------------------------------------------
#[allow(dead_code)]
pub(crate) mod context;
#[allow(dead_code)]
pub(crate) mod dispatch;
#[allow(dead_code)]
pub(crate) mod emoji;
#[allow(dead_code)]
pub(crate) mod mcp_port;
#[allow(dead_code)]
pub(crate) mod prompt_destination;

// ---------------------------------------------------------------------------
// Test harness — split surface. The *ports* (`CommandRunner`, `CommandOutput`,
// `CommandRunnerError`, `Clock`, `RealClock`) are a production contract: the
// production adapter `cosmon_transport::command_runner::RealCommandRunner`
// links the port unconditionally, so the module must always compile. The
// *mocks* (`MockCommandRunner`, `RecordedCall`, `FixedClock`, `AdvancingClock`)
// ship `.expect("mock lock")` and stay gated behind `test`/`test-harness` so
// they never enter a normal published build — sibling crates that need them
// enable `features = ["test-harness"]` in their dev-dependencies (see
// cosmon-runtime). The gate moved from the whole module onto the mock items
// (move 3, task-20260622-da94; port-vs-mock split, task-20260623-0af1 — the
// blanket module gate broke `cargo check --workspace` by hiding the port).
// ---------------------------------------------------------------------------
pub mod harness;

// ---------------------------------------------------------------------------
// Curated root re-exports — the only non-module entries in the public contract.
// ---------------------------------------------------------------------------

// T-SUBJECT surfaces `Subject` (and the surrounding auth shape) so downstream
// `cosmon_state::ops::*` verbs can import it as `cosmon_core::Subject` without
// reaching into the (now hidden) `auth` module.
pub use auth::{AuthError, JwtClaims, Scope, Subject, SubjectBuilder, TenantApiKey, TenantId};

// `Clearance` and `DepthExceeded` appear in [`error::CosmonError`]'s public
// variants but live in hidden modules; re-export them at the root so the kernel
// error type's contract is fully nameable and documented without exposing the
// `clearance` / `agent` module paths.
pub use agent::DepthExceeded;
pub use clearance::Clearance;
