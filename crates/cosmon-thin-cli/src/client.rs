// SPDX-License-Identifier: Apache-2.0

//! Thin HTTP client generic over [`IsoVerb`].
//!
//! [`Client`] holds a base URL, a JWT bearer token, and a `reqwest::Client`.
//! Its single method, [`Client::call`], reads the verb's compile-time
//! metadata to assemble a request, serialises the body to JSON, attaches the
//! bearer header, and deserialises the response.
//!
//! The error type [`ClientError`] is **dedicated to this verb-call surface** —
//! intentionally not a flatten through some workspace `CosmonError` mega-enum.
//! Callers (cs-thin's render layer, future tests) want to map specific
//! failures to specific exit codes / surfaces, and a small typed enum is
//! easier to maintain than a leaky catch-all.

use crate::IsoVerb;
use thiserror::Error;

/// Errors produced by [`Client::call`].
///
/// `#[non_exhaustive]` because new transport-level conditions (TLS-only,
/// rate-limiting, BYOK) are anticipated; consumers should not write
/// exhaustive matches.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Network or TLS failure — server unreachable, DNS error, certificate
    /// rejected, connection reset, etc.
    #[error("network error: {0}")]
    Network(String),

    /// HTTP 401 Unauthorized — the JWT was missing, malformed, expired, or
    /// rejected by the server.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// HTTP 404 Not Found — the verb's path resolved on the server, but the
    /// underlying resource (molecule, tag, …) does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// HTTP 4xx (except 401, 404) — caller-side error: bad arguments, schema
    /// mismatch, validation failure.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// HTTP 5xx — server-side error: bug, dependency outage, panic.
    #[error("server error: {0}")]
    Server(String),

    /// JSON encode/decode failure or any other transport-side issue not
    /// covered by the variants above.
    #[error("client error: {0}")]
    Other(String),
}

/// Configuration-bag HTTP client for [`IsoVerb`] dispatch.
///
/// One instance per cosmon endpoint; cheap to clone (it wraps an
/// `Arc<reqwest::Client>` internally).
#[derive(Debug, Clone)]
pub struct Client {
    base_url: String,
    jwt_token: String,
    http: reqwest::Client,
}

impl Client {
    /// Build a new client.
    ///
    /// `base_url` should be the schemed origin (e.g. `https://api.example.com`)
    /// without a trailing slash. `jwt_token` is the bearer token sent in the
    /// `Authorization` header on every call.
    #[must_use]
    pub fn new(base_url: impl Into<String>, jwt_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            jwt_token: jwt_token.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Build a new client with a caller-supplied `reqwest::Client`.
    ///
    /// Useful in tests (mock transport) and when the caller wants a custom
    /// timeout / TLS configuration.
    #[must_use]
    pub fn with_http(
        base_url: impl Into<String>,
        jwt_token: impl Into<String>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            jwt_token: jwt_token.into(),
            http,
        }
    }

    /// Fire one verb against the configured endpoint.
    ///
    /// Reads `V::METHOD`, `V::PATH` at compile time; serialises `req` as JSON
    /// in the body (for `POST`/`PUT`/`PATCH`/`DELETE`) or skips the body (for
    /// `GET`); attaches `Authorization: Bearer <token>`; deserialises the
    /// response body as JSON of type `V::Response`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Network`] on transport failure, [`ClientError::Auth`]
    /// on 401, [`ClientError::NotFound`] on 404, [`ClientError::BadRequest`] on
    /// other 4xx, [`ClientError::Server`] on 5xx, and [`ClientError::Other`]
    /// for JSON encode/decode errors.
    pub async fn call<V: IsoVerb>(&self, req: V::Request) -> Result<V::Response, ClientError> {
        let url = format!("{}{}", self.base_url, V::PATH);

        let method = reqwest::Method::from_bytes(V::METHOD.as_bytes())
            .map_err(|e| ClientError::Other(format!("invalid method `{}`: {e}", V::METHOD)))?;

        let mut builder = self
            .http
            .request(method.clone(), &url)
            .bearer_auth(&self.jwt_token);

        if !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD) {
            builder = builder.json(&req);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;

        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<V::Response>()
                .await
                .map_err(|e| ClientError::Other(format!("decode response: {e}")));
        }

        let body = resp.text().await.unwrap_or_default();
        Err(match status.as_u16() {
            401 => ClientError::Auth(body),
            404 => ClientError::NotFound(body),
            400..=499 => ClientError::BadRequest(body),
            500..=599 => ClientError::Server(body),
            other => ClientError::Other(format!("HTTP {other}: {body}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs_and_trims_trailing_slash() {
        let c = Client::new("https://example.com/", "tok");
        assert_eq!(c.base_url, "https://example.com");
        assert_eq!(c.jwt_token, "tok");
    }

    #[test]
    fn client_with_custom_http() {
        let http = reqwest::Client::new();
        let c = Client::with_http("https://x.test", "t", http);
        assert_eq!(c.base_url, "https://x.test");
    }

    #[test]
    fn client_error_display() {
        let e = ClientError::NotFound("molecule".into());
        assert!(format!("{e}").contains("molecule"));
    }
}
