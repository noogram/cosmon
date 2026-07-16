// SPDX-License-Identifier: AGPL-3.0-only

//! `cs panel` — convene a constitutional panel over an artifact.
//!
//! A panel raises the cost of amending a galaxy's DNA from `O(1 PR)` to
//! `O(panel convocation)`: a supermajority of named perspectives, on the
//! record, rather than a single author and a single merge. This is the
//! cosmon-side primitive behind the *constitutional ratchet*. Use it
//! to gate any amendment of an `operator-uncapturable` DNA bullet, a
//! `forbid_operator_*` lint, or the addition of an `operator_*` field.
//!
//! Two subcommands mirror the two halves of a deliberation:
//!
//! - **`cs panel convene`** — read the artifact (PR diff), hash it, and
//!   deterministically seat the panel: a fixed constitutional *core* plus
//!   one or more *rotating* seats filled by the diff hash. Because the
//!   rotating seat is a pure function of the diff, the convener cannot pick
//!   a friendly judge after seeing the test — the "audience-after-the-test"
//!   pathology. Output is the convened panel.
//! - **`cs panel decide`** — re-seat the same panel from the same diff,
//!   tally the supplied ballots under the supermajority rule (default 4/5),
//!   emit the verdict, and inscribe a [`RoleLog`] to disk. The tally
//!   refuses ballots from non-panelists and refuses to render a verdict
//!   until every seat has voted, so the panel cannot be padded or thinned.
//!
//! The heavy step — actually dispatching each persona to an LLM for a
//! reasoned vote — is intentionally *not* here. This command supplies the
//! uncapturable structure (deterministic seating, exact-quorum tally,
//! durable role-log); the votes are supplied by the convener (today, by
//! hand or by a wrapper that fans out the personas). Keeping dispatch out
//! of the primitive keeps the core auditable and the CLI composable.
//!
//! # Exit codes (`decide`)
//!
//! - `0` — the panel **approved** (supermajority reached).
//! - `2` — the panel **refused** (supermajority not reached).
//! - `1` — error (bad roster, missing votes, I/O failure).

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use cosmon_core::panel::{
    tally, Ballot, PanelComposition, PanelRoster, PanelVerdict, Persona, RoleLog,
    SupermajorityRule, Vote,
};
use cosmon_hash::Hash;

use super::Context;

/// The default constitutional core — four fixed perspectives.
///
/// Matches the cosmon deliberation vocabulary (the recurring `deep-think`
/// panelists). The fifth seat is always hash-pinned from [`DEFAULT_POOL`].
const DEFAULT_CORE: &[&str] = &["wheeler", "torvalds", "feynman", "shannon"];

/// The default rotation pool — the fifth seat is drawn from here by diff hash.
const DEFAULT_POOL: &[&str] = &["jobs", "jr", "godel", "einstein", "hawking", "dirac"];

/// Arguments for `cs panel`.
#[derive(clap::Args)]
pub struct Args {
    /// Sub-command: `convene` (seat the panel) or `decide` (tally + role-log).
    #[command(subcommand)]
    pub command: PanelCommand,
}

/// `cs panel` subcommands.
#[derive(clap::Subcommand)]
pub enum PanelCommand {
    /// Seat the panel deterministically from the artifact hash and print it
    Convene(ConveneArgs),
    /// Tally ballots from the seated panel, emit verdict, inscribe role-log
    Decide(DecideArgs),
}

/// Shared roster flags for both subcommands.
#[derive(clap::Args, Clone)]
pub struct RosterArgs {
    /// Comma-separated constitutional core (always seated).
    #[arg(long, value_delimiter = ',')]
    pub core: Vec<String>,

    /// Comma-separated rotation pool (hash-pinned seats drawn from here).
    #[arg(long, value_delimiter = ',')]
    pub pool: Vec<String>,

    /// Number of rotating seats filled from the pool by the diff hash.
    #[arg(long, default_value_t = 1)]
    pub seats: usize,

    /// Supermajority rule as `NUM/DEN` (default `4/5`).
    #[arg(long, default_value = "4/5")]
    pub rule: String,
}

/// Arguments for `cs panel convene`.
#[derive(clap::Args)]
pub struct ConveneArgs {
    #[command(flatten)]
    pub roster: RosterArgs,

    /// Path to the artifact (e.g. PR diff). Reads stdin when omitted.
    #[arg(long, value_name = "PATH")]
    pub diff: Option<PathBuf>,

    /// Use a precomputed artifact hash (64-char hex) instead of hashing bytes.
    #[arg(long, value_name = "HEX")]
    pub artifact_hash: Option<String>,
}

/// Arguments for `cs panel decide`.
#[derive(clap::Args)]
pub struct DecideArgs {
    #[command(flatten)]
    pub roster: RosterArgs,

    /// Path to the artifact (e.g. PR diff). Reads stdin when omitted and no
    /// `--artifact-hash` is given.
    #[arg(long, value_name = "PATH")]
    pub diff: Option<PathBuf>,

    /// Use a precomputed artifact hash (64-char hex) instead of hashing bytes.
    #[arg(long, value_name = "HEX")]
    pub artifact_hash: Option<String>,

    /// A ballot, repeatable: `persona=approve` / `persona=refuse[:reason]`.
    #[arg(long = "vote", value_name = "PERSONA=VOTE[:REASON]")]
    pub votes: Vec<String>,

    /// Where to inscribe the role-log JSON. Defaults to stdout (with `--json`)
    /// or no file (human mode prints a summary). Use `-` for stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// Execute the `panel` command.
///
/// # Errors
///
/// Returns an error on roster/parse failure or I/O failure. `decide` may
/// also exit the process with status `2` when the panel refuses.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        PanelCommand::Convene(c) => run_convene(ctx, c),
        PanelCommand::Decide(d) => run_decide(ctx, d),
    }
}

/// Parse a `NUM/DEN` supermajority rule.
fn parse_rule(s: &str) -> anyhow::Result<SupermajorityRule> {
    let (num, den) = s
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("rule must be NUM/DEN, e.g. 4/5; got '{s}'"))?;
    let numerator: u32 = num
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad rule numerator '{num}': {e}"))?;
    let denominator: u32 = den
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad rule denominator '{den}': {e}"))?;
    if denominator == 0 {
        anyhow::bail!("rule denominator must not be zero");
    }
    Ok(SupermajorityRule {
        numerator,
        denominator,
    })
}

/// Build the roster from CLI flags, applying defaults when a list is empty.
fn build_roster(args: &RosterArgs) -> anyhow::Result<PanelRoster> {
    let core_names: Vec<String> = if args.core.is_empty() {
        DEFAULT_CORE.iter().map(|s| (*s).to_owned()).collect()
    } else {
        args.core.clone()
    };
    let pool_names: Vec<String> = if args.pool.is_empty() {
        DEFAULT_POOL.iter().map(|s| (*s).to_owned()).collect()
    } else {
        args.pool.clone()
    };
    let core = core_names
        .iter()
        .map(|s| Persona::new(s.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let pool = pool_names
        .iter()
        .map(|s| Persona::new(s.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let rule = parse_rule(&args.rule)?;
    PanelRoster::new(core, pool, args.seats, rule).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolve the artifact hash from `--artifact-hash`, `--diff`, or stdin.
fn resolve_hash(diff: Option<&PathBuf>, artifact_hash: Option<&String>) -> anyhow::Result<Hash> {
    if let Some(hex) = artifact_hash {
        return hex
            .trim()
            .parse::<Hash>()
            .map_err(|e| anyhow::anyhow!("bad --artifact-hash: {e:?}"));
    }
    let bytes = if let Some(path) = diff {
        std::fs::read(path).map_err(|e| anyhow::anyhow!("read diff {}: {e}", path.display()))?
    } else {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            anyhow::bail!(
                "no artifact: pass --diff <path>, pipe the diff on stdin, \
                 or supply --artifact-hash <hex>"
            );
        }
        let mut buf = Vec::new();
        stdin
            .read_to_end(&mut buf)
            .map_err(|e| anyhow::anyhow!("read stdin: {e}"))?;
        buf
    };
    Ok(Hash::of_bytes(&bytes))
}

/// Convene and print the panel.
fn run_convene(ctx: &Context, args: &ConveneArgs) -> anyhow::Result<()> {
    let roster = build_roster(&args.roster)?;
    let hash = resolve_hash(args.diff.as_ref(), args.artifact_hash.as_ref())?;
    let comp = roster.convene(&hash).map_err(|e| anyhow::anyhow!("{e}"))?;

    if ctx.json {
        let out = serde_json::json!({
            "artifact_hash": comp.artifact_hash.to_hex(),
            "core": comp.core.iter().map(Persona::as_str).collect::<Vec<_>>(),
            "pinned": comp.pinned.iter().map(Persona::as_str).collect::<Vec<_>>(),
            "seated": comp.seated.iter().map(Persona::as_str).collect::<Vec<_>>(),
            "panel_size": comp.size(),
            "required": comp.required(),
            "rule": format!("{}/{}", comp.rule.numerator, comp.rule.denominator),
        });
        println!("{out}");
    } else {
        print_composition(&comp);
        println!();
        println!("To decide, collect each seat's vote and run:");
        println!(
            "  cs panel decide --artifact-hash {} \\",
            comp.artifact_hash.to_hex()
        );
        for p in &comp.seated {
            println!("    --vote {p}=approve \\");
        }
        println!("    --out panel.role-log.json");
    }
    Ok(())
}

/// Parse a single `persona=vote[:reason]` ballot string.
fn parse_vote(spec: &str) -> anyhow::Result<Ballot> {
    let (persona_str, rest) = spec
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("vote must be PERSONA=VOTE[:REASON]; got '{spec}'"))?;
    let (vote_str, reason) = match rest.split_once(':') {
        Some((v, r)) => (v, Some(r.trim().to_owned())),
        None => (rest, None),
    };
    let persona = Persona::new(persona_str).map_err(|e| anyhow::anyhow!("{e}"))?;
    let vote = vote_str
        .parse::<Vote>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let reason = reason.filter(|r| !r.is_empty());
    Ok(Ballot::new(persona, vote, reason))
}

/// Tally ballots, emit verdict, inscribe role-log.
fn run_decide(ctx: &Context, args: &DecideArgs) -> anyhow::Result<()> {
    let roster = build_roster(&args.roster)?;
    let hash = resolve_hash(args.diff.as_ref(), args.artifact_hash.as_ref())?;
    let comp = roster.convene(&hash).map_err(|e| anyhow::anyhow!("{e}"))?;

    let ballots = args
        .votes
        .iter()
        .map(|s| parse_vote(s))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let log = tally(&comp, &ballots).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Inscribe the role-log if requested.
    let serialized = serde_json::to_vec_pretty(&log)?;
    if let Some(out) = &args.out {
        if out.as_os_str() == "-" {
            println!("{}", String::from_utf8_lossy(&serialized));
        } else {
            std::fs::write(out, &serialized)
                .map_err(|e| anyhow::anyhow!("write role-log {}: {e}", out.display()))?;
        }
    }

    emit_verdict(ctx, &log, args.out.as_ref());

    // Exit code carries the verdict for scripting / CI gates.
    match log.verdict {
        PanelVerdict::Approve => Ok(()),
        PanelVerdict::Refuse => std::process::exit(2),
    }
}

/// Print a convened composition in human form.
fn print_composition(comp: &PanelComposition) {
    println!(
        "panel convened — {} seats, {}-of-{} to carry",
        comp.size(),
        comp.required(),
        comp.size()
    );
    println!("  artifact : {}", comp.artifact_hash.to_hex());
    let core: Vec<&str> = comp.core.iter().map(Persona::as_str).collect();
    println!("  core     : {}", core.join(", "));
    let pinned: Vec<&str> = comp.pinned.iter().map(Persona::as_str).collect();
    println!(
        "  pinned   : {}  (selected by the diff hash)",
        pinned.join(", ")
    );
}

/// Print the verdict and tally, respecting `--json`.
fn emit_verdict(ctx: &Context, log: &RoleLog, out: Option<&PathBuf>) {
    if ctx.json {
        let obj = serde_json::json!({
            "artifact_hash": log.artifact_hash.to_hex(),
            "verdict": log.verdict,
            "approvals": log.approvals,
            "refusals": log.refusals,
            "required": log.required,
            "panel_size": log.composition.size(),
            "seated": log.composition.seated.iter().map(Persona::as_str).collect::<Vec<_>>(),
            "out": out.map(|p| p.display().to_string()),
        });
        println!("{obj}");
    } else {
        let label = match log.verdict {
            PanelVerdict::Approve => "APPROVE",
            PanelVerdict::Refuse => "REFUSE",
        };
        println!(
            "panel verdict: {label} — {}/{} approved ({} required)",
            log.approvals,
            log.composition.size(),
            log.required
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rule() {
        let r = parse_rule("4/5").unwrap();
        assert_eq!(r.numerator, 4);
        assert_eq!(r.denominator, 5);
        assert!(parse_rule("4").is_err());
        assert!(parse_rule("4/0").is_err());
    }

    #[test]
    fn default_roster_seats_five() {
        let args = RosterArgs {
            core: vec![],
            pool: vec![],
            seats: 1,
            rule: "4/5".into(),
        };
        let roster = build_roster(&args).unwrap();
        assert_eq!(roster.panel_size(), 5);
        assert_eq!(roster.core.len(), 4);
    }

    #[test]
    fn parses_vote_with_and_without_reason() {
        let b = parse_vote("wheeler=approve").unwrap();
        assert_eq!(b.persona.as_str(), "wheeler");
        assert_eq!(b.vote, Vote::Approve);
        assert!(b.reason.is_none());

        let b = parse_vote("torvalds=refuse:breaks the lock invariant").unwrap();
        assert_eq!(b.vote, Vote::Refuse);
        assert_eq!(b.reason.as_deref(), Some("breaks the lock invariant"));

        assert!(parse_vote("no-equals-sign").is_err());
        assert!(parse_vote("wheeler=maybe").is_err());
    }

    #[test]
    fn resolve_hash_prefers_explicit_hex() {
        let hex = Hash::of_bytes(b"x").to_hex();
        let h = resolve_hash(None, Some(&hex)).unwrap();
        assert_eq!(h.to_hex(), hex);
    }

    #[test]
    fn convene_then_decide_is_consistent() {
        // The same hash seats the same panel in both phases.
        let hex = Hash::of_bytes(b"diff-bytes").to_hex();
        let roster = build_roster(&RosterArgs {
            core: vec![],
            pool: vec![],
            seats: 1,
            rule: "4/5".into(),
        })
        .unwrap();
        let h: Hash = hex.parse().unwrap();
        let comp = roster.convene(&h).unwrap();
        // Build approving ballots for every seat → must approve.
        let ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .map(|p| Ballot::new(p.clone(), Vote::Approve, None))
            .collect();
        let log = tally(&comp, &ballots).unwrap();
        assert_eq!(log.verdict, PanelVerdict::Approve);
    }
}
