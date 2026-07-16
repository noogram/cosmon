// SPDX-License-Identifier: Apache-2.0

//! Compile-time registry of all `#[verb]`-annotated functions.
//!
//! Each `#[verb]` invocation appends a [`VerbDescriptor`] to the [`VERBS`]
//! distributed slice. The slice is populated at link time — there is no
//! runtime registration call, no plugin loader, no global mutex. To enumerate
//! every verb the binary exposes, call [`all`].
//!
//! This is the introspection point used by `cs-thin` itself (e.g. for `--help`
//! generation) and by the `api_surface_freeze` test in the workspace, which
//! asserts that the macro-derived surface matches the §8p whitelist.

use crate::Principal;

/// Static, link-time descriptor of a single HTTP verb.
///
/// Carried by the [`VERBS`] distributed slice. Holds **only metadata** — no
/// function pointers, no closures. Behaviour lives in the function the macro
/// is attached to (which is invoked server-side by `cs-api`/`cs-rpp`, not
/// here). `cs-thin` only needs the strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerbDescriptor {
    /// Source-level function name.
    pub name: &'static str,
    /// HTTP method literal, upper-case (`"GET"`, `"POST"`, …).
    pub method: &'static str,
    /// URL path template, starting with `/`.
    pub path: &'static str,
    /// Wire-form principal: `"tenant"`, `"operator"`, or `"worker"`.
    ///
    /// Stored as a string for the link-time slice (associated `const`
    /// expressions involving `Principal` are not yet usable as initialisers
    /// for `linkme::distributed_slice` entries on stable). Use
    /// [`VerbDescriptor::principal`] to recover the typed [`Principal`].
    pub principal_str: &'static str,
}

impl VerbDescriptor {
    /// Typed [`Principal`] decoded from [`Self::principal_str`].
    ///
    /// Returns `None` if the underlying string is not one of the recognised
    /// principal classes — a shape that cannot occur in macro-generated
    /// descriptors (the macro validates), but is exposed defensively for
    /// hand-written entries in tests.
    #[must_use]
    pub const fn principal(&self) -> Option<Principal> {
        // Const string equality is awkward on stable; fall back to bytes.
        let bytes = self.principal_str.as_bytes();
        match bytes {
            b"tenant" => Some(Principal::Tenant),
            b"operator" => Some(Principal::Operator),
            b"worker" => Some(Principal::Worker),
            _ => None,
        }
    }
}

/// Distributed slice of every [`VerbDescriptor`] registered by `#[verb]`.
///
/// Populated at link time; do not append to this slice manually. To register
/// a verb, annotate its function with `#[cosmon_thin_macro::verb(...)]`.
#[linkme::distributed_slice]
pub static VERBS: [VerbDescriptor];

/// All registered verbs, in link-order.
///
/// Order is *not guaranteed* to match source order; callers that need a
/// stable ordering (test snapshots, `--help` rendering) should sort by
/// [`VerbDescriptor::name`].
#[must_use]
pub fn all() -> &'static [VerbDescriptor] {
    &VERBS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principal_decoding() {
        let d = VerbDescriptor {
            name: "x",
            method: "GET",
            path: "/v1/x",
            principal_str: "operator",
        };
        assert_eq!(d.principal(), Some(Principal::Operator));

        let bogus = VerbDescriptor {
            name: "y",
            method: "GET",
            path: "/v1/y",
            principal_str: "alien",
        };
        assert_eq!(bogus.principal(), None);
    }

    #[test]
    fn registry_callable() {
        // No verbs are wired in the foundation; we only assert the call
        // succeeds and returns a slice (length 0 today).
        let verbs = all();
        // Length is link-dependent; do not pin a specific value here.
        let _ = verbs.len();
    }
}
