// SPDX-License-Identifier: AGPL-3.0-only

//! Axum handlers for the five auth-claude endpoints. Each handler
//! resolves the per-adapter [`crate::auth_claude::AuthClaudeState`] and
//! translates a session-machine result into the HTTP envelope defined
//! by the OpenAPI 3.1 schema (`auth-claude-api.openapi.yaml`, v1.1).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth_claude::anthropic::{exchange_code, TokenExchangeError};
use crate::auth_claude::credentials::write_credentials_file;
use crate::auth_claude::pkce::{
    build_authorize_url, new_code_verifier, new_oauth_state, new_session_id,
};
use crate::auth_claude::state::{AuthSession, AuthState, SessionError};
use crate::auth_claude::store::StoreError;
use crate::AppState;

/// Maximum number of concurrent open sessions (per container) before
/// `POST /start` and `POST /email` are throttled with 429 — spec §8.
pub const MAX_OPEN_SESSIONS: usize = 5;

/// `POST /v1/auth/claude/start` — create a new session in `INIT`.
pub async fn start(State(state): State<Arc<AppState>>) -> Response {
    let Some(ac) = state.auth_claude.as_ref() else {
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "auth-claude surface is not enabled on this adapter",
            None,
            None,
        );
    };

    let open = match ac.store.count_open() {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(event = "auth.session.store_error", error = %e);
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Cannot persist session to storage",
                None,
                None,
            );
        }
    };
    if open >= MAX_OPEN_SESSIONS {
        tracing::warn!(event = "auth.ratelimit.hit", open_sessions = open);
        return err_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
            "Max 5 concurrent auth sessions per container.",
            None,
            None,
        );
    }

    let now = Utc::now();
    let ttl =
        chrono::Duration::from_std(ac.config.session_ttl).unwrap_or(chrono::Duration::minutes(15));
    let session_id = new_session_id();
    let session = AuthSession::new(session_id.clone(), now, ttl);
    if let Err(e) = ac.store.upsert(&session) {
        tracing::error!(event = "auth.session.store_error", session_id = %session_id, error = %e);
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "Cannot persist session to storage",
            Some(&session_id),
            None,
        );
    }

    tracing::info!(
        event = "auth.session.started",
        session_id = %session.session_id,
        created_at = %session.created_at.to_rfc3339(),
        ttl_at = %session.ttl_at.to_rfc3339(),
    );

    Json(json!({
        "session_id": session.session_id,
        "state": session.wire_state(),
        "created_at": session.created_at.to_rfc3339(),
        "ttl_at": session.ttl_at.to_rfc3339(),
    }))
    .into_response()
}

/// Request body for `POST /v1/auth/claude/email`.
#[derive(Debug, Deserialize)]
pub struct EmailBody {
    /// Session id to attach the email to.
    pub session_id: String,
    /// Operator email (Claude Max account address).
    pub email: String,
}

/// `POST /v1/auth/claude/email` — submit email, build PKCE authorize URL.
pub async fn submit_email(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmailBody>,
) -> Response {
    let Some(ac) = state.auth_claude.as_ref() else {
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "auth-claude surface is not enabled on this adapter",
            None,
            None,
        );
    };

    if !body.email.contains('@') {
        return err_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Field 'email' must be a syntactically valid email address",
            Some(&body.session_id),
            None,
        );
    }

    let mut session = match ac.store.load(&body.session_id) {
        Ok(s) => s,
        Err(StoreError::NotFound | StoreError::InvalidSessionId) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "No session matches the given session_id.",
                Some(&body.session_id),
                None,
            );
        }
        Err(e) => {
            tracing::error!(event = "auth.session.store_error", session_id = %body.session_id, error = %e);
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Cannot read session from storage",
                Some(&body.session_id),
                None,
            );
        }
    };

    let now = Utc::now();
    if session.is_ttl_expired(now) {
        session.transition_to_expired();
        let _ = ac.store.upsert(&session);
        return err_response(
            StatusCode::GONE,
            "session_expired",
            "Session has expired; create a new one.",
            Some(&body.session_id),
            Some(session.wire_state()),
        );
    }

    let code_verifier = new_code_verifier();
    let oauth_state = new_oauth_state();
    let verification_url = build_authorize_url(
        &ac.config.authorize_url,
        &ac.config.client_id,
        &ac.config.redirect_uri,
        &ac.config.scopes,
        &code_verifier,
        &oauth_state,
    );
    let expires_at = session.ttl_at;

    let email_hash = sha256_hex(&body.email);

    match session.transition_to_email(
        body.email,
        code_verifier,
        oauth_state.clone(),
        verification_url.clone(),
        expires_at,
    ) {
        Ok(()) => {}
        Err(_) => {
            return err_response(
                StatusCode::CONFLICT,
                "session_state_mismatch",
                &format!(
                    "Cannot submit email on session in state {}",
                    session.wire_state()
                ),
                Some(&body.session_id),
                Some(session.wire_state()),
            );
        }
    }

    if let Err(e) = ac.store.upsert(&session) {
        tracing::error!(event = "auth.session.store_error", session_id = %session.session_id, error = %e);
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "Cannot persist session to storage",
            Some(&session.session_id),
            None,
        );
    }

    tracing::info!(
        event = "auth.session.email_submitted",
        session_id = %session.session_id,
        email_hash_sha256 = %email_hash,
    );
    tracing::info!(
        event = "auth.session.authorize_url_built",
        session_id = %session.session_id,
        verification_url = %verification_url,
        oauth_state = %oauth_state,
        expires_at = %expires_at.to_rfc3339(),
    );

    Json(json!({
        "session_id": session.session_id,
        "state": session.wire_state(),
        "verification_url": verification_url,
        "oauth_state": oauth_state,
        "expires_at": expires_at.to_rfc3339(),
    }))
    .into_response()
}

/// `GET /v1/auth/claude/{session_id}` — read session view.
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    let Some(ac) = state.auth_claude.as_ref() else {
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "auth-claude surface is not enabled on this adapter",
            None,
            None,
        );
    };

    let session = match ac.store.load(&session_id) {
        Ok(s) => s,
        Err(StoreError::NotFound | StoreError::InvalidSessionId) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "No session matches the given session_id.",
                Some(&session_id),
                None,
            );
        }
        Err(e) => {
            tracing::error!(event = "auth.session.store_error", session_id = %session_id, error = %e);
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Cannot read session from storage",
                Some(&session_id),
                None,
            );
        }
    };

    Json(serialise_view(&session)).into_response()
}

/// Request body for `POST /v1/auth/claude/confirm`.
#[derive(Debug, Deserialize)]
pub struct ConfirmBody {
    /// Session id targeted by the confirm.
    pub session_id: String,
    /// Authorization code copied from Anthropic's manual-redirect page.
    pub authorization_code: String,
}

/// `POST /v1/auth/claude/confirm` — paste-back, token exchange, write
/// credentials.
pub async fn confirm(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConfirmBody>,
) -> Response {
    let Some(ac) = state.auth_claude.as_ref() else {
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "auth-claude surface is not enabled on this adapter",
            None,
            None,
        );
    };

    if body.authorization_code.is_empty() {
        return err_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Field 'authorization_code' is required.",
            Some(&body.session_id),
            None,
        );
    }

    let mut session = match ac.store.load(&body.session_id) {
        Ok(s) => s,
        Err(StoreError::NotFound | StoreError::InvalidSessionId) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "No session matches the given session_id.",
                Some(&body.session_id),
                None,
            );
        }
        Err(e) => {
            tracing::error!(event = "auth.session.store_error", session_id = %body.session_id, error = %e);
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Cannot read session from storage",
                Some(&body.session_id),
                None,
            );
        }
    };

    let now = Utc::now();
    if session.is_ttl_expired(now) {
        session.transition_to_expired();
        let _ = ac.store.upsert(&session);
        return err_response(
            StatusCode::GONE,
            "session_expired",
            "Session has expired; create a new one.",
            Some(&body.session_id),
            Some(session.wire_state()),
        );
    }

    if !matches!(session.state, AuthState::AwaitingUserApproval) {
        return err_response(
            StatusCode::CONFLICT,
            "session_state_mismatch",
            &format!("Cannot confirm session in state {}", session.wire_state()),
            Some(&body.session_id),
            Some(session.wire_state()),
        );
    }

    let Some(code_verifier) = session.code_verifier.clone() else {
        // Defensive — shouldn't happen with a well-formed AWAITING_USER_APPROVAL
        // record, but signals a corrupted on-disk session.
        tracing::error!(
            event = "auth.session.code_exchange_failed",
            session_id = %session.session_id,
            error_code = "missing_code_verifier",
        );
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "Session is missing PKCE state on disk",
            Some(&body.session_id),
            Some(session.wire_state()),
        );
    };

    // The code pasted from Anthropic's manual-redirect page has the form
    // `authorizationCode#state`. The official `claude` CLI splits on '#'
    // and REQUIRES both halves non-empty (claude-code v2.1.88
    // `ConsoleOAuthFlow.tsx:157-169`). Reproduce that contract exactly.
    let Some((authorization_code, pasted_state)) = body.authorization_code.split_once('#') else {
        return err_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Field 'authorization_code' must be in the format 'authorizationCode#state'. \
             Copy the full code from Anthropic's page.",
            Some(&body.session_id),
            Some(session.wire_state()),
        );
    };
    if authorization_code.is_empty() || pasted_state.is_empty() {
        return err_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Field 'authorization_code' must be in the format 'authorizationCode#state' \
             (both halves are required).",
            Some(&body.session_id),
            Some(session.wire_state()),
        );
    }

    // CSRF guard (RFC 6749 §10.12): the state echoed back in the pasted
    // code MUST match the state we minted at the authorize step. The
    // official CLI performs the same check before token exchange.
    match session.oauth_state.as_deref() {
        Some(expected) if expected == pasted_state => {}
        _ => {
            tracing::warn!(
                event = "auth.session.state_mismatch",
                session_id = %session.session_id,
            );
            return err_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "The state returned with the authorization code does not match this \
                 session (possible CSRF, or the code is from a different session).",
                Some(&body.session_id),
                Some(session.wire_state()),
            );
        }
    }

    let exchange_result = exchange_code(
        &ac.http,
        &ac.config,
        authorization_code,
        pasted_state,
        &code_verifier,
    )
    .await;

    match exchange_result {
        Ok(token_resp) => {
            let account_email = token_resp.account.as_ref().and_then(|a| a.email.clone());

            // Write credentials file BEFORE marking COMPLETED — if the write
            // fails, the session remains in AWAITING_USER_APPROVAL and the
            // operator can retry confirm with the same authorization_code
            // (Anthropic single-use rule will refuse, so this is effectively
            // a one-shot, but the state machine doesn't lie).
            if let Err(e) = write_credentials_file(&ac.config.credentials_path, &token_resp) {
                tracing::error!(
                    event = "auth.session.credentials_write_failed",
                    session_id = %session.session_id,
                    path = %ac.config.credentials_path.display(),
                    error = %e,
                );
                return err_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service_unavailable",
                    "Failed to persist credentials to disk",
                    Some(&session.session_id),
                    Some(session.wire_state()),
                );
            }

            // Transition + persist. Errors here would corrupt the on-disk
            // session view, but the credentials are already written — log
            // loudly and surface 500.
            if let Err(_e) = session.transition_to_completed(account_email.clone()) {
                tracing::error!(event = "auth.session.transition_failed",
                    session_id = %session.session_id, target = "COMPLETED");
            }
            if let Err(e) = ac.store.upsert(&session) {
                tracing::error!(event = "auth.session.store_error",
                    session_id = %session.session_id, error = %e);
            }

            tracing::info!(
                event = "auth.session.code_exchanged",
                session_id = %session.session_id,
                account_email_present = account_email.is_some(),
                expires_in = token_resp.expires_in.unwrap_or(0),
            );

            let mut body = json!({
                "ok": true,
                "state": session.wire_state(),
            });
            if let Some(email) = account_email {
                body["account_email"] = Value::String(email);
            }
            Json(body).into_response()
        }
        Err(err) => {
            let (status, code) = match &err {
                TokenExchangeError::AnthropicRefused { .. }
                | TokenExchangeError::MalformedResponse(_) => {
                    (StatusCode::BAD_GATEWAY, "token_exchange_failed")
                }
                TokenExchangeError::Unreachable(_) | TokenExchangeError::Server { .. } => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "anthropic_unreachable")
                }
            };

            let message = format!("{err}");
            let error_code = match &err {
                TokenExchangeError::AnthropicRefused { error, .. } => error.clone(),
                _ => code.to_owned(),
            };

            let _ = session.transition_to_failed(SessionError {
                code: error_code.clone(),
                message: message.clone(),
            });
            let _ = ac.store.upsert(&session);

            tracing::warn!(
                event = "auth.session.code_exchange_failed",
                session_id = %session.session_id,
                error_code = %error_code,
                error_message = %message,
            );

            err_response(
                status,
                code,
                &message,
                Some(&session.session_id),
                Some(session.wire_state()),
            )
        }
    }
}

/// `DELETE /v1/auth/claude/{session_id}` — cancel + cleanup.
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    let Some(ac) = state.auth_claude.as_ref() else {
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "auth-claude surface is not enabled on this adapter",
            None,
            None,
        );
    };

    // Verify the session exists so we can emit a useful event; if not,
    // a DELETE on a missing id returns 404 (per OpenAPI).
    match ac.store.load(&session_id) {
        Ok(_) => {}
        Err(StoreError::NotFound | StoreError::InvalidSessionId) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "No session matches the given session_id.",
                Some(&session_id),
                None,
            );
        }
        Err(e) => {
            tracing::error!(event = "auth.session.store_error", session_id = %session_id, error = %e);
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Cannot read session from storage",
                Some(&session_id),
                None,
            );
        }
    }

    if let Err(e) = ac.store.delete(&session_id) {
        tracing::error!(event = "auth.session.store_error", session_id = %session_id, error = %e);
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "Cannot delete session",
            Some(&session_id),
            None,
        );
    }

    tracing::info!(
        event = "auth.session.cancelled",
        session_id = %session_id,
        reason = "explicit_delete",
    );

    Json(json!({ "ok": true })).into_response()
}

/// Compose the JSON object for `GET /v1/auth/claude/{session_id}` and
/// for any handler returning a session view.
fn serialise_view(session: &AuthSession) -> Value {
    #[derive(Serialize)]
    struct View<'a> {
        session_id: &'a str,
        state: &'a str,
        created_at: String,
        ttl_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        verification_url: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        oauth_state: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        account_email: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a SessionError>,
    }
    let view = View {
        session_id: &session.session_id,
        state: session.wire_state(),
        created_at: session.created_at.to_rfc3339(),
        ttl_at: session.ttl_at.to_rfc3339(),
        verification_url: session.verification_url.as_deref(),
        oauth_state: session.oauth_state.as_deref(),
        expires_at: session.expires_at.map(|d| d.to_rfc3339()),
        account_email: session.account_email.as_deref(),
        error: session.error.as_ref(),
    };
    serde_json::to_value(&view).unwrap_or(Value::Null)
}

/// Build a uniform `ErrorResponse` envelope per the OpenAPI schema.
fn err_response(
    status: StatusCode,
    code: &str,
    message: &str,
    session_id: Option<&str>,
    current_state: Option<&str>,
) -> Response {
    let mut error = json!({
        "code": code,
        "message": message,
    });
    if let Some(id) = session_id {
        error["session_id"] = Value::String(id.to_owned());
    }
    if let Some(s) = current_state {
        error["current_state"] = Value::String(s.to_owned());
    }
    (status, Json(json!({ "error": error }))).into_response()
}

fn sha256_hex(s: &str) -> String {
    use std::fmt::Write;
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}
