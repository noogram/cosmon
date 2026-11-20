// SPDX-License-Identifier: AGPL-3.0-only

//! `cs land-train` — land an operator-built, operator-gated train on the base
//! branch, then close each member with `cs done --expect-head` (issue #213,
//! ADR-190 D3, increment D1).
//!
//! # Why this spelling
//!
//! The verb names what moves and what lands. Bare `cs land` is taken by
//! history: ADR-176 records it as a second name for `cs done`, withdrawn on
//! 2026-09-07, and reusing it for a different operation would make every
//! older reference to it ambiguous. `land-train` follows the hyphenated
//! verb-object shape `harvest-authority` already uses, and leaves `cs done`
//! the only closer.
//!
//! # The seam
//!
//! The locked phase — every landing check and the compare-and-swap
//! fast-forward — is [`cosmon_harvest::train::land`], which releases the trunk
//! lock before it returns. This module then closes each pending member by
//! running this same binary as `cs done <id> --expect-head <head>`, one child
//! at a time, in train order, stopping at the first failure. A child process
//! and not an in-process call, because each closure must take and release the
//! trunk lock itself, and because its exit code and output are exactly those
//! of the `cs done` an operator would have typed.

use std::process::Command;

use anyhow::Context as _;
use cosmon_harvest::train::{self, ExcludedHead, LandTrainRequest, Landing, TrainMember};

use super::Context;

/// Arguments for `cs land-train`.
#[derive(clap::Args)]
pub struct Args {
    /// The base-branch commit the train was built on and gated against.
    ///
    /// The landing is a compare-and-swap: the base branch must still be at
    /// this commit, or the train is refused as "moved after the gate".
    #[arg(long, value_name = "REV")]
    pub train_base: String,

    /// The train's tip: the gated commit the base branch fast-forwards to.
    #[arg(long, value_name = "REV")]
    pub train_tip: String,

    /// One member, as `<molecule id>=<pinned branch head>`. Repeat once per
    /// member merge, in the order the train merged them (oldest first).
    #[arg(long = "member", value_name = "ID=HEAD", required = true)]
    pub members: Vec<String>,

    /// Repeat once per removed member with its pinned head. Refuses any
    /// train member whose pinned head contains an excluded head in its
    /// ancestry. An excluded ID must be absent from --member, and its
    /// head must resolve to a commit.
    #[arg(long = "exclude", value_name = "ID=HEAD")]
    pub exclusions: Vec<String>,
}

/// Execute `cs land-train`.
///
/// # Errors
///
/// Any landing refusal (nothing moved, nothing closed), and the first member
/// closure that fails (the base has moved; the members closed so far and the
/// ones still open are named).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let members = args
        .members
        .iter()
        .map(|m| TrainMember::parse(m))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let exclusions = args
        .exclusions
        .iter()
        .map(|spec| ExcludedHead::parse(spec))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let request = LandTrainRequest {
        train_base: args.train_base.clone(),
        train_tip: args.train_tip.clone(),
        members,
        exclusions,
    };
    let harvest_ctx = cosmon_harvest::HarvestContext {
        verbose: ctx.verbose,
        json: ctx.json,
        config: ctx.config.clone(),
        repo_root: None,
    };
    let outcome = train::land(&harvest_ctx, &request)?;
    for warning in &outcome.warnings {
        eprintln!("⚠ {warning}");
    }

    let short = |oid: &str| oid.chars().take(10).collect::<String>();
    if !ctx.json {
        match outcome.landing {
            Landing::Landed => println!(
                "land-train: fast-forwarded {} {} → {} (trunk lock released)",
                outcome.base_branch,
                short(&outcome.train_base),
                short(&outcome.train_tip)
            ),
            Landing::AlreadyLanded => println!(
                "land-train: {} is already at the train tip {}; landing skipped, finishing the \
                 closures",
                outcome.base_branch,
                short(&outcome.train_tip)
            ),
        }
        for mol in &outcome.already_closed {
            println!("  · {mol}: already closed, not closed again");
        }
    }

    let closed = close_members(ctx, &outcome)?;

    if ctx.json {
        let payload = serde_json::json!({
            "command": "land-train",
            "base_branch": outcome.base_branch,
            "train_base": outcome.train_base,
            "train_tip": outcome.train_tip,
            "landing": match outcome.landing {
                Landing::Landed => "landed",
                Landing::AlreadyLanded => "already_landed",
            },
            "closed": closed,
            "already_closed": outcome
                .already_closed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        });
        println!("{payload}");
    } else {
        println!(
            "land-train: closed {} member(s){}",
            closed.len(),
            if outcome.already_closed.is_empty() {
                String::new()
            } else {
                format!(", {} already closed", outcome.already_closed.len())
            }
        );
    }
    Ok(())
}

/// Close each pending member with a `cs done <id> --expect-head <head>` child,
/// in train order, stopping at the first failure. Returns the ids closed.
///
/// Runs after [`train::land`] returned, so the trunk lock is already released
/// and each child takes it for itself.
fn close_members(ctx: &Context, outcome: &train::LandingOutcome) -> anyhow::Result<Vec<String>> {
    let short = |oid: &str| oid.chars().take(10).collect::<String>();
    let exe = std::env::current_exe().context("cs land-train: cannot locate the cs binary")?;
    let mut closed = Vec::new();
    for (i, member) in outcome.to_close.iter().enumerate() {
        if !ctx.json {
            println!(
                "  → cs done {} --expect-head {}",
                member.molecule,
                short(&member.head)
            );
        }
        let mut cmd = Command::new(&exe);
        if let Some(state_dir) = ctx.config.as_ref() {
            cmd.arg("--config").arg(state_dir);
        }
        if ctx.json {
            cmd.arg("--json");
        }
        // `--no-auto-propel`: the member is already reachable from the base,
        // so there is no merge to conflict; the flag only guarantees no
        // worker is ever resumed from a landing.
        cmd.args(["done", member.molecule.as_str(), "--no-auto-propel"])
            .args(["--expect-head", &member.head]);
        let status = cmd
            .status()
            .with_context(|| format!("cs land-train: could not run cs done {}", member.molecule))?;
        if !status.success() {
            let open: Vec<String> = outcome.to_close[i..]
                .iter()
                .map(|m| m.molecule.to_string())
                .collect();
            anyhow::bail!(
                "cs land-train: the train landed on {} at {}, but `cs done {} --expect-head {}` \
                 failed ({status}). Closed: [{}]. Not closed: [{}]. Fix the cause and rerun the \
                 same `cs land-train` command: it skips the landing and closes only the members \
                 still open.",
                outcome.base_branch,
                outcome.train_tip,
                member.molecule,
                member.head,
                closed.join(", "),
                open.join(", ")
            );
        }
        closed.push(member.molecule.to_string());
    }

    Ok(closed)
}
