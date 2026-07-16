// SPDX-License-Identifier: AGPL-3.0-only

//! In-process publish-subscribe bus for molecule lifecycle events.
//!
//! The adapter publishes a [`MoleculeEvent`] to the bus from each
//! mutation route (`nucleate`, `tag`, `collapse`, `freeze`, `stuck`,
//! `tackle`). The SSE handler (`GET /v1/events`) subscribes to the bus
//! and forwards events
//! matching the caller's tenant `noyau` (and optional `molecule_id`)
//! to the wire as `text/event-stream`.
//!
//! The bus is a [`tokio::sync::broadcast`] channel — lossy on slow
//! consumers, fan-out without per-subscriber persistence. Adapters
//! that need history must reconnect with `Last-Event-ID` and rely on
//! the underlying filesystem state for catch-up; the bus only carries
//! the live tail.
//!
//! The channel lives in [`AppState`] as an `Arc<EventBus>` so every
//! handler can publish without taking a lock.
//!
//! [`AppState`]: crate::AppState

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

/// Default backlog the broadcast channel retains for late subscribers.
/// The window is small on purpose — SSE is for *live* signals, not for
/// history replay; subscribers that want history should query the per-
/// tenant filesystem state instead.
pub const DEFAULT_CAPACITY: usize = 256;

/// One molecule lifecycle signal carried by the bus. The payload is a
/// serializable struct rather than a raw JSON value so publishers and
/// the SSE handler share a single type. Cross-noyau filtering is
/// enforced at the SSE handler boundary using the [`Self::noyau`]
/// field; publishers MUST populate the field from the admission
/// [`Spark`].
///
/// [`Spark`]: crate::admission::Spark
#[derive(Debug, Clone, Serialize)]
pub struct MoleculeEvent {
    /// SSE event name (`molecule.state_changed`,
    /// `molecule.event_appended`, …). Kept short and stable; new event
    /// names are additive (a v1 subscriber MUST ignore unknown event
    /// names rather than reject).
    pub event: &'static str,
    /// Tenant noyau the event belongs to. Used by the SSE handler to
    /// gate cross-noyau visibility — a noyau-A subscriber NEVER sees a
    /// noyau-B event. NOT emitted on the wire.
    #[serde(skip_serializing)]
    pub noyau: String,
    /// Molecule the event refers to. Always present (even on
    /// `molecule.event_appended` whose payload also carries the id) so
    /// `?molecule_id=` filtering does not need to inspect the inner
    /// data payload.
    pub molecule_id: String,
    /// RFC3339 timestamp the event was published at.
    pub timestamp: String,
    /// Event-specific data. Serialised verbatim in the SSE `data:` line.
    pub data: serde_json::Value,
}

impl MoleculeEvent {
    /// Build a `molecule.state_changed` event.
    pub fn state_changed(
        noyau: impl Into<String>,
        molecule_id: impl Into<String>,
        old_state: impl Into<String>,
        new_state: impl Into<String>,
    ) -> Self {
        let molecule_id = molecule_id.into();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let data = serde_json::json!({
            "molecule_id": molecule_id,
            "old_state": old_state.into(),
            "new_state": new_state.into(),
            "timestamp": timestamp,
        });
        Self {
            event: "molecule.state_changed",
            noyau: noyau.into(),
            molecule_id,
            timestamp,
            data,
        }
    }

    /// Build a `drain.started` event — the resident drain loop was
    /// spawned on the DAG rooted at `root_id` (B2 bounded drain).
    /// `bounds` is the resolved B1/B2/B3 triple
    /// echoed for diagnosticability; the client can read it, never
    /// write it.
    #[allow(clippy::needless_pass_by_value)]
    pub fn drain_started(
        noyau: impl Into<String>,
        root_id: impl Into<String>,
        bounds: serde_json::Value,
    ) -> Self {
        let molecule_id = root_id.into();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let data = serde_json::json!({
            "molecule_id": molecule_id,
            "bounds": bounds,
            "timestamp": timestamp,
        });
        Self {
            event: "drain.started",
            noyau: noyau.into(),
            molecule_id,
            timestamp,
            data,
        }
    }

    /// Build a `drain.terminated` event — the resident drain loop
    /// exited with the NAMED `reason` token (I4 — never a stall):
    /// `drained`, `budget_exhausted`, `molecule_quota_exceeded`,
    /// `max_depth_exceeded`, `timeout`, or `error`. The tokens mirror
    /// the `cs run` exit codes 0/90/91/92/124 (B1 moussage); the mirror
    /// is pinned by
    /// `drain_exit_reason_mirrors_reject_labels` in
    /// `routes::molecules`.
    pub fn drain_terminated(
        noyau: impl Into<String>,
        root_id: impl Into<String>,
        reason: &'static str,
    ) -> Self {
        let molecule_id = root_id.into();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let data = serde_json::json!({
            "molecule_id": molecule_id,
            "reason": reason,
            "timestamp": timestamp,
        });
        Self {
            event: "drain.terminated",
            noyau: noyau.into(),
            molecule_id,
            timestamp,
            data,
        }
    }

    /// Build a `molecule.event_appended` event for a domain event the
    /// state machine wrote to the molecule's append-only log.
    #[allow(clippy::needless_pass_by_value)]
    pub fn event_appended(
        noyau: impl Into<String>,
        molecule_id: impl Into<String>,
        event_payload: serde_json::Value,
    ) -> Self {
        let molecule_id = molecule_id.into();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let data = serde_json::json!({
            "molecule_id": molecule_id,
            "event": event_payload,
            "timestamp": timestamp,
        });
        Self {
            event: "molecule.event_appended",
            noyau: noyau.into(),
            molecule_id,
            timestamp,
            data,
        }
    }
}

/// The bus itself. Wraps a [`broadcast::Sender`] so publishers clone
/// cheaply and the receiver count is observable for tests.
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<MoleculeEvent>,
}

impl EventBus {
    /// Build a bus with the given backlog capacity. Capacity 0 is
    /// rejected by the underlying channel; callers should use
    /// [`DEFAULT_CAPACITY`].
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Build a bus with [`DEFAULT_CAPACITY`].
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    /// Publish an event. Silent no-op when nobody is subscribed
    /// (broadcast returns [`broadcast::error::SendError`] which we
    /// deliberately ignore — events are best-effort, not durable).
    pub fn publish(&self, event: MoleculeEvent) {
        let _ = self.sender.send(event);
    }

    /// Subscribe to the live tail. The returned receiver yields every
    /// event published after the call; older events are not replayed.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<MoleculeEvent> {
        self.sender.subscribe()
    }

    /// Current subscriber count — useful in tests to assert that the
    /// SSE handler installed a receiver before publication.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

/// Convenience alias used by [`AppState`].
///
/// [`AppState`]: crate::AppState
pub type SharedEventBus = Arc<EventBus>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_with_no_subscribers_is_silent() {
        let bus = EventBus::with_default_capacity();
        bus.publish(MoleculeEvent::state_changed(
            "a",
            "task-xxx",
            "running",
            "completed",
        ));
        assert_eq!(bus.receiver_count(), 0);
    }

    #[tokio::test]
    async fn subscriber_receives_published_event() {
        let bus = EventBus::with_default_capacity();
        let mut rx = bus.subscribe();
        bus.publish(MoleculeEvent::state_changed(
            "a", "task-1", "pending", "running",
        ));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.event, "molecule.state_changed");
        assert_eq!(got.noyau, "a");
        assert_eq!(got.molecule_id, "task-1");
    }

    #[tokio::test]
    async fn event_payload_carries_old_and_new_state() {
        let evt = MoleculeEvent::state_changed("a", "task-1", "pending", "running");
        assert_eq!(evt.data["old_state"], "pending");
        assert_eq!(evt.data["new_state"], "running");
        assert_eq!(evt.data["molecule_id"], "task-1");
    }

    #[tokio::test]
    async fn appended_payload_carries_inner_event() {
        let inner = serde_json::json!({"kind": "comment", "body": "hi"});
        let evt = MoleculeEvent::event_appended("a", "task-1", inner.clone());
        assert_eq!(evt.event, "molecule.event_appended");
        assert_eq!(evt.data["event"], inner);
    }

    #[tokio::test]
    async fn noyau_is_skipped_on_serialisation() {
        let evt = MoleculeEvent::state_changed("a", "task-1", "pending", "running");
        let s = serde_json::to_string(&evt).unwrap();
        assert!(!s.contains("\"noyau\""));
    }
}
