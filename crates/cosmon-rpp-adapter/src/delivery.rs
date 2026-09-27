// SPDX-License-Identifier: AGPL-3.0-only

//! How an API-dispatched worker receives its briefing (issue #81 point 1).
//!
//! # What was measured
//!
//! On cosmon-server v3.11 a molecule dispatched through the RPP API sat with
//! its briefing in Claude Code's input field, unsubmitted, for over six
//! minutes; one Enter typed by hand made it run and finish in 29 seconds. The
//! in-process executor wrote the briefing the moment the tmux session existed
//! — while Claude Code was still drawing its startup screen — and never looked
//! at the pane again. `cs tackle` does neither: it waits for the TUI to show a
//! composer before pasting, then re-presses submit until the composer no
//! longer holds the briefing (issue #40).
//!
//! # What this port does
//!
//! [`RppBriefingDelivery`] gives the API path the same three steps, built from
//! the same parts `cs tackle` uses:
//!
//! 1. wait for the adapter's readiness verdict — [`ClaudeTuiProbe`] for
//!    Claude, which also answers the trust and bypass dialogs, [`CodexProbe`]
//!    for codex;
//! 2. paste the briefing through the transport;
//! 3. hold it to the delivery postcondition
//!    ([`cosmon_transport::briefing_delivery::confirm_briefing_delivery`]).
//!
//! The executor records the resulting report and fails the dispatch when the
//! composer was still holding the briefing at the end of the budget, so a
//! stranded worker becomes an error the tenant sees instead of a slot that
//! stays `running`.

use std::time::{Duration, Instant};

use cosmon_core::transport::{TransportBackend, TransportError};
use cosmon_runtime::{BriefingDelivery, BriefingDeliveryContext, BriefingDeliveryReport};
use cosmon_transport::readiness::{ClaudeTuiProbe, CodexProbe, LiveProbe, Liveness};

/// Readiness window for a freshly spawned worker.
///
/// Twice `cs tackle`'s 30 s: a server's first dispatch may pay for Claude
/// Code's first-run work in the container, and a dispatch that gives up
/// early here tears the worker down.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll interval of the readiness wait — `cs tackle`'s value.
const READY_POLL: Duration = Duration::from_millis(500);

/// How long the composer is watched for the briefing to leave it.
///
/// Longer than `cs tackle`'s 8 s in-band window, because nothing takes over
/// on this path when it closes: `cs tackle` hands the rest to a detached
/// `cs briefing-backstop`, and the server has no such process.
const CONFIRM_BUDGET: Duration = Duration::from_secs(30);

/// Poll interval of the confirmation — the receipt kernel's own.
const CONFIRM_POLL: Duration = cosmon_transport::briefing_submit::BRIEFING_SUBMIT_POLL;

/// The briefing delivery every adapter-side dispatch installs.
#[derive(Debug, Clone)]
pub struct RppBriefingDelivery {
    ready_timeout: Duration,
    ready_poll: Duration,
    confirm_budget: Duration,
    confirm_poll: Duration,
}

impl Default for RppBriefingDelivery {
    fn default() -> Self {
        Self {
            ready_timeout: READY_TIMEOUT,
            ready_poll: READY_POLL,
            confirm_budget: CONFIRM_BUDGET,
            confirm_poll: CONFIRM_POLL,
        }
    }
}

impl RppBriefingDelivery {
    /// The same port with other windows — for tests that cannot spend the
    /// production ones in real time.
    #[must_use]
    pub fn with_windows(
        ready_timeout: Duration,
        ready_poll: Duration,
        confirm_budget: Duration,
        confirm_poll: Duration,
    ) -> Self {
        Self {
            ready_timeout,
            ready_poll,
            confirm_budget,
            confirm_poll,
        }
    }

    /// Wait until the worker's TUI accepts input.
    ///
    /// Adapters without a probe here are not waited for; the confirmation
    /// that follows still observes their composer.
    fn await_ready(
        &self,
        backend: &dyn TransportBackend,
        ctx: &BriefingDeliveryContext<'_>,
    ) -> Result<(), TransportError> {
        let verdict = match ctx.adapter {
            "claude" => {
                let (status, liveness) = ClaudeTuiProbe::await_live_with_status(
                    backend,
                    ctx.worker,
                    self.ready_timeout,
                    self.ready_poll,
                )?;
                (liveness, status.to_string())
            }
            "codex" => {
                let liveness = CodexProbe.await_live(
                    backend,
                    ctx.worker,
                    self.ready_timeout,
                    self.ready_poll,
                )?;
                (liveness, liveness.to_string())
            }
            _ => return Ok(()),
        };
        match verdict {
            (Liveness::Live, _) => Ok(()),
            (_, status) => Err(TransportError::SpawnFailed(format!(
                "{} worker {} never reached a work-accepting state within {:?} \
                 (status={status}); the briefing was not sent",
                ctx.adapter,
                ctx.worker.name(),
                self.ready_timeout
            ))),
        }
    }
}

impl BriefingDelivery for RppBriefingDelivery {
    fn deliver(
        &self,
        backend: &dyn TransportBackend,
        ctx: &BriefingDeliveryContext<'_>,
    ) -> Result<BriefingDeliveryReport, TransportError> {
        self.await_ready(backend, ctx)?;
        let started = Instant::now();
        let poll = self.confirm_poll;
        let report = cosmon_transport::briefing_delivery::deliver_briefing(
            backend,
            ctx.worker,
            ctx.briefing,
            self.confirm_budget,
            ctx.writer,
            ctx.submit,
            &mut || started.elapsed(),
            &mut || std::thread::sleep(poll),
        )?;
        Ok(BriefingDeliveryReport {
            outcome: report.outcome,
            resubmits: report.resubmits,
            elapsed: report.elapsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    use cosmon_core::id::{MoleculeId, WorkerId};
    use cosmon_core::injection::{BriefingDeliveryOutcome, InjectionOrigin, InjectionProvenance};
    use cosmon_core::transport::{AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle};

    const BRIEFING: &str = "first line of the briefing\nEnd of briefing: start work.";

    /// Claude Code's startup screen: no composer yet.
    const PANE_STARTING: &str = "\n  Claude Code is starting…\n";

    /// A ready Claude composer holding the unsubmitted briefing.
    const PANE_PENDING: &str = "\
  ⏵⏵ bypass permissions on (shift+tab to cycle)
 ❯ [Pasted text #1 +2 lines]
";

    /// The same composer after the submit landed.
    const PANE_CLEAR: &str = "\
  ⏵⏵ bypass permissions on (shift+tab to cycle)
 ❯ Type your message
";

    /// A Claude pane that shows its startup screen for `startup_frames`
    /// captures, then a composer. Once pasted, the composer holds the
    /// briefing until it has received `submits_to_clear` submits
    /// (`None`: never).
    struct FakeClaude {
        startup_frames: Cell<u32>,
        submits_to_clear: Option<u32>,
        pasted: Cell<bool>,
        pasted_while_starting: Cell<bool>,
        submits: Cell<u32>,
        log: RefCell<Vec<&'static str>>,
    }

    impl FakeClaude {
        fn new(startup_frames: u32, submits_to_clear: Option<u32>) -> Self {
            Self {
                startup_frames: Cell::new(startup_frames),
                submits_to_clear,
                pasted: Cell::new(false),
                pasted_while_starting: Cell::new(false),
                submits: Cell::new(0),
                log: RefCell::new(Vec::new()),
            }
        }

        fn starting(&self) -> bool {
            self.startup_frames.get() > 0
        }
    }

    impl TransportBackend for FakeClaude {
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
            if input.is_empty() || input == "\r" || input == "\n" {
                self.log.borrow_mut().push("submit");
                // A submit sent before the composer exists is dropped, as
                // Claude Code drops it while drawing its startup screen.
                if !self.starting() && self.pasted.get() {
                    self.submits.set(self.submits.get() + 1);
                }
            } else {
                self.log.borrow_mut().push("paste");
                self.pasted.set(true);
                self.pasted_while_starting.set(self.starting());
            }
            Ok(())
        }
        fn capture_output(&self, _id: &WorkerId, _lines: usize) -> Result<String, TransportError> {
            if self.starting() {
                self.startup_frames.set(self.startup_frames.get() - 1);
                return Ok(PANE_STARTING.to_owned());
            }
            let cleared = self
                .submits_to_clear
                .is_some_and(|n| self.submits.get() >= n);
            if self.pasted.get() && !cleared {
                Ok(PANE_PENDING.to_owned())
            } else {
                Ok(PANE_CLEAR.to_owned())
            }
        }
        fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
            Ok(Vec::new())
        }
        fn graceful_exit(
            &self,
            _id: &WorkerId,
            _timeout: Duration,
        ) -> Result<bool, TransportError> {
            Ok(true)
        }
    }

    fn fast() -> RppBriefingDelivery {
        RppBriefingDelivery::with_windows(
            Duration::from_secs(5),
            Duration::from_millis(1),
            Duration::from_millis(200),
            Duration::from_millis(1),
        )
    }

    fn deliver(backend: &FakeClaude) -> Result<BriefingDeliveryReport, TransportError> {
        let molecule = MoleculeId::new("task-20260925-259e").expect("molecule id");
        let worker = WorkerId::new("worker-259e").expect("worker id");
        let writer = InjectionProvenance::new(InjectionOrigin::TackleBriefing, "briefing");
        let submit = InjectionProvenance::new(InjectionOrigin::TackleBriefing, "briefing-submit");
        fast().deliver(
            backend,
            &BriefingDeliveryContext {
                molecule: &molecule,
                adapter: "claude",
                worker: &worker,
                briefing: BRIEFING,
                writer: &writer,
                submit: &submit,
            },
        )
    }

    /// The issue #81 shape: the worker is still starting when the dispatch
    /// reaches it, and the first submit is swallowed. The briefing is pasted
    /// only once the composer exists, and re-submitted until it leaves.
    #[test]
    fn a_briefing_reaching_a_starting_worker_is_submitted() {
        let pane = FakeClaude::new(3, Some(1));
        let report = deliver(&pane).expect("delivery");

        assert!(
            !pane.pasted_while_starting.get(),
            "the briefing must not be pasted before the composer exists"
        );
        assert_eq!(report.outcome, BriefingDeliveryOutcome::Delivered);
        assert!(pane.submits.get() >= 1, "the briefing was never submitted");
    }

    /// A composer that never lets go of the briefing is reported as
    /// undelivered, which the executor turns into a failed dispatch.
    #[test]
    fn a_composer_that_keeps_the_briefing_is_reported_undelivered() {
        let pane = FakeClaude::new(0, None);
        let report = deliver(&pane).expect("delivery");

        assert_eq!(report.outcome, BriefingDeliveryOutcome::Undelivered);
        assert!(report.resubmits > 0, "the port must have re-pressed submit");
    }

    /// A worker that never shows a composer is not sent a briefing at all.
    #[test]
    fn a_worker_that_never_becomes_ready_is_not_sent_the_briefing() {
        let pane = FakeClaude::new(u32::MAX, Some(1));
        let delivery = RppBriefingDelivery::with_windows(
            Duration::from_millis(50),
            Duration::from_millis(1),
            Duration::from_millis(200),
            Duration::from_millis(1),
        );
        let molecule = MoleculeId::new("task-20260925-259e").expect("molecule id");
        let worker = WorkerId::new("worker-259e").expect("worker id");
        let writer = InjectionProvenance::new(InjectionOrigin::TackleBriefing, "briefing");
        let result = delivery.deliver(
            &pane,
            &BriefingDeliveryContext {
                molecule: &molecule,
                adapter: "claude",
                worker: &worker,
                briefing: BRIEFING,
                writer: &writer,
                submit: &writer,
            },
        );

        assert!(result.is_err(), "a never-ready worker must fail delivery");
        assert!(
            !pane.pasted.get(),
            "nothing may be pasted into a startup screen"
        );
    }
}
