// SPDX-License-Identifier: AGPL-3.0-only

//! Pure folding contract for provider lifecycle observations.
//!
//! A connected provider client owns transport and feeds this reducer observed
//! events.  The reducer neither opens a socket nor inspects a process, screen,
//! quota, clock, or period of silence.  That keeps app-server I/O outside the
//! domain core and makes duplicate and stale delivery deterministic.

use serde::{Deserialize, Serialize};

use crate::presence::SessionState;

/// Capability of the source behind a lifecycle reading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleSource {
    /// A connected provider event stream observes the full admitted lifecycle.
    LiveLifecycle,
    /// A callback observes only terminal turn completion.
    TurnEndOnly,
    /// No source is connected, so the lifecycle state is unknown.
    #[default]
    Unavailable,
}

impl LifecycleSource {
    /// Stable wire token used by presence readers.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LiveLifecycle => "live_lifecycle",
            Self::TurnEndOnly => "turn_end_only",
            Self::Unavailable => "unavailable",
        }
    }
}

/// An event observed by a connected provider client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLifecycleEvent {
    /// The provider started a turn.
    TurnStarted,
    /// The provider requested approval from the user.
    ApprovalRequested,
    /// The provider reported terminal completion of the turn.
    TurnCompleted,
    /// The client lost the source connection.
    Disconnected,
}

/// A client-assigned order key, monotone per provider session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderEventSequence(u64);

impl ProviderEventSequence {
    /// Construct a sequence value supplied by the connected client.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the underlying wire value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Typed reading exposed to lifecycle consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleReading {
    /// Capability of the source that produced this reading.
    pub source: LifecycleSource,
    /// Observed state, absent when the source is unavailable.
    pub state: Option<SessionState>,
}

impl Default for LifecycleReading {
    fn default() -> Self {
        Self {
            source: LifecycleSource::Unavailable,
            state: None,
        }
    }
}

/// Result of attempting to fold one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryDisposition {
    /// A newer event changed or reaffirmed the current reading.
    Applied,
    /// The exact last sequence was delivered again.
    Duplicate,
    /// A sequence older than the last accepted event was delivered.
    Stale,
}

/// Stateful, I/O-free lifecycle fold for one provider session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderLifecycleFold {
    last_sequence: Option<ProviderEventSequence>,
    reading: LifecycleReading,
}

impl ProviderLifecycleFold {
    /// Reconstitute the fold from a durable presence reading.
    #[must_use]
    pub const fn restore(
        last_sequence: Option<ProviderEventSequence>,
        reading: LifecycleReading,
    ) -> Self {
        Self {
            last_sequence,
            reading,
        }
    }

    /// Fold one event if and only if its sequence is newer.
    ///
    /// Equal delivery is idempotent. Older delivery is stale. Neither can
    /// overwrite the current reading. A disconnect explicitly clears state;
    /// it never manufactures `idle`.
    pub fn apply(
        &mut self,
        sequence: ProviderEventSequence,
        event: ProviderLifecycleEvent,
    ) -> DeliveryDisposition {
        if let Some(last) = self.last_sequence {
            if sequence == last {
                return DeliveryDisposition::Duplicate;
            }
            if sequence < last {
                return DeliveryDisposition::Stale;
            }
        }

        self.last_sequence = Some(sequence);
        self.reading = match event {
            ProviderLifecycleEvent::TurnStarted => LifecycleReading {
                source: LifecycleSource::LiveLifecycle,
                state: Some(SessionState::Working),
            },
            ProviderLifecycleEvent::ApprovalRequested => LifecycleReading {
                source: LifecycleSource::LiveLifecycle,
                state: Some(SessionState::WaitingPermission),
            },
            ProviderLifecycleEvent::TurnCompleted => LifecycleReading {
                source: LifecycleSource::LiveLifecycle,
                state: Some(SessionState::Idle),
            },
            ProviderLifecycleEvent::Disconnected => LifecycleReading::default(),
        };
        DeliveryDisposition::Applied
    }

    /// Current typed reading.
    #[must_use]
    pub const fn reading(self) -> LifecycleReading {
        self.reading
    }

    /// Last accepted source sequence.
    #[must_use]
    pub const fn last_sequence(self) -> Option<ProviderEventSequence> {
        self.last_sequence
    }
}

/// Injectable source owned by a connected provider client.
///
/// Implementations may read an app-server transport; fixtures implement the
/// same trait without I/O. `None` means the source is currently drained, not
/// that the session is idle. Connection loss must be delivered explicitly as
/// [`ProviderLifecycleEvent::Disconnected`].
pub trait ProviderEventSource {
    /// Adapter-specific read error.
    type Error;

    /// Read the next observed event and its client-assigned order key.
    fn next_event(
        &mut self,
    ) -> Result<Option<(ProviderEventSequence, ProviderLifecycleEvent)>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::VecDeque;

    #[derive(Deserialize)]
    struct FixtureLine {
        sequence: u64,
        method: String,
    }

    struct FixtureSource(VecDeque<(ProviderEventSequence, ProviderLifecycleEvent)>);

    impl ProviderEventSource for FixtureSource {
        type Error = std::convert::Infallible;

        fn next_event(
            &mut self,
        ) -> Result<Option<(ProviderEventSequence, ProviderLifecycleEvent)>, Self::Error> {
            Ok(self.0.pop_front())
        }
    }

    fn fixture() -> FixtureSource {
        let events = include_str!("../tests/fixtures/codex-app-server-lifecycle.jsonl")
            .lines()
            .map(|line| {
                let row: FixtureLine = serde_json::from_str(line).unwrap();
                let event = match row.method.as_str() {
                    "turn/started" => ProviderLifecycleEvent::TurnStarted,
                    "item/commandExecution/requestApproval" => {
                        ProviderLifecycleEvent::ApprovalRequested
                    }
                    "turn/completed" => ProviderLifecycleEvent::TurnCompleted,
                    "transport/disconnected" => ProviderLifecycleEvent::Disconnected,
                    other => panic!("unadmitted fixture method: {other}"),
                };
                (ProviderEventSequence::new(row.sequence), event)
            })
            .collect();
        FixtureSource(events)
    }

    #[test]
    fn fixture_maps_start_approval_completion_and_disconnect() {
        let mut source = fixture();
        let mut fold = ProviderLifecycleFold::default();
        let expected = [
            LifecycleReading {
                source: LifecycleSource::LiveLifecycle,
                state: Some(SessionState::Working),
            },
            LifecycleReading {
                source: LifecycleSource::LiveLifecycle,
                state: Some(SessionState::WaitingPermission),
            },
            LifecycleReading {
                source: LifecycleSource::LiveLifecycle,
                state: Some(SessionState::Idle),
            },
            LifecycleReading::default(),
        ];

        for want in expected {
            let (sequence, event) = source.next_event().unwrap().unwrap();
            assert_eq!(fold.apply(sequence, event), DeliveryDisposition::Applied);
            assert_eq!(fold.reading(), want);
        }
    }

    #[test]
    fn duplicate_delivery_is_idempotent() {
        let mut fold = ProviderLifecycleFold::default();
        let sequence = ProviderEventSequence::new(7);
        assert_eq!(
            fold.apply(sequence, ProviderLifecycleEvent::TurnStarted),
            DeliveryDisposition::Applied
        );
        let before = fold;
        assert_eq!(
            fold.apply(sequence, ProviderLifecycleEvent::TurnCompleted),
            DeliveryDisposition::Duplicate
        );
        assert_eq!(fold, before);
    }

    #[test]
    fn out_of_order_delivery_cannot_overwrite_newer_observation() {
        let mut fold = ProviderLifecycleFold::default();
        fold.apply(
            ProviderEventSequence::new(9),
            ProviderLifecycleEvent::ApprovalRequested,
        );
        let before = fold;
        assert_eq!(
            fold.apply(
                ProviderEventSequence::new(8),
                ProviderLifecycleEvent::TurnCompleted
            ),
            DeliveryDisposition::Stale
        );
        assert_eq!(fold, before);
    }

    #[test]
    fn drained_source_is_not_idle() {
        let mut source = FixtureSource(VecDeque::new());
        assert_eq!(source.next_event().unwrap(), None);
        assert_eq!(
            ProviderLifecycleFold::default().reading(),
            LifecycleReading::default()
        );
    }
}
