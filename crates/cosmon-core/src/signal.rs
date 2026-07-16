// SPDX-License-Identifier: AGPL-3.0-only

//! Inter-agent signal bus — structured communication between workers.
//!
//! Implements the domain types and hexagonal port for ADR-015 (`SQLite` Signal Bus).
//! The signal bus provides structured, queryable inter-agent messaging within a fleet.
//!
//! # Architecture
//!
//! - [`Signal`] — the domain type (fleet-scoped, typed payload, priority-aware)
//! - [`SignalKind`] — discriminates lifecycle vs data vs nudge signals
//! - [`SignalBus`] — hexagonal port (zero I/O trait, adapter in `cosmon-signals`)
//!
//! # Channel Routing (ADR-015 §4)
//!
//! | Priority | Channel | Rationale |
//! |----------|---------|-----------|
//! | Critical | Dolt Bead | Durable audit trail, versioned |
//! | High | SignalBus + tmux nudge | Structured + push hint |
//! | Normal | SignalBus | Structured, queryable |
//! | Low | JSONL File | Append-only, minimal overhead |

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;
use crate::error::CosmonError;
use crate::id::{FleetId, SignalId, WorkerId};
use crate::message::MessagePriority;

// ---------------------------------------------------------------------------
// SignalKind
// ---------------------------------------------------------------------------

/// Discriminates the category of a signal.
///
/// - `Lifecycle`: state transitions, heartbeats, spawn/stop commands
/// - `Data`: structured payloads (results, queries, coordination)
/// - `Nudge`: lightweight attention requests (no payload expected)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SignalKind {
    /// State transitions, heartbeats, spawn/stop commands.
    Lifecycle,
    /// Structured payloads — results, queries, coordination messages.
    #[default]
    Data,
    /// Lightweight attention requests (typically paired with tmux hint).
    Nudge,
}

impl fmt::Display for SignalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lifecycle => f.write_str("lifecycle"),
            Self::Data => f.write_str("data"),
            Self::Nudge => f.write_str("nudge"),
        }
    }
}

impl FromStr for SignalKind {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lifecycle" => Ok(Self::Lifecycle),
            "data" => Ok(Self::Data),
            "nudge" => Ok(Self::Nudge),
            _ => Err(ParseEnumError {
                type_name: "SignalKind",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Signal
// ---------------------------------------------------------------------------

/// A structured inter-agent message in the signal bus.
///
/// Signals are fleet-scoped: a signal belongs to exactly one fleet and can
/// target a specific worker or broadcast to all workers in that fleet
/// (when `to_worker` is `None`).
///
/// # Examples
///
/// ```
/// use cosmon_core::signal::{Signal, SignalKind};
/// use cosmon_core::message::MessagePriority;
/// use cosmon_core::id::{FleetId, WorkerId};
///
/// let signal = Signal::new(
///     FleetId::new("fleet-alpha").unwrap(),
///     WorkerId::new("ep-polecat").unwrap(),
///     Some(WorkerId::new("ep-refinery").unwrap()),
///     SignalKind::Data,
///     MessagePriority::Normal,
///     serde_json::json!({"action": "sync", "target": "main"}),
/// );
///
/// assert_eq!(signal.kind(), SignalKind::Data);
/// assert!(signal.to_worker().is_some());
/// assert!(!signal.is_broadcast());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    /// Unique signal identifier (assigned by the bus adapter on emit).
    /// `None` for signals not yet persisted.
    id: Option<SignalId>,
    /// Fleet this signal belongs to.
    fleet: FleetId,
    /// Worker that emitted the signal.
    from_worker: WorkerId,
    /// Target worker, or `None` for fleet broadcast.
    to_worker: Option<WorkerId>,
    /// Signal category.
    kind: SignalKind,
    /// Routing priority.
    priority: MessagePriority,
    /// JSON payload.
    payload: serde_json::Value,
    /// Timestamp when the signal was created.
    created_at: DateTime<Utc>,
    /// Timestamp when the signal was read by the receiver.
    /// `None` means unread.
    read_at: Option<DateTime<Utc>>,
}

impl Signal {
    /// Create a new signal (not yet persisted — `id` is `None`).
    #[must_use]
    pub fn new(
        fleet: FleetId,
        from_worker: WorkerId,
        to_worker: Option<WorkerId>,
        kind: SignalKind,
        priority: MessagePriority,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: None,
            fleet,
            from_worker,
            to_worker,
            kind,
            priority,
            payload,
            created_at: Utc::now(),
            read_at: None,
        }
    }

    /// Create a signal with all fields specified (for hydrating from storage).
    ///
    /// Takes a pre-built `Signal` (via [`Signal::new`]) and sets the persisted
    /// fields: `id`, `created_at`, and optionally `read_at`.
    #[must_use]
    pub fn hydrate(
        mut self,
        id: SignalId,
        created_at: DateTime<Utc>,
        read_at: Option<DateTime<Utc>>,
    ) -> Self {
        self.id = Some(id);
        self.created_at = created_at;
        self.read_at = read_at;
        self
    }

    /// Signal identifier (assigned after emit).
    #[must_use]
    pub fn id(&self) -> Option<&SignalId> {
        self.id.as_ref()
    }

    /// Set the signal ID (called by [`SignalBus`] adapters after persistence).
    ///
    /// Prefer [`Signal::hydrate`] when reconstructing from storage.
    /// This method is for the emit path where the adapter assigns an ID
    /// to a signal that was just inserted.
    ///
    /// # Panics
    /// Debug-asserts that `id` has not already been set — a signal should
    /// only be persisted once.
    pub fn set_id(&mut self, id: SignalId) {
        debug_assert!(self.id.is_none(), "signal ID already set");
        self.id = Some(id);
    }

    /// Fleet this signal belongs to.
    #[must_use]
    pub fn fleet(&self) -> &FleetId {
        &self.fleet
    }

    /// Worker that emitted this signal.
    ///
    /// Named `sender` rather than `from_worker` to avoid the `from_*` convention
    /// (which clippy reserves for constructors). The serialized field name remains
    /// `from_worker` for SQL schema alignment.
    #[must_use]
    pub fn sender(&self) -> &WorkerId {
        &self.from_worker
    }

    /// Target worker, or `None` for broadcast.
    #[must_use]
    pub fn to_worker(&self) -> Option<&WorkerId> {
        self.to_worker.as_ref()
    }

    /// Whether this signal targets all workers in the fleet.
    #[must_use]
    pub fn is_broadcast(&self) -> bool {
        self.to_worker.is_none()
    }

    /// Signal category.
    #[must_use]
    pub fn kind(&self) -> SignalKind {
        self.kind
    }

    /// Routing priority.
    #[must_use]
    pub fn priority(&self) -> MessagePriority {
        self.priority
    }

    /// JSON payload.
    #[must_use]
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }

    /// Creation timestamp.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Read timestamp (`None` if unread).
    #[must_use]
    pub fn read_at(&self) -> Option<DateTime<Utc>> {
        self.read_at
    }

    /// Whether this signal has been read.
    #[must_use]
    pub fn is_read(&self) -> bool {
        self.read_at.is_some()
    }

    /// Mark this signal as read at the given timestamp.
    pub fn mark_read(&mut self, at: DateTime<Utc>) {
        self.read_at = Some(at);
    }

    /// Whether this signal requires a tmux push hint.
    ///
    /// Returns `true` when the signal warrants an active tmux nudge for
    /// sub-second delivery. Two conditions trigger the hint:
    /// - **High priority** signals routed through the [`SignalBus`] channel.
    /// - **Nudge kind** signals, regardless of priority (ADR-015 Phase 4).
    ///
    /// Critical signals route through Dolt Bead, not the signal bus, so they
    /// never need a push hint even if they happen to be Nudge kind.
    #[must_use]
    pub fn needs_push_hint(&self) -> bool {
        if self.priority == MessagePriority::Critical {
            return false;
        }
        self.priority == MessagePriority::High || self.kind == SignalKind::Nudge
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id_str = self
            .id
            .as_ref()
            .map_or_else(|| "?".to_owned(), ToString::to_string);
        let target = self
            .to_worker
            .as_ref()
            .map_or_else(|| "*".to_owned(), ToString::to_string);
        write!(
            f,
            "signal[{}] {} → {} ({}/{})",
            id_str, self.from_worker, target, self.kind, self.priority,
        )
    }
}

// ---------------------------------------------------------------------------
// SignalBus trait (hexagonal port)
// ---------------------------------------------------------------------------

/// Hexagonal port for inter-agent signaling.
///
/// Adapters (e.g. `cosmon-signals` with `SQLite` WAL) implement this trait.
/// The core domain uses it without knowing the storage backend.
///
/// Signals are fleet-scoped: all three methods require an explicit [`FleetId`]
/// to ensure the fleet boundary is the communication boundary (ADR-015 §6).
///
/// # Contract
///
/// - `emit` persists a signal and returns the assigned [`SignalId`].
/// - `receive` returns unread signals for a worker (including broadcasts)
///   and marks them as read. Results are ordered oldest-first.
/// - `pending` returns a fast count of unread signals without reading them.
pub trait SignalBus: Send + Sync {
    /// Persist a signal and return its assigned ID.
    ///
    /// The signal's [`Signal::fleet`] determines which fleet's bus receives it.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the signal cannot be persisted.
    fn emit(&self, signal: &Signal) -> Result<SignalId, CosmonError>;

    /// Receive unread signals for a worker within a fleet (including broadcasts).
    ///
    /// Returns up to `limit` signals, oldest first. Marks returned signals
    /// as read atomically.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the query fails.
    fn receive(
        &self,
        fleet: &FleetId,
        worker: &WorkerId,
        limit: usize,
    ) -> Result<Vec<Signal>, CosmonError>;

    /// Count unread signals for a worker within a fleet (including broadcasts).
    ///
    /// This is a fast check — no signals are marked as read.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the query fails.
    fn pending(&self, fleet: &FleetId, worker: &WorkerId) -> Result<usize, CosmonError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fleet() -> FleetId {
        FleetId::new("fleet-alpha").unwrap()
    }

    fn test_worker(name: &str) -> WorkerId {
        WorkerId::new(name).unwrap()
    }

    #[test]
    fn test_signal_new_has_no_id() {
        let signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            Some(test_worker("ep-refinery")),
            SignalKind::Data,
            MessagePriority::Normal,
            serde_json::json!({"action": "sync"}),
        );
        assert!(signal.id().is_none());
        assert!(!signal.is_broadcast());
        assert!(!signal.is_read());
        assert_eq!(signal.kind(), SignalKind::Data);
        assert_eq!(signal.priority(), MessagePriority::Normal);
    }

    #[test]
    fn test_signal_broadcast() {
        let signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Lifecycle,
            MessagePriority::High,
            serde_json::json!({"event": "heartbeat"}),
        );
        assert!(signal.is_broadcast());
        assert!(signal.to_worker().is_none());
    }

    #[test]
    fn test_signal_mark_read() {
        let mut signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            Some(test_worker("ep-refinery")),
            SignalKind::Data,
            MessagePriority::Normal,
            serde_json::json!(null),
        );
        assert!(!signal.is_read());

        let now = Utc::now();
        signal.mark_read(now);
        assert!(signal.is_read());
        assert_eq!(signal.read_at(), Some(now));
    }

    #[test]
    fn test_signal_set_id() {
        let mut signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Nudge,
            MessagePriority::Low,
            serde_json::json!(null),
        );
        assert!(signal.id().is_none());

        let id = SignalId::new("42").unwrap();
        signal.set_id(id.clone());
        assert_eq!(signal.id(), Some(&id));
    }

    #[test]
    fn test_signal_needs_push_hint() {
        let high = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Data,
            MessagePriority::High,
            serde_json::json!(null),
        );
        assert!(high.needs_push_hint());

        let critical = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Lifecycle,
            MessagePriority::Critical,
            serde_json::json!(null),
        );
        // Critical routes to DoltBead, not SignalBus — no push hint needed.
        assert!(!critical.needs_push_hint());

        let normal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Data,
            MessagePriority::Normal,
            serde_json::json!(null),
        );
        assert!(!normal.needs_push_hint());

        let low = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Data,
            MessagePriority::Low,
            serde_json::json!(null),
        );
        assert!(!low.needs_push_hint());

        // Nudge kind triggers push hint regardless of priority (ADR-015 Phase 4).
        let nudge_normal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Nudge,
            MessagePriority::Normal,
            serde_json::json!(null),
        );
        assert!(nudge_normal.needs_push_hint());

        let nudge_low = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Nudge,
            MessagePriority::Low,
            serde_json::json!(null),
        );
        assert!(nudge_low.needs_push_hint());

        // Critical Nudge still routes to DoltBead — no push hint.
        let nudge_critical = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Nudge,
            MessagePriority::Critical,
            serde_json::json!(null),
        );
        assert!(!nudge_critical.needs_push_hint());
    }

    #[test]
    fn test_signal_hydrate_roundtrip() {
        let id = SignalId::new("7").unwrap();
        let now = Utc::now();
        let signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            Some(test_worker("ep-refinery")),
            SignalKind::Data,
            MessagePriority::Normal,
            serde_json::json!({"key": "value"}),
        )
        .hydrate(id.clone(), now, None);
        assert_eq!(signal.id(), Some(&id));
        assert_eq!(signal.created_at(), now);
        assert!(!signal.is_read());
    }

    #[test]
    fn test_signal_serde_roundtrip() {
        let mut signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            Some(test_worker("ep-refinery")),
            SignalKind::Data,
            MessagePriority::High,
            serde_json::json!({"x": 42}),
        );
        signal.set_id(SignalId::new("99").unwrap());

        let json = serde_json::to_string(&signal).unwrap();
        let parsed: Signal = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id(), signal.id());
        assert_eq!(parsed.fleet(), signal.fleet());
        assert_eq!(parsed.sender(), signal.sender());
        assert_eq!(parsed.to_worker(), signal.to_worker());
        assert_eq!(parsed.kind(), signal.kind());
        assert_eq!(parsed.priority(), signal.priority());
        assert_eq!(parsed.payload(), signal.payload());
    }

    #[test]
    fn test_signal_display() {
        let mut signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            Some(test_worker("ep-refinery")),
            SignalKind::Data,
            MessagePriority::Normal,
            serde_json::json!(null),
        );
        // Before ID assigned
        let display = signal.to_string();
        assert!(display.contains('?'));
        assert!(display.contains("ep-polecat"));
        assert!(display.contains("ep-refinery"));

        // After ID assigned
        signal.set_id(SignalId::new("42").unwrap());
        let display = signal.to_string();
        assert!(display.contains("42"));
    }

    #[test]
    fn test_signal_display_broadcast() {
        let signal = Signal::new(
            test_fleet(),
            test_worker("ep-polecat"),
            None,
            SignalKind::Nudge,
            MessagePriority::Low,
            serde_json::json!(null),
        );
        let display = signal.to_string();
        assert!(display.contains('*'), "broadcast should show * target");
    }

    #[test]
    fn test_signal_kind_default() {
        assert_eq!(SignalKind::default(), SignalKind::Data);
    }

    #[test]
    fn test_signal_kind_display_roundtrip() {
        for kind in [SignalKind::Lifecycle, SignalKind::Data, SignalKind::Nudge] {
            let s = kind.to_string();
            let parsed: SignalKind = s.parse().unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn test_signal_kind_parse_error() {
        let result: Result<SignalKind, _> = "unknown".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_signal_kind_serde_roundtrip() {
        for kind in [SignalKind::Lifecycle, SignalKind::Data, SignalKind::Nudge] {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: SignalKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn test_signal_kind_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&SignalKind::Lifecycle).unwrap(),
            "\"lifecycle\""
        );
        assert_eq!(
            serde_json::to_string(&SignalKind::Data).unwrap(),
            "\"data\""
        );
        assert_eq!(
            serde_json::to_string(&SignalKind::Nudge).unwrap(),
            "\"nudge\""
        );
    }
}
