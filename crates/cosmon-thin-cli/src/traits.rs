// SPDX-License-Identifier: Apache-2.0

//! The [`IsoVerb`] trait and [`Principal`] enum.
//!
//! `IsoVerb` is the compile-time contract that every `#[verb]`-annotated
//! function adheres to. The macro generates a request struct that implements
//! the trait; runtime code (the [`crate::client::Client`]) is generic over
//! that trait and reads the four `const` items to assemble HTTP requests.
//!
//! Why a trait with associated `const`s rather than a runtime descriptor? So
//! that the dispatch table is **closed at compile time**: if a verb is not
//! annotated, `Client::call::<V>()` does not type-check. This is the §8p
//! invariant ("API ⊊ CLI subset") expressed in the type system, not enforced
//! by a lint.

use serde::{de::DeserializeOwned, Serialize};

/// Authorisation principal for an HTTP verb.
///
/// `tenant` covers the multi-tenant `SaaS` surface, `operator` the single-user
/// pilot, and `worker` the inside-worktree CLI calls. The variants intentionally
/// match the principal classes named in ADR-080 §5.1 (operator-only verbs).
///
/// `#[non_exhaustive]` because new principal classes (e.g. `byok`,
/// `external-orchestrator`) are anticipated under cosmon-saas evolution; we do
/// not want consumers to write exhaustive matches that break on V1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Principal {
    /// `SaaS` tenant — multi-user, JWT-scoped.
    Tenant,
    /// Single-user operator (the pilot).
    Operator,
    /// Worker process inside a tackled worktree.
    Worker,
}

impl Principal {
    /// Lower-case wire-form name of the principal.
    ///
    /// Round-trips with the `principal = "..."` argument of `#[verb]`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Operator => "operator",
            Self::Worker => "worker",
        }
    }
}

/// Compile-time HTTP-verb metadata bound to a request type.
///
/// One implementation per `#[verb]`-annotated function. The associated
/// `Request`/`Response` types are the JSON bodies; `METHOD`/`PATH` build the
/// URL; `PRINCIPAL` drives authorisation; `VERB_NAME` is the underlying
/// function name (used for diagnostics and the registry).
///
/// # Example
///
/// ```no_run
/// use cosmon_thin_cli::{IsoVerb, Principal};
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize)]
/// pub struct PingRequest {}
///
/// #[derive(Serialize, Deserialize)]
/// pub struct PingResponse {}
///
/// impl IsoVerb for PingRequest {
///     const METHOD: &'static str = "GET";
///     const PATH: &'static str = "/v1/ping";
///     const PRINCIPAL: Principal = Principal::Operator;
///     const VERB_NAME: &'static str = "ping";
///     type Request = Self;
///     type Response = PingResponse;
/// }
/// ```
pub trait IsoVerb {
    /// HTTP method (`"GET"`, `"POST"`, `"PUT"`, `"PATCH"`, `"DELETE"`).
    const METHOD: &'static str;
    /// URL path template, must start with `/`.
    const PATH: &'static str;
    /// Authorisation principal for this verb.
    const PRINCIPAL: Principal;
    /// Source-level verb name (the underlying function's identifier).
    const VERB_NAME: &'static str;
    /// Request body type; serialised as JSON.
    type Request: Serialize;
    /// Response body type; deserialised from JSON.
    type Response: DeserializeOwned;
}

#[cfg(test)]
mod tests {
    use super::Principal;

    #[test]
    fn principal_as_str_roundtrip() {
        assert_eq!(Principal::Tenant.as_str(), "tenant");
        assert_eq!(Principal::Operator.as_str(), "operator");
        assert_eq!(Principal::Worker.as_str(), "worker");
    }
}
