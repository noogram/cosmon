// SPDX-License-Identifier: AGPL-3.0-only

//! **Pre-registered falsification bench** for mode-C tool-call robustness —
//! the deterministic, in-binary discriminator (task-20260707-5fe6,
//! delib-20260707-df9b §M-BENCH).
//!
//! # Why this file exists
//!
//! A bench that only checks "the worker finished" scores a *lucky run* — one
//! where the local model happened to chunk its script from the first turn and
//! never tripped ollama's server-side tool-call parser — as a PASS, proving
//! nothing about the fix. The panel's load-bearing insight (turing, synthesis
//! Q6) is that the bench must **discriminate a real fix from a lucky run**, and
//! that requires **three-plus** verdicts, not two:
//!
//! - `RECOVERED`   — the 500 **fired** (a `tool_parse_reinject` event is on
//!   disk) AND the loop **survived** with artefacts. The only PASS.
//! - `DIED`        — the 500 **fired**, recovery was **attempted**, and the
//!   worker still ended Stuck with **zero** artefacts. A FAIL.
//! - `INCONCLUSIVE` — the 500 **never fired** (`fired == 0`). Proves nothing.
//! - `AMBIGUOUS`   — fired, but the survival/death signals disagree — never
//!   silently upgraded to PASS.
//!
//! The `fired >= 1` gate is what rejects the lucky run: without it, a run that
//! never exercised the recovery path would score PASS.
//!
//! # Where the discriminating power lives
//!
//! The live shell replay
//! ([`scripts/mode-c-bench/`](../../../../scripts/mode-c-bench)) needs the
//! pinned `gpt-oss:120b` model + the exact ollama build whose parser rejects
//! the whole-script tool call — a JOINT property of model-output-shape ×
//! server-parser. When that pin is absent the honest result is
//! `INCONCLUSIVE-UNAVAILABLE`, and `scripts/mode-c-bench/negative-control.sh`
//! proves power the slow way (two cargo builds across the fix boundary). This
//! file provides the *fast* deterministic proof, against a mock server,
//! exercising cosmon's real [`run_agent_loop`]:
//!
//! - **recovery reaches 200** (500,500,200, retries on) → `RECOVERED`.
//! - **recovery exhausts** (500 forever, retries on) → `DIED`: the same
//!   `tool_parse_reinject` events fire, then the typed `tool_call_parse` fatal
//!   lands, with no artefact.
//! - **no provocation** (server 200s immediately) → `INCONCLUSIVE`
//!   (`fired == 0`).
//!
//! The SAME `ParseErrorResponder` stimulus flips `RECOVERED → DIED` on whether
//! the server ever recovers, and the `INCONCLUSIVE` arm proves the predicate
//! refuses to reward a run that never fired the 500.
//!
//! The marker strings and the verdict table below are mirrored verbatim by
//! `scripts/mode-c-bench/lib.sh` (`classify_verdict` / `batch_predicate`); a
//! drift is caught by `cargo test` here and `classify.sh --selftest` there.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_provider::openai::{run_agent_loop, telemetry_for, OpenAIProvider, RetryPolicy};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ---------------------------------------------------------------------------
// The pre-registered predicate — pure over four disk-derived facts.
// Mirrors lib.sh's `classify_verdict`.
// ---------------------------------------------------------------------------

/// The pre-registered verdicts. Collapsing to a boolean is the mistake that
/// lets a lucky run masquerade as a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// 500 fired, loop survived, artefacts exist — the only PASS.
    Recovered,
    /// 500 fired, recovery attempted, worker Stuck with zero artefacts — FAIL.
    Died,
    /// 500 never fired — proves nothing.
    Inconclusive,
    /// Fired, but survival/death signals disagree — never upgraded to PASS.
    Ambiguous,
}

/// The pre-registered per-run predicate, fixed BEFORE any run. Byte-aligned
/// with `classify_verdict` in `scripts/mode-c-bench/lib.sh`.
fn classify_verdict(fired: u32, completed: bool, artefacts: bool, died: u32) -> Verdict {
    // `fired < 1` → the provocation did not hit → the run proves nothing.
    if fired < 1 {
        return Verdict::Inconclusive;
    }
    if completed && artefacts && died < 1 {
        return Verdict::Recovered;
    }
    if died >= 1 && !artefacts {
        return Verdict::Died;
    }
    Verdict::Ambiguous
}

/// FIRED marker — the typed `AdapterLivenessProbed { Retried, reason }` row a
/// tool-parse recovery writes (delib-20260707-df9b M1 ride-along). Its
/// presence is the `fired >= 1` gate. Mirrors `MARKER_FIRED` in lib.sh.
const MARKER_FIRED: &str = "tool_parse_reinject";

/// DEATH markers — the typed silent-failure Stuck rows an unrecovered fatal
/// writes (`emit_silent_failure`). Mirrors `MARKER_DEATH` in lib.sh.
const MARKER_DEATH: &[&str] = &["tool_call_parse", "SF-1 http", "SF-1 server_error"];

/// Count JSONL lines containing any of `needles` — mirrors lib.sh's
/// `grep -Ec`.
fn count_marker(events_jsonl: &str, needles: &[&str]) -> u32 {
    events_jsonl
        .lines()
        .filter(|line| needles.iter().any(|n| line.contains(n)))
        .count() as u32
}

/// Classify a completed run from its `events.jsonl` content plus the two
/// caller-derived facts (`completed`, `artefacts`) — exactly the shape lib.sh
/// computes from `cs observe` + a dir listing.
fn classify(events_jsonl: &str, completed: bool, artefacts: bool) -> Verdict {
    let fired = count_marker(events_jsonl, &[MARKER_FIRED]);
    let died = count_marker(events_jsonl, MARKER_DEATH);
    classify_verdict(fired, completed, artefacts, died)
}

// ---------------------------------------------------------------------------
// Mock server — the mode-C failure shape: HTTP 500 with a tool-call parse
// error body, optionally recovering to 200 after `fail_first` calls.
// ---------------------------------------------------------------------------

/// Answers 500 `error parsing tool call` for the first `fail_first` calls,
/// then 200 `finish_reason:"stop"`. `fail_first = 0` never 500s (the
/// self-chunking / lucky-run shape); `u32::MAX` never recovers (the
/// retry-budget-exhaustion shape).
struct ParseErrorResponder {
    calls: Arc<Mutex<u32>>,
    fail_first: u32,
}

impl Respond for ParseErrorResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut guard = self.calls.lock().expect("lock");
        *guard += 1;
        let nth = *guard;
        if nth <= self.fail_first {
            ResponseTemplate::new(500).set_body_json(json!({
                "error": { "message": "error parsing tool call: unexpected end of JSON input" }
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "recovered and done" },
                    "finish_reason": "stop"
                }]
            }))
        }
    }
}

/// Run cosmon's real mode-C loop against a mock server that 500s `fail_first`
/// times, under the given retry policy, with disk telemetry wired. Returns the
/// `events.jsonl` content, whether the loop completed, and whether it produced
/// a non-empty artefact — the three inputs `classify` consumes.
async fn run_scenario(fail_first: u32, retry: RetryPolicy) -> (String, bool, bool) {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ParseErrorResponder { calls, fail_first })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mol_id = MoleculeId::new("task-20260707-5fe6").expect("mol id");
    let worker_id = WorkerId::new("bench-mode-c").expect("worker id");
    let telemetry = telemetry_for(
        mol_id,
        worker_id,
        dir.path().to_owned(),
        "mode-c-bench-uuid",
    );

    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(retry);

    let outcome = run_agent_loop(&provider, "Briefing.", dir.path(), Some(&telemetry)).await;

    // Mirror the live harness: a completed worker materialises its returned
    // synthesis as the canonical artefact. A failed loop leaves none.
    let (completed, artefacts) = match &outcome {
        Ok(synthesis) if !synthesis.is_empty() => {
            std::fs::write(dir.path().join("synthesis.md"), synthesis).expect("write synthesis");
            (true, true)
        }
        Ok(_) => (true, false),
        Err(_) => (false, false),
    };

    let events = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap_or_default();
    (events, completed, artefacts)
}

fn retries_on() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(10),
    }
}

// ---------------------------------------------------------------------------
// The truth table — the predicate in isolation (no I/O). Proves it rejects
// both a lucky run and a death.
// ---------------------------------------------------------------------------

#[test]
fn predicate_truth_table_rejects_lucky_run_and_death() {
    // The only PASS: fired, completed, with artefacts, no death.
    assert_eq!(classify_verdict(1, true, true, 0), Verdict::Recovered);
    assert_eq!(classify_verdict(2, true, true, 0), Verdict::Recovered);

    // Fired, recovery attempted, then Stuck with no artefact -> DIED.
    assert_eq!(classify_verdict(3, false, false, 1), Verdict::Died);

    // The lucky run: never fired the 500. Completed WITH artefacts, yet the
    // predicate refuses to reward it. This is the load-bearing clause.
    assert_eq!(
        classify_verdict(0, true, true, 0),
        Verdict::Inconclusive,
        "a run that never fired the 500 must NEVER score PASS, however clean"
    );

    // Signals disagree (death marker WITH artefacts) -> AMBIGUOUS, not PASS.
    assert_eq!(classify_verdict(1, true, true, 1), Verdict::Ambiguous);
    // Fired + completed but no artefact -> AMBIGUOUS (not RECOVERED).
    assert_eq!(classify_verdict(1, true, false, 0), Verdict::Ambiguous);
}

// ---------------------------------------------------------------------------
// Discriminating power against cosmon's real loop — the in-binary flip.
// ---------------------------------------------------------------------------

/// GREEN: recovery reaches 200 — two 500s then a 200 yields `RECOVERED` (two
/// `tool_parse_reinject` events on disk, loop returns, artefact written).
#[tokio::test]
async fn recovery_reaches_200_scores_recovered() {
    let retry = RetryPolicy {
        max_retries: 4,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(10),
    };
    let (events, completed, artefacts) = run_scenario(2, retry).await;

    assert_eq!(
        count_marker(&events, &[MARKER_FIRED]),
        2,
        "two re-injects must be disk-evaluable; got: {events}"
    );
    assert_eq!(count_marker(&events, MARKER_DEATH), 0);
    assert_eq!(
        classify(&events, completed, artefacts),
        Verdict::Recovered,
        "500,500,200 with recovery must score RECOVERED"
    );
}

/// RED (proof of power): recovery **exhausts** — a server that 500s forever
/// fires the SAME `tool_parse_reinject` events, then surfaces the typed
/// `tool_call_parse` fatal with no artefact → `DIED`. The same stimulus flips
/// verdict on whether the server ever recovers.
#[tokio::test]
async fn recovery_exhausts_scores_died() {
    let (events, completed, artefacts) = run_scenario(u32::MAX, retries_on()).await;

    assert!(
        count_marker(&events, &[MARKER_FIRED]) >= 1,
        "recovery is attempted, so at least one re-inject fires; got: {events}"
    );
    assert!(
        count_marker(&events, MARKER_DEATH) >= 1,
        "an exhausted tool-call parse fatal must write a typed death marker; got: {events}"
    );
    assert!(!completed && !artefacts);
    assert_eq!(
        classify(&events, completed, artefacts),
        Verdict::Died,
        "a never-recovering 500 must score DIED"
    );
}

/// INCONCLUSIVE: a server that never 500s (self-chunking / lucky-run shape)
/// completes with an artefact yet scores `INCONCLUSIVE` — `fired == 0`. Proves
/// the bench does not reward a run that never exercised the recovery path.
#[tokio::test]
async fn no_provocation_scores_inconclusive() {
    let retry = RetryPolicy {
        max_retries: 4,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(10),
    };
    let (events, completed, artefacts) = run_scenario(0, retry).await;

    assert_eq!(count_marker(&events, &[MARKER_FIRED]), 0);
    assert_eq!(count_marker(&events, MARKER_DEATH), 0);
    assert!(
        completed && artefacts,
        "a clean run completes with an artefact"
    );
    assert_eq!(
        classify(&events, completed, artefacts),
        Verdict::Inconclusive,
        "a run that never fired the 500 is INCONCLUSIVE, never PASS"
    );
}

// ---------------------------------------------------------------------------
// The aggregate N-run predicate — mirrors lib.sh's `batch_predicate`.
// ---------------------------------------------------------------------------

/// The final verdict over an N-run replay: PASS iff >=1 RECOVERED and 0 DIED
/// and 0 AMBIGUOUS; PASS-WITH-AMBIGUITY if recovered amid ambiguity; FAIL on
/// any DIED; INCONCLUSIVE otherwise.
fn batch_predicate(runs: &[Verdict]) -> &'static str {
    let died = runs.iter().filter(|v| **v == Verdict::Died).count();
    let rec = runs.iter().filter(|v| **v == Verdict::Recovered).count();
    let amb = runs.iter().filter(|v| **v == Verdict::Ambiguous).count();
    if died >= 1 {
        "FAIL"
    } else if rec >= 1 && amb == 0 {
        "PASS"
    } else if rec >= 1 {
        "PASS-WITH-AMBIGUITY"
    } else {
        "INCONCLUSIVE"
    }
}

#[test]
fn aggregate_predicate_matches_pre_registration() {
    use Verdict::*;
    // One death anywhere -> FAIL, even amid recoveries.
    assert_eq!(
        batch_predicate(&[Recovered, Recovered, Died, Recovered, Inconclusive]),
        "FAIL"
    );
    // >=1 recovered, 0 died, 0 ambiguous -> PASS.
    assert_eq!(
        batch_predicate(&[
            Inconclusive,
            Recovered,
            Inconclusive,
            Inconclusive,
            Inconclusive
        ]),
        "PASS"
    );
    // Recovered amid ambiguity -> PASS-WITH-AMBIGUITY.
    assert_eq!(
        batch_predicate(&[Recovered, Ambiguous]),
        "PASS-WITH-AMBIGUITY"
    );
    // All inconclusive -> INCONCLUSIVE (provocation too weak; do not declare victory).
    assert_eq!(batch_predicate(&[Inconclusive; 5]), "INCONCLUSIVE");
}
