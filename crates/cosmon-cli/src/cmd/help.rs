// SPDX-License-Identifier: AGPL-3.0-only

//! `cs help` — single-entry-point documentation, like `git help`.
//!
//! With no arguments, prints all commands grouped by category.
//! With a command name, delegates to `cs <command> --help`.
//!
//! Category assignment is a UX decision that stays hand-written here,
//! but every subcommand's description is looked up at runtime from the
//! real clap tree (`Cli::command()`), so there is a single source of
//! truth: the `#[command(about = ...)]` / doc comment on each variant
//! in `main.rs`. Adding a new subcommand still requires extending
//! `command_groups()` so it appears in the grouped reference; omitting
//! it is caught by the `help_goldens` CI test.

use clap::CommandFactory;
use std::process;

use crate::Cli;

/// Arguments for the `help` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Command to show help for (delegates to `cs <command> --help`)
    pub command: Option<String>,
}

/// Run the help command.
///
/// - No args → print grouped command reference.
/// - `charter` → print the visual charter swatch. The charter is not a
///   real subcommand (it has no state to mutate), so it lives here as
///   a help-time view of `cosmon-style`.
/// - With any other command name → exec `cs <command> --help`.
pub fn run(args: &Args) -> anyhow::Result<()> {
    match args.command.as_deref() {
        None => {
            print_grouped_reference();
            Ok(())
        }
        Some("charter") => {
            cosmon_style::print_swatch();
            Ok(())
        }
        Some("guide") => {
            print!("{}", include_str!("../../../../docs/handbook.md"));
            Ok(())
        }
        Some(cmd) => delegate_to_subcommand(cmd),
    }
}

/// Re-exec as `cs <command> --help` so clap generates the detailed output.
fn delegate_to_subcommand(cmd: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "cs".into());
    let status = process::Command::new(exe)
        .args([cmd, "--help"])
        .status()
        .map_err(|e| anyhow::anyhow!("failed to exec cs {cmd} --help: {e}"))?;
    process::exit(status.code().unwrap_or(1));
}

/// A category in the grouped command reference: its display heading,
/// a URL-safe `slug` (the basename of its generated `reference/<slug>.md`
/// page in the mdBook), and the ordered command slots it contains.
///
/// This one table is the single source of truth for **two** surfaces:
/// the terminal `cs help` grouped reference ([`print_grouped_reference`])
/// and the generated mdBook Reference section
/// ([`crate::cmd::markdown_help`]). Keeping both readers on one map is the
/// anti-drift discipline of ADR-B1′ §5.3 — the taxonomy lives here, never
/// duplicated in clap attributes (clap 4.6 has no subcommand-grouping
/// feature) nor in the book source.
pub(crate) struct CommandGroup {
    /// Human-facing heading, e.g. `"Molecule lifecycle"`.
    pub title: &'static str,
    /// Basename of the generated `reference/<slug>.md` page.
    pub slug: &'static str,
    /// Ordered command slots rendered under this heading.
    pub slots: Vec<CommandSlot>,
}

/// Category layout for the grouped command reference.
///
/// Category membership is a UX decision that stays hand-written — it
/// is not encoded anywhere in clap metadata. Descriptions, however,
/// are derived from the real clap tree at render time (see
/// [`resolve_group`]), so changing a subcommand's doc comment in
/// `main.rs` is sufficient to update `cs help`.
///
/// The set of `Derived` command names across every group is also the
/// **allowlist** for the generated Reference (ADR-B1′ §5.2): only these
/// user-facing verbs are published; every other top-level command carries
/// `#[command(hide = true)]` and is omitted from both surfaces.
///
/// "Synthetic" entries (like `guide`) are not real clap subcommands;
/// their descriptions are provided inline and fall through the
/// resolver. They are skipped by the markdown generator (no `--help`
/// to render).
// This is a flat declarative table (7 command groups × their slots), not
// control flow. The length is data, so `too_many_lines` does not apply.
#[allow(clippy::too_many_lines)]
pub(crate) fn command_group_layout() -> Vec<CommandGroup> {
    use CommandSlot::{Derived, Synthetic};
    vec![
        CommandGroup {
            title: "Molecule lifecycle",
            slug: "lifecycle",
            slots: vec![
                Derived("spark"),
                Derived("drop"),
                Derived("listen"),
                Derived("nucleate"),
                Derived("observe"),
                Derived("evolve"),
                Derived("complete"),
                Derived("collapse"),
                Derived("stuck"),
                Derived("await-operator"),
                Derived("freeze"),
                Derived("thaw"),
                Derived("decay"),
                Derived("merge"),
                Derived("transform"),
                Derived("tag"),
            ],
        },
        CommandGroup {
            title: "Fleet management",
            slug: "fleet",
            slots: vec![
                Derived("ensemble"),
                Derived("purge"),
                Derived("kill"),
                Derived("quench"),
                Derived("teardown"),
                Derived("resume"),
                Derived("patrol"),
                Derived("fleet"),
                Derived("wait"),
            ],
        },
        CommandGroup {
            title: "Execution",
            slug: "execution",
            slots: vec![
                Derived("tackle"),
                Derived("done"),
                Derived("sync"),
                Derived("harvest"),
                Derived("run"),
                Derived("spore"),
            ],
        },
        CommandGroup {
            title: "Project",
            slug: "project",
            slots: vec![
                Derived("init"),
                Derived("trust"),
                Derived("config"),
                Derived("status"),
                Derived("project"),
                Derived("reconcile"),
                Derived("scheduler"),
                Derived("daemons"),
                Derived("migrate"),
                Derived("deps"),
                Derived("mission"),
                Derived("diverge"),
                Derived("galaxies"),
                Derived("topology"),
            ],
        },
        // [B1′: R10] The old catch-all "Tools" group split by its real
        // organizing principle. Observability = watching the fleet run;
        // Integrity & audit = proving what the fleet did (the "trace
        // matters" wedge). health/pulse move here from Fleet — they read
        // fleet *health*, an observability concern.
        CommandGroup {
            title: "Observability",
            slug: "observability",
            slots: vec![
                Derived("peek"),
                Derived("tail"),
                Derived("errors"),
                Derived("health"),
                Derived("pulse"),
                Derived("doctor"),
            ],
        },
        CommandGroup {
            title: "Integrity & audit",
            slug: "integrity",
            slots: vec![
                Derived("verify"),
                Derived("verify-trace"),
                Derived("verify-graph"),
                Derived("spec-audit"),
                Derived("release-audit"),
                Derived("notarize"),
                Derived("witness"),
                Derived("key"),
            ],
        },
        CommandGroup {
            title: "Tools",
            slug: "tools",
            slots: vec![
                Synthetic("guide", "Operator handbook"),
                Derived("pilot"),
                Derived("prime"),
                Derived("paths"),
                Derived("archive"),
                Derived("session"),
                Derived("inbox"),
                Derived("panel"),
                Derived("notify"),
                Derived("opt-in-share"),
                Derived("demo"),
                Derived("whisper"),
            ],
        },
    ]
}

/// A slot in the grouped command reference.
#[derive(Clone, Copy)]
pub(crate) enum CommandSlot {
    /// A real clap subcommand — description is resolved from
    /// `Cli::command().find_subcommand(name).get_about()`.
    Derived(&'static str),
    /// A pseudo-command (not in the clap tree). Description is inline.
    Synthetic(&'static str, &'static str),
}

/// Resolve a category layout into concrete `(name, description)` pairs
/// by reading subcommand metadata from the clap tree.
///
/// Any `Derived` slot whose name does not exist as a subcommand of the
/// root `cs` command is dropped from the rendered output — this makes
/// the function resilient to typos but relies on the
/// `help_goldens` integration test to catch them in CI.
fn resolve_group(root: &clap::Command, slots: &[CommandSlot]) -> Vec<(&'static str, String)> {
    slots
        .iter()
        .filter_map(|slot| match *slot {
            CommandSlot::Derived(name) => root
                .find_subcommand(name)
                .and_then(|cmd| cmd.get_about().map(|a| (name, a.to_string()))),
            CommandSlot::Synthetic(name, desc) => Some((name, desc.to_string())),
        })
        .collect()
}

/// Print the full command reference, grouped by category.
#[allow(clippy::too_many_lines)]
fn print_grouped_reference() {
    let version = env!("CARGO_PKG_VERSION");
    println!("cs {version} — Cosmon agent orchestrator\n");
    println!("Usage: cs [OPTIONS] <COMMAND>\n");

    let root = Cli::command();
    for group in command_group_layout() {
        println!("{}:", group.title);
        for (name, desc) in resolve_group(&root, &group.slots) {
            println!("  {name:<20} {desc}");
        }
        println!();
    }

    println!("Global options:");
    println!("  --config <PATH>      Path to configuration file");
    println!("  --verbose, -v        Enable verbose output");
    println!("  --json               Output in JSON format");
    println!();
    println!("Pilot workflow — the full cycle is nucleate → tackle → wait → done:");
    println!("  cs nucleate ...              # create molecule");
    println!("  cs tackle <id>               # spawn ONE worker (always leaf, no DAG walk)");
    println!("  cs wait <id> &               # background wait, get notified");
    println!("                               # pilot stays free for other work");
    println!("  cs done <id>                 # merge branch + teardown (required!)");
    println!();
    println!("  For a DAG of N≥1 nodes (one node = leaf, N nodes = orchestration):");
    println!("  cs run <root> --poll-interval 5     # walks the DAG, dispatches each");
    println!("                                       # ready node via cs tackle, calls");
    println!("                                       # cs done on completion. Wrap in");
    println!("                                       # detached tmux to free the pilot:");
    println!("  tmux new -d -s runtime cs run <root>");
    println!();
    println!("  cs tackle = one node (single worker, no DAG walk).");
    println!("  cs run    = N nodes (resident runtime, walks the DAG).");
    println!("  Picking the verb is the choice; --leaf and --force-runtime are gone.");
    println!();
    println!("  Cross-galaxy edges (Phase 1, ADR-035):");
    println!("  cs nucleate ... --blocked-by <alias>:<mol_id>    # alias resolves via");
    println!("    cosmon-registry, ~/.cosmon/galaxy-aliases.toml, or ~/galaxies/<alias>/");
    println!("  cs nucleate ... --blocks <alias>@<mol_id>        # `:` and `@` accepted");
    println!("    (one-writer-per-galaxy: edge recorded locally only; remote galaxy is");
    println!("     probed best-effort and a warning is printed if it cannot be reached)");
    println!();
    println!("  Grafting onto a completed DAG? See docs/tutorials/graft-dag.md");
    println!("  One primitive, three patterns:       docs/handbook.md#one-primitive");
    println!();
    println!("  NEVER poll 'cs observe' in a loop — use 'cs wait' instead.");
    println!("  NEVER skip 'cs done' — without it the branch never merges to main.");
    println!();
    println!("Monitoring — the operator's toolkit (use these, not tmux/tail/cat):");
    println!("  cs peek                      # watchdog TUI — every unfinished molecule");
    println!("                               #   (running, pending, frozen, starved; the");
    println!("                               #   archive hidden by default; press A to");
    println!("                               #   cycle unfinished → all → unfinished, or");
    println!("                               #   pass --phase on the CLI)");
    println!("                               #   p = tmux pane capture, j/k = navigate");
    println!("                               #   b/l/e/s/r/N/g/T/v/X = briefing/log/events/");
    println!(
        "                               #   synthesis/responses/notes/git/tree/verify/eXceptions"
    );
    println!("                               #   +/-/= = zoom ville → immeuble → peau");
    println!("                               #   n/t/m/w/. = nucleate/tackle/done/whisper/note");
    println!("                               #           (docs/guides/peek-zoom.md)");
    println!("  cs peek --phase done,failed  # + the archive (completed + collapsed)");
    println!("                               #   --phase is the temporality axis and");
    println!("                               #   says nothing about the project scope");
    println!("  cs peek --all-galaxies       # the perimeter axis: same phases, every");
    println!("                               #   project (same word as cs tail)");
    println!("  cs peek --all                # sugar for --all-galaxies --phase all:");
    println!("                               #   everything, every project, archive");
    println!("                               #   included (multi-galaxy)");
    println!("  cs peek --snapshot           # byte-deterministic 120-col view");
    println!("                               #   (same bytes on any device, cf.");
    println!("                               #    docs/guides/peek-snapshot.md)");
    println!("                               #   --phase/--all widen the wheat-paste byte");
    println!("                               #   stream the same way.");
    println!("  cs peek --json               # machine view: status + heartbeat +");
    println!("                               #   last_activity per molecule. Raw core");
    println!("                               #   status, same word as cs observe --json.");
    println!("  cs ensemble --tag temp:hot   # actionable backlog snapshot");
    println!("  cs wait <id> &               # block on a worker (pilot stays responsive)");
    println!();
    println!("  NEVER 'tmux attach' to a worker — use 'cs peek' + p.");
    println!("  NEVER 'watch cs observe' or 'while cs observe' — use 'cs wait'.");
    println!("  NEVER 'tail -f events.jsonl' by hand — use 'cs peek' event tab.");
    println!();
    println!("Registering a service — two métiers, two tools:");
    println!("  cosmon-scheduler     the house's alarm clock. Periodic, short-lived gestures.");
    println!("                       TOML: ~/.config/cosmon/patrols.toml");
    println!("                       Operator view: cs scheduler status");
    println!("                       Use for: cron/interval fires (executor-pulse every 2h,");
    println!("                                chronicle-lint Sunday 09:00, WhatsApp sync 15min).");
    println!();
    println!("  cosmon-daemon-supervisor   the night watchman. Long-running processes that must");
    println!("                             stay alive (Telegram bot, Emacs daemon, MCP servers).");
    println!("                             TOML: ~/.config/cosmon/daemons.toml");
    println!(
        "                             Operator views: cs daemons list | status | logs | reload"
    );
    println!();
    println!("  Decision rule — does the command finish on its own?");
    println!("    Yes, and I want it re-fired on a cadence         -> scheduler (patrols.toml)");
    println!(
        "    No, it runs forever and must be restarted if it dies -> supervisor (daemons.toml)"
    );
    println!();
    println!("  Both share one kill-switch: `touch ~/.cosmon/stand-down.lock` silences every");
    println!("  patrol and SIGTERMs every supervised daemon until the file is removed.");
    println!();
    println!("  See 'cs help scheduler' and 'cs help daemons' for config examples, hot-reload,");
    println!("  and the canonical 'réveil / veilleur de nuit' image (chronicle");
    println!("  2026-04-19 'Deux métiers, deux outils').");
    println!();
    println!("  Future: dynamic registration. Today both layers are TOML-edited by hand.");
    println!("  A planned design (see `cs ensemble --tag temp:warm | grep register`) will let");
    println!("  a service register itself at runtime via an event/API/molecule, no manual edit.");
    println!("  Until then, edit the TOML and the layer picks it up on its next tick (scheduler)");
    println!("  or via `cs daemons reload` (supervisor).");
    println!();
    println!("Run 'cs help <command>' for detailed help on a specific command.");
    println!("Run 'cs help guide' for the operator handbook.");
    println!("Run 'cs help charter' to see the unified visual charter swatch.");
}
