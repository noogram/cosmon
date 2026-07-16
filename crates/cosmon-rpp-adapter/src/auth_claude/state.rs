// SPDX-License-Identifier: AGPL-3.0-only

//! Session state machine. The six states and the transitions are
//! defined in the protocol spec v1.1 §3.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Session lifecycle state. Order is canonical for matching against
/// the OpenAPI `SessionState` enum (`AWAITING_EMAIL` is an alias for
/// `INIT` in client-facing responses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthState {
    /// Session created; email not yet submitted.
    Init,
    /// PKCE verifier + state generated; authorize URL built; waiting
    /// for the operator to paste back the `authorization_code` from
    /// Anthropic's manual-redirect page.
    AwaitingUserApproval,
    /// Token exchange succeeded; credentials written.
    Completed,
    /// Token exchange failed permanently (Anthropic refused, network
    /// error after retries).
    Failed,
    /// Session deleted (by DELETE or by TTL when no progression).
    Cancelled,
    /// Session TTL expired with no paste-back.
    Expired,
}

impl AuthState {
    /// Wire label for client responses. `INIT` is canonicalised to
    /// `AWAITING_EMAIL` in client-facing payloads (spec §3.1).
    #[must_use]
    pub fn wire_label(self) -> &'static str {
        match self {
            Self::Init => "AWAITING_EMAIL",
            Self::AwaitingUserApproval => "AWAITING_USER_APPROVAL",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::Expired => "EXPIRED",
        }
    }
}

/// Error payload attached to a session in `FAILED` / `EXPIRED` state.
/// Mirrors the OpenAPI `ErrorPayload` schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

/// Persistent session record. Written as one JSON file per session
/// under `<state_dir>/auth-sessions/<session_id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    /// Unique session identifier — format `auth-YYYYMMDD-<6 hex>`.
    pub session_id: String,
    /// Wall-clock creation time (UTC).
    pub created_at: DateTime<Utc>,
    /// Absolute time at which the session is GC'd if no progression.
    pub ttl_at: DateTime<Utc>,
    /// Lifecycle state.
    pub state: AuthState,
    /// Operator email (set on `POST /email`). Plaintext on disk for
    /// audit; never echoed back to the client after first submission.
    pub email: Option<String>,
    /// PKCE `code_verifier` (43–128 char URL-safe base64). Secret;
    /// **purged** from the session after token exchange.
    pub code_verifier: Option<String>,
    /// PKCE `state` parameter (CSRF token). Surfaced to the client as
    /// `oauth_state` for round-trip validation.
    pub oauth_state: Option<String>,
    /// Built Anthropic authorize URL (carries `code_challenge` etc).
    pub verification_url: Option<String>,
    /// Absolute time at which the upstream PKCE flow expires
    /// (same as `ttl_at` for now; carried separately so a future
    /// distinction is possible without schema churn).
    pub expires_at: Option<DateTime<Utc>>,
    /// Error payload (set in `FAILED` / `EXPIRED`).
    pub error: Option<SessionError>,
    /// Account email returned by Anthropic on successful token exchange.
    pub account_email: Option<String>,
}

impl AuthSession {
    /// Create a fresh session in `INIT` (alias `AWAITING_EMAIL`).
    #[must_use]
    pub fn new(session_id: String, now: DateTime<Utc>, ttl: chrono::Duration) -> Self {
        Self {
            session_id,
            created_at: now,
            ttl_at: now + ttl,
            state: AuthState::Init,
            email: None,
            code_verifier: None,
            oauth_state: None,
            verification_url: None,
            expires_at: None,
            error: None,
            account_email: None,
        }
    }

    /// True if `ttl_at` is past `now`.
    #[must_use]
    pub fn is_ttl_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.ttl_at
    }

    /// Returns the wire-shaped client-facing state, applying the
    /// `INIT → AWAITING_EMAIL` alias.
    #[must_use]
    pub fn wire_state(&self) -> &'static str {
        self.state.wire_label()
    }
}

/// Transition error returned by [`AuthSession::transition_to_email`] etc.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    /// The endpoint requires a different source state.
    #[error("session in state {current_state} cannot transition via {endpoint}")]
    InvalidTransition {
        /// Endpoint that was called.
        endpoint: &'static str,
        /// State the session was actually in.
        current_state: &'static str,
    },
    /// The session TTL has expired.
    #[error("session expired")]
    Expired,
}

impl AuthSession {
    /// Apply the `POST /email` transition.
    /// Caller must have already verified TTL.
    pub fn transition_to_email(
        &mut self,
        email: String,
        code_verifier: String,
        oauth_state: String,
        verification_url: String,
        expires_at: DateTime<Utc>,
    ) -> Result<(), TransitionError> {
        if !matches!(self.state, AuthState::Init) {
            return Err(TransitionError::InvalidTransition {
                endpoint: "POST /email",
                current_state: self.state.wire_label(),
            });
        }
        self.email = Some(email);
        self.code_verifier = Some(code_verifier);
        self.oauth_state = Some(oauth_state);
        self.verification_url = Some(verification_url);
        self.expires_at = Some(expires_at);
        self.state = AuthState::AwaitingUserApproval;
        Ok(())
    }

    /// Apply the `POST /confirm` success transition: token exchange
    /// succeeded. `code_verifier` is purged from the persisted session.
    pub fn transition_to_completed(
        &mut self,
        account_email: Option<String>,
    ) -> Result<(), TransitionError> {
        if !matches!(self.state, AuthState::AwaitingUserApproval) {
            return Err(TransitionError::InvalidTransition {
                endpoint: "POST /confirm",
                current_state: self.state.wire_label(),
            });
        }
        self.code_verifier = None;
        self.account_email = account_email;
        self.state = AuthState::Completed;
        Ok(())
    }

    /// Apply the `POST /confirm` failure transition: Anthropic
    /// refused the token exchange. `code_verifier` is purged.
    pub fn transition_to_failed(&mut self, error: SessionError) -> Result<(), TransitionError> {
        if !matches!(self.state, AuthState::AwaitingUserApproval) {
            return Err(TransitionError::InvalidTransition {
                endpoint: "POST /confirm",
                current_state: self.state.wire_label(),
            });
        }
        self.code_verifier = None;
        self.error = Some(error);
        self.state = AuthState::Failed;
        Ok(())
    }

    /// Apply the GC `EXPIRED` transition.
    pub fn transition_to_expired(&mut self) {
        if matches!(
            self.state,
            AuthState::AwaitingUserApproval | AuthState::Init
        ) {
            self.code_verifier = None;
            self.state = AuthState::Expired;
            self.error = Some(SessionError {
                code: "session_expired".to_owned(),
                message: "Session TTL expired before paste-back".to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 5, 19, 14, 0, 0).unwrap()
    }

    #[test]
    fn new_session_is_init() {
        let s = AuthSession::new(
            "auth-20260519-a8f2c1".into(),
            now(),
            chrono::Duration::minutes(15),
        );
        assert_eq!(s.state, AuthState::Init);
        assert_eq!(s.wire_state(), "AWAITING_EMAIL");
        assert_eq!(s.ttl_at, now() + chrono::Duration::minutes(15));
    }

    #[test]
    fn email_transition_from_init_ok() {
        let mut s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        let r = s.transition_to_email(
            "x@y".into(),
            "verifier".into(),
            "state".into(),
            "https://auth/?…".into(),
            now() + chrono::Duration::minutes(15),
        );
        assert!(r.is_ok());
        assert_eq!(s.state, AuthState::AwaitingUserApproval);
        assert_eq!(s.code_verifier.as_deref(), Some("verifier"));
    }

    #[test]
    fn email_transition_rejected_from_awaiting() {
        let mut s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        s.state = AuthState::AwaitingUserApproval;
        let r = s.transition_to_email(
            "x@y".into(),
            "v".into(),
            "st".into(),
            "u".into(),
            now() + chrono::Duration::minutes(15),
        );
        assert!(matches!(r, Err(TransitionError::InvalidTransition { .. })));
    }

    #[test]
    fn confirm_success_purges_verifier() {
        let mut s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        s.transition_to_email(
            "x@y".into(),
            "verifier-secret".into(),
            "st".into(),
            "u".into(),
            now() + chrono::Duration::minutes(15),
        )
        .unwrap();
        s.transition_to_completed(Some("anth@x".into())).unwrap();
        assert_eq!(s.state, AuthState::Completed);
        assert!(
            s.code_verifier.is_none(),
            "verifier must be purged after success"
        );
        assert_eq!(s.account_email.as_deref(), Some("anth@x"));
    }

    #[test]
    fn confirm_failure_purges_verifier_and_records_error() {
        let mut s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        s.transition_to_email(
            "x@y".into(),
            "verifier-secret".into(),
            "st".into(),
            "u".into(),
            now() + chrono::Duration::minutes(15),
        )
        .unwrap();
        s.transition_to_failed(SessionError {
            code: "invalid_grant".into(),
            message: "expired_code".into(),
        })
        .unwrap();
        assert_eq!(s.state, AuthState::Failed);
        assert!(s.code_verifier.is_none());
        assert_eq!(s.error.as_ref().unwrap().code, "invalid_grant");
    }

    #[test]
    fn confirm_rejected_outside_awaiting() {
        let mut s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        // Still in INIT — confirm should refuse.
        let r = s.transition_to_completed(None);
        assert!(matches!(r, Err(TransitionError::InvalidTransition { .. })));
    }

    #[test]
    fn ttl_expiry_check() {
        let s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        assert!(!s.is_ttl_expired(now()));
        assert!(!s.is_ttl_expired(now() + chrono::Duration::minutes(14)));
        assert!(s.is_ttl_expired(now() + chrono::Duration::minutes(15)));
        assert!(s.is_ttl_expired(now() + chrono::Duration::minutes(20)));
    }

    #[test]
    fn expired_transition_only_from_init_or_awaiting() {
        let mut s = AuthSession::new("s".into(), now(), chrono::Duration::minutes(15));
        s.transition_to_email(
            "x@y".into(),
            "verifier".into(),
            "st".into(),
            "u".into(),
            now() + chrono::Duration::minutes(15),
        )
        .unwrap();
        s.transition_to_expired();
        assert_eq!(s.state, AuthState::Expired);
        assert!(s.code_verifier.is_none());

        // After expired, calling again is a no-op (idempotent — state machine guard).
        s.transition_to_expired();
        assert_eq!(s.state, AuthState::Expired);
    }
}
