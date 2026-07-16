// SPDX-License-Identifier: AGPL-3.0-only

//! cosmon-mcp — remote-MCP transport substrate for the Cosmon HTTP surface.
//!
//! # Status: active library — one consumer, one purpose (reclassified 2026-07-12, C14)
//!
//! This crate was **deprecated 2026-04-11** as a standalone worker/pilot MCP
//! surface: workers use the [`cs`] CLI exclusively (the CLI-first invariant in
//! `docs/architectural-invariants.md`), the human pilot uses the CLI, and no
//! real consumer needed a parallel *stdio* MCP surface. Maintaining two
//! parallel surfaces caused real drift bugs — stale `cosmon-mcp` binaries
//! answered "molecule not found" while the CLI saw the same molecule, because
//! the MCP process was pinned to an older build of the state store.
//!
//! The git analogy killed the **stdio** surface: git has no MCP server yet
//! LLMs use it daily via shell. The legacy stdio entry points
//! (`serve_stdio`, the standalone `cosmon-mcp` binary, and `cs mcp`) were
//! therefore **removed 2026-07-12** — see decision C14
//! (`task-20260712-74a1/outcomes.md`).
//!
//! What survives — and why the crate is *not* deleted — is the **remote**
//! surface: [`streamable_http_service`] mints a Streamable-HTTP tower service
//! that [`cosmon-rpp-adapter`] nests behind its bearer/OIDC gate to serve the
//! remote-tenant MCP endpoint (delib-20260709-943e). The crate is now a
//! transport-only library with exactly one consumer (`cosmon-rpp-adapter`, a
//! live workspace member), holding no opinion about authentication or tenancy.
//! It is compiled as a first-class transitive dependency of that adapter — the
//! "out of default workspace members" note is a historical artefact of the
//! dead stdio era.
//!
//! [`cs`]: https://docs.rs/cosmon-cli
//! [`cosmon-rpp-adapter`]: https://docs.rs/cosmon-rpp-adapter

#![forbid(unsafe_code)]

mod tools;

pub use tools::{CosmonService, HttpStatePin, DENY_REMOTE_TOOLS};

use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

/// The concrete Streamable-HTTP tower service type this crate hands to a
/// host router (e.g. `cosmon-rpp-adapter`) via `.nest_service("/mcp", …)`.
///
/// It is a plain `tower::Service<http::Request<B>>` and implements `Clone`,
/// so it slots straight into axum's `nest_service` / `fallback_service`.
pub type CosmonHttpService = StreamableHttpService<CosmonService, LocalSessionManager>;

/// Build the Cosmon MCP server as a **Streamable HTTP** tower service
/// (MCP 2025-03-26 transport), ready to be nested on an existing axum
/// router.
///
/// # Why a factory, not a router
///
/// The MCP surface is a *third projection* of the same `cosmon_state` /
/// `cosmon_core` core the REST routes already project (delib-20260709-943e,
/// torvalds). It therefore ships **inside** the host binary's single
/// listener rather than as a second process — the caller writes
/// `.nest_service("/mcp", cosmon_mcp::streamable_http_service())` behind
/// its own bearer/OIDC gate. This crate stays transport-only: it holds no
/// opinion about authentication, tenancy, or ingress, which live one layer
/// up in the host (cosmon is the Resource Server; the AS is elsewhere).
///
/// # Session model
///
/// Uses rmcp's [`LocalSessionManager`] purely for **transport framing**
/// (correlating a client's message stream via `Mcp-Session-Id`). Per the
/// panel's D1 reconciliation, this session must never cache authorization
/// or tenant state — those are re-derived per request by the host gate.
/// `stateful_mode` stays on (the default) so both Claude Desktop and
/// claude.ai's Streamable-HTTP clients are served; a session lost to a
/// redeploy is re-established rather than hard-404'd.
///
/// A fresh [`CosmonService`] is minted per logical session by the factory
/// closure via [`CosmonService::new_remote`].
///
/// # Tool-exposure partition (deny-remote)
///
/// This is a **remote** connector, so the factory mints
/// [`CosmonService::new_remote`] — the remote-safe tool partition — rather
/// than the full [`CosmonService::new`] set (the historical trusted-local
/// partition, now used only as the base `new_remote` filters down from).
/// The worker-internal and teardown verbs
/// ([`DENY_REMOTE_TOOLS`]) are absent from `tools/list` over
/// this transport, so they cannot be called or injection-targeted
/// (delib-20260709-943e M3, turing exploit #1 defense — see
/// [`DENY_REMOTE_TOOLS`]). Per-tool scope and
/// per-tenant resolution for the verbs that *remain* live one layer up in the
/// host adapter's gate.
#[must_use]
pub fn streamable_http_service() -> CosmonHttpService {
    StreamableHttpService::new(
        || Ok(CosmonService::new_remote()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}
