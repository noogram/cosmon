// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence and message wire requests (issue #212, W7a and W7b).
//! Mission, attachment, author, role and lease epoch are deliberately absent:
//! the host takes the first three from the binding, and neither presence nor
//! a message confers authority.

use serde::{Deserialize, Serialize};

/// A retryable attach; retrying with this exact request and key returns the
/// same attachment record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachRequest {
    /// Caller-retained retry key.
    pub idempotency_key: String,
    /// Optional display label; never authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A retryable pilot message (S-SEND); retrying with this exact request and
/// key returns the same message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    /// Caller-retained retry key.
    pub idempotency_key: String,
    /// Recipient: an attachment or local session that pilots the mission.
    pub to: String,
    /// Message body, at most 65536 bytes.
    pub message: String,
    /// Seconds after which an unacknowledged message reads as expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
}
