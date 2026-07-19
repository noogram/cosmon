// SPDX-License-Identifier: AGPL-3.0-only

//! HTTP route handlers — the §8p frozen surface (V0 = one route).
//!
//! See [ADR-080 §4](../../../docs/adr/080-remote-pilot-port-https-oidc.md)
//! and the freeze test at
//! `tests/api_surface_freeze.rs` for the canonical surface list.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{Html, IntoResponse, Json};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::AppState;

pub mod admin;
pub mod artifacts;
pub mod auth_me;
pub mod avatar;
pub mod dist;
pub mod events_stream;
pub mod logs_stream;
pub mod mcp;
pub mod molecules;
pub mod noyaux;
pub mod oauth_discovery;
pub mod observability;
pub mod quota;
pub mod result;
pub mod workers;

pub use observability::{diagnostics_handler, metrics_handler};

/// `GET /healthz` — liveness check, never gated by JWT. Excluded
/// from the §8p frozen surface (it is operational, not user-facing).
///
/// Body is minimal-plus-version:
///
/// * `version` — the adapter binary's version, aligned on the cosmon
///   release version (release `v0.2.1` ⇒ `"0.2.1"`). Before this field
///   the deployed version could only be deduced by behavioural
///   inference; the operator and external integrators (Tenant-Demo)
///   both need it. Since the shipped-binary alignment it is also the
///   number on the tarball the operator downloaded and the one
///   `cosmon-rpp-adapter --version` prints — one number, not three.
/// * `api_surface_version` — the number of `surface_added` events the
///   binary was compiled with ([`crate::surface_events::SURFACE_EVENTS`]).
///   The data file is append-only, so the counter is monotonic: a
///   client comparing its own compiled-in count can print an
///   informative stderr note on mismatch — never blocking, never
///   `/v2/`.
///
/// Both fields are additive and frozen-by-test
/// (`tests/observability.rs::healthz_stays_minimal_after_d820`); any
/// further growth belongs on `/diagnostics`.
pub async fn healthz(State(_state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "ok": true,
        "service": "cosmon-rpp-adapter",
        "version": env!("CARGO_PKG_VERSION"),
        "api_surface_version": crate::surface_events::SURFACE_EVENTS.len(),
    })))
}

/// `GET /` — minimal HTML landing page for visitors who hit the
/// adapter root in a browser. Lists the three useful entry points
/// (Forgejo at `/git/`, the liveness probe, and the molecule API).
/// Never gated by JWT — it carries no tenant data.
///
/// This handler is informational; replacing it with a redirect is a
/// per-deployment concern (e.g. a guest-slice can rewrite at the
/// Tailscale Serve layer if it prefers `/git/` as the default).
pub async fn root_landing(State(_state): State<Arc<AppState>>) -> Html<&'static str> {
    Html(ROOT_LANDING_HTML)
}

const ROOT_LANDING_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>cosmon-rpp-adapter</title>
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    body { font-family: ui-sans-serif, system-ui, sans-serif; max-width: 40rem;
           margin: 4rem auto; padding: 0 1rem; color: #222; line-height: 1.5; }
    h1 { font-size: 1.4rem; margin-bottom: 0.5rem; }
    code { background: #f3f3f3; padding: 0.1rem 0.3rem; border-radius: 3px; }
    li { margin: 0.3rem 0; }
    .muted { color: #666; font-size: 0.9rem; }
  </style>
</head>
<body>
  <h1>cosmon-rpp-adapter</h1>
  <p class="muted">Remote Pilot Port — §8j HTTPS+OIDC ingress (ADR-080).</p>
  <ul>
    <li><a href="/git/">/git/</a> — Forgejo (git provider + OIDC IdP)</li>
    <li><a href="/healthz">/healthz</a> — adapter liveness probe</li>
    <li><a href="/install.sh">/install.sh</a> — <code>curl &lt;host&gt;/install.sh | sh</code> (installs <code>just</code> + the justfile)</li>
    <li><a href="/dist/justfile">/dist/justfile</a> — recipes that drive the molecule API over HTTP</li>
    <li><a href="/dist/CLAUDE.md">/dist/CLAUDE.md</a> — recommended CLAUDE.md block for the tenant's agent (copy it into your global CLAUDE.md)</li>
    <li><code>GET /v1/molecules</code> — molecule API (Bearer JWT required)</li>
  </ul>
  <p class="muted">Sign in via the Forgejo Web UI (<code>/git/user/login</code>),
     then exchange the OAuth2 code for a JWT scoped to this instance.</p>
</body>
</html>
"#;

/// Placeholder substituted at serve time with the request's base URL
/// (scheme + host) so the served `install.sh`/`justfile` point back at
/// the host the tenant fetched them from.
const COSMON_HOST_PLACEHOLDER: &str = "__COSMON_HOST__";

/// `install.sh` template (cosmon-remote Phase 0). Embedded at compile
/// time; `__COSMON_HOST__` is rewritten per request.
const INSTALL_SH_TEMPLATE: &str = include_str!("../../assets/install.sh");

/// Tenant-facing justfile template (cosmon-remote Phase 0). Embedded at
/// compile time; `__COSMON_HOST__` is rewritten per request.
const DIST_JUSTFILE_TEMPLATE: &str = include_str!("../../assets/justfile");

/// Recommended CLAUDE.md block for the tenant's agent (avatar-surface
/// C2). Frozen prose: the
/// block encodes only what `--help` can never teach an agent (the
/// single-slit invariant, the two-badge order, what burns credit, the
/// named §8p refusals) and delegates everything else to discovery —
/// the short block IS the test that the CLI self-documents. Static:
/// no `__COSMON_HOST__`, the text is host-agnostic by design.
///
/// Auto-reference discipline (godel L1): the surface may *serve* the
/// doc that pilots the client agent — it must never expose a route
/// that *writes* it. GET only; the law lives one level above the
/// agent that obeys it.
const DIST_CLAUDE_MD: &str = include_str!("../../assets/CLAUDE.md");

/// Reconstruct the base URL (`scheme://host`) the request was sent to,
/// so served artefacts point back at the same host the tenant used.
///
/// Scheme resolution, in order:
///
/// 1. an explicit `X-Forwarded-Proto` (set by a TLS-terminating reverse
///    proxy — Tailscale Serve / nginx — in front of the adapter). A
///    chained proxy may send a comma-separated list (`"https, http"`);
///    the first token is the original client-facing scheme;
/// 2. otherwise `http` — the adapter *itself* always listens in
///    plaintext (`main.rs`: bare `TcpListener` + `axum::serve`, no
///    rustls), so absent a forwarding header the connection we actually
///    received is HTTP, and that is the honest scheme to template.
///
/// The `Host` header carries any non-default port verbatim.
///
/// # Why not "non-loopback ⇒ https"
///
/// The earlier heuristic guessed `https` for any non-loopback host. But
/// tenant deployments (AWS Tenant-Demo VM, a local VM) serve the adapter in
/// **clear HTTP**, with TLS — when present at all — terminated upstream
/// by Tailscale/nginx. Such a proxy is contractually required to
/// advertise itself via `X-Forwarded-Proto`. When it does, branch 1
/// honours it; when no TLS sits in front, there is no header and the
/// connection truly is plaintext. The old guess templated
/// `https://<host>/dist/binary/...`, so `curl install.sh | sh` fetched
/// the binary over HTTPS against a plaintext port and died with
/// `curl: (35) SSL wrong version number` — the Dave onboarding finding
/// (2026-06-05). Reporting the connection's real scheme fixes the
/// install without the tenant having to pass `COSMON_HOST=http://…` by
/// hand.
fn request_base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

/// `GET /install.sh` — the cosmon-remote bootstrap script.
/// Operational, unauthenticated, outside `/v1/` (same class as `/` and
/// `/healthz`, so it never counts toward the §8p frozen API surface).
/// The script carries no secret; authority lives entirely in the JWT
/// the tenant later mints and the Tailscale ACL.
///
/// Phase 1 extends the templating beyond
/// `__COSMON_HOST__`: when `install_templating` is configured, the
/// per-deployment four-tuple `(sub, aud, oidc_url, noyau)` lands in the
/// served script as a multi-line block of `cosmon-remote config set`
/// commands so the tenant's profile is persisted ready-to-use after
/// `install.sh | sh`. Empty fields are skipped server-side — no
/// `config set` line is emitted at all, avoiding the Phase 0
/// case-pattern bug where conditional `is_unset` matched the literal
/// placeholder strings.
pub async fn serve_install_sh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let base = request_base_url(&headers);
    let t = &state.install_templating;
    // `oidc_url` may itself contain `__COSMON_HOST__` (typical when the
    // OIDC mock is served at a path on the same host). Substitute that
    // first so a single value can be expressed relative to whichever
    // host the request landed on.
    let oidc_url = t.oidc_url.replace(COSMON_HOST_PLACEHOLDER, &base);
    let config_block = build_config_set_block(&t.sub, &t.aud, &oidc_url, &t.noyau);
    let body = INSTALL_SH_TEMPLATE
        .replace(COSMON_HOST_PLACEHOLDER, &base)
        .replace("__COSMON_CONFIG_SET_BLOCK__", &config_block);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
}

/// Build the multi-line shell block of `cosmon-remote config set`
/// commands. One line per non-empty deployment field; an empty block
/// (or a single comment line) when nothing is configured.
///
/// Single-quoted shell literals are used so the operator can put
/// special characters in the value without escape-hell — at the cost
/// of disallowing a literal `'` in any of the four fields. That
/// trade-off is intentional: the four values are URLs and identifiers,
/// none of which need an apostrophe. The guard below refuses to emit
/// a line if a `'` slips through, falling back to a warning comment.
fn build_config_set_block(sub: &str, aud: &str, oidc_url: &str, noyau: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let pairs = [
        ("sub", sub),
        ("aud", aud),
        ("oidc-url", oidc_url),
        ("noyau", noyau),
    ];
    let mut any = false;
    for (key, val) in pairs {
        if val.is_empty() {
            continue;
        }
        if val.contains('\'') {
            let _ = writeln!(
                out,
                "# skipped {key}: value contains a single quote, set it manually"
            );
            continue;
        }
        let _ = writeln!(
            out,
            "\"$BIN_PATH\" --profile \"$PROFILE\" config set {key} '{val}'"
        );
        any = true;
    }
    if !any {
        out.push_str("# (no per-deployment fields configured server-side — set sub/aud/oidc-url/noyau manually)\n");
    }
    out
}

/// `GET /dist/justfile` — the tenant-facing justfile that wraps the
/// molecule + auth-claude API in `just` recipes. Operational,
/// unauthenticated, outside `/v1/` (excluded from the §8p freeze). The
/// `ADAPTER_URL` is pinned to the serving host at fetch time.
pub async fn serve_dist_justfile(headers: HeaderMap) -> impl IntoResponse {
    let body = DIST_JUSTFILE_TEMPLATE.replace(COSMON_HOST_PLACEHOLDER, &request_base_url(&headers));
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

/// `GET /dist/CLAUDE.md` — the recommended CLAUDE.md block the tenant
/// copies into their agent's global CLAUDE.md. Operational,
/// unauthenticated, outside `/v1/` (excluded from the §8p freeze, same
/// class as `/install.sh` and `/dist/justfile`). Read-only by
/// construction: no route writes this document (godel L1 — the law
/// lives one level above the agent that obeys it).
pub async fn serve_dist_claude_md() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        DIST_CLAUDE_MD,
    )
}

/// `GET /health/backends` — IFBDD-prep diagnostic endpoint
/// (T-V1-IFBDD-METER). Returns a snapshot of the in-RAM
/// [`crate::BackendHealthRegistry`]. Like `/healthz` it sits outside
/// `/v1/` and never counts toward the §8p frozen API surface;
/// unauthenticated by design — this is operator diagnostics, not
/// tenant-facing data.
pub async fn backends_health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let backends = state.backend_health.snapshot();
    Json(json!({ "backends": backends }))
}
