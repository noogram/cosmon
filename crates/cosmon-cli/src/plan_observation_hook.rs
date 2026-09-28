// SPDX-License-Identifier: AGPL-3.0-only

//! Internal status-line pipe, invoked only by explicit worker launch settings.
//! It forwards stdin verbatim, persists only allowlisted plan fields, and skips
//! ordinary CLI startup. It never reads or modifies provider configuration.

use cosmon_core::{
    id::WorkerId,
    plan_observation::{claude_plan, PlanObservationStore, PlanSource},
};
use cosmon_state::plan_observation::FilePlanObservationStore;
use std::io::{self, Read, Write};

/// Intercept `cs plan-observation-hook ROOT WORKER` before CLI initialization.
///
/// The caller must exit with the returned code. Persistence failure cannot
/// break an existing status line: its complete input is still passed through.
/// The collector has no stdout of its own and never logs the private payload.
#[must_use]
pub fn intercept() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.to_str()? != "plan-observation-hook" {
        return None;
    }
    let root = args.next();
    let worker = args
        .next()
        .and_then(|s| s.to_str().and_then(|s| WorkerId::new(s).ok()));
    let extra = args.next().is_some();
    let input = io::stdin();
    let mut output = io::stdout().lock();
    // Bound parsing memory. Oversized input is still forwarded in full but is
    // not stored. A status-line payload should be far smaller than one MiB.
    let mut input = input.lock();
    let mut bytes = Vec::new();
    if input
        .by_ref()
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Some(0);
    }
    if !extra && bytes.len() <= 1_048_576 {
        if let (Some(root), Some(worker), Ok(raw)) = (root, worker, std::str::from_utf8(&bytes)) {
            let sample = claude_plan(raw, PlanSource::ClaudeStatusLine, chrono::Utc::now());
            let _ = FilePlanObservationStore::new(root.into()).save(&worker, &sample);
        }
    }
    let _ = output.write_all(&bytes);
    let _ = io::copy(&mut input, &mut output);
    Some(0)
}
