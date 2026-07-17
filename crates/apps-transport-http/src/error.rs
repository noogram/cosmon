// SPDX-License-Identifier: Apache-2.0

//! Canonical [`ApplicationError`] type with stable JSON wire shape.
//!
//! Every cluster daemon should return errors as JSON
//! `{"error": "...", "code": "...", "detail": "..."}` so the Swift
//! `AppsTransportHTTP` client can route them uniformly to typed cases:
//!
//! - 400 `bad_request` — caller sent something malformed.
//! - 404 `not_found` — resource does not exist.
//! - 409 `conflict` — write would violate a uniqueness rule.
//! - 422 `unprocessable` — payload syntactically valid, semantically wrong
//!   (e.g. unknown enum variant).
//! - 500 `internal` — surfaced bug. Logged at error level.
//!
//! Use [`ApplicationError`] in handlers; it implements
//! [`axum::response::IntoResponse`].

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable JSON wire shape for application errors. Roundtrips through
/// `serde_json` so tests and clients can decode it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
}

/// Application-level error produced by handlers.
///
/// `Internal` boxes an `anyhow::Error` so handler code can use the `?`
/// operator across heterogeneous error types without manual conversion.
#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unprocessable: {0}")]
    Unprocessable(String),
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl ApplicationError {
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Unprocessable(_) => "unprocessable",
            Self::Internal(_) => "internal",
        }
    }

    #[must_use]
    pub fn body(&self) -> ErrorBody {
        let detail = match self {
            Self::BadRequest(d)
            | Self::NotFound(d)
            | Self::Conflict(d)
            | Self::Unprocessable(d) => Some(d.clone()),
            Self::Internal(e) => {
                tracing::error!(error = %e, chain = ?e.chain().collect::<Vec<_>>(), "handler internal error");
                None
            }
        };
        ErrorBody {
            error: self.to_string(),
            code: self.code().to_string(),
            detail,
        }
    }
}

impl IntoResponse for ApplicationError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = self.body();
        (status, Json(body)).into_response()
    }
}

/// Build a [`Response`] from any [`ApplicationError`]-like value. Useful
/// for hand-rolled handlers that want to short-circuit without going
/// through the `IntoResponse` trait.
#[must_use]
pub fn error_response(err: ApplicationError) -> Response {
    err.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_request_maps_to_400() {
        let e = ApplicationError::BadRequest("missing surface_name".into());
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
        assert_eq!(e.code(), "bad_request");
        let b = e.body();
        assert_eq!(b.code, "bad_request");
        assert_eq!(b.detail.as_deref(), Some("missing surface_name"));
    }

    #[test]
    fn not_found_maps_to_404() {
        let e = ApplicationError::NotFound("surface 'x'".into());
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        assert_eq!(e.code(), "not_found");
    }

    #[test]
    fn conflict_maps_to_409() {
        let e = ApplicationError::Conflict("duplicate pin".into());
        assert_eq!(e.status(), StatusCode::CONFLICT);
        assert_eq!(e.code(), "conflict");
    }

    #[test]
    fn unprocessable_maps_to_422() {
        let e = ApplicationError::Unprocessable("unknown action".into());
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(e.code(), "unprocessable");
    }

    #[test]
    fn internal_maps_to_500_and_drops_detail() {
        let e = ApplicationError::Internal(anyhow::anyhow!("disk full"));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.code(), "internal");
        let b = e.body();
        // `detail` is dropped for 500s — the server logs the chain, the
        // client just sees the canonical message.
        assert!(b.detail.is_none());
    }
}
