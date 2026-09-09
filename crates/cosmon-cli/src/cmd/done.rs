// SPDX-License-Identifier: AGPL-3.0-only

//! `cs done` — the CLI half of the sealed harvest transaction.
//!
//! The transaction itself — merge with lineage trailers, the publish /
//! identity / confidentiality gates, the `[hooks] pre_done` gate, the tmux /
//! worktree / branch teardown — is [`cosmon_harvest`]. It used to live here,
//! inside the binary, which meant the only way for anything else to close a
//! molecule was to spawn `cs` as a child or to write a second copy of the
//! door. ADR-176 §12's follow-up lifted it out; what is left in this module
//! is the seam:
//!
//! * `main.rs` parses [`Args`] (the clap surface still belongs to the binary
//!   even though the struct is defined next to the transaction, so that the
//!   route's [`Args::from_harvest_options`] and the operator's command line
//!   produce the *same* argument set);
//! * [`run`] converts the CLI's [`Context`] into a
//!   [`cosmon_harvest::HarvestContext`] and calls the library.
//!
//! The eight door refusals (exit codes 70–77) reach `main` through
//! [`RefusedHarvest`], unchanged and un-copied.

use super::Context;

#[allow(unused_imports)] // `MergeStrategy` and `RefusedHarvest` are re-exported
// for callers and tests that name `cmd::done::…`, not used by this seam itself.
pub use cosmon_harvest::{refusal_exit_code, Args, MergeStrategy, RefusedHarvest};

/// Run the harvest for `args`, in the galaxy this invocation resolved.
///
/// # Errors
///
/// Every refusal and failure of [`cosmon_harvest::run`], unchanged — including
/// [`RefusedHarvest`], which `main` downcasts to recover the door's exit code.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    cosmon_harvest::run(
        &cosmon_harvest::HarvestContext {
            verbose: ctx.verbose,
            json: ctx.json,
            config: ctx.config.clone(),
            // The CLI is standing in the galaxy it acts on, so the library's
            // own "the repository containing the current directory" answer is
            // the right one — and is what `cs done` has always used.
            repo_root: None,
        },
        args,
    )
    // The transaction runs the ADR-168 §D6 lease guard itself and refuses
    // with the library's own type. `main` reads the exit code off
    // `GuardError`, so without this the refusal would reach an operator with
    // the right sentence and the wrong exit code (1 instead of 16) — and a
    // script branching on 16 would read a co-pilotage refusal as a generic
    // failure.
    .map_err(super::guard::rehome_unleased_pilot_gesture)
}
