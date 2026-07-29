// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-origin policy for `cs-api`.
//!
//! `cs-api` serves native clients — the Mac menubar app and the
//! iOS/iPad pilot — which speak `URLSession` and never send an `Origin`
//! header, because the same-origin policy is a browser rule and they
//! are not browsers. CORS therefore buys those clients nothing at all;
//! it only decides which *web pages* the browser will let talk to this
//! daemon.
//!
//! That matters here more than it would elsewhere, because the daemon
//! has no authentication and `POST /molecules/{id}/tackle` spawns a
//! worker. `Access-Control-Allow-Origin: *` on such a surface means any
//! page the operator happens to open can issue writes to it. So the
//! default is [`CorsPolicy::Deny`]: no CORS headers, no preflight
//! answer, nothing for a browser to act on.
//!
//! An operator who is genuinely building a local web pilot can name the
//! origins they mean with `--allow-web-origin <ORIGIN>` (repeatable).
//! The allow-list is matched exactly and echoed back; there is no
//! wildcard and no way to spell one.
//!
//! # Why omitting a header is not a refusal
//!
//! The first version of this module only *withheld*
//! `Access-Control-Allow-Origin` from an unlisted origin. That is the
//! property next door, not the property wanted. A browser's *simple*
//! request — a `POST` with `Content-Type: text/plain`, no preflight —
//! is sent to the server first and judged afterwards: the missing
//! header stops the attacking page from *reading* the response, long
//! after `POST /molecules/{id}/tackle` has already spawned a worker.
//! For a daemon whose writes are the whole risk, that ordering is the
//! bug.
//!
//! So the middleware refuses **before** the handler: any request
//! carrying an `Origin` this policy does not name is answered `403`
//! without ever calling `next.run`, and that includes every request
//! under [`CorsPolicy::Deny`], where no origin is named at all. The
//! middleware is therefore always installed — under `Deny` it is not a
//! decoration that can be skipped, it is the gate.
//!
//! Native clients are untouched: they send no `Origin`, so they never
//! meet the check.

use std::sync::Arc;

use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use thiserror::Error;

/// Which browser origins, if any, may talk to this daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CorsPolicy {
    /// Emit no CORS headers. Native clients are unaffected; browsers
    /// refuse cross-origin requests. This is the default.
    #[default]
    Deny,
    /// Echo back exactly these origins when they match the request's
    /// `Origin` header. Never a wildcard.
    Allow(Vec<HeaderValue>),
}

/// The operator wrote something that cannot be an `Origin`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OriginError {
    /// Not representable as an HTTP header value at all.
    #[error("`{0}` is not a valid HTTP header value and cannot be an Origin")]
    NotAHeaderValue(String),
    /// A wildcard, spelled out. Refused rather than silently narrowed,
    /// so the operator learns that this surface has no wildcard.
    #[error(
        "`*` is not accepted: cs-api has no authentication, so a wildcard origin would \
         let any page the operator visits drive this daemon. Name each origin \
         explicitly, e.g. --allow-web-origin http://localhost:5173"
    )]
    Wildcard,
}

impl CorsPolicy {
    /// Build a policy from the operator's `--allow-web-origin` values.
    ///
    /// An empty list yields [`CorsPolicy::Deny`], which is the same
    /// thing said two ways: no named origin means no origin allowed.
    ///
    /// # Errors
    ///
    /// Returns [`OriginError`] if any entry is `*` or is not a legal
    /// header value.
    pub fn from_allowed_origins<I, S>(origins: I) -> Result<Self, OriginError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut parsed = Vec::new();
        for origin in origins {
            let raw = origin.as_ref().trim();
            if raw == "*" {
                return Err(OriginError::Wildcard);
            }
            let value = HeaderValue::from_str(raw)
                .map_err(|_| OriginError::NotAHeaderValue(raw.to_owned()))?;
            parsed.push(value);
        }
        if parsed.is_empty() {
            Ok(Self::Deny)
        } else {
            Ok(Self::Allow(parsed))
        }
    }

    /// Whether any browser origin is allowed at all.
    ///
    /// This is a question about the policy, not about the middleware
    /// stack: the middleware is installed unconditionally, because
    /// [`CorsPolicy::Deny`] has to *refuse* browser requests and an
    /// absent layer refuses nothing.
    #[must_use]
    pub fn is_permissive(&self) -> bool {
        matches!(self, Self::Allow(_))
    }

    /// The matching allow-list entry for a request's `Origin`, if any.
    fn matching_origin(&self, headers: &HeaderMap) -> Option<HeaderValue> {
        let Self::Allow(allowed) = self else {
            return None;
        };
        let origin = headers.get(header::ORIGIN)?;
        allowed.iter().find(|a| *a == origin).cloned()
    }
}

/// Middleware enforcing a [`CorsPolicy`].
///
/// Always installed, for both policies (see
/// [`crate::router_with_cors`]). Under [`CorsPolicy::Deny`] it is the
/// only thing standing between a web page and a worker spawn.
///
/// The order is the point: an unlisted `Origin` is answered before
/// `next.run` is ever awaited, so the handler — and with it any
/// subprocess or on-disk write — is not entered.
pub(crate) async fn layer(
    policy: Arc<CorsPolicy>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let matched = policy.matching_origin(req.headers());
    let claims_an_origin = req.headers().contains_key(header::ORIGIN);

    // A browser that names an origin this policy does not allow gets
    // nothing done on its behalf — not a preflight, and not the simple
    // request that needs no preflight. `matched` is `None` for every
    // request under `Deny`, which is exactly the intent there.
    if claims_an_origin && matched.is_none() {
        return refusal();
    }

    if req.method() == Method::OPTIONS {
        let mut response = Response::new(axum::body::Body::empty());
        *response.status_mut() = if matched.is_some() {
            StatusCode::NO_CONTENT
        } else {
            // A preflight with no `Origin` at all: not a browser, and
            // nothing to allow. Refuse rather than pass an unroutable
            // OPTIONS down to the handlers.
            StatusCode::FORBIDDEN
        };
        inject(response.headers_mut(), matched.as_ref());
        return response;
    }

    let mut response = next.run(req).await;
    inject(response.headers_mut(), matched.as_ref());
    response
}

/// The refusal handed to an origin the policy does not name: `403`,
/// empty body, `Vary: Origin` and no `Access-Control-*` header.
fn refusal() -> Response {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::FORBIDDEN;
    inject(response.headers_mut(), None);
    response
}

/// Write the allow headers for a matched origin, plus `Vary: Origin`
/// so a cache never serves one origin's response to another.
fn inject(headers: &mut HeaderMap, matched: Option<&HeaderValue>) {
    headers.append(header::VARY, HeaderValue::from_static("origin"));
    let Some(origin) = matched else {
        return;
    };
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_origins_is_deny() {
        let policy =
            CorsPolicy::from_allowed_origins(Vec::<String>::new()).expect("empty is legal");
        assert_eq!(policy, CorsPolicy::Deny);
        assert!(!policy.is_permissive());
    }

    #[test]
    fn default_is_deny() {
        assert_eq!(CorsPolicy::default(), CorsPolicy::Deny);
    }

    #[test]
    fn wildcard_is_refused_by_name() {
        assert_eq!(
            CorsPolicy::from_allowed_origins(["*"]),
            Err(OriginError::Wildcard)
        );
    }

    #[test]
    fn garbage_origin_is_refused() {
        assert!(matches!(
            CorsPolicy::from_allowed_origins(["hello\nworld"]),
            Err(OriginError::NotAHeaderValue(_))
        ));
    }

    #[test]
    fn allow_list_matches_exactly() {
        let policy = CorsPolicy::from_allowed_origins(["http://localhost:5173"])
            .expect("a legal origin parses");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:5173"),
        );
        assert!(policy.matching_origin(&headers).is_some());

        let mut other = HeaderMap::new();
        other.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.test"),
        );
        assert!(policy.matching_origin(&other).is_none());
    }

    #[test]
    fn deny_never_matches() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:5173"),
        );
        assert!(CorsPolicy::Deny.matching_origin(&headers).is_none());
    }
}
