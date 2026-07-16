// SPDX-License-Identifier: AGPL-3.0-only

//! Task dispatch — create beads and nudge targets.
//!
//! Plain-function interface for dispatching work to agents. Routes through
//! the appropriate channel based on [`MessagePriority`] (ADR-015):
//!
//! - **Critical** → Dolt Bead (durable, versioned) via `bd create` + `gt nudge`
//! - **High/Normal** → Signal Bus (structured, queryable) + nudge
//! - **Low** → nudge only; caller appends JSONL event via `cosmon-filestore`
//!
//! Follows ADR-COS-001: standalone functions, no trait indirection.
//!
//! The historical Adapter-dispatch helpers (`get_adapter`,
//! `get_adapter_or_err`, `AdapterNotFound`) were deleted — the real
//! `cs tackle` dispatch is a literal `match adapter.as_str()` on five
//! names, and the dyn-dispatch table had zero non-test callers.

use std::process::Command;

use cosmon_core::message::{select_channel, Channel, MessagePriority};

use crate::beads::{self, BeadsError};

/// Error type for task dispatch operations.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// The bead could not be created.
    #[error("bead creation failed: {0}")]
    BeadFailed(#[from] BeadsError),

    /// The nudge to the target agent failed.
    #[error("nudge failed: {0}")]
    NudgeFailed(String),

    /// An I/O error occurred running a subprocess.
    #[error("I/O error: {0}")]
    Io(String),
}

/// What to create as a GT task.
#[derive(Debug, Clone)]
pub struct TaskSpec<'a> {
    /// Bead title (short summary of the task).
    pub title: &'a str,
    /// Bead type (e.g. "task", "bug").
    pub bead_type: &'a str,
    /// Message priority — determines channel routing.
    pub priority: MessagePriority,
    /// Optional longer description.
    pub description: Option<&'a str>,
    /// Optional assignee (agent address, e.g. "cosmon/polecats/jasper").
    pub assignee: Option<&'a str>,
}

/// Where to deliver the nudge after dispatch.
#[derive(Debug, Clone)]
pub struct NudgeTarget<'a> {
    /// Gas Town agent address (e.g. "cosmon/polecats/jasper").
    pub address: &'a str,
}

/// Result of a [`send_task`] call.
#[derive(Debug, Clone)]
pub struct SendResult {
    /// The channel that was used for dispatch.
    pub channel: Channel,
    /// Bead ID, present only when channel is [`Channel::DoltBead`].
    pub bead_id: Option<String>,
}

/// Dispatch a task, routing through the channel selected by priority.
///
/// Channel selection follows [`select_channel`] (ADR-015):
/// - **Critical** → creates a Dolt Bead via `bd create`, then nudges the target.
///   Returns `SendResult` with `bead_id`.
/// - **High/Normal** → Signal Bus (caller emits via `SignalBus` trait).
///   Nudges the target for immediacy.
/// - **Low** → nudges the target only. The caller is responsible for
///   appending a `TaskDispatched` event to the JSONL log (via `cosmon-filestore`).
///
/// # Errors
///
/// Returns [`DispatchError::BeadFailed`] if bead creation fails (Critical only),
/// or [`DispatchError::NudgeFailed`] if the nudge cannot be delivered.
pub fn send_task(
    spec: &TaskSpec<'_>,
    target: &NudgeTarget<'_>,
) -> Result<SendResult, DispatchError> {
    let channel = select_channel(spec.priority);

    let bead_id = match channel {
        Channel::DoltBead => {
            let id = create_task_bead(spec)?;
            Some(id)
        }
        // TODO(ADR-015): wire SignalBus.emit() here when cosmon-signals adapter lands
        Channel::SignalBus | Channel::JsonlFile | Channel::IpcDirect | _ => None,
    };

    // Nudge the target regardless of channel.
    let message = if let Some(ref id) = bead_id {
        format!("New task assigned: {id}")
    } else {
        format!("New task: {}", spec.title)
    };
    nudge(target.address, &message)?;

    Ok(SendResult { channel, bead_id })
}

/// Create a bead with optional description and assignee fields.
fn create_task_bead(spec: &TaskSpec<'_>) -> Result<String, DispatchError> {
    let bead_id = beads::create_bead(spec.title, spec.bead_type, Some(spec.priority.to_numeric()))?;

    let has_updates = spec.description.is_some() || spec.assignee.is_some();
    if has_updates {
        let mut args = vec!["update", &bead_id];

        let desc_flag;
        if let Some(desc) = spec.description {
            desc_flag = format!("--notes={desc}");
            args.push(&desc_flag);
        }

        let assignee_flag;
        if let Some(assignee) = spec.assignee {
            assignee_flag = format!("--assignee={assignee}");
            args.push(&assignee_flag);
        }

        let output = Command::new("bd")
            .args(&args)
            .output()
            .map_err(|e| DispatchError::Io(format!("failed to run bd update: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DispatchError::Io(format!("bd update failed: {stderr}")));
        }
    }

    Ok(bead_id)
}

/// Send a nudge to a Gas Town agent via `gt nudge`.
///
/// Nudges are zero-cost (no Dolt commit) and best-effort — if the target
/// is not running, the nudge is silently lost.
fn nudge(target: &str, message: &str) -> Result<(), DispatchError> {
    let output = Command::new("gt")
        .args(["nudge", target, message])
        .output()
        .map_err(|e| DispatchError::Io(format!("failed to run gt nudge: {e}")))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(DispatchError::NudgeFailed(format!(
            "gt nudge to {target} failed: {stderr}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_spec_with_priority() {
        let spec = TaskSpec {
            title: "fix the widget",
            bead_type: "task",
            priority: MessagePriority::High,
            description: None,
            assignee: None,
        };
        assert_eq!(spec.title, "fix the widget");
        assert_eq!(spec.priority, MessagePriority::High);
    }

    #[test]
    fn test_task_spec_critical_selects_dolt() {
        let spec = TaskSpec {
            title: "critical fix",
            bead_type: "task",
            priority: MessagePriority::Critical,
            description: None,
            assignee: None,
        };
        assert_eq!(select_channel(spec.priority), Channel::DoltBead);
    }

    #[test]
    fn test_task_spec_high_selects_signal_bus() {
        let spec = TaskSpec {
            title: "urgent coordination",
            bead_type: "task",
            priority: MessagePriority::High,
            description: None,
            assignee: None,
        };
        assert_eq!(select_channel(spec.priority), Channel::SignalBus);
    }

    #[test]
    fn test_task_spec_normal_selects_signal_bus() {
        let spec = TaskSpec {
            title: "routine check",
            bead_type: "task",
            priority: MessagePriority::Normal,
            description: None,
            assignee: None,
        };
        assert_eq!(select_channel(spec.priority), Channel::SignalBus);
    }

    #[test]
    fn test_task_spec_low_selects_jsonl() {
        let spec = TaskSpec {
            title: "background sweep",
            bead_type: "task",
            priority: MessagePriority::Low,
            description: None,
            assignee: None,
        };
        assert_eq!(select_channel(spec.priority), Channel::JsonlFile);
    }

    #[test]
    fn test_nudge_target_construction() {
        let target = NudgeTarget {
            address: "cosmon/polecats/jasper",
        };
        assert_eq!(target.address, "cosmon/polecats/jasper");
    }

    #[test]
    fn test_send_result_dolt_has_bead_id() {
        let result = SendResult {
            channel: Channel::DoltBead,
            bead_id: Some("cs-abc".to_owned()),
        };
        assert_eq!(result.channel, Channel::DoltBead);
        assert!(result.bead_id.is_some());
    }

    #[test]
    fn test_send_result_jsonl_has_no_bead_id() {
        let result = SendResult {
            channel: Channel::JsonlFile,
            bead_id: None,
        };
        assert_eq!(result.channel, Channel::JsonlFile);
        assert!(result.bead_id.is_none());
    }

    #[test]
    fn test_dispatch_error_display() {
        let err = DispatchError::NudgeFailed("timeout".to_owned());
        assert!(err.to_string().contains("nudge failed"));

        let err = DispatchError::Io("broken pipe".to_owned());
        assert!(err.to_string().contains("I/O error"));
    }
}
