// SPDX-License-Identifier: AGPL-3.0-only

//! Bijection gate for the DELIVERED tenant binary.
//!
//! Before the fusion, `routes_and_verbs_are_bijective` (adapter tests)
//! pinned canon ↔ `#[verb]` registry, but the binary tenants actually
//! install (`cosmon-remote`) was covered by NO bijection — its routes
//! were free-floating string literals. This test closes the
//! triangle:
//!
//! ```text
//! canon (surface_events.txt)
//!   ↕ adapter test `routes_and_verbs_are_bijective` (pre-existing)
//! #[verb] registry (link-time slice)
//!   ↕ THIS test (same name, delivered-binary side)
//! cosmon-remote ROUTES_USED (build-time canon fold → client methods)
//! ```
//!
//! All three describe the same tenant-verb set or nothing merges.

use std::collections::BTreeSet;

use cosmon_remote::canon::{self, Exposure};
use cosmon_surface_canon::normalise_path;

/// The delivered binary's tenant-verb route set must be EXACTLY the
/// `#[verb]` registry — every annotated verb reachable from the
/// installed CLI, every CLI tenant-verb route annotated.
#[test]
fn routes_and_verbs_are_bijective() {
    let used: BTreeSet<(String, String)> = canon::ROUTES_USED
        .iter()
        .filter(|r| r.exposure == Exposure::TenantVerb)
        .map(|r| (r.method.to_owned(), normalise_path(r.path)))
        .collect();

    let registry: BTreeSet<(String, String)> = cosmon_thin_cli::registry::all()
        .iter()
        .map(|d| (d.method.to_owned(), normalise_path(d.path)))
        .collect();

    let used_only: Vec<_> = used.difference(&registry).cloned().collect();
    let registry_only: Vec<_> = registry.difference(&used).cloned().collect();

    assert!(
        registry_only.is_empty(),
        "tenant verbs in the #[verb] registry NOT consumed by the delivered \
         cosmon-remote binary: {registry_only:?} — wire the verb (canon const + \
         client method + clap command) or reclassify the canon line",
    );
    assert!(
        used_only.is_empty(),
        "routes the delivered binary consumes as tenant-verb with NO #[verb] \
         annotation: {used_only:?} — §8p drift forbidden",
    );
    assert_eq!(used, registry);
}

/// §8p (ADR-080 §5): the delivered binary may never dial an
/// operator-only route. Structural — reads the canon's `exposure`
/// column, no hand list.
#[test]
fn consumed_routes_are_never_operator_only() {
    for route in canon::ROUTES_USED {
        assert!(
            route.exposure != Exposure::OperatorOnly,
            "{} {} is operator-only but consumed by the tenant binary",
            route.method,
            route.path,
        );
    }
}

/// One client method per route — a duplicate entry in `ROUTES_USED`
/// would hide a missing consumption elsewhere.
#[test]
fn consumed_routes_are_unique() {
    let mut seen = BTreeSet::new();
    for route in canon::ROUTES_USED {
        assert!(
            seen.insert((route.method, route.path)),
            "duplicate ROUTES_USED entry: {} {}",
            route.method,
            route.path,
        );
    }
}

/// Path substitution arity for every consumed route: feeding exactly
/// `placeholder_count` values must succeed (the client call sites use
/// fixed-arity slices; this exercises the contract for all of them).
#[test]
fn consumed_route_placeholders_substitute_cleanly() {
    for route in canon::ROUTES_USED {
        let n = route.path.matches('{').count();
        let values: Vec<String> = (0..n).map(|i| format!("x{i}")).collect();
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let rendered = route.path_with(&refs);
        assert!(
            !rendered.contains('{') && !rendered.contains('}'),
            "{}: unsubstituted placeholder in {rendered}",
            route.path,
        );
    }
}
