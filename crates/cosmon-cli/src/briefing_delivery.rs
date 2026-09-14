// SPDX-License-Identifier: AGPL-3.0-only

//! The briefing delivery postcondition: "text was written" is not "a
//! submission landed" (issue #40).
//!
//! # What was measured
//!
//! Six of six `cs tackle --adapter codex` dispatches left the briefing as
//! `› [Pasted Content N chars]` in the composer while `cs tackle` reported a
//! successful spawn. Reproduced outside `cs tackle` against codex 0.154.0
//! (task-20260914-a8b3, captures in that molecule's `evidence/`), the cause has
//! two halves that are only fatal together:
//!
//! 1. codex's readiness probe fires on the startup banner, which is drawn
//!    while codex still shows `model: loading`. A submit keystroke sent in
//!    that window is **dropped**; the paste itself is kept. With the paste
//!    and the Enter 500 ms apart right after the banner, the briefing stayed
//!    unsubmitted for the whole 20 s observation — collapsed 16 KB paste and
//!    verbatim 300-byte paste alike. The same sequence 3 s or 8 s after the
//!    banner submitted, and a later bare Enter submitted the stranded one.
//! 2. The submit-retry loop that should have re-pressed read the composer as
//!    `Clear` on its first poll: the composer classifier took codex's closed
//!    `╭…╰` **banner** box for the composer and never looked at the `›` line
//!    below it. One wrong reading, no retry, and nothing downstream checked.
//!
//! Half 2 is fixed in the classifier. This module is the part that makes the
//! next unknown half 2 loud instead of silent: after the injection, look at
//! the pane on the transport port, re-issue the submit while the composer
//! still holds the briefing, and report a typed outcome the caller can fail on.
//!
//! # Shape
//!
//! The decision is the existing I/O-free receipt kernel
//! ([`run_briefing_submit_loop`]); the observation is
//! [`TransportBackend::capture_output`] classified by the same classifier the
//! tmux submit loop uses. Nothing here knows which adapter it is watching, so
//! it holds for any adapter whose composer the classifier can read — which is
//! what lets a fake transport exercise it.

use std::time::Duration;

use cosmon_core::id::WorkerId;
use cosmon_core::injection::{BriefingDeliveryOutcome, InjectionProvenance};
use cosmon_core::transport::{TransportBackend, TransportError};
use cosmon_transport::tmux::{classify_composer_capture, ComposerState};

use crate::briefing_backstop::{run_briefing_submit_loop, BriefingSubmitOutcome};

/// Lines of pane tail read per observation.
///
/// Enough to contain a codex or Claude composer with its footer and the
/// banner above it; the classifier only ever consults the bottom of it.
const CAPTURE_LINES: usize = 60;

/// What the postcondition established about one briefing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BriefingDeliveryReport {
    /// The typed outcome — the value recorded in
    /// [`EventV2::BriefingDelivery`](cosmon_core::event_v2::EventV2::BriefingDelivery).
    pub outcome: BriefingDeliveryOutcome,
    /// Submit keystrokes issued by the postcondition itself, after the
    /// injection's own.
    pub resubmits: u32,
    /// Time spent observing, as reported by the injected clock.
    pub elapsed: Duration,
}

/// The briefing did not demonstrably land: the error a spawn fails with.
///
/// Named rather than an ad-hoc string so the refusal is greppable and a
/// caller can tell it apart from a transport fault.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "briefing not delivered to worker {worker}: {} after {} re-issued submit(s) in {:?}",
    report.outcome,
    report.resubmits,
    report.elapsed
)]
pub struct BriefingUndelivered {
    /// The worker whose composer was observed.
    pub worker: String,
    /// What the observation found.
    pub report: BriefingDeliveryReport,
}

/// Map the receipt kernel's exit onto the recorded delivery outcome.
///
/// Pure and total, so the vocabulary of the ledger cannot drift from the
/// vocabulary of the loop.
#[must_use]
pub fn delivery_outcome(outcome: BriefingSubmitOutcome) -> BriefingDeliveryOutcome {
    match outcome {
        BriefingSubmitOutcome::Delivered => BriefingDeliveryOutcome::Delivered,
        BriefingSubmitOutcome::StuckPasted => BriefingDeliveryOutcome::Undelivered,
        BriefingSubmitOutcome::Unobservable => BriefingDeliveryOutcome::Unobservable,
        BriefingSubmitOutcome::SessionGone => BriefingDeliveryOutcome::SessionGone,
    }
}

/// The postcondition as a verdict: only positive evidence of delivery passes.
///
/// # Errors
///
/// Returns [`BriefingUndelivered`] for every outcome but
/// [`BriefingDeliveryOutcome::Delivered`].
pub fn require_delivered(
    worker: &WorkerId,
    report: BriefingDeliveryReport,
) -> Result<(), BriefingUndelivered> {
    if report.outcome.is_delivered() {
        Ok(())
    } else {
        Err(BriefingUndelivered {
            worker: worker.name().to_owned(),
            report,
        })
    }
}

/// Observe a worker's composer after a briefing injection, re-issuing the
/// submit while it still holds the briefing, for at most `budget`.
///
/// Every re-issued submit goes through
/// [`TransportBackend::send_input_observed`] with `submit_provenance`, so each
/// keystroke keeps its writer identity in the ledger. `now` returns elapsed
/// time since the start and `sleep` waits one poll; both are injected so the
/// bound is testable without real time.
pub fn confirm_briefing_delivery<B: TransportBackend + ?Sized>(
    backend: &B,
    worker: &WorkerId,
    briefing: &str,
    budget: Duration,
    submit_provenance: &InjectionProvenance,
    now: &mut dyn FnMut() -> Duration,
    sleep: &mut dyn FnMut(),
) -> BriefingDeliveryReport {
    let mut resubmits: u32 = 0;
    let outcome = run_briefing_submit_loop(
        budget,
        &mut || match backend.capture_output(worker, CAPTURE_LINES) {
            Ok(pane) => Some(classify_composer_capture(&pane, briefing)),
            Err(TransportError::NotFound(_)) => None,
            Err(_) => Some(ComposerState::Unobservable),
        },
        &mut || {
            resubmits = resubmits.saturating_add(1);
            // Best-effort: a failed keystroke shows up as a composer that is
            // still pending on the next look, which is where it is judged.
            let _ = backend.send_input_observed(worker, "", submit_provenance);
        },
        now,
        sleep,
    );
    BriefingDeliveryReport {
        outcome: delivery_outcome(outcome),
        resubmits,
        elapsed: now(),
    }
}

/// Inject a briefing and hold it to its delivery postcondition.
///
/// The adapter-neutral replacement for a bare `send_input_observed`: write
/// (recorded as `InputInjected` by the seam), then observe
/// ([`confirm_briefing_delivery`]). The report is returned on both paths so
/// the caller can record it before deciding what a failure costs.
///
/// # Errors
///
/// Returns the [`TransportError`] of the injection itself. A briefing that was
/// written but not delivered is **not** an error here: it is a report, which
/// [`require_delivered`] turns into [`BriefingUndelivered`].
#[allow(clippy::too_many_arguments)]
pub fn deliver_briefing<B: TransportBackend + ?Sized>(
    backend: &B,
    worker: &WorkerId,
    briefing: &str,
    budget: Duration,
    writer: &InjectionProvenance,
    submit_provenance: &InjectionProvenance,
    now: &mut dyn FnMut() -> Duration,
    sleep: &mut dyn FnMut(),
) -> Result<BriefingDeliveryReport, TransportError> {
    backend.send_input_observed(worker, briefing, writer)?;
    Ok(confirm_briefing_delivery(
        backend,
        worker,
        briefing,
        budget,
        submit_provenance,
        now,
        sleep,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    use cosmon_core::event_v2::{Envelope, EventV2};
    use cosmon_core::id::MoleculeId;
    use cosmon_core::injection::InjectionOrigin;
    use cosmon_core::transport::{AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle};

    const BRIEFING: &str = "first line of the briefing\nEnd of briefing: start work.";

    /// A codex-shaped pane holding the collapsed paste under the banner box.
    const PANE_PENDING: &str = "\
╭──────────────────────────────╮
│ >_ OpenAI Codex (v0.154.0)   │
│ model:       loading         │
╰──────────────────────────────╯
› [Pasted Content 16030 chars]
  gpt-6-astra default fast · /tmp/wt
";

    /// The same pane after the submit landed.
    const PANE_CLEAR: &str = "\
╭──────────────────────────────╮
│ >_ OpenAI Codex (v0.154.0)   │
╰──────────────────────────────╯
• Working (1s • esc to interrupt)
› Ask Codex to do anything
  gpt-6-astra high fast · /tmp/wt
";

    /// A transport whose composer clears after `clears_after` submits
    /// (`None`: never). Counts every bare submit it receives.
    struct FakePane {
        clears_after: Option<u32>,
        submits: Cell<u32>,
        pastes: RefCell<Vec<String>>,
    }

    impl FakePane {
        fn new(clears_after: Option<u32>) -> Self {
            Self {
                clears_after,
                submits: Cell::new(0),
                pastes: RefCell::new(Vec::new()),
            }
        }
    }

    impl TransportBackend for FakePane {
        fn spawn(
            &self,
            _agent: &AgentDefinition,
            _config: &RuntimeConfig,
        ) -> Result<SpawnHandle, TransportError> {
            Err(TransportError::SpawnFailed("not in this test".to_owned()))
        }
        fn terminate(&self, _id: &WorkerId) -> Result<(), TransportError> {
            Ok(())
        }
        fn is_alive(&self, _id: &WorkerId) -> Result<bool, TransportError> {
            Ok(true)
        }
        fn send_input(&self, _id: &WorkerId, input: &str) -> Result<(), TransportError> {
            if input.is_empty() {
                self.submits.set(self.submits.get() + 1);
            } else {
                // The injection's own paste + Enter, as the tmux seam sends it.
                self.pastes.borrow_mut().push(input.to_owned());
            }
            Ok(())
        }
        fn capture_output(&self, _id: &WorkerId, _lines: usize) -> Result<String, TransportError> {
            let cleared = self.clears_after.is_some_and(|n| self.submits.get() >= n);
            Ok(if cleared { PANE_CLEAR } else { PANE_PENDING }.to_owned())
        }
        fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
            Ok(Vec::new())
        }
        fn graceful_exit(&self, _id: &WorkerId, _t: Duration) -> Result<bool, TransportError> {
            Ok(true)
        }
    }

    fn worker() -> WorkerId {
        WorkerId::new("codex-worker-a8b3").unwrap()
    }

    fn writer() -> InjectionProvenance {
        InjectionProvenance::new(InjectionOrigin::TackleBriefing, "briefing")
    }

    fn submit() -> InjectionProvenance {
        InjectionProvenance::new(InjectionOrigin::TackleBriefing, "briefing-submit")
    }

    /// Run the postcondition on virtual time: each poll advances one second.
    fn deliver(pane: &FakePane) -> BriefingDeliveryReport {
        let clock = Cell::new(Duration::ZERO);
        deliver_briefing(
            pane,
            &worker(),
            BRIEFING,
            Duration::from_secs(20),
            &writer(),
            &submit(),
            &mut || clock.get(),
            &mut || clock.set(clock.get() + Duration::from_secs(1)),
        )
        .unwrap()
    }

    /// Falsifier 1: the pane never clears. The submit is re-issued, and the
    /// verdict is a named failure — not the silent success a bare write gave.
    #[test]
    fn a_paste_that_never_clears_resubmits_then_fails_by_name() {
        let pane = FakePane::new(None);
        let report = deliver(&pane);

        assert_eq!(pane.pastes.borrow().len(), 1, "briefing injected once");
        assert!(report.resubmits >= 1, "the submit was re-issued");
        assert_eq!(pane.submits.get(), report.resubmits);
        assert_eq!(report.outcome, BriefingDeliveryOutcome::Undelivered);
        assert!(
            report.elapsed >= Duration::from_secs(20),
            "bounded window spent"
        );

        let err = require_delivered(&worker(), report).unwrap_err();
        assert_eq!(err.report.outcome, BriefingDeliveryOutcome::Undelivered);
        assert!(err.to_string().contains("briefing not delivered"));
    }

    /// Falsifier 2: the pane clears on the first re-issued submit. Exactly one
    /// submit, and the postcondition passes.
    #[test]
    fn a_pane_that_clears_on_the_first_submit_gets_exactly_one() {
        let pane = FakePane::new(Some(1));
        let report = deliver(&pane);

        assert_eq!(pane.submits.get(), 1, "exactly one submit");
        assert_eq!(report.resubmits, 1);
        assert_eq!(report.outcome, BriefingDeliveryOutcome::Delivered);
        assert!(require_delivered(&worker(), report).is_ok());
    }

    /// The claude-shaped nominal path: the composer is already clear at the
    /// first look. No keystroke is added, and the postcondition passes.
    #[test]
    fn an_already_clear_composer_passes_without_a_resubmit() {
        let pane = FakePane::new(Some(0));
        let report = deliver(&pane);

        assert_eq!(pane.submits.get(), 0);
        assert_eq!(report.outcome, BriefingDeliveryOutcome::Delivered);
    }

    /// A vanished session is its own outcome, and fails the postcondition.
    #[test]
    fn a_vanished_session_is_session_gone() {
        struct Gone;
        impl TransportBackend for Gone {
            fn spawn(
                &self,
                _a: &AgentDefinition,
                _c: &RuntimeConfig,
            ) -> Result<SpawnHandle, TransportError> {
                Err(TransportError::SpawnFailed(String::new()))
            }
            fn terminate(&self, _id: &WorkerId) -> Result<(), TransportError> {
                Ok(())
            }
            fn is_alive(&self, _id: &WorkerId) -> Result<bool, TransportError> {
                Ok(false)
            }
            fn send_input(&self, _id: &WorkerId, _i: &str) -> Result<(), TransportError> {
                Ok(())
            }
            fn capture_output(&self, id: &WorkerId, _l: usize) -> Result<String, TransportError> {
                Err(TransportError::NotFound(id.clone()))
            }
            fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
                Ok(Vec::new())
            }
            fn graceful_exit(&self, _id: &WorkerId, _t: Duration) -> Result<bool, TransportError> {
                Ok(false)
            }
        }
        let clock = Cell::new(Duration::ZERO);
        let report = confirm_briefing_delivery(
            &Gone,
            &worker(),
            BRIEFING,
            Duration::from_secs(20),
            &submit(),
            &mut || clock.get(),
            &mut || clock.set(clock.get() + Duration::from_secs(1)),
        );
        assert_eq!(report.outcome, BriefingDeliveryOutcome::SessionGone);
        assert!(require_delivered(&worker(), report).is_err());
    }

    /// Falsifier 3: a delivered and an undelivered briefing differ in the
    /// recorded trace — asserted on parsed events, not on log text.
    #[test]
    fn delivered_and_undelivered_briefings_differ_in_the_trace() {
        let dir = tempfile::tempdir().unwrap();
        let mol = MoleculeId::new("task-20260914-a8b3").unwrap();

        for pane in [FakePane::new(Some(1)), FakePane::new(None)] {
            let report = deliver(&pane);
            cosmon_state::events::input_injection::emit_briefing_delivery(
                dir.path(),
                Some(&mol),
                &worker(),
                "codex",
                &writer(),
                report.outcome,
                report.resubmits,
                u64::try_from(report.elapsed.as_millis()).unwrap(),
            );
        }

        let raw = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let rows: Vec<(BriefingDeliveryOutcome, InjectionOrigin, String)> = raw
            .lines()
            .map(|l| Envelope::from_line(l).unwrap().event)
            .filter_map(|e| match e {
                EventV2::BriefingDelivery {
                    outcome,
                    origin,
                    purpose,
                    ..
                } => Some((outcome, origin, purpose)),
                _ => None,
            })
            .collect();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, BriefingDeliveryOutcome::Delivered);
        assert_eq!(rows[1].0, BriefingDeliveryOutcome::Undelivered);
        assert_ne!(rows[0].0, rows[1].0, "the trace tells them apart");
        // Writer identity is kept on both rows.
        for (_, origin, purpose) in &rows {
            assert_eq!(*origin, InjectionOrigin::TackleBriefing);
            assert_eq!(purpose, "briefing");
        }
    }
}
