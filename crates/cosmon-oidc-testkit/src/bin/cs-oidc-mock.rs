// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-oidc-mock` — V0 demo `IdP` for the §8j HTTPS+OIDC ingress
//! adapter (ADR-080), and the login `IdP` for the container smoke
//! (GitHub issue #53).
//!
//! This binary is a `main` around [`cosmon_oidc_testkit::idp::router`]:
//! it parses flags, optionally pre-stages the JWKS file, binds a socket
//! and serves. **Every handler lives in the library**, so the router a
//! `cosmon-remote` test serves in-process is the same code this binary
//! serves in a container — see that module's header for why.
//!
//! The surface, in the order a client meets it:
//!
//! - `GET /.well-known/openid-configuration` — RFC 8414 discovery.
//!   Advertises `/authorize`, `/token`, `/jwks.json`, and `S256` as the
//!   only PKCE method.
//! - `GET /authorize` — auto-approves (no consent UI) and 302s back to
//!   the requested `redirect_uri` with `code` and the caller's `state`.
//! - `POST /token` — exchanges a single-use, 60-second code for a signed
//!   `{access_token, id_token}` pair, refusing any request whose PKCE
//!   verifier does not match the challenge presented at `/authorize`.
//! - `GET /jwks.json` (alias: `GET /jwks`) — the JWK record for the
//!   embedded test key in RFC 7517 shape. Consumed by the
//!   `cosmon-rpp-adapter::JwksStore` (currently disk-pinned via
//!   `--write-jwks-out`; live HTTP fetch is V1+).
//! - `POST /issue` — the side-channel minter, no OAuth dance. Query
//!   string: `?sub=<noyau-id>&scopes=<csv>&lifetime=<secs>&aud=<override>&jti=<id>`.
//!   Both the `scope` (RFC 8693 / OIDC singular, space-separated) and
//!   `scopes` (cosmon historical plural, comma-separated) spellings are
//!   accepted, mirroring the receive-side generosity already present in
//!   `cosmon-rpp-adapter::jwt`.
//! - `GET /healthz` — liveness.
//!
//! **`--issuer` must be reachable.** A client walking the login flow
//! fetches `<issuer>/.well-known/openid-configuration`, so in a compose
//! stack the issuer is the service URL (`http://oidc-mock:8444`), not a
//! decorative label.
//!
//! **DEMO ONLY.** The RSA private key shipped with this crate is
//! committed in plaintext (`assets/test_rsa_private.pem`) so the
//! resulting tokens are trivially forgeable by anyone with the source.
//! Do NOT deploy this binary outside an embargoed test loop. Replace
//! with Keycloak self-hosted (or equivalent) for V1+.

#![forbid(unsafe_code)]
#![allow(clippy::missing_docs_in_private_items)]

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use cosmon_oidc_testkit::idp::{self, DEFAULT_LIFETIME_SEC, DEFAULT_SUBJECT};
use cosmon_oidc_testkit::{IdpConfig, DEFAULT_AUDIENCE, DEFAULT_ISSUER, DEFAULT_KID};

/// Default bind address — picked to avoid collision with the
/// rpp-adapter (`8443`) and the Tailscale-served brew tap (`8765`).
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8444";

#[derive(Debug, Parser)]
#[command(
    // clap defaults `name` to CARGO_PKG_NAME, so this binary announced itself
    // as `cosmon-oidc-testkit 0.2.1` — a name the user never typed and cannot
    // find in the tarball. Pin it to the shipped binary name (the one in
    // packaging/shipped-binaries.txt), same reason the versions are aligned:
    // what the user reads must be what the user ran.
    name = "cs-oidc-mock",
    version,
    about = "V0 demo IdP for cosmon-rpp-adapter (ADR-080) — OIDC discovery, authorization-code + PKCE login, JWKS, JWT issuance. Embedded RSA test key."
)]
struct Cli {
    /// Bind address (default `0.0.0.0:8444`).
    #[arg(long, default_value = DEFAULT_BIND_ADDR)]
    bind: String,

    /// `iss` claim emitted in every JWT and pinned in the JWKS file.
    /// Also the base URL of the discovery document, so a login client
    /// must be able to reach it: in docker-compose that is the service
    /// URL (`http://oidc-mock:8444`), not a placeholder.
    #[arg(long, default_value = DEFAULT_ISSUER)]
    issuer: String,

    /// Audience(s) accepted on `/issue` and `/authorize`, and written
    /// into the JWKS file. Repeat the flag to allow multiple audiences —
    /// one per nucleon binding the rpp-adapter needs to reach. The first
    /// entry is the default `aud` claim when `POST /issue` does not pass
    /// one. Because cosmon pins `aud == client_id`, this is also the
    /// allow-list `/authorize` checks its `client_id` against.
    #[arg(long, default_values_t = vec![DEFAULT_AUDIENCE.to_owned()])]
    audience: Vec<String>,

    /// `kid` advertised in the JWKS and embedded in every JWT header.
    #[arg(long, default_value = DEFAULT_KID)]
    kid: String,

    /// The `sub` `/authorize` signs in as when the request carries no
    /// `login_hint`. This `IdP` auto-approves, so there is no consent
    /// screen to pick an identity at.
    #[arg(long, default_value = DEFAULT_SUBJECT)]
    subject: String,

    /// Lifetime, in seconds, of the JWTs minted by `/token` and by
    /// `/issue` when the caller asks for none.
    #[arg(long, default_value_t = DEFAULT_LIFETIME_SEC)]
    lifetime: u64,

    /// Pre-stage the JWKS file at this path before serving. Format
    /// matches `cosmon-rpp-adapter::JwksStore::load`. Use this in
    /// docker-compose to feed the rpp-adapter's pinned-from-disk
    /// JWKS without an HTTP round-trip.
    #[arg(long)]
    write_jwks_out: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    if cli.audience.is_empty() {
        anyhow::bail!("at least one --audience is required");
    }

    if let Some(path) = cli.write_jwks_out.as_ref() {
        idp::write_jwks_file(path, &cli.issuer, &cli.audience, &cli.kid)?;
        tracing::info!(path = %path.display(), "pre-staged JWKS file for adapter");
    }

    // The sabotage seam (`corrupt_code_challenge`) is deliberately not a
    // flag: it exists to let a Rust test show its PKCE check red, and a
    // running IdP that can be told to break PKCE from the command line is
    // a footgun with no operator use.
    let app = idp::router(IdpConfig {
        issuer: cli.issuer.clone(),
        audiences: cli.audience.clone(),
        kid: cli.kid.clone(),
        default_lifetime_secs: cli.lifetime,
        subject: cli.subject.clone(),
        corrupt_code_challenge: false,
    })?;

    let addr: SocketAddr = cli
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --bind address `{}`: {e}", cli.bind))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::warn!(
        addr = %addr,
        issuer = %cli.issuer,
        audiences = ?cli.audience,
        "cs-oidc-mock listening — DEMO ONLY, embedded test key, NOT for production",
    );
    axum::serve(listener, app).await?;
    Ok(())
}
