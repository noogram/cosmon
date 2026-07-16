// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-oidc-mock` — V0 demo `IdP` for the §8j HTTPS+OIDC ingress
//! adapter (ADR-080).
//!
//! Two endpoints, no client registration, one embedded RSA-2048 key:
//!
//! - `GET /jwks.json` (alias: `GET /jwks`) — returns the JWK record for
//!   the embedded test key in RFC 7517 shape. Consumed by the
//!   `cosmon-rpp-adapter::JwksStore` (currently disk-pinned via
//!   `--write-jwks-out`; live HTTP fetch is V1+).
//! - `POST /issue` — mints a signed JWT. Query string:
//!   `?sub=<noyau-id>&scopes=<csv>&lifetime=<secs>&aud=<override>&jti=<id>`.
//!   The `scope` (RFC 8693 / OIDC singular, space-separated) and
//!   `scopes` (cosmon historical plural, comma-separated) parameters
//!   are both accepted; either spelling produces the same scope set
//!   on the token, mirroring the receive-side generosity already
//!   present in `cosmon-rpp-adapter::jwt`. No `client_id`, no
//!   `client_secret`, no refresh tokens — V0 only.
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
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use cosmon_oidc_testkit::{
    DEFAULT_AUDIENCE, DEFAULT_ISSUER, DEFAULT_KID, TEST_RSA_E_B64URL, TEST_RSA_N_B64URL,
    TEST_RSA_PRIVATE_PEM,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::{json, Value};

/// Default lifetime for JWTs issued by `POST /issue` when the caller
/// does not pass `?lifetime=<secs>`.
const DEFAULT_LIFETIME_SEC: u64 = 600;

/// Default bind address — picked to avoid collision with the
/// rpp-adapter (`8443`) and the Tailscale-served brew tap (`8765`).
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8444";

#[derive(Debug, Parser)]
#[command(
    version,
    about = "V0 demo IdP for cosmon-rpp-adapter (ADR-080) — JWKS + JWT issuance, embedded RSA test key."
)]
struct Cli {
    /// Bind address (default `0.0.0.0:8444`).
    #[arg(long, default_value = DEFAULT_BIND_ADDR)]
    bind: String,

    /// `iss` claim emitted in every JWT and pinned in the JWKS file.
    #[arg(long, default_value = DEFAULT_ISSUER)]
    issuer: String,

    /// Audience(s) accepted on `/issue` and written into the JWKS file.
    /// Repeat the flag to allow multiple audiences — one per nucleon
    /// binding the rpp-adapter needs to reach. The first entry is the
    /// default `aud` claim when `POST /issue` does not pass one.
    #[arg(long, default_values_t = vec![DEFAULT_AUDIENCE.to_owned()])]
    audience: Vec<String>,

    /// `kid` advertised in the JWKS and embedded in every JWT header.
    #[arg(long, default_value = DEFAULT_KID)]
    kid: String,

    /// Pre-stage the JWKS file at this path before serving. Format
    /// matches `cosmon-rpp-adapter::JwksStore::load`. Use this in
    /// docker-compose to feed the rpp-adapter's pinned-from-disk
    /// JWKS without an HTTP round-trip.
    #[arg(long)]
    write_jwks_out: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    issuer: String,
    audiences: Vec<String>,
    kid: String,
    encoding_key: Arc<EncodingKey>,
}

impl AppState {
    fn default_audience(&self) -> &str {
        self.audiences
            .first()
            .map_or(DEFAULT_AUDIENCE, String::as_str)
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("issuer", &self.issuer)
            .field("audiences", &self.audiences)
            .field("kid", &self.kid)
            .field("encoding_key", &"<opaque>")
            .finish()
    }
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
        write_jwks_file(path, &cli.issuer, &cli.audience, &cli.kid)?;
        tracing::info!(path = %path.display(), "pre-staged JWKS file for adapter");
    }

    let encoding_key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())
        .map_err(|e| anyhow::anyhow!("embedded RSA test key is malformed: {e}"))?;

    let state = AppState {
        issuer: cli.issuer.clone(),
        audiences: cli.audience.clone(),
        kid: cli.kid.clone(),
        encoding_key: Arc::new(encoding_key),
    };

    let app = Router::new()
        .route("/jwks.json", get(get_jwks))
        .route("/jwks", get(get_jwks))
        .route("/issue", post(post_issue))
        .route("/healthz", get(get_healthz))
        .with_state(state);

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

async fn get_healthz() -> &'static str {
    "ok"
}

async fn get_jwks(State(state): State<AppState>) -> Json<Value> {
    Json(jwks_body(&state.kid))
}

#[derive(Debug, Deserialize)]
struct IssueQuery {
    /// Subject — the `sub` claim. Required.
    sub: Option<String>,
    /// Optional audience override (defaults to the configured `--audience`).
    aud: Option<String>,
    /// Cosmon historical plural — comma-separated scopes
    /// (e.g. `?scopes=cosmon:molecule:read,cosmon:molecule:write`).
    /// Either `scopes` or `scope` may be passed; both are accepted
    /// for symmetry with the adapter's receive-side generosity
    /// (see `cosmon-rpp-adapter::jwt::RawClaims`).
    scopes: Option<String>,
    /// RFC 8693 / OIDC singular — space-separated scopes
    /// (e.g. `?scope=cosmon:molecule:read+cosmon:molecule:write`).
    /// Accepted alongside `scopes`; if both are passed, `scope`
    /// wins (the OIDC-spec spelling is canonical).
    scope: Option<String>,
    /// Token lifetime in seconds (defaults to 600).
    lifetime: Option<u64>,
    /// Optional `jti` override (defaults to `jti-<now>-<sub>`).
    jti: Option<String>,
}

/// Default scope set assigned to the issued JWT when neither
/// `?scope=` nor `?scopes=` is passed. Matches the V0 read-only
/// surface so a no-arg `POST /issue` produces a token sufficient for
/// `GET /v1/molecules/:id` without further configuration.
const DEFAULT_SCOPES: &str = "cosmon:molecule:read";

/// Parse the scope set from the two accepted spellings.
///
/// Precedence: `scope` (OIDC-spec singular, space-separated) wins
/// over `scopes` (cosmon plural, comma-separated). When both are
/// absent, fall back to [`DEFAULT_SCOPES`]. Empty / whitespace-only
/// segments are dropped so callers can pass `"a, b , ,c"` without
/// issuing the empty scope.
fn parse_scopes(scope: Option<&str>, scopes: Option<&str>) -> Vec<String> {
    if let Some(s) = scope {
        return s
            .split(' ')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    let raw = scopes.unwrap_or(DEFAULT_SCOPES);
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn post_issue(
    State(state): State<AppState>,
    Query(q): Query<IssueQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let sub = q.sub.ok_or((
        StatusCode::BAD_REQUEST,
        "`sub` query parameter is required".to_owned(),
    ))?;
    let aud = q.aud.unwrap_or_else(|| state.default_audience().to_owned());
    if !state.audiences.iter().any(|a| a == &aud) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "audience `{aud}` is not in the configured allow-list — pass --audience to cs-oidc-mock"
            ),
        ));
    }
    let scopes = parse_scopes(q.scope.as_deref(), q.scopes.as_deref());
    let lifetime = q.lifetime.unwrap_or(DEFAULT_LIFETIME_SEC);
    let now_s = chrono::Utc::now().timestamp();
    let jti = q.jti.unwrap_or_else(|| format!("jti-{now_s}-{sub}"));

    let claims = json!({
        "iss": state.issuer,
        "sub": sub,
        "aud": aud,
        "iat": now_s,
        "exp": now_s + i64::try_from(lifetime).unwrap_or(0),
        "jti": jti,
        "scopes": scopes,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(state.kid.clone());
    let token = encode(&header, &claims, &state.encoding_key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("encode: {e}")))?;

    Ok(Json(json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": lifetime,
        "jti": jti,
        "iss": state.issuer,
        "aud": aud,
        "scopes": scopes,
    })))
}

/// The single JWK record for the embedded RSA-2048 test key, in
/// RFC 7517 shape.
///
/// **This is the one place the key fields live.** Both the live `/jwks`
/// response ([`jwks_body`]) and the disk projection ([`write_jwks_file`])
/// wrap this record; they differ *only* in their envelope (the disk file
/// adds `iss` / `audiences`), never in the key itself. Before this
/// collapse the two builders had already drifted — the wire emitted
/// `"kty": "RSA"` and the disk file emitted `"kty": "rsa"`. RFC 7518 §6.1
/// is case-sensitive, so a strict relying party accepts the former and
/// rejects the latter. Writing `"RSA"` exactly once makes that drift
/// unrepresentable.
fn jwk_record(kid: &str) -> Value {
    json!({
        "kid": kid,
        "alg": "RS256",
        // RFC 7518 §6.1 — case-sensitive. MUST be uppercase `RSA`.
        "kty": "RSA",
        "use": "sig",
        "n": TEST_RSA_N_B64URL,
        "e": TEST_RSA_E_B64URL,
    })
}

fn jwks_body(kid: &str) -> Value {
    json!({ "keys": [jwk_record(kid)] })
}

fn write_jwks_file(
    path: &std::path::Path,
    issuer: &str,
    audiences: &[String],
    kid: &str,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = json!({
        "iss": issuer,
        "audiences": audiences,
        "keys": [jwk_record(kid)],
    });
    std::fs::write(path, serde_json::to_vec_pretty(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scopes_plural_csv_matches_singular_space_separated() {
        let plural = parse_scopes(None, Some("cosmon:molecule:read,cosmon:molecule:write"));
        let singular = parse_scopes(Some("cosmon:molecule:read cosmon:molecule:write"), None);
        assert_eq!(plural, singular);
        assert_eq!(
            plural,
            vec![
                "cosmon:molecule:read".to_owned(),
                "cosmon:molecule:write".to_owned()
            ]
        );
    }

    #[test]
    fn parse_scopes_singular_wins_over_plural_when_both_present() {
        // OIDC-spec spelling is canonical when both are given.
        let scopes = parse_scopes(Some("a b"), Some("c,d"));
        assert_eq!(scopes, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn parse_scopes_default_when_neither_param_present() {
        let scopes = parse_scopes(None, None);
        assert_eq!(scopes, vec!["cosmon:molecule:read".to_owned()]);
    }

    #[test]
    fn jwk_record_emits_uppercase_rsa_kty() {
        // RFC 7518 §6.1 is case-sensitive; a strict relying party
        // rejects `"rsa"`. The single constructor must always say `RSA`.
        assert_eq!(jwk_record("kid-1")["kty"], "RSA");
    }

    #[test]
    fn wire_and_disk_jwks_carry_an_identical_key_record() {
        // The whole point of the collapse: the live `/jwks` response and
        // the disk projection differ only in their envelope, never in the
        // key record. Drift between `RSA`/`rsa` is now unrepresentable.
        let wire = jwks_body("kid-1");
        let dir = std::env::temp_dir().join(format!("cs-oidc-mock-test-{}", std::process::id()));
        let path = dir.join("oidc-mock.json");
        write_jwks_file(&path, "http://idp.test", &["aud-1".to_owned()], "kid-1")
            .expect("write jwks file");
        let disk: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read jwks file")).expect("parse");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(wire["keys"][0], disk["keys"][0]);
        // …and the disk envelope carries the extra fields the wire omits.
        assert_eq!(disk["iss"], "http://idp.test");
        assert_eq!(disk["audiences"][0], "aud-1");
        assert!(wire.get("iss").is_none());
    }

    #[test]
    fn parse_scopes_drops_empty_segments_and_trims_whitespace() {
        // Plural CSV: stray comma + spaces.
        let plural = parse_scopes(None, Some("a, b , ,c"));
        assert_eq!(plural, vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        // Singular SSV: double space.
        let singular = parse_scopes(Some("a  b   c"), None);
        assert_eq!(
            singular,
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }
}
