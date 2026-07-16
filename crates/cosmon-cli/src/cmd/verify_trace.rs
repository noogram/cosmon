// SPDX-License-Identifier: AGPL-3.0-only

//! `cs verify-trace` — replay an events.jsonl log against the scheduler spec.
//!
//! This is the Phase 3 CI-gate entry point.
//! It consumes an NDJSON events log and either certifies the trace as a
//! refinement of the shipped invariant set or reports the first violation.
//!
//! Wheeler's key insight (insight I1 of the synthesis): `events.jsonl` is
//! already the labeled-transition-system trace of a cosmon run. The
//! validator therefore does not need to explore a state space — it only
//! has to check that each observed transition is legal.
//!
//! Usage:
//!
//! ```text
//! cs verify-trace path/to/events.jsonl
//! cs verify-trace .cosmon/state/events.jsonl --json
//! cat events.jsonl | cs verify-trace -
//! ```

use std::io::Read;
use std::path::PathBuf;

use cosmon_verify::{baseline_invariants, TraceValidator, ValidationError, ValidationOutcome};

use super::Context;

/// Arguments for `cs verify-trace`.
#[derive(clap::Args)]
pub struct Args {
    /// Path to the `events.jsonl` trace. Use `-` to read from stdin.
    pub trace: PathBuf,

    /// Tolerate lines whose shape is not recognised by `EventV2` or the
    /// legacy migration helper — they are counted and skipped instead of
    /// failing the whole replay. Required when replaying historical fleet
    /// logs that pre-date the canonical schema.
    #[arg(long)]
    pub skip_unknown: bool,
}

/// Run the trace validator and report the outcome.
///
/// Exit code is 0 on certification, 1 on violation or parse error. The
/// caller (the top-level CLI dispatcher) converts the returned `anyhow::Error`
/// into a non-zero exit code — violations are surfaced via a sentinel error
/// so both JSON and human output paths can share the same formatting.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let validator = TraceValidator::new(baseline_invariants()).with_skip_unknown(args.skip_unknown);

    let outcome = if args.trace.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        validator.validate_str(&buf)
    } else {
        validator.validate_path(&args.trace)
    };

    match outcome {
        Ok(ValidationOutcome::Ok {
            events_replayed,
            molecules_seen,
            skipped_unknown,
        }) => {
            if ctx.json {
                let j = serde_json::json!({
                    "status": "ok",
                    "events_replayed": events_replayed,
                    "molecules_seen": molecules_seen,
                    "skipped_unknown": skipped_unknown,
                });
                println!("{j}");
            } else {
                println!(
                    "\u{2705} trace certified: {events_replayed} events, {molecules_seen} molecules, {skipped_unknown} unknown skipped"
                );
            }
            Ok(())
        }
        Ok(ValidationOutcome::Violation {
            events_replayed_before,
            violation,
        }) => {
            if ctx.json {
                let j = serde_json::json!({
                    "status": "violation",
                    "events_replayed_before": events_replayed_before,
                    "violation": violation,
                });
                println!("{j}");
            } else {
                println!("\u{274C} trace violation after {events_replayed_before} events");
                println!("   {violation}");
            }
            anyhow::bail!("trace violation: {violation}")
        }
        Err(ValidationError::Parse { line, source }) => {
            anyhow::bail!("parse error on line {line}: {source}")
        }
        Err(ValidationError::Io(e)) => Err(e.into()),
    }
}
