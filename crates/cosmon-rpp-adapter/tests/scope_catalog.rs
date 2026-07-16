// SPDX-License-Identifier: AGPL-3.0-only

//! Scope catalog pin.
//!
//! Pins the order and content of [`cosmon_rpp_adapter::auth::scopes::ALL`]
//! so that adding a scope is a conscious one-line change (additive,
//! minor bump) and reordering is rejected (documentation generation
//! and `cs auth scopes --list` planning depend on stable ordering).

use cosmon_rpp_adapter::auth::scopes::{
    ALL, ARTIFACT_READ, ARTIFACT_WRITE, EVENTS_SUBSCRIBE, LOGS_SUBSCRIBE, MOLECULE_READ,
    MOLECULE_WRITE, PILOTE_CONVERSE, WORKER_READ, WORKER_SPAWN, WORKER_TERMINATE, WORLD_OBSERVE,
};

#[test]
fn scope_catalog_is_pinned() {
    assert_eq!(
        ALL,
        &[
            MOLECULE_READ,
            MOLECULE_WRITE,
            WORKER_SPAWN,
            WORKER_READ,
            WORKER_TERMINATE,
            ARTIFACT_READ,
            ARTIFACT_WRITE,
            EVENTS_SUBSCRIBE,
            LOGS_SUBSCRIBE,
            PILOTE_CONVERSE,
            WORLD_OBSERVE,
        ],
        "scope catalog must remain stable — adding a scope is additive \
         (minor bump) and requires updating this pin, ADR-080 §6.5, and \
         the OpenAPI docs in the same PR. Last addition: \
         cosmon:pilote:converse + cosmon:world:observe \
         (task-20260524-270a, D-AVATAR canaux b+d + task-20260525-738e lifecycle)."
    );
}

#[test]
fn molecule_and_worker_scopes_have_distinct_namespaces() {
    // Sanity: read/write are molecule-scoped, spawn/read/terminate are
    // worker-scoped. Mixing namespaces (e.g. `cosmon:molecule:spawn`)
    // would conflate cost classes — the whole point of the v1.0.0-rc
    // grid is the namespace separation. Artifacts get their own
    // `cosmon:artifact:*` namespace (e653, task-20260522-ef4f) for the
    // same reason: artifact payloads have a different blast radius
    // than molecule state. Events live under `cosmon:events:*`
    // (task-20260522-c46a) — a long-lived SSE tail is a different
    // blast radius than periodic observe. Logs live under
    // `cosmon:logs:*` (task-20260523-ad25) — the raw pane reveals
    // intermediate worker reasoning, a different surface again.
    assert!(MOLECULE_READ.starts_with("cosmon:molecule:"));
    assert!(MOLECULE_WRITE.starts_with("cosmon:molecule:"));
    assert!(WORKER_SPAWN.starts_with("cosmon:worker:"));
    assert!(WORKER_READ.starts_with("cosmon:worker:"));
    assert!(WORKER_TERMINATE.starts_with("cosmon:worker:"));
    assert!(ARTIFACT_READ.starts_with("cosmon:artifact:"));
    assert!(ARTIFACT_WRITE.starts_with("cosmon:artifact:"));
    assert!(EVENTS_SUBSCRIBE.starts_with("cosmon:events:"));
    assert!(LOGS_SUBSCRIBE.starts_with("cosmon:logs:"));
}

#[test]
fn scope_literals_match_published_strings() {
    // Pin the exact wire-format strings. Changing any of these is a
    // breaking change that requires migrating every existing JWT and
    // every admin-nucleon binding — must never happen silently.
    assert_eq!(MOLECULE_READ, "cosmon:molecule:read");
    assert_eq!(MOLECULE_WRITE, "cosmon:molecule:write");
    assert_eq!(WORKER_SPAWN, "cosmon:worker:spawn");
    assert_eq!(WORKER_READ, "cosmon:worker:read");
    assert_eq!(WORKER_TERMINATE, "cosmon:worker:terminate");
    assert_eq!(ARTIFACT_READ, "cosmon:artifact:read");
    assert_eq!(ARTIFACT_WRITE, "cosmon:artifact:write");
    assert_eq!(EVENTS_SUBSCRIBE, "cosmon:events:subscribe");
    assert_eq!(LOGS_SUBSCRIBE, "cosmon:logs:subscribe");
}
