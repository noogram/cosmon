// SPDX-License-Identifier: AGPL-3.0-only

//! `/mcp` — the Model Context Protocol surface, nested on the adapter's
//! single `:8080` listener as a **third projection** of the same
//! `cosmon_state`/`cosmon_core` core the `/v1/...` REST routes project
//! (delib-20260709-943e, torvalds' data-structure verdict).
//!
//! # Topology (why nested, not a second process)
//!
//! The MCP server is `.nest_service("/mcp", …)` behind the existing bearer
//! layer — kernel co-location: one OIDC gate, one JWKS loop, one listener.
//! cosmon stays a **Resource Server**; it validates tokens minted elsewhere
//! (the avatar's Forgejo AS) and never issues identity itself. The MCP crate
//! ([`cosmon_mcp`]) is transport-only; authentication and tenancy live here,
//! one layer up.
//!
//! # This milestone — transport + gate + tool-exposure partition
//!
//! This module ships the **transport plumbing** the panel named: the
//! Streamable-HTTP service nested behind a *bearer-required* gate so the
//! surface is never anonymously reachable over the network (defense in depth
//! atop the Tailscale wire).
//!
//! **M3 (delib-20260709-943e, turing's Q5) has landed the tool-exposure
//! partition:**
//!
//! * The nested service is [`cosmon_mcp::streamable_http_service`], which
//!   mints [`cosmon_mcp::CosmonService::new_remote`] — the worker-internal /
//!   teardown verbs (`evolve`, `complete`, `nudge`, `declare`, `energy_log`;
//!   see [`cosmon_mcp::DENY_REMOTE_TOOLS`]) are **absent** from `tools/list`
//!   over this connector, so they cannot be called or injection-targeted.
//!   `done` / `whisper` / `tackle` have no MCP-tool counterpart and are
//!   deny-by-absence.
//! * `tackle` (the REST verb `POST /v1/molecules/{id}/tackle`) is gated by
//!   `MOLECULE_WRITE` **AND** `WORKER_SPAWN` (M1) **plus** a hard per-noyau
//!   live-worker ceiling checked at the pre-spawn seam
//!   ([`crate::routes::molecules::DEFAULT_TACKLE_CEILING_PER_NOYAU`]) — a
//!   noyau already at the cap is refused with `429 tackle_ceiling` before any
//!   credit is burned.
//!
//! Two path-B seams from the same deliberation are **still** follow-up:
//!
//! * severing the tool `cwd` param from filesystem resolution so the
//!   statedir comes from `jwt.noyau` only — the two-lens `cwd` finding;
//! * the RFC 9728 `/.well-known/oauth-protected-resource` document and the
//!   `WWW-Authenticate: Bearer resource_metadata=…` 401 challenge — tolnay's
//!   Q4/silent-break trap.
//!
//! Each of those is a distinct seam tracked back to delib-20260709-943e.
//!
//! **Per-tool scope enforcement has landed (F3-1, task-20260712-0294).**
//! [`require_valid_bearer`] now classifies every request by its JSON-RPC
//! method + tool name and requires the *same* scope the REST twin does:
//! `cosmon:molecule:write` for the mutating tool partition
//! (`nucleate`/`freeze`/`thaw`/`collapse`/`decay`/`merge`/`transform`) and
//! `cosmon:molecule:read` as the floor for the read tools and the protocol
//! handshake. A tenant-valid JWT bearing only `openid` can therefore no
//! longer invoke a state-mutating tool that its REST counterpart
//! (`POST /v1/molecules`, which requires `cosmon:molecule:write`) would
//! refuse — closing the within-tenant scope-escalation gap the identity +
//! audience pin left open (the pin proves *which* galaxy, not *what* the
//! caller may do inside it). Cross-tenant pivots were, and remain, blocked
//! by the audience pin independently. This is the per-tool scope enforcement
//! [`cosmon_mcp`] documents living "one layer up in the host adapter's gate".

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use cosmon_mcp::HttpStatePin;

use crate::admission::Verb;
use crate::jwt::JwtVerifier;
use crate::routes::molecules::{
    authorise_scope_public, build_spark_public, SCOPE_MOLECULE_READ, SCOPE_MOLECULE_WRITE,
};
use crate::AppState;
use crate::RppRejectReason;

/// Build the gated `/mcp` sub-router: the [`cosmon_mcp`] Streamable-HTTP
/// service behind [`require_valid_bearer`].
///
/// Mounted by [`crate::router`] with `.nest("/mcp", mcp_router(state))`.
/// axum strips the `/mcp` prefix before the inner service sees the request;
/// the Streamable-HTTP service dispatches on HTTP *method* (POST/GET/DELETE)
/// rather than path, so the empty remaining path is exactly what it expects.
///
/// The returned router keeps `Arc<AppState>` as its declared state type so it
/// composes into the parent via `.nest("/mcp", …)`; the gate middleware binds
/// its own copy of the state through `from_fn_with_state`, and the inner
/// Streamable-HTTP service needs no router state at all.
pub fn mcp_router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .fallback_service(cosmon_mcp::streamable_http_service())
        .layer(axum::middleware::from_fn_with_state(
            state,
            require_valid_bearer,
        ))
}

/// Gate every `/mcp` request: validate the bearer JWT, then **resolve the
/// tenant** and pin the MCP state directory to it before the tool dispatch
/// runs.
///
/// Pipeline (M2 tenant-isolation seam, delib-20260709-943e conv. #6):
///
/// 1. Extract `Authorization: Bearer <jwt>`; reject if absent.
/// 2. Validate the JWT against the pinned JWKS — reuses the hardened
///    [`JwtVerifier::validate`] core verbatim (algorithm whitelist,
///    `jku`/`x5u` refusal, issuer pinning, posture lifetime cap), the same
///    validation the REST handlers run per-request.
/// 3. Run the five-clause admission boundary at [`Verb::McpToolCall`] via
///    [`build_spark_public`]. This resolves `spark.noyau` from the
///    audience-pinned `(iss,sub,aud)` binding and enforces the
///    `CrossTenantPivot` guard — a token can only reach the galaxy it
///    carries the audience for. The caller NEVER supplies the noyau.
/// 4. **Per-tool scope check (F3-1).** Buffer the JSON-RPC body, classify
///    the request by its method + tool name ([`required_scope`]), and require
///    the same scope the REST twin enforces — `cosmon:molecule:write` for the
///    mutating tool partition, `cosmon:molecule:read` as the floor otherwise —
///    via [`authorise_scope_public`]. Step 3 proves *which* galaxy the token
///    may reach; this step proves *what* it may do inside it. Without it a
///    write-unscoped-but-tenant-valid JWT could mutate state over MCP that
///    `POST /v1/molecules` would 403.
/// 5. Insert an [`HttpStatePin`] rooted at `<galaxies_root>/<noyau>/.cosmon`
///    into the request extensions. The nested [`cosmon_mcp`] service reads
///    it per request and resolves **every** tool's state / formulas / config
///    against it, ignoring the client-supplied `cwd`. A JWT for noyau A
///    therefore resolves a molecule id belonging to noyau B to *not-found*
///    under tenant A's tree — never to B's data.
///
/// The pin is re-derived here on every request and never cached in the MCP
/// session (panel D1), so reusing a peer's `Mcp-Session-Id` cannot pivot
/// tenants.
async fn require_valid_bearer(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    // 1 + 2. Bearer extraction and JWT validation.
    let jwt = match extract_bearer(&req) {
        Ok(token) => match JwtVerifier::validate(&state.jwks.load(), token, state.posture) {
            Ok(jwt) => jwt,
            Err(e) => return state.reject(e).into_response(),
        },
        Err(e) => return state.reject(e).into_response(),
    };

    // 3. Admission boundary → resolve the tenant noyau (audience pin +
    //    CrossTenantPivot guard). Target is `None`: the tool payload, not
    //    the URL, names the molecule, and the noyau resolution does not
    //    depend on it.
    let spark = match build_spark_public(&state, &jwt, Verb::McpToolCall, None) {
        Ok(spark) => spark,
        Err(e) => return e.into_response(),
    };

    // 4. Per-tool scope check (F3-1, task-20260712-0294). Buffer the
    //    JSON-RPC body so the mutating tool partition can be identified,
    //    then require the SAME scope the REST twin enforces. Without this a
    //    tenant-valid JWT carrying only `openid` clears the audience pin
    //    above and reaches a state-mutating tool that `POST /v1/molecules`
    //    would 403 — a within-tenant scope escalation.
    //
    // A body larger than the cap (or a broken stream) cannot be classified;
    // refuse `413` rather than dispatch an unclassified request.
    let (parts, body) = req.into_parts();
    let Ok(body_bytes) = axum::body::to_bytes(body, MAX_MCP_BODY_BYTES).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").into_response();
    };
    let scope_check = match required_scope(&parts.method, &body_bytes) {
        RequiredScope::Write => authorise_scope_public(
            &state,
            &jwt,
            "mcp_tool_call",
            &[SCOPE_MOLECULE_WRITE],
            SCOPE_MOLECULE_WRITE,
        ),
        RequiredScope::Read => authorise_scope_public(
            &state,
            &jwt,
            "mcp_tool_call",
            &[SCOPE_MOLECULE_READ, SCOPE_MOLECULE_WRITE],
            SCOPE_MOLECULE_READ,
        ),
    };
    if let Err(e) = scope_check {
        return e.into_response();
    }

    // 4. Pin the MCP state directory to the tenant root. This is the ONLY
    //    place the statedir is chosen; the tool `cwd` parameter is inert
    //    downstream because the pin takes absolute precedence.
    let tenant_cosmon = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon");
    let pin = HttpStatePin::new(tenant_cosmon.join("state"), tenant_cosmon.join("formulas"));
    let mut req = Request::from_parts(parts, Body::from(body_bytes));
    req.extensions_mut().insert(pin);

    next.run(req).await
}

/// Upper bound on the MCP request body buffered to classify the required
/// scope. MCP tool-call payloads are small JSON envelopes; 4 MiB is far above
/// any legitimate `nucleate` variables map yet bounds the memory a single
/// request can force the gate to hold. A larger body is refused `413` rather
/// than dispatched unclassified.
const MAX_MCP_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The scope tier an `/mcp` request must satisfy, mirroring the REST scope
/// discipline: `Read` for observation + the protocol handshake, `Write` for
/// any state mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequiredScope {
    /// `cosmon:molecule:read` (or `:write`, which implies read).
    Read,
    /// `cosmon:molecule:write` — the mutating tool partition.
    Write,
}

/// MCP tools that only *read* tenant state. A `tools/call` naming one of
/// these needs the read floor; **everything else** — a mutating tool, an
/// unknown/future tool, or a malformed name — needs write (fail-closed, so a
/// mutation can never slip under the read floor). This is the exact REST
/// parity split: the write side is `nucleate`/`freeze`/`thaw`/`collapse`/
/// `decay`/`merge`/`transform`, matching the `MOLECULE_WRITE` REST verbs; the
/// worker-internal mutators (`evolve`/`complete`/…) are already denied by
/// absence ([`cosmon_mcp::DENY_REMOTE_TOOLS`]) and, if ever re-exposed, fall
/// on the write side here too.
const MCP_READ_TOOLS: &[&str] = &[
    "cosmon_observe",
    "cosmon_ensemble",
    "cosmon_wait",
    "cosmon_search",
    "cosmon_get",
    "cosmon_list",
    "cosmon_count",
    "cosmon_export",
    "cosmon_stats",
    "cosmon_aggregate",
    "cosmon_energy",
    "cosmon_fleet_templates",
];

/// Classify an `/mcp` request into the scope tier it must satisfy.
///
/// Only `POST` carries a JSON-RPC message; `GET` (open the SSE stream) and
/// `DELETE` (end the session) are read-tier transport operations. For a
/// `POST`, the body is parsed as a JSON-RPC message (or batch array); a
/// `tools/call` whose tool is **not** in [`MCP_READ_TOOLS`] escalates the
/// whole request to `Write`. A body that does not parse into a mutating
/// `tools/call` floors at `Read` — it cannot mutate, and the inner service
/// rejects malformed frames on its own.
fn required_scope(method: &Method, body: &[u8]) -> RequiredScope {
    if *method != Method::POST {
        return RequiredScope::Read;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return RequiredScope::Read;
    };
    // A single message or a JSON-RPC batch — the strongest scope any element
    // requires governs the whole request.
    let messages: Vec<&serde_json::Value> = match &value {
        serde_json::Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    for msg in messages {
        if msg.get("method").and_then(serde_json::Value::as_str) == Some("tools/call") {
            let tool = msg
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !MCP_READ_TOOLS.contains(&tool) {
                return RequiredScope::Write;
            }
        }
    }
    RequiredScope::Read
}

/// Extract the bearer token from a request's `Authorization` header,
/// mirroring `routes::molecules::extract_bearer` (kept private there).
fn extract_bearer(req: &Request) -> Result<&str, RppRejectReason> {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .ok_or(RppRejectReason::MissingAuthorization)?;
    let s = header.to_str().map_err(|_| RppRejectReason::MalformedJwt)?;
    let stripped = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .ok_or(RppRejectReason::MalformedJwt)?;
    Ok(stripped.trim())
}

#[cfg(test)]
mod tests {
    use super::{required_scope, RequiredScope};
    use axum::http::Method;

    fn tools_call(tool: &str) -> Vec<u8> {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": {} }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn mutating_tool_calls_require_write() {
        for tool in [
            "cosmon_nucleate",
            "cosmon_freeze",
            "cosmon_thaw",
            "cosmon_collapse",
            "cosmon_decay",
            "cosmon_merge",
            "cosmon_transform",
        ] {
            assert_eq!(
                required_scope(&Method::POST, &tools_call(tool)),
                RequiredScope::Write,
                "{tool} must require write scope to match its REST twin"
            );
        }
    }

    #[test]
    fn read_tool_calls_only_require_read() {
        for tool in [
            "cosmon_observe",
            "cosmon_ensemble",
            "cosmon_wait",
            "cosmon_search",
            "cosmon_get",
            "cosmon_list",
            "cosmon_count",
            "cosmon_export",
            "cosmon_stats",
            "cosmon_aggregate",
            "cosmon_energy",
            "cosmon_fleet_templates",
        ] {
            assert_eq!(
                required_scope(&Method::POST, &tools_call(tool)),
                RequiredScope::Read,
                "{tool} is read-only and must floor at read scope"
            );
        }
    }

    #[test]
    fn unknown_or_worker_internal_tool_fails_closed_to_write() {
        // A future / unrecognised tool, and a worker-internal mutator that
        // is denied-by-absence today, both fall on the write side so a
        // mutation can never slip under the read floor.
        for tool in [
            "cosmon_future_thing",
            "cosmon_evolve",
            "cosmon_complete",
            "",
        ] {
            assert_eq!(
                required_scope(&Method::POST, &tools_call(tool)),
                RequiredScope::Write,
                "unknown/worker-internal tool `{tool}` must fail closed to write"
            );
        }
    }

    #[test]
    fn protocol_methods_and_non_post_floor_at_read() {
        // The handshake / listing methods are read-tier.
        for method_name in [
            "initialize",
            "tools/list",
            "ping",
            "notifications/initialized",
        ] {
            let body = serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": method_name, "params": {}
            })
            .to_string()
            .into_bytes();
            assert_eq!(
                required_scope(&Method::POST, &body),
                RequiredScope::Read,
                "protocol method `{method_name}` must floor at read"
            );
        }
        // GET (open SSE) and DELETE (end session) carry no tool call.
        assert_eq!(required_scope(&Method::GET, b""), RequiredScope::Read);
        assert_eq!(required_scope(&Method::DELETE, b""), RequiredScope::Read);
        // An unparseable body cannot name a mutating tool → read floor.
        assert_eq!(
            required_scope(&Method::POST, b"not json at all"),
            RequiredScope::Read
        );
    }

    #[test]
    fn batch_escalates_to_write_if_any_element_mutates() {
        let batch = serde_json::json!([
            { "jsonrpc": "2.0", "id": 1, "method": "tools/call",
              "params": { "name": "cosmon_observe" } },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": { "name": "cosmon_nucleate" } }
        ])
        .to_string()
        .into_bytes();
        assert_eq!(required_scope(&Method::POST, &batch), RequiredScope::Write);
    }
}
