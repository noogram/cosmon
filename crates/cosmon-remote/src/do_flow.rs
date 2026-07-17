// SPDX-License-Identifier: AGPL-3.0-only

//! `do` — the one-gesture composition nucleate → credit guard →
//! tackle → follow.
//!
//! PURELY client-side: this module composes three existing §8p routes
//! (`POST /v1/molecules`, `POST /v1/molecules/{id}/tackle`,
//! `GET /v1/molecules/{id}`) plus the best-effort `GET /v1/events`
//! tail. Zero new routes; doctrine §5.1 untouched — operator-side the
//! pilot keeps the tackle gesture, tenant-side it is the tenant's own
//! molecule and budget, and `molecule nucleate` alone stays available
//! as the advanced path.
//!
//! The golden first hour becomes `login → do → result`
//! (4 gestures instead of 10).
//!
//! # The credit guard
//!
//! The FIRST spend of the flow is the tackle (nucleate writes state,
//! burns nothing). Before it, `do` shows [`CREDIT_GUARD_PROMPT`] once:
//! a confirmed interactive *yes* is persisted via [`GuardMemory`]
//! (`credit_guard_acknowledged` in `config.toml`), so the question is
//! asked exactly once per install — the D-AVATAR interruption
//! asymmetry applied to the client's wallet. `--yes` skips the prompt
//! for scripts WITHOUT persisting (a script's consent is not the
//! operator's).

use std::collections::BTreeMap;
use std::time::Duration;

use crate::client::{Client, NucleateRequest, SseEvent};
use crate::error::{Error, Result};

/// The credit-guard prompt shown before the first worker dispatch.
/// One line, the essential semantics only: an agent launches, credit
/// burns, nothing has been spent yet.
pub const CREDIT_GUARD_PROMPT: &str =
    "⚠ this dispatches an agent worker — it LAUNCHES AN AGENT and BURNS CREDIT \
     (Anthropic billing). Continue? [y/N] ";

/// Where the one-time guard acknowledgment is remembered.
///
/// Implemented by [`crate::config::ProfileStore`] (persists
/// `credit_guard_acknowledged = true` in `config.toml`) and by an
/// in-RAM double in tests.
pub trait GuardMemory {
    /// Whether the operator has already acknowledged the guard.
    fn acknowledged(&self) -> bool;
    /// Persist the acknowledgment (called after an interactive *yes*).
    fn remember(&mut self) -> Result<()>;
}

impl GuardMemory for crate::config::ProfileStore {
    fn acknowledged(&self) -> bool {
        self.read_top()
            .is_ok_and(|t| t.credit_guard_acknowledged.unwrap_or(false))
    }

    fn remember(&mut self) -> Result<()> {
        let mut top = self.read_top()?;
        top.credit_guard_acknowledged = Some(true);
        self.write_top(&top)
    }
}

/// In-RAM [`GuardMemory`] — tests and ephemeral runs.
#[derive(Debug, Default)]
pub struct EphemeralGuardMemory {
    acked: bool,
}

impl GuardMemory for EphemeralGuardMemory {
    fn acknowledged(&self) -> bool {
        self.acked
    }

    fn remember(&mut self) -> Result<()> {
        self.acked = true;
        Ok(())
    }
}

/// Options of one `do` invocation.
#[derive(Debug, Clone)]
pub struct DoOptions {
    /// Formula to nucleate (default `task-work` — the standard
    /// one-shot work unit; `--formula` overrides).
    pub formula: String,
    /// Optional molecule kind.
    pub kind: Option<String>,
    /// Variables (the topic rides here as `topic`).
    pub variables: BTreeMap<String, String>,
    /// Tags.
    pub tags: Vec<String>,
    /// Skip the credit guard without persisting (scripts/CI).
    pub assume_yes: bool,
    /// Poll cadence for the observe loop.
    pub poll_interval: Duration,
    /// Give-up deadline for the follow phase. Reaching it is NOT an
    /// error — the worker keeps running server-side; `do` reports how
    /// to pick the result up later.
    pub poll_timeout: Duration,
    /// Tail `GET /v1/events` for this molecule while polling
    /// (best-effort: a dropped stream never fails the flow).
    pub follow_events: bool,
}

impl Default for DoOptions {
    fn default() -> Self {
        Self {
            formula: "task-work".to_owned(),
            kind: None,
            variables: BTreeMap::new(),
            tags: Vec::new(),
            assume_yes: false,
            poll_interval: Duration::from_secs(5),
            poll_timeout: Duration::from_secs(1800),
            follow_events: true,
        }
    }
}

/// What one `do` produced.
#[derive(Debug, Clone)]
pub struct DoOutcome {
    /// The nucleated molecule.
    pub molecule_id: String,
    /// Terminal status when the follow phase saw one (`completed`,
    /// `failed`, `collapsed`); `None` when the poll deadline passed
    /// first (worker still running server-side).
    pub terminal_status: Option<String>,
    /// Whether the credit guard was displayed this run.
    pub guard_shown: bool,
}

/// Statuses after which polling stops.
fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "collapsed")
}

/// Run the composition. `confirm` is the interactive edge (reads one
/// answer for the guard prompt); `progress` receives human-readable
/// step lines (the CLI prints them, tests collect them).
///
/// # Errors
///
/// - any wire error from nucleate / tackle / observe;
/// - [`Error::Auth`] with a `credit guard declined` message when the
///   operator answers no — the molecule stays pending and the message
///   names the manual dispatch gesture.
pub async fn run_do<G, C, P>(
    client: &Client,
    opts: DoOptions,
    guard: &mut G,
    mut confirm: C,
    mut progress: P,
) -> Result<DoOutcome>
where
    G: GuardMemory,
    C: FnMut(&str) -> std::io::Result<bool>,
    P: FnMut(&str),
{
    // 1. Nucleate — writes tenant state, burns nothing.
    let body = NucleateRequest {
        formula: opts.formula.clone(),
        kind: opts.kind.clone(),
        variables: opts.variables.clone(),
        tags: opts.tags.clone(),
    };
    let env = client.nucleate(&body).await?;
    let molecule_id = env.molecule.id.clone();
    progress(&format!("nucleated: {molecule_id}"));

    // 2. Credit guard — BEFORE the first spend (the tackle). Asked
    //    once: a confirmed yes is remembered; `--yes` skips without
    //    remembering.
    let mut guard_shown = false;
    if !opts.assume_yes && !guard.acknowledged() {
        guard_shown = true;
        let granted = confirm(CREDIT_GUARD_PROMPT).map_err(Error::Io)?;
        if !granted {
            return Err(Error::Auth(format!(
                "credit guard declined — molecule {molecule_id} stays pending \
                 (no credit spent); dispatch it later with \
                 `molecule tackle {molecule_id}`"
            )));
        }
        guard.remember()?;
        progress("credit guard acknowledged (remembered — asked once)");
    }

    // 3. Tackle — the spend.
    let tackled = client.tackle(&molecule_id).await?;
    progress(&format!(
        "tackled: {} worker={}",
        tackled.tackle.molecule_id,
        tackled.tackle.worker_session.as_deref().unwrap_or("-"),
    ));

    // 4. Follow. Best-effort SSE tail in the background (a dropped
    //    stream is silent — the observe poll below is the
    //    authoritative terminator), observe poll in the foreground.
    let sse_task = if opts.follow_events {
        let sse_client = client.clone();
        let sse_id = molecule_id.clone();
        Some(tokio::spawn(async move {
            let _ = sse_client
                .events_stream(Some(&sse_id), None, |evt: SseEvent| {
                    eprintln!("event: {} {}", evt.event, evt.data);
                })
                .await;
        }))
    } else {
        None
    };

    let deadline = tokio::time::Instant::now() + opts.poll_timeout;
    let mut last_status = String::from("pending");
    let terminal_status = loop {
        tokio::time::sleep(opts.poll_interval).await;
        let observed = client.get_molecule(&molecule_id).await?;
        let status = observed.molecule.status.clone();
        if status != last_status {
            progress(&format!("status: {last_status} → {status}"));
            last_status.clone_from(&status);
        }
        if is_terminal(&status) {
            break Some(status);
        }
        if tokio::time::Instant::now() >= deadline {
            progress(&format!(
                "follow deadline reached — the worker keeps running; \
                 check later with `molecule result {molecule_id}`"
            ));
            break None;
        }
    };

    if let Some(task) = sse_task {
        task.abort();
    }

    Ok(DoOutcome {
        molecule_id,
        terminal_status,
        guard_shown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_statuses_are_the_closed_set() {
        for s in ["completed", "failed", "collapsed"] {
            assert!(is_terminal(s), "{s} must terminate the follow loop");
        }
        for s in ["pending", "running", "frozen", "stuck"] {
            assert!(!is_terminal(s), "{s} must keep polling");
        }
    }

    #[test]
    fn guard_prompt_names_the_spend() {
        // The gate (delib T4): the guard must say an agent launches
        // and credit burns BEFORE the first spend. Pin the two
        // load-bearing words.
        assert!(CREDIT_GUARD_PROMPT.contains("AGENT"));
        assert!(CREDIT_GUARD_PROMPT.contains("CREDIT"));
    }

    #[test]
    fn ephemeral_memory_remembers() {
        let mut m = EphemeralGuardMemory::default();
        assert!(!m.acknowledged());
        m.remember().unwrap();
        assert!(m.acknowledged());
    }
}
