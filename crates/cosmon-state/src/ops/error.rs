// SPDX-License-Identifier: AGPL-3.0-only

//! `OpsError` — wire-stable error contract for cosmon verbs.
//!
//! Every verb-level error enum under [`crate::ops`] (e.g.
//! [`super::observe::ObserveError`]) implements [`OpsError`] so that
//! cross-process callers — the cs-cli, the in-process cs-api routes, the
//! out-of-process Receptionist (RPP) — can map a failure to an HTTP
//! status without `match`ing string messages.
//!
//! # Why a trait, not a mega-enum
//!
//! The library-first promotion made the per-verb error a hard
//! constraint: each verb keeps its own dedicated
//! error type so its variants stay relevant to its semantics. A
//! `CosmonError` flattening every shape would force the cs-cli to dispatch
//! by string match again, defeating the promotion. [`OpsError`] is the
//! shared contract those disjoint enums all honour — three small methods,
//! no shared state, no inheritance.
//!
//! # Why u16, not `http::StatusCode`
//!
//! `cosmon-state` is the persistence-port crate. It must not pull in an
//! HTTP runtime (axum, hyper, http) — the trait would otherwise leak
//! through the dependency graph into every state-only consumer (filestore,
//! crashtest, archive). Callers convert to a typed status at the
//! boundary: `http::StatusCode::from_u16(err.http_status())` is the
//! one-line bridge in the cs-api / RPP.
//!
//! # Wire stability
//!
//! Each [`OpsError::tag`] is a kebab-case string that becomes part of the
//! public contract with HTTP clients of the RPP. **Tags are append-only**:
//! once a tag has shipped, it stays — adding a new variant adds a new
//! tag, never reuses an old one. The canonical-form test in this module
//! enforces the kebab-case rule (`[a-z0-9-]+`, no leading/trailing dash,
//! no double-dash).

use serde::{Deserialize, Serialize};

/// Wire-stable error contract for all cosmon-state verb errors.
///
/// Implementors expose three things that do not depend on the rendering
/// channel: a stable `tag` (the public, kebab-case label), an
/// `http_status` (so HTTP servers can map without `match`-on-string), and
/// a `to_wire` projection (the serializable payload that crosses process
/// boundaries).
///
/// Implementations live next to the error enum they describe: that keeps
/// the mapping cohesive — when a new variant is added, the trait `match`
/// arm is in the same file and the compiler flags the missing case at
/// the next `cargo check`.
pub trait OpsError: std::error::Error {
    /// Stable, kebab-case label for this error.
    ///
    /// # Stability contract
    ///
    /// Once a tag has been shipped, it is part of the public wire
    /// contract and **must not change**. Renaming a variant is fine; the
    /// tag stays. Adding a new variant adds a new tag.
    fn tag(&self) -> &'static str;

    /// HTTP status code this error maps to.
    ///
    /// Returned as `u16` to keep `cosmon-state` free of an HTTP runtime
    /// dependency (see module docs). HTTP-aware callers convert with
    /// `http::StatusCode::from_u16(...)`.
    fn http_status(&self) -> u16;

    /// Project this error into its wire form.
    ///
    /// The default body is `self.to_string()` — the `Display` impl that
    /// every `thiserror::Error` already produces. Implementors override
    /// only if they need to redact identifiers (turing G9) or carry
    /// structured detail.
    fn to_wire(&self) -> ErrorWire {
        ErrorWire {
            tag: self.tag().to_owned(),
            http_status: self.http_status(),
            message: self.to_string(),
        }
    }
}

/// Serializable cross-process error payload.
///
/// Carries the three fields a wire client needs to act on a failure:
/// the stable `tag` (machine-readable dispatch), the `http_status` (so
/// non-HTTP transports can still surface a coarse class), and the
/// human-readable `message` (for logs and operator UI). Identical layout
/// across every verb — RPP routes serialize this verbatim into their
/// response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorWire {
    /// Stable kebab-case label, e.g. `"molecule-not-found"`.
    pub tag: String,
    /// Mapped HTTP status, e.g. `404`.
    pub http_status: u16,
    /// Human-readable message — typically `Display`.
    pub message: String,
}

/// Validate that `tag` is strict kebab-case.
///
/// Rules:
/// - non-empty
/// - characters in `[a-z0-9-]`
/// - no leading or trailing dash
/// - no consecutive dashes
///
/// Used by the trait's contract tests; exposed `pub` so consumers
/// (cs-api, RPP) can audit ad-hoc tags landing through their own
/// converters.
#[must_use]
pub fn is_kebab_case(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    if tag.starts_with('-') || tag.ends_with('-') {
        return false;
    }
    let mut prev_dash = false;
    for c in tag.chars() {
        match c {
            'a'..='z' | '0'..='9' => prev_dash = false,
            '-' => {
                if prev_dash {
                    return false;
                }
                prev_dash = true;
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_case_accepts_canonical_tags() {
        assert!(is_kebab_case("molecule-not-found"));
        assert!(is_kebab_case("store-unavailable"));
        assert!(is_kebab_case("double-writer"));
        assert!(is_kebab_case("a"));
        assert!(is_kebab_case("step-2"));
    }

    #[test]
    fn kebab_case_rejects_non_canonical_tags() {
        assert!(!is_kebab_case(""));
        assert!(!is_kebab_case("MoleculeNotFound"));
        assert!(!is_kebab_case("molecule_not_found"));
        assert!(!is_kebab_case("-leading"));
        assert!(!is_kebab_case("trailing-"));
        assert!(!is_kebab_case("double--dash"));
        assert!(!is_kebab_case("with space"));
        assert!(!is_kebab_case("with:colon"));
    }

    #[test]
    fn error_wire_roundtrips_through_json() {
        let wire = ErrorWire {
            tag: "molecule-not-found".into(),
            http_status: 404,
            message: "molecule not found: task-20260503-aaaa".into(),
        };
        let s = serde_json::to_string(&wire).unwrap();
        let back: ErrorWire = serde_json::from_str(&s).unwrap();
        assert_eq!(back, wire);
        // Field order is documented but not contract; check substrings.
        assert!(s.contains("\"tag\":\"molecule-not-found\""));
        assert!(s.contains("\"http_status\":404"));
    }

    #[test]
    fn default_to_wire_uses_display_for_message() {
        #[derive(Debug, thiserror::Error)]
        #[error("toy display message")]
        struct Toy;

        impl OpsError for Toy {
            fn tag(&self) -> &'static str {
                "toy-error"
            }
            fn http_status(&self) -> u16 {
                418
            }
        }

        let w = Toy.to_wire();
        assert_eq!(w.tag, "toy-error");
        assert_eq!(w.http_status, 418);
        assert_eq!(w.message, "toy display message");
    }
}
