// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence wire requests (issue #212, W7a). Mission, attachment, role
//! and lease epoch are deliberately absent: the host takes the first two from
//! the binding, and presence confers no authority.

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
