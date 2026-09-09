// SPDX-License-Identifier: AGPL-3.0-only

//! The sealed harvest transaction, as a library.
//!
//! # Why this crate exists
//!
//! The harvest — merge the worker's branch with its lineage trailers, run the
//! publish / identity / confidentiality gates, run the `[hooks] pre_done`
//! gate, tear the worker down — had exactly one implementation, and it lived
//! *inside* the `cs` binary as `cosmon_cli::cmd::done`. A binary's `main.rs`
//! module is not callable, so anything else that wanted to close a molecule
//! had two options and both were bad: spawn `cs` as a child process, or write
//! a second copy of the transaction. ADR-176 §11 names the second one as the
//! failure the shared [`cosmon_core::harvest_door::DoorRefusal`] vocabulary
//! exists to prevent, and §12 recorded a typed `501
//! harvest_effect_unavailable` on `POST /v1/molecules/{id}/done` rather than
//! commit either.
//!
//! This crate is the third option, and the one §12's follow-up asked for: the
//! transaction moved out of the binary into a library, with **one
//! implementation and two callers** —
//!
//! - `cs done`, which parses [`Args`] off the command line and calls [`run`];
//! - `cosmon_rpp_adapter::harvest_effect::LibraryHarvestEffect`, which builds
//!   the same [`Args`] with [`Args::from_harvest_options`] and calls the same
//!   [`run`].
//!
//! No gate is duplicated, no refusal is re-derived, and the eight refusal
//! codes (70–77) reach both callers from the same [`RefusedHarvest`].
//!
//! # Why here and not in `cosmon-core` or `cosmon-filestore`
//!
//! `docs/architectural-invariants.md` keeps the domain core I/O-free, and the
//! harvest is I/O in every direction at once: `git`, tmux, the filesystem,
//! subprocess hooks. So it cannot live in [`cosmon_core`], and it belongs
//! *behind* a port rather than inside one — which is exactly the shape
//! [`cosmon_filestore::harvest_door::SealedHarvestEffect`] already declares.
//!
//! `cosmon-filestore` was the other candidate, since it already owns the
//! door's **decision** half. It was rejected because the decision half is
//! small, pure-ish and read-only over the state store, while the effect half
//! drives git, tmux, cargo and operator-supplied shell. Growing the store
//! crate by ~6 000 lines of process orchestration would make "the crate that
//! reads and writes molecule state" also "the crate that runs your build",
//! and every consumer of the former would compile the latter. A separate
//! crate keeps `cargo tree` honest about who takes that dependency.
//!
//! The whole workspace is AGPL-3.0-only (THESIS.md's licence partition puts
//! the Apache-2.0 boundary at the published *client* surface —
//! `cosmon-thin-cli`, the wire types — and nothing here is on that side), so
//! the split adds no licence question.
//!
//! # What did not move
//!
//! The clap tree, `main.rs`'s dispatch, and the `Context` the rest of the CLI
//! threads around. [`HarvestContext`] is this crate's own three-field view of
//! the two things the transaction actually reads from it — `--json` and the
//! resolved state directory — so a library caller with no CLI does not have
//! to fabricate one.

#![allow(clippy::missing_panics_doc)]

use std::path::PathBuf;

// ADR renumbering across a harvest merge — the collision resolver that runs
// when both the trunk and the branch added an ADR with the same number.
pub mod adr;
// Resolution of a molecule's integration base branch — the single place that
// answers "which trunk does this molecule's work belong to?" for both the
// branch cut (`cs tackle`) and the harvest (`cs done`).
pub mod base_branch;
// The `[harvest_authority]` decision: whether a harvest of this molecule, by
// this caller, is authorised at all.
pub mod done_authority;
// The jailed-`sh` seam for repository-supplied hook commands.
pub mod egress_delegate;
// The census of everywhere `cs` can put text in a worker's composer
// (COSMON #26 residual).
//
// Every module here is documented by its own `//!` header and NOT by a `///`
// on the declaration: an outer doc comment merges with the inner one and
// drags intra-doc link resolution up into *this* module's scope, so every
// `[`Item`]` a module writes about itself silently stops resolving. Whichever
// crate these modules live in, that stays true — it cost one doc-gate run to
// re-learn.
pub mod injection_provenance;
// The lineage trailers (`Mol-Id`, `Mission-Id`, `Depends-On`, `Base-Sync`)
// a completion merge stamps onto its merge commit.
pub mod lineage;
// Paths and identifiers the transaction resolves from a `HarvestContext`.
pub mod paths;
// The pilot-lease gesture guard: on a leased mission, only the holder may
// harvest.
pub mod pilot_gesture;
// Resolution of a galaxy's target repository — the single place that answers
// "which git repository does this galaxy's work land in?".
pub mod target_repo;
// The transaction itself.
pub mod transaction;
// Repo-supplied shell trust gate (B5, RCE-by-clone) — the `direnv allow` of
// cosmon. Every `sh -c` on a string the repository supplies (formula
// `command`/`verification` steps, `post_merge`/`pre_done` hooks) is gated on
// a per-repo, human-granted trust marker recorded outside the repo.
pub mod trust;
// The worktree a molecule was tackled in, and the mismatch guards that keep
// a blanket commit out of a tree the molecule never claimed.
pub mod worktree;

pub use transaction::{refusal_exit_code, run, Args, MergeStrategy, RefusedHarvest, TeardownPlan};

/// The slice of CLI context the harvest transaction actually reads.
///
/// `cs done` builds one from its own `Context`; the RPP adapter's library
/// effect builds one from the tenant's galaxy root. Three fields because
/// three is what the transaction reads — a library caller should not have to
/// synthesise a clap `Context` it has no use for.
#[derive(Debug, Clone, Default)]
pub struct HarvestContext {
    /// Whether the caller asked for verbose narration.
    pub verbose: bool,
    /// Whether output is NDJSON rather than prose. A library caller leaves
    /// this `false` and reads the returned `Result` instead.
    pub json: bool,
    /// The resolved state directory (`.cosmon/state`), or `None` to let
    /// [`cosmon_filestore::resolve_state_dir`] walk up from the process's
    /// working directory — the same discovery `cs` performs.
    pub config: Option<PathBuf>,
    /// The directory the harvest acts *in*, when the caller is not standing
    /// in it.
    ///
    /// The galaxy's `[project] target_repo` still wins when it is declared.
    /// This field only replaces the last-resort answer — "the repository
    /// containing the current directory" — which is correct for a CLI and
    /// wrong for a server: an adapter handling a request for a tenant galaxy
    /// is cwd'd in its own installation, and reading `current_dir()` there
    /// would merge the tenant's branch into the adapter's repository.
    /// `None` keeps the CLI's process-wide answer, unchanged.
    pub repo_root: Option<PathBuf>,
}

impl HarvestContext {
    /// A context pinned to an explicit state directory.
    ///
    /// The library caller's constructor: a server handling a request has no
    /// meaningful working directory, so it must name the galaxy it is acting
    /// in rather than let walk-up discovery guess.
    #[must_use]
    pub fn at_state_dir(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            verbose: false,
            json: false,
            config: Some(state_dir.into()),
            repo_root: None,
        }
    }

    /// The same, naming the working tree the harvest acts in.
    ///
    /// This is the constructor a server wants: it has both halves — which
    /// galaxy's state, and which checkout — and neither can come from a
    /// working directory it does not have.
    #[must_use]
    pub fn at(state_dir: impl Into<PathBuf>, repo_root: impl Into<PathBuf>) -> Self {
        Self {
            verbose: false,
            json: false,
            config: Some(state_dir.into()),
            repo_root: Some(repo_root.into()),
        }
    }

    /// The state directory honoured by this invocation.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.config
            .clone()
            .unwrap_or_else(|| cosmon_filestore::resolve_state_dir(None))
    }
}
