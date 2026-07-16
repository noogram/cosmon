// SPDX-License-Identifier: AGPL-3.0-only

//! `OAuth2` scope catalog for the cosmon RPP surface.
//!
//! ADR-080 §6.5 — preventive scope-per-costly-verb grid, inscribed
//! before v1.0.0 freeze. Adding a scope is additive (minor bump);
//! reusing an existing one for a costly verb is a doctrinal regression.
//!
//! # Grid
//!
//! | Verb (route)                                  | Scope minimale                                  | Coût           | Statut              |
//! |-----------------------------------------------|-------------------------------------------------|----------------|---------------------|
//! | `nucleate`, `tag`, `freeze`, `collapse`, `stuck` | [`MOLECULE_WRITE`]                            | gratuit        | **existe**          |
//! | `tackle`                                      | [`MOLECULE_WRITE`] **+** [`WORKER_SPAWN`] (AND) | $$$ Anthropic | **nouveau v1.0.0-rc** |
//! | `cancel` / `kill` (futur)                     | [`WORKER_TERMINATE`]                            | irréversible   | **réservé** (additif v2) |
//! | `observe`, `ensemble`                         | [`MOLECULE_READ`] (ou `:write` qui implique)    | gratuit        | **existe**          |
//!
//! # Invariant
//!
//! *Toute opération qui coûte de l'argent réel ou est structurellement
//! irréversible exige une scope distincte, créée **avant** la première
//! utilisation, et **jamais** rétrocédée à une scope amont par
//! implication silencieuse.*

/// Read-only access to the per-tenant molecule store. Cheapest scope.
///
/// Granted by JWT (typically tenant identity) or by admin-nucleon
/// binding. Implied by [`MOLECULE_WRITE`] (write ⊃ read) but **not** by
/// [`WORKER_TERMINATE`] (terminate does not imply read of remaining
/// molecules — operator may delegate kill rights without observability).
pub const MOLECULE_READ: &str = "cosmon:molecule:read";

/// Mutate cheap, reversible molecule state: nucleate, tag, freeze,
/// collapse, stuck. **Does not** authorise tackle (spawn) or cancel
/// (terminate).
pub const MOLECULE_WRITE: &str = "cosmon:molecule:write";

/// Authorise the adapter to spawn a worker against a Pending molecule.
/// Required **in addition to** [`MOLECULE_WRITE`] for
/// `POST /v1/molecules/{id}/tackle`. Spawning is non-trivial because
/// it commits an `ANTHROPIC_API_KEY` to a tmux session that will burn
/// token credit until natural termination.
///
/// Rationale: a tenant that grants only `write` MUST NOT be able to
/// burn the operator's Anthropic budget. Silently widening `write` to
/// cover `spawn` is the failure mode this scope prevents.
pub const WORKER_SPAWN: &str = "cosmon:worker:spawn";

/// Read-only listing of active workers in the per-tenant noyau
/// (`GET /v1/workers`). Distinct from
/// [`MOLECULE_READ`] because the worker surface exposes runtime
/// session-level facts (tmux session names, PIDs, start instants) that
/// a tenant which only needs molecule state should not be granted —
/// session metadata can reveal worker placement and supervision
/// posture that the per-molecule observe surface deliberately hides.
///
/// Additive at v1.4: a tenant
/// that owns a JWT carrying `cosmon:worker:read` can answer "which
/// molecules currently have a live worker bound?" without polling
/// `GET /v1/molecules` for the whole noyau just to filter on the
/// `process` field locally. The route does **not** authorise spawning
/// (that remains under [`WORKER_SPAWN`]) nor terminating (reserved
/// under [`WORKER_TERMINATE`]) — it is observability only.
pub const WORKER_READ: &str = "cosmon:worker:read";

/// Authorise terminating an already-running worker session. Reserved
/// for the future `POST /v1/molecules/{id}/cancel` (or `kill`) route.
/// Distinct from [`WORKER_SPAWN`] because terminating mid-flight may
/// leave the molecule in an inconsistent half-tackled state requiring
/// operator reconciliation.
///
/// **Not** wired in v1.0.0-rc — declared here so the namespace is
/// reserved (any tenant token that includes `cosmon:worker:terminate`
/// today is a no-op; tomorrow it will gate cancel/kill).
pub const WORKER_TERMINATE: &str = "cosmon:worker:terminate";

/// Read access to per-molecule artifacts (the worker's outputs on
/// disk under `/tmp/cosmon/<noyau>/<molecule_id>/`). Required by
/// `GET /v1/molecules/{id}/artifacts` (manifest) and
/// `GET /v1/molecules/{id}/artifacts/{token}` (binary stream).
///
/// Distinct from [`MOLECULE_READ`] because artifacts can contain
/// worker-side payloads (LLM outputs, screenshots, generated files)
/// whose exposure is decided independently from molecule observability.
/// A tenant that grants `:molecule:read` to a dashboard does not have
/// to also grant `:artifact:read`.
pub const ARTIFACT_READ: &str = "cosmon:artifact:read";

/// Write access to per-molecule artifacts (push back-utterance).
/// Required by `PUT /v1/molecules/{id}/artifacts/{name}`. Distinct
/// from [`MOLECULE_WRITE`] because uploading binary payloads has a
/// different blast radius than nucleating / tagging.
pub const ARTIFACT_WRITE: &str = "cosmon:artifact:write";

/// Subscribe to the per-tenant SSE stream of molecule lifecycle
/// events (`GET /v1/events`). Distinct from
/// [`MOLECULE_READ`] because the SSE channel is a long-lived tail —
/// a token that only grants periodic observe should not be able to
/// convert itself into a real-time view of every state transition
/// for the noyau. Required at the SSE handler boundary; absence
/// yields 403 with
/// `AuthzDecisionEvaluated{verb=events_subscribe}`.
pub const EVENTS_SUBSCRIBE: &str = "cosmon:events:subscribe";

/// Subscribe to the per-molecule SSE stream of worker tmux output
/// lines (`GET /v1/molecules/{id}/logs`).
/// Distinct from [`EVENTS_SUBSCRIBE`] because the logs channel
/// reveals the worker's intermediate reasoning and tool calls (the
/// raw `claude` pane), not just the molecule state machine. A
/// tenant token granted `cosmon:events:subscribe` for a dashboard
/// must not be able to lift itself into a live view of what claude
/// is actually saying inside the pane. Required at the logs handler
/// boundary; absence yields 403 with
/// `AuthzDecisionEvaluated{verb=logs_subscribe}`.
pub const LOGS_SUBSCRIBE: &str = "cosmon:logs:subscribe";

/// Audit-trail label: the JWT itself carried the scope.
pub const GRANT_SOURCE_JWT: &str = "jwt";

/// Audit-trail label: the admin nucleon binding granted the scope
/// implicitly (T23).
pub const GRANT_SOURCE_BINDING: &str = "binding";

/// Authorise a pilote to converse with an avatar-tiers through the
/// canal (b) surface (`POST /v1/avatar/converse`). The avatar-tiers
/// must have consented via
/// explicit binding (on-by-binding); the scope gates the caller's
/// right to initiate the conversation. Distinct from molecule
/// scopes because canal (b) is an inter-pilot channel, not a
/// molecule lifecycle verb.
pub const PILOTE_CONVERSE: &str = "cosmon:pilote:converse";

/// Authorise an external source to push perception data into an
/// avatar's perception log through the canal (d) surface
/// (`POST /v1/avatar/perceive`). OFF by
/// default — the adapter only admits requests when the per-source
/// feature flag is enabled. Distinct from molecule scopes because
/// canal (d) is world→avatar afference, not molecule lifecycle.
pub const WORLD_OBSERVE: &str = "cosmon:world:observe";

/// All declared scopes — for `cs auth scopes --list` (planned),
/// `tests/scope_catalog.rs` exhaustiveness check, and documentation
/// generation. Order MUST be stable (tested).
pub const ALL: &[&str] = &[
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
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_scopes_have_stable_order() {
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
        );
    }

    #[test]
    fn molecule_scopes_are_well_formed() {
        for s in [MOLECULE_READ, MOLECULE_WRITE] {
            assert!(
                s.starts_with("cosmon:molecule:"),
                "{s} must be molecule scope"
            );
        }
    }

    #[test]
    fn worker_scopes_are_well_formed() {
        for s in [WORKER_SPAWN, WORKER_READ, WORKER_TERMINATE] {
            assert!(s.starts_with("cosmon:worker:"), "{s} must be worker scope");
        }
    }

    #[test]
    fn artifact_scopes_are_well_formed() {
        for s in [ARTIFACT_READ, ARTIFACT_WRITE] {
            assert!(
                s.starts_with("cosmon:artifact:"),
                "{s} must be artifact scope"
            );
        }
    }

    #[test]
    fn events_scope_is_well_formed() {
        assert!(
            EVENTS_SUBSCRIBE.starts_with("cosmon:events:"),
            "{EVENTS_SUBSCRIBE} must be events scope"
        );
    }

    #[test]
    fn logs_scope_is_well_formed() {
        assert!(
            LOGS_SUBSCRIBE.starts_with("cosmon:logs:"),
            "{LOGS_SUBSCRIBE} must be logs scope"
        );
    }

    #[test]
    fn logs_scope_distinct_from_events_scope() {
        // Doctrinal regression guard: logs and events MUST be separate
        // scopes. The two channels reveal different surfaces — events
        // is the state-machine tail, logs is the worker's live pane.
        assert_ne!(LOGS_SUBSCRIBE, EVENTS_SUBSCRIBE);
    }

    #[test]
    fn pilote_scope_is_well_formed() {
        assert!(
            PILOTE_CONVERSE.starts_with("cosmon:pilote:"),
            "{PILOTE_CONVERSE} must be pilote scope"
        );
    }

    #[test]
    fn world_scope_is_well_formed() {
        assert!(
            WORLD_OBSERVE.starts_with("cosmon:world:"),
            "{WORLD_OBSERVE} must be world scope"
        );
    }

    #[test]
    fn avatar_scopes_distinct_from_molecule_scopes() {
        assert_ne!(PILOTE_CONVERSE, MOLECULE_READ);
        assert_ne!(PILOTE_CONVERSE, MOLECULE_WRITE);
        assert_ne!(WORLD_OBSERVE, MOLECULE_READ);
        assert_ne!(WORLD_OBSERVE, MOLECULE_WRITE);
    }

    #[test]
    fn grant_sources_are_distinct() {
        assert_ne!(GRANT_SOURCE_JWT, GRANT_SOURCE_BINDING);
    }
}
