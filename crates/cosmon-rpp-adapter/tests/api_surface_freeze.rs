// SPDX-License-Identifier: AGPL-3.0-only

//! §8p surface freeze — pins the public route set AND verifies the
//! bijection between axum routes and `#[verb]`-annotated functions
//! (T-CST-PARITY).
//!
//! Adding or removing a `/v1/...` route MUST fail this test until the
//! snapshot is regenerated *and* `docs/guides/api-cli-coverage.md` is
//! updated *and* ADR-080 (or a successor) is amended in the same PR.
//! See ADR-080 §4.2 (R3 in §12) for the governance contract.
//!
//! The bijection test (`routes_and_verbs_are_bijective`) closes the
//! second drift channel: someone may add a `#[verb]` annotation in
//! `cosmon-state::ops::*` or `cosmon-thin-cli::verbs::*` and forget to
//! mount the corresponding route on the axum router (or remove a verb
//! and leave a dangling route). The test compares
//! [`cosmon_rpp_adapter::frozen_api_surface`] (the canonical surface)
//! against [`cosmon_thin_cli::registry::all`] (the compile-time
//! aggregation of `#[verb]` annotations) and refuses any drift.

use std::collections::BTreeSet;

use cosmon_rpp_adapter::frozen_api_surface;
use cosmon_rpp_adapter::surface_events::SURFACE_EVENTS;

#[test]
fn surface_length_matches_event_log() {
    // I-ADDITIVE-COUNTERS (ADR-110 §I3): the length of the §8p surface
    // is derived from the event log, never asserted against a literal.
    // The check exists to document the invariant (and to fail loudly if
    // a future refactor drops the projection between EVENTS and ROUTES).
    assert_eq!(
        frozen_api_surface().len(),
        SURFACE_EVENTS.len(),
        "surface length must equal event count — both project from data/surface_events.txt",
    );
}

#[test]
fn surface_routes_project_from_event_log() {
    // Stronger version of the length check: each route at index `i`
    // is the `method_path` of the event at the same index. Lets
    // reviewers reason about the fold without re-reading build.rs.
    // This is the backward-compat pin: the 29 routes are byte-identical
    // to the pre-fold hand-edited list, just derived from the data file.
    let surface = frozen_api_surface();
    for (i, ev) in SURFACE_EVENTS.iter().enumerate() {
        assert_eq!(
            surface[i], ev.method_path,
            "surface[{i}] = {:?} but event[{i}].method_path = {:?}",
            surface[i], ev.method_path,
        );
    }
}

#[test]
fn event_log_has_no_duplicate_routes() {
    // The append-only discipline forbids mounting the same route
    // twice. Two duplicate `(method, path)` rows would also break the
    // bijection test, but flagging duplicates here is cheaper and
    // more diagnostic.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for ev in SURFACE_EVENTS {
        assert!(
            seen.insert(ev.method_path),
            "duplicate surface event for {:?} — appended twice to data/surface_events.txt",
            ev.method_path,
        );
    }
}

#[test]
fn every_event_carries_a_method_path_and_molecule_id() {
    for ev in SURFACE_EVENTS {
        assert!(
            ev.method_path.split_once(' ').is_some(),
            "event {:?}: method_path must be 'METHOD PATH'",
            ev.method_path,
        );
        assert!(
            !ev.molecule_id.is_empty(),
            "event {:?}: molecule_id is required (use task ID or ADR slug)",
            ev.method_path,
        );
    }
}

/// Every scope named in the canon must exist in the adapter's scope
/// catalog ([`cosmon_rpp_adapter::auth::scopes::ALL`]). Joins two
/// compile-time consts — a typo'd scope column cannot reach `main`.
#[test]
fn every_canon_scope_exists_in_the_catalog() {
    for ev in SURFACE_EVENTS {
        if ev.scope == "-" {
            continue;
        }
        for part in ev.scope.split('+') {
            assert!(
                cosmon_rpp_adapter::auth::scopes::ALL.contains(&part),
                "event {:?}: scope {part:?} is not declared in auth::scopes::ALL — \
                 either fix the canon line or declare the scope first",
                ev.method_path,
            );
        }
    }
}

/// §8p (ADR-080 §5): no event on the frozen surface may be classified
/// `operator-only` — the canon's `exposure` column replaced the
/// hand-maintained `forbidden` verb list that used to live here
/// (delete a copy, don't add a checker).
///
/// Belt-and-braces: the route paths are also cross-checked against
/// [`cosmon_thin_cli::coverage::OPERATOR_ONLY`] — the one canonical
/// operator-only list (itself ADR-synced by
/// `operator_only_in_sync_with_adr`) — so a mislabelled canon line
/// (e.g. a `/kill` route declared `tenant-verb`) still fails.
#[test]
fn surface_does_not_expose_operator_only_verbs() {
    use cosmon_rpp_adapter::surface_events::Exposure;

    for ev in SURFACE_EVENTS {
        assert!(
            ev.exposure != Exposure::OperatorOnly,
            "event {:?} is classified operator-only but sits on the §8p frozen \
             surface — ADR-080 §5 forbids exposing it; remove the route",
            ev.method_path,
        );
    }

    let surface_concat = frozen_api_surface().join(" ");
    for entry in cosmon_thin_cli::coverage::OPERATOR_ONLY {
        // Compound verbs (`security activate`) gate on their first token.
        let verb = entry.name.split_whitespace().next().unwrap_or(entry.name);
        let pat = format!("/{verb}");
        assert!(
            !surface_concat.contains(&pat),
            "operator-only verb `{verb}` ({}) leaked into the public surface — ADR-080 §5",
            entry.adr_ref,
        );
    }
}

/// Bijection between axum routes and `#[verb]` annotations.
///
/// Each side must be a projection of the other:
///
/// - Every event classified `tenant-verb` in the canon
///   (`data/surface_events.txt`, `exposure` column) MUST be carried
///   by at least one entry in [`cosmon_thin_cli::registry::VERBS`]
///   (i.e. a `#[verb]` annotation in `cosmon-state::ops` or in the
///   client-side stubs at `cosmon-thin-cli::verbs`).
/// - Every annotated verb in the registry MUST resolve to a
///   `tenant-verb` route in the frozen surface — annotating without
///   mounting is a §8p violation.
///
/// Routes classified `adapter-only` intentionally have no `#[verb]`
/// counterpart (filesystem-mediated artifacts, SSE streams, auth
/// flow, discovery…) — the per-route rationale lives in the canon's
/// `blurb` column. The classification used to be a hand-maintained
/// `is_adapter_only()` predicate here; it is now the `exposure`
/// column of the source itself (delete a copy, don't add a checker).
///
/// Path templates are normalised on the fly (`:id` ↔ `{id}`) so the
/// Express-style placeholders used in `cosmon-thin-cli::verbs::*` line
/// up with the axum-style placeholders used in `frozen_api_surface`.
#[test]
fn routes_and_verbs_are_bijective() {
    use cosmon_rpp_adapter::surface_events::Exposure;

    let surface_pairs: BTreeSet<(String, String)> = SURFACE_EVENTS
        .iter()
        .filter(|ev| ev.exposure == Exposure::TenantVerb)
        .map(|ev| {
            let (method, path) = ev.method_path.split_once(' ').unwrap_or_else(|| {
                panic!("malformed surface event (no space): {:?}", ev.method_path)
            });
            (
                method.to_ascii_uppercase(),
                normalise_path_placeholders(path),
            )
        })
        .collect();

    let verb_pairs: BTreeSet<(String, String)> = cosmon_thin_cli::registry::all()
        .iter()
        .map(|d| {
            (
                d.method.to_ascii_uppercase(),
                normalise_path_placeholders(d.path),
            )
        })
        .collect();

    let surface_only: Vec<_> = surface_pairs.difference(&verb_pairs).cloned().collect();
    let verbs_only: Vec<_> = verb_pairs.difference(&surface_pairs).cloned().collect();

    assert!(
        surface_only.is_empty(),
        "routes classified `tenant-verb` in the canon with NO matching #[verb] annotation: \
         {surface_only:?}; either annotate the impl in cosmon-state::ops::* (and mirror in \
         cosmon-thin-cli::verbs::*) or reclassify the canon line `adapter-only` — §8p drift forbidden",
    );
    assert!(
        verbs_only.is_empty(),
        "verbs annotated with #[verb] but NOT mounted on the axum router: {verbs_only:?}; \
         either mount the route and append a `tenant-verb` line to data/surface_events.txt, \
         or drop the annotation — §8p drift forbidden",
    );

    // Belt-and-braces: equal sets after diff means equal sets total.
    assert_eq!(
        surface_pairs, verb_pairs,
        "the canon's tenant-verb events and the #[verb] registry must describe the same \
         set of (method, path) pairs",
    );
}

/// Normalise a path so `:id` and `{id}` style placeholders compare
/// equal. The canonical form chosen here is the Express-style colon
/// prefix (`:id`) since that is what the macro accepts on the
/// annotation site; the axum router uses `{id}` since axum 0.7. The
/// bijection test treats them as identical.
fn normalise_path_placeholders(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if let Some(name) = seg.strip_prefix(':') {
                format!(":{name}")
            } else if seg.starts_with('{') && seg.ends_with('}') {
                format!(":{}", &seg[1..seg.len() - 1])
            } else {
                seg.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn placeholder_normalisation_round_trips() {
    assert_eq!(
        normalise_path_placeholders("/v1/molecules/:id"),
        "/v1/molecules/:id"
    );
    assert_eq!(
        normalise_path_placeholders("/v1/molecules/{id}"),
        "/v1/molecules/:id"
    );
    assert_eq!(
        normalise_path_placeholders("/v1/molecules/:id/tags"),
        "/v1/molecules/:id/tags"
    );
    assert_eq!(
        normalise_path_placeholders("/v1/molecules/{id}/tags"),
        "/v1/molecules/:id/tags"
    );
    assert_eq!(
        normalise_path_placeholders("/v1/molecules"),
        "/v1/molecules"
    );
}
