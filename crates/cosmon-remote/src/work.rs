// SPDX-License-Identifier: AGPL-3.0-only

//! Advisory collaboration wire requests. Sender identity is deliberately absent.

use cosmon_core::work_message::{
    Confidentiality, ContextObservation, DeliveryAdapter, Disposition,
};
use serde::{Deserialize, Serialize};

/// A retryable send; retaining this exact request and key avoids duplicate admission.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    /// Declared scope revision observed by the caller.
    pub scope_revision: String,
    /// Receiving seat; the host derives the sending seat from its binding.
    pub recipient: String,
    /// Caller-retained retry key.
    pub idempotency_key: String,
    /// Advisory client clock, never a receipt.
    pub sender_time: String,
    /// Earlier envelope answered by this result.
    pub reply_to: Option<String>,
    /// Advisory phase label.
    pub phase: Option<String>,
    /// Declared confidentiality class.
    pub confidentiality: Confidentiality,
    /// Lifetime override.
    pub ttl_secs: Option<u64>,
    /// Exact UTF-8 payload bytes stored by the host.
    pub payload: String,
}

/// An explicit safe-point delivery request, or a read-only peek.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    /// Declared scope revision observed by the caller.
    pub scope_revision: String,
    /// Provider-neutral adapter label.
    pub adapter: DeliveryAdapter,
    /// Optional evidence about context inclusion.
    pub context_observation: Option<ContextObservation>,
    /// Inspect without recording delivery.
    pub peek: bool,
}

/// An acknowledgment is separate from a result sent with `reply_to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckRequest {
    /// Declared scope revision observed by the caller.
    pub scope_revision: String,
    /// Digest from the received envelope.
    pub payload_digest: String,
    /// How the recipient treated the evidence.
    pub disposition: Disposition,
    /// Optional key of an already admitted result.
    pub reply: Option<String>,
    /// Optional bounded consumption note.
    pub note: Option<String>,
}
