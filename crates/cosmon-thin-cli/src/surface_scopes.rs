// SPDX-License-Identifier: Apache-2.0

//! Compile-time scope table folded from the §8p surface canon
//! (`crates/cosmon-rpp-adapter/data/surface_events.txt`) by `build.rs`.
//!
//! Exposes [`SURFACE_SCOPES`], the `(METHOD, colon-path, scope)` table
//! consumed by `cs-thin help` — the scope a verb prints is the scope
//! the canon line declares, not a hand-maintained mirror. The former
//! mirror (`help::scope_for`) was deleted in favour of this fold.

include!(concat!(env!("OUT_DIR"), "/surface_scopes_generated.rs"));

/// Minimal scope expression for one registry descriptor, looked up by
/// `(method, path)` in [`SURFACE_SCOPES`]. Returns `None` when no
/// canon line carries the pair — which the help unit test
/// (`scope_map_covers_every_registered_verb`) forbids for registered
/// verbs.
#[must_use]
pub fn scope_for(method: &str, path: &str) -> Option<&'static str> {
    SURFACE_SCOPES
        .iter()
        .find(|(m, p, _)| m.eq_ignore_ascii_case(method) && *p == path)
        .map(|(_, _, scope)| *scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_not_empty() {
        // The canon always carries at least the V0 read-only base.
        assert!(!SURFACE_SCOPES.is_empty());
    }

    #[test]
    fn lookup_normalises_nothing_at_runtime() {
        // Paths in the table are already colon-form: the observe route
        // must be found under the registry's spelling, not axum's.
        assert_eq!(
            scope_for("GET", "/v1/molecules/:id"),
            Some("cosmon:molecule:read"),
        );
        assert_eq!(scope_for("GET", "/v1/molecules/{id}"), None);
    }

    #[test]
    fn tackle_carries_its_compound_scope() {
        // The AND grid (scopes.rs §3.2): tackle = write + spawn. The
        // old hand-map under-reported this as write-only.
        assert_eq!(
            scope_for("POST", "/v1/molecules/:id/tackle"),
            Some("cosmon:molecule:write+cosmon:worker:spawn"),
        );
    }
}
