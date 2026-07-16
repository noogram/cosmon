// SPDX-License-Identifier: AGPL-3.0-only

//! Message types for agent communication.
//!
//! Defines the priority levels and transport channels that form the nervous
//! tissue architecture — a three-channel system where critical signals flow
//! through Dolt Beads, structured coordination through the Signal Bus (ADR-015),
//! and low-priority ephemeral data through JSONL files.
//!
//! # Channel Selection
//!
//! The orchestrator uses [`select_channel`] to route messages:
//!
//! | Priority | Channel | Rationale |
//! |----------|---------|-----------|
//! | Critical | Dolt Bead | Must survive crashes, auditable, versioned |
//! | High | Signal Bus | Structured + tmux push hint for immediacy |
//! | Normal | Signal Bus | Structured, queryable inter-agent messaging |
//! | Low | JSONL File | Ephemeral coordination, minimal overhead |

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;

/// Priority level for inter-agent messages.
///
/// Variant order determines [`Ord`]: `Critical` is highest priority.
/// The ordering is *descending* — `Critical > High > Normal > Low` — so that
/// `max()` on a collection of priorities yields the most urgent one.
///
/// Priority maps directly to a [`Channel`] via [`select_channel`]:
/// Critical and High messages need crash-safe, versioned storage (Dolt Bead),
/// while Normal and Low messages use fast append-only JSONL.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MessagePriority {
    // Variant order determines Ord: first variant is smallest.
    /// Routine, deferrable messages (e.g. telemetry, optional reports).
    Low,
    /// Default priority for most inter-agent communication.
    #[default]
    Normal,
    /// Time-sensitive messages that should be processed promptly.
    High,
    /// Lifecycle-critical signals (heartbeats, kill commands). Never dropped.
    Critical,
}

impl MessagePriority {
    /// Convert a numeric priority (1 = highest) to a `MessagePriority`.
    ///
    /// Maps: 1 → Critical, 2 → High, 3 → Normal, 4+ → Low.
    /// `None` defaults to Normal.
    #[must_use]
    pub fn from_numeric(priority: Option<u8>) -> Self {
        match priority {
            Some(1) => Self::Critical,
            Some(2) => Self::High,
            Some(3) | None => Self::Normal,
            Some(_) => Self::Low,
        }
    }

    /// Convert to the numeric priority used by the beads CLI.
    #[must_use]
    pub fn to_numeric(self) -> u8 {
        match self {
            Self::Critical => 1,
            Self::High => 2,
            Self::Normal => 3,
            Self::Low => 4,
        }
    }
}

impl fmt::Display for MessagePriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => f.write_str("low"),
            Self::Normal => f.write_str("normal"),
            Self::High => f.write_str("high"),
            Self::Critical => f.write_str("critical"),
        }
    }
}

impl FromStr for MessagePriority {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "normal" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            _ => Err(ParseEnumError {
                type_name: "MessagePriority",
                value: s.to_owned(),
            }),
        }
    }
}

/// Transport channel for inter-agent communication.
///
/// Each variant represents a distinct physical transport mechanism. The nervous
/// tissue architecture routes messages to channels based on priority and
/// latency requirements. See [`select_channel`] for the routing logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Channel {
    /// Direct inter-process communication (e.g. Unix socket, pipe).
    /// Lowest latency — used for critical control signals.
    IpcDirect,
    /// JSONL file-based channel (append-only log files).
    /// Low overhead — used for ephemeral, low-priority data.
    JsonlFile,
    /// Dolt-backed bead storage channel.
    /// Highest durability — used for persistent, auditable messages.
    DoltBead,
    /// `SQLite` signal bus (ADR-015).
    /// Structured, queryable inter-agent messaging. High-priority signals
    /// get an additional tmux push hint for sub-second delivery.
    SignalBus,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IpcDirect => f.write_str("ipc_direct"),
            Self::JsonlFile => f.write_str("jsonl_file"),
            Self::DoltBead => f.write_str("dolt_bead"),
            Self::SignalBus => f.write_str("signal_bus"),
        }
    }
}

impl FromStr for Channel {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ipc_direct" => Ok(Self::IpcDirect),
            "jsonl_file" => Ok(Self::JsonlFile),
            "dolt_bead" => Ok(Self::DoltBead),
            "signal_bus" => Ok(Self::SignalBus),
            _ => Err(ParseEnumError {
                type_name: "Channel",
                value: s.to_owned(),
            }),
        }
    }
}

/// Select the appropriate channel based on message priority.
///
/// This is the core routing decision in the communication fabric (ADR-015 §4):
/// - Critical → [`Channel::DoltBead`] (durable audit trail, versioned)
/// - High → [`Channel::SignalBus`] (structured + tmux push hint)
/// - Normal → [`Channel::SignalBus`] (structured, queryable)
/// - Low → [`Channel::JsonlFile`] (append-only, minimal overhead)
///
/// # Examples
///
/// ```
/// use cosmon_core::message::{MessagePriority, Channel, select_channel};
///
/// assert_eq!(select_channel(MessagePriority::Critical), Channel::DoltBead);
/// assert_eq!(select_channel(MessagePriority::High), Channel::SignalBus);
/// assert_eq!(select_channel(MessagePriority::Normal), Channel::SignalBus);
/// assert_eq!(select_channel(MessagePriority::Low), Channel::JsonlFile);
/// ```
#[must_use]
pub fn select_channel(priority: MessagePriority) -> Channel {
    match priority {
        MessagePriority::Critical => Channel::DoltBead,
        MessagePriority::High | MessagePriority::Normal => Channel::SignalBus,
        MessagePriority::Low => Channel::JsonlFile,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        assert!(MessagePriority::Low < MessagePriority::Normal);
        assert!(MessagePriority::Normal < MessagePriority::High);
        assert!(MessagePriority::High < MessagePriority::Critical);

        // max() should yield the most urgent priority
        let priorities = [
            MessagePriority::Low,
            MessagePriority::Critical,
            MessagePriority::Normal,
        ];
        assert_eq!(priorities.iter().max(), Some(&MessagePriority::Critical));
    }

    #[test]
    fn test_priority_default() {
        assert_eq!(MessagePriority::default(), MessagePriority::Normal);
    }

    #[test]
    fn test_priority_display_roundtrip() {
        for p in [
            MessagePriority::Low,
            MessagePriority::Normal,
            MessagePriority::High,
            MessagePriority::Critical,
        ] {
            let s = p.to_string();
            let parsed: MessagePriority = s.parse().unwrap();
            assert_eq!(parsed, p);
        }
    }

    #[test]
    fn test_priority_parse_error() {
        let result: Result<MessagePriority, _> = "urgent".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_select_channel_critical_to_dolt() {
        assert_eq!(select_channel(MessagePriority::Critical), Channel::DoltBead);
    }

    #[test]
    fn test_select_channel_high_normal_to_signal_bus() {
        assert_eq!(select_channel(MessagePriority::High), Channel::SignalBus);
        assert_eq!(select_channel(MessagePriority::Normal), Channel::SignalBus);
    }

    #[test]
    fn test_select_channel_low_to_jsonl() {
        assert_eq!(select_channel(MessagePriority::Low), Channel::JsonlFile);
    }

    #[test]
    fn test_from_numeric_mapping() {
        assert_eq!(
            MessagePriority::from_numeric(Some(1)),
            MessagePriority::Critical
        );
        assert_eq!(
            MessagePriority::from_numeric(Some(2)),
            MessagePriority::High
        );
        assert_eq!(
            MessagePriority::from_numeric(Some(3)),
            MessagePriority::Normal
        );
        assert_eq!(MessagePriority::from_numeric(Some(4)), MessagePriority::Low);
        assert_eq!(
            MessagePriority::from_numeric(Some(99)),
            MessagePriority::Low
        );
        assert_eq!(MessagePriority::from_numeric(None), MessagePriority::Normal);
    }

    #[test]
    fn test_to_numeric_roundtrip() {
        for p in [
            MessagePriority::Critical,
            MessagePriority::High,
            MessagePriority::Normal,
            MessagePriority::Low,
        ] {
            assert_eq!(MessagePriority::from_numeric(Some(p.to_numeric())), p);
        }
    }

    #[test]
    fn test_channel_display_roundtrip() {
        for c in [
            Channel::IpcDirect,
            Channel::JsonlFile,
            Channel::DoltBead,
            Channel::SignalBus,
        ] {
            let s = c.to_string();
            let parsed: Channel = s.parse().unwrap();
            assert_eq!(parsed, c);
        }
    }

    #[test]
    fn test_channel_parse_error() {
        let result: Result<Channel, _> = "redis".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_priority_serde_roundtrip() {
        for p in [
            MessagePriority::Low,
            MessagePriority::Normal,
            MessagePriority::High,
            MessagePriority::Critical,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let parsed: MessagePriority = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, p);
        }
    }

    #[test]
    fn test_channel_serde_roundtrip() {
        for c in [
            Channel::IpcDirect,
            Channel::JsonlFile,
            Channel::DoltBead,
            Channel::SignalBus,
        ] {
            let json = serde_json::to_string(&c).unwrap();
            let parsed: Channel = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, c);
        }
    }

    #[test]
    fn test_serde_snake_case_format() {
        // Verify serde uses snake_case as configured
        assert_eq!(
            serde_json::to_string(&MessagePriority::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&Channel::IpcDirect).unwrap(),
            "\"ipc_direct\""
        );
        assert_eq!(
            serde_json::to_string(&Channel::DoltBead).unwrap(),
            "\"dolt_bead\""
        );
        assert_eq!(
            serde_json::to_string(&Channel::SignalBus).unwrap(),
            "\"signal_bus\""
        );
    }
}
