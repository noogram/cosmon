// SPDX-License-Identifier: AGPL-3.0-only

//! `wait` — the ONE polling loop of the tenant client.
//!
//! # Why the wait lives here and not on the server
//!
//! `cs wait` blocks until a molecule reaches a status. The obvious remote
//! translation — a route that blocks — is the wrong one: it makes the
//! adapter hold a thread, a connection and a timeout negotiation per
//! waiting client, and it makes the adapter own a piece of state ("who is
//! waiting on what") that belongs to nobody but the waiter. The client
//! owns its own patience. What the server owes it is
//! `GET /v1/molecules/{id}/status`, an answer cheap enough to ask for
//! repeatedly, and nothing else.
//!
//! So this module is the whole of `wait`: a bounded loop over a cheap
//! conditional read. Nothing it does creates server-side state, which is
//! the property `tests/wait_flow.rs` pins by counting what the server saw.
//!
//! # One loop, two callers
//!
//! [`poll_until`] is the only polling loop in the crate. `wait` maps its
//! outcome onto exit codes; `do` uses it as the follow phase of its
//! composition and treats a deadline as information rather than failure.
//! They differ in what they *do* with an outcome, never in how they reach
//! it — before this module they were two loops, and the second one had
//! already drifted onto the expensive read.
//!
//! # Exact ids only
//!
//! Like `cs wait`, the molecule argument is an exact id and never a
//! prefix. A prefix resolver would have to list molecules to disambiguate
//! — one expensive read to start a loop built to be cheap — and it would
//! do so once, at the start, so a molecule nucleated during a long wait
//! could make the prefix ambiguous after the loop had already committed
//! to a match. A wait that silently follows the wrong molecule is worse
//! than one that refuses an ambiguous name.

use std::time::Duration;

use crate::client::{Client, StatusPoll};
use crate::error::Result;

/// The statuses `wait` treats as the end of the road when the caller
/// names none.
///
/// This is the client's *request*, not its notion of terminality — the
/// server answers that with the `terminal` field, which is what
/// [`poll_until`] actually stops on. Kept as a default so `wait <id>`
/// with no `--for` means the same thing as `cs wait <id>`.
pub const DEFAULT_TARGETS: &[&str] = &["completed", "collapsed"];

/// Options of one wait.
#[derive(Debug, Clone)]
pub struct WaitOptions {
    /// Statuses that count as success. Matched case-sensitively against
    /// the server's `snake_case` words.
    pub targets: Vec<String>,
    /// Give-up deadline.
    pub timeout: Duration,
    /// Cadence between polls. Clamped to the remaining budget by
    /// [`poll_until`], so a value larger than `timeout` still terminates
    /// on time.
    pub poll_interval: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            targets: DEFAULT_TARGETS.iter().map(|s| (*s).to_owned()).collect(),
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// How a wait ended.
///
/// Three outcomes, not two: a molecule that collapses while you waited
/// for `completed` did not time out and did not succeed, and collapsing
/// those two into one answer is what makes a script retry a molecule that
/// will never move again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// A requested status was reached.
    Reached(WaitReport),
    /// The molecule reached a terminal status that was *not* requested.
    OtherTerminal(WaitReport),
    /// The deadline passed with the molecule still running.
    TimedOut(WaitReport),
}

impl WaitOutcome {
    /// The report, whatever the outcome.
    #[must_use]
    pub fn report(&self) -> &WaitReport {
        match self {
            Self::Reached(r) | Self::OtherTerminal(r) | Self::TimedOut(r) => r,
        }
    }

    /// Stable slug for `--json` and for tests: `reached` ·
    /// `other_terminal` · `timeout`.
    #[must_use]
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Reached(_) => "reached",
            Self::OtherTerminal(_) => "other_terminal",
            Self::TimedOut(_) => "timeout",
        }
    }
}

/// What one wait observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitReport {
    /// The molecule waited on.
    pub molecule_id: String,
    /// Last status seen.
    pub status: String,
    /// Last phase seen.
    pub phase: String,
    /// Whether the last status was terminal, per the server.
    pub terminal: bool,
    /// Polls issued, including the first.
    pub polls: u32,
    /// Polls the server answered `304` — the ones that cost no body.
    pub unchanged_polls: u32,
    /// Observed status changes.
    pub transitions: u32,
    /// Wall-clock spent waiting.
    pub elapsed: Duration,
}

/// Poll `GET /v1/molecules/{id}/status` until a target is reached, the
/// molecule ends some other way, or the deadline passes.
///
/// The first poll happens **before** the first sleep, so an
/// already-terminal molecule returns immediately with one poll and no
/// wait — the idempotence `cs wait` has.
///
/// # The deadline bounds the request, not only the sleep
///
/// `opts.timeout` is an absolute deadline computed once, and *every*
/// request is issued with the budget that is left. A request still in
/// flight when the budget runs out is abandoned and reported as
/// [`WaitOutcome::TimedOut`]; the loop also refuses to start a further
/// poll once the deadline has passed. That is deliberate even when the
/// late answer would have been a success: the caller's contract is its
/// own clock, not the server's. A `--timeout 2` that returns a
/// completion at five seconds has answered a question nobody asked, and
/// the script that set the budget cannot tell that from a fast answer.
///
/// This is separate from the profile's transport timeout, which bounds a
/// single request against a dead peer and knows nothing about how long
/// this wait has left. Whichever is shorter wins; only this one is what
/// the caller asked for.
///
/// Every poll after the first is conditional on the previous answer's
/// entity-tag, so a molecule that is not moving costs a `304` and no
/// body. `progress` is called once per observed transition (never per
/// poll): a loop that prints on every tick is a loop nobody leaves
/// running.
///
/// # Errors
///
/// Any wire error from the status read. A missing molecule surfaces as
/// the server's `404` rather than a silent forever-wait.
pub async fn poll_until<P>(
    client: &Client,
    molecule_id: &str,
    opts: &WaitOptions,
    mut progress: P,
) -> Result<WaitOutcome>
where
    P: FnMut(&str, &str),
{
    let started = tokio::time::Instant::now();
    let deadline = started + opts.timeout;
    // A zero interval would spin; a huge one must not outlive the budget.
    let interval = opts.poll_interval.max(Duration::from_millis(1));

    let mut etag: Option<String> = None;
    let mut last: Option<(String, String, bool)> = None;
    let mut polls: u32 = 0;
    let mut unchanged_polls: u32 = 0;
    let mut transitions: u32 = 0;

    // Built once so the deadline paths and the terminal paths report the
    // same shape; `last` is the answer seen INSIDE the budget, which is
    // the only one the caller was promised.
    macro_rules! timed_out {
        () => {{
            let (status, phase, terminal) = last.clone().unwrap_or_else(|| {
                // Only reachable when every poll so far was a `304` with no
                // prior body — a server answering conditionally to a tag we
                // never received — or when the very first request outlived
                // the budget. Report it as unknown rather than invent one.
                ("unknown".to_owned(), "unknown".to_owned(), false)
            });
            return Ok(WaitOutcome::TimedOut(WaitReport {
                molecule_id: molecule_id.to_owned(),
                status,
                phase,
                terminal,
                polls,
                unchanged_polls,
                transitions,
                elapsed: started.elapsed(),
            }));
        }};
    }

    loop {
        // What is left of the budget bounds the request itself. Without
        // this the sleep is clamped but the next request is not, so a slow
        // answer can win minutes after the deadline the caller advertised.
        // The first poll is not exempt — it is immediate (no sleep precedes
        // it), not unbounded.
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            timed_out!();
        }
        let poll =
            match tokio::time::timeout(remaining, client.get_status(molecule_id, etag.as_deref()))
                .await
            {
                Ok(result) => result?,
                // The request was still outstanding when the budget ran out.
                // That is the promised outcome, not a transport error.
                Err(_elapsed) => timed_out!(),
            };
        polls = polls.saturating_add(1);
        match poll {
            StatusPoll::NotModified { etag: tag } => {
                unchanged_polls = unchanged_polls.saturating_add(1);
                if tag.is_some() {
                    etag = tag;
                }
            }
            StatusPoll::Fresh {
                etag: tag,
                envelope,
            } => {
                etag = tag;
                let current = (
                    envelope.status.clone(),
                    envelope.phase.clone(),
                    envelope.terminal,
                );
                let moved = last.as_ref().map(|(s, _, _)| s.as_str()) != Some(current.0.as_str());
                if moved {
                    if let Some((from, _, _)) = &last {
                        transitions = transitions.saturating_add(1);
                        progress(from, &current.0);
                    }
                    last = Some(current);
                }
            }
        }

        // A `304` means the previous answer still stands, so the verdict is
        // read from `last` either way — which is why an unchanged poll can
        // still terminate a wait on an already-terminal molecule.
        if let Some((status, phase, terminal)) = &last {
            let report = |polls, unchanged_polls, transitions| WaitReport {
                molecule_id: molecule_id.to_owned(),
                status: status.clone(),
                phase: phase.clone(),
                terminal: *terminal,
                polls,
                unchanged_polls,
                transitions,
                elapsed: started.elapsed(),
            };
            if opts.targets.iter().any(|t| t == status) {
                return Ok(WaitOutcome::Reached(report(
                    polls,
                    unchanged_polls,
                    transitions,
                )));
            }
            if *terminal {
                return Ok(WaitOutcome::OtherTerminal(report(
                    polls,
                    unchanged_polls,
                    transitions,
                )));
            }
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            timed_out!();
        }
        // Clamp to what is left, so `--poll-interval 100 --timeout 3`
        // wakes at 3 s rather than at 100.
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(interval.min(remaining)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_targets_are_the_cs_wait_defaults() {
        assert_eq!(WaitOptions::default().targets, ["completed", "collapsed"]);
        assert_eq!(WaitOptions::default().timeout, Duration::from_secs(600));
        assert_eq!(WaitOptions::default().poll_interval, Duration::from_secs(5));
    }

    #[test]
    fn the_three_outcomes_have_distinct_slugs() {
        let r = WaitReport {
            molecule_id: "task-20260907-b25f".to_owned(),
            status: "running".to_owned(),
            phase: "live".to_owned(),
            terminal: false,
            polls: 1,
            unchanged_polls: 0,
            transitions: 0,
            elapsed: Duration::ZERO,
        };
        let slugs = [
            WaitOutcome::Reached(r.clone()).slug(),
            WaitOutcome::OtherTerminal(r.clone()).slug(),
            WaitOutcome::TimedOut(r).slug(),
        ];
        assert_eq!(slugs, ["reached", "other_terminal", "timeout"]);
        let unique: std::collections::BTreeSet<_> = slugs.iter().collect();
        assert_eq!(unique.len(), 3, "a script matches on these");
    }
}
