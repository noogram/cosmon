// SPDX-License-Identifier: AGPL-3.0-only

//! OAuth client-id **reverse discovery** — the server side of the
//! `cosmon-remote` login contract (delib-20260710-33b7 §C8, child C4
//! `task-20260710-909a`).
//!
//! # Why this module exists
//!
//! Forgejo generates each OAuth app's `client_id` **at runtime** and
//! hardcodes `aud = client_id` (`routers/web/auth/oauth.go:224/400`, no
//! setting). It has **no** Dynamic Client Registration (RFC 7591). So a
//! pre-provisioned client (`cosmon-remote`) cannot know its own
//! `client_id` ahead of time — it must **learn** it. This is not
//! self-registration; it is *reverse discovery*: cosmon-server, which
//! already knows the `client_id` (it crosses the container boundary in
//! the `forgejo-issuer.toml` handoff as the pinned `audiences`, see
//! [`crate::trust_bootstrap`]), **publishes** it.
//!
//! `client_id` is **public** — it needs integrity, not confidentiality
//! (buterin): swapping it swaps the audience, so the document is served
//! over the TLS/install channel and the client validates
//! `doc.issuer == expected_issuer` before trusting it. The
//! resource-server wall that makes the audience an isolation boundary is
//! separate and already enforced: [`crate::jwt::JwtVerifier::validate`]
//! checks the token `aud` against the **closed allowlist** of pinned
//! audiences ([`crate::jwt::JwksStore::audiences_for`]) — never a
//! wildcard. Presenting an `aud=A` token to a `B` resource fails there.
//!
//! # The wire document (`schema_version = 1`)
//!
//! `GET <host>/.well-known/cosmon-oauth-clients` returns one
//! cosmon-namespaced document (deliberately **not** an IANA well-known —
//! it does not squat `oauth-authorization-server`, which Forgejo itself
//! serves). It is **audience-keyed**, so both the CLI app (`aud=A`,
//! `cs-rpp-adapter`) and the MCP connector app (`aud=B`, `claude-web`)
//! live in the same document — the client filters to its own audience.
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "issuer": "https://forgejo.example.ts.net",
//!   "authorization_endpoint": "https://forgejo.example.ts.net/login/oauth/authorize",
//!   "token_endpoint": "https://forgejo.example.ts.net/login/oauth/access_token",
//!   "clients": [
//!     { "audience": "cs-rpp-adapter", "client_id": "…", "redirect_uris": ["http://127.0.0.1:7777/callback"], "scopes": ["cosmon:molecule:read"] },
//!     { "audience": "claude-web",     "client_id": "…", "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "scopes": ["cosmon:mcp"] }
//!   ]
//! }
//! ```
//!
//! # Evolution rules (semver for JSON — tolnay Q7)
//!
//! - `schema_version` is a monotonic integer. **Additive** fields (new
//!   object keys, new array elements) do **not** bump it; it bumps only
//!   when a field is removed or re-typed. A client reads it first and
//!   **fails closed** if `schema_version > max_supported` — it never
//!   silently misparses a newer shape. This module refuses to *serve* a
//!   registry file whose `schema_version` this build does not own
//!   ([`DiscoveryError::UnsupportedSchema`]), so a mis-provisioned file
//!   fails loud instead of shipping a half-understood document.
//! - Audience-keyed lookup, **never** positional: a client selects by
//!   matching `audience` ([`ClientRegistry::lookup`]), so reordering or
//!   inserting clients is non-breaking.
//! - Clients MUST ignore unknown fields (serde default — **no**
//!   `deny_unknown_fields`, which is a forward-compat trap on a wire
//!   type). The `#[non_exhaustive]` DTOs keep field additions
//!   non-breaking for downstream too.
//!
//! # Where the data comes from (server side)
//!
//! Two sources, highest precedence first:
//!
//! 1. `<state_dir>/security/oauth-clients.toml` — the **authoritative**
//!    registry, written by the provisioner (`task-20260710-d988`, the
//!    2-app Forgejo image) or the operator. It carries the full document
//!    including per-audience `redirect_uris`/`scopes`. Served verbatim
//!    after validation.
//! 2. Absent → **derived** from `<state_dir>/security/trusted-issuers.toml`
//!    ([`crate::jwks_fetch::TrustedIssuers`]). Since Forgejo hardcodes
//!    `aud = client_id`, every pinned `audience` *is* a `client_id`; the
//!    authorize/token endpoints are the Forgejo canonical paths joined to
//!    the issuer. This keeps the endpoint functional on today's converged
//!    handoff without waiting for the richer file; the explicit file
//!    overrides it whenever present.
//!
//! When neither yields a usable issuer the loader returns `Ok(None)` and
//! the route answers `404 discovery_unconfigured` — the surface is
//! discoverable-but-inert, never a lie.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::jwks_fetch::{TrustedIssuer, TrustedIssuers};

/// The `schema_version` this build emits and is willing to serve. A
/// registry file declaring any other version is refused fail-closed
/// ([`DiscoveryError::UnsupportedSchema`]) rather than served
/// half-understood.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Relative path of the authoritative registry file under `state_dir`.
pub const REGISTRY_REL_PATH: &str = "security/oauth-clients.toml";

/// Forgejo's canonical `OAuth2` authorization endpoint path, joined to the
/// issuer in the derive-from-trusted-issuers fallback
/// (`modules/setting/oauth2.go`). The explicit `oauth-clients.toml`
/// overrides this for non-Forgejo `IdPs`.
pub const FORGEJO_AUTHORIZE_PATH: &str = "/login/oauth/authorize";

/// Forgejo's canonical `OAuth2` token endpoint path (Forgejo names it
/// `access_token`, not `token`), joined to the issuer in the fallback.
pub const FORGEJO_TOKEN_PATH: &str = "/login/oauth/access_token";

/// The reverse-discovery document. **Audience-keyed**, covering every
/// provisioned OAuth app (A = CLI, B = MCP connector) in one payload.
///
/// This is the **server-side** representation: it derives `Serialize` to
/// *produce* the wire document. The canonical *client* mirror
/// (`cosmon-remote`, `task-20260710-2565`) is deserialize-only and
/// `#[non_exhaustive]`; the shared contract is the JSON shape and
/// `schema_version`, not the Rust type. `Deserialize` is derived here too
/// so an operator-authored `oauth-clients.toml` and the roundtrip tests
/// parse into the same type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ClientRegistry {
    /// Monotonic schema version. See [`CURRENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The OIDC issuer URL — matched byte-for-byte against the client's
    /// pinned `expected_issuer` before the document is trusted.
    pub issuer: String,
    /// `OAuth2` authorization endpoint the client drives the PKCE flow
    /// against.
    pub authorization_endpoint: String,
    /// `OAuth2` token endpoint the client exchanges the code / refreshes
    /// against.
    pub token_endpoint: String,
    /// One entry per provisioned OAuth app, keyed by `audience`.
    #[serde(default)]
    pub clients: Vec<OAuthClient>,
}

impl ClientRegistry {
    /// Audience-keyed lookup — the client's only sanctioned selector.
    /// Returns the first client whose `audience` matches exactly, or
    /// `None`. Never positional: reordering `clients` is non-breaking.
    ///
    /// # Examples
    ///
    /// ```
    /// use cosmon_rpp_adapter::oauth_discovery::{ClientRegistry, OAuthClient};
    ///
    /// let reg = ClientRegistry::new(
    ///     "https://idp.test".to_owned(),
    ///     "https://idp.test/login/oauth/authorize".to_owned(),
    ///     "https://idp.test/login/oauth/access_token".to_owned(),
    ///     vec![
    ///         OAuthClient::new("cs-rpp-adapter".to_owned(), "cs-rpp-adapter".to_owned()),
    ///         OAuthClient::new("claude-web".to_owned(), "claude-web".to_owned()),
    ///     ],
    /// );
    /// assert_eq!(reg.lookup("claude-web").unwrap().client_id, "claude-web");
    /// assert!(reg.lookup("unknown").is_none());
    /// ```
    #[must_use]
    pub fn lookup(&self, audience: &str) -> Option<&OAuthClient> {
        self.clients.iter().find(|c| c.audience == audience)
    }

    /// Construct a `schema_version = `[`CURRENT_SCHEMA_VERSION`] registry.
    /// The public constructor (the struct is `#[non_exhaustive]`, so it
    /// cannot be struct-literal-built from another crate).
    #[must_use]
    pub fn new(
        issuer: String,
        authorization_endpoint: String,
        token_endpoint: String,
        clients: Vec<OAuthClient>,
    ) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            issuer,
            authorization_endpoint,
            token_endpoint,
            clients,
        }
    }

    /// Fail-closed structural + version validation, run before a file is
    /// served (or after a roundtrip in tests).
    ///
    /// # Errors
    ///
    /// - [`DiscoveryError::UnsupportedSchema`] if `schema_version` is not
    ///   [`CURRENT_SCHEMA_VERSION`].
    /// - [`DiscoveryError::Degenerate`] if the issuer, either endpoint,
    ///   the `clients` list, or any client's `audience`/`client_id` is
    ///   empty — a document a client could not act on.
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(DiscoveryError::UnsupportedSchema {
                found: self.schema_version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        if self.issuer.trim().is_empty() {
            return Err(DiscoveryError::Degenerate {
                reason: "empty issuer".to_owned(),
            });
        }
        if self.authorization_endpoint.trim().is_empty() || self.token_endpoint.trim().is_empty() {
            return Err(DiscoveryError::Degenerate {
                reason: "empty authorization_endpoint or token_endpoint".to_owned(),
            });
        }
        if self.clients.is_empty() {
            return Err(DiscoveryError::Degenerate {
                reason: "no clients declared".to_owned(),
            });
        }
        for c in &self.clients {
            if c.audience.trim().is_empty() || c.client_id.trim().is_empty() {
                return Err(DiscoveryError::Degenerate {
                    reason: format!(
                        "client with empty audience or client_id (audience={:?})",
                        c.audience
                    ),
                });
            }
        }
        Ok(())
    }
}

/// One provisioned OAuth app, keyed by `audience`. For Forgejo,
/// `client_id == audience` (the `aud = client_id` hardcode), but the two
/// fields are carried separately so a non-Forgejo `IdP` whose `aud` differs
/// from its `client_id` is representable without a schema bump.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OAuthClient {
    /// The `aud` claim this app's tokens carry — the client's selector
    /// ([`ClientRegistry::lookup`]) and the RS-side isolation key.
    pub audience: String,
    /// The `OAuth2` `client_id` the client presents in the authorize/token
    /// requests. Public, integrity-checked, not secret.
    pub client_id: String,
    /// Redirect URIs the `IdP` provisioned for this app. The client
    /// asserts the loopback URI it is about to use is present before
    /// starting the flow (fail if the server provisioned a different
    /// port). Empty in the derived fallback.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// Scopes provisioned for this app, advisory to the client. Empty in
    /// the derived fallback.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl OAuthClient {
    /// Minimal constructor (`redirect_uris`/`scopes` empty). The struct
    /// is `#[non_exhaustive]`; this is the cross-crate builder.
    #[must_use]
    pub fn new(audience: String, client_id: String) -> Self {
        Self {
            audience,
            client_id,
            redirect_uris: Vec::new(),
            scopes: Vec::new(),
        }
    }

    /// Builder-style setter for the provisioned redirect URIs.
    #[must_use]
    pub fn with_redirect_uris(mut self, uris: Vec<String>) -> Self {
        self.redirect_uris = uris;
        self
    }

    /// Builder-style setter for the provisioned scopes.
    #[must_use]
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = scopes;
        self
    }
}

/// Errors that refuse to serve a discovery document. All are fail-closed:
/// a client sees a `404`/`500`, never a half-parsed or stale-versioned
/// registry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DiscoveryError {
    /// Filesystem error reading the registry or the trusted-issuers file.
    #[error("io reading {path}: {source}")]
    Io {
        /// Path the read failed on.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The registry file exists but is not valid TOML for [`ClientRegistry`].
    #[error("malformed oauth-clients registry at {path}: {reason} — fix or remove the file (fail-closed)")]
    Malformed {
        /// Offending registry file.
        path: PathBuf,
        /// Parse failure detail.
        reason: String,
    },
    /// The registry declares a `schema_version` this build does not own.
    /// Fail-closed on newer *and* older: serving a version we cannot
    /// vouch for is a silent-misparse hazard.
    #[error(
        "oauth-clients registry declares schema_version {found}, this build serves {supported} \
         (fail-closed) — re-provision the registry or upgrade the adapter"
    )]
    UnsupportedSchema {
        /// Version found in the file.
        found: u32,
        /// Version this build serves ([`CURRENT_SCHEMA_VERSION`]).
        supported: u32,
    },
    /// The registry parsed but is structurally unusable (empty issuer,
    /// endpoint, client list, or client id/audience).
    #[error("oauth-clients registry is structurally degenerate: {reason}")]
    Degenerate {
        /// Why the document is unusable.
        reason: String,
    },
}

/// Load the discovery document to serve from `state_dir`, resolving the
/// two sources in precedence order.
///
/// 1. `<state_dir>/`[`REGISTRY_REL_PATH`] — authoritative, served after
///    [`ClientRegistry::validate`].
/// 2. Absent → derived from `<state_dir>/security/trusted-issuers.toml`
///    ([`derive_from_trusted_issuers`]), **also** served only after
///    [`ClientRegistry::validate`].
///
/// Both branches funnel through the same `validate()` gate, so
/// *everything `load_registry` returns has passed validation* is one
/// enforced invariant — the route never serves an unvalidated document,
/// whatever its provenance. The derived branch is validated too even
/// though `derive_from_trusted_issuers` builds structurally-sound docs by
/// construction today: the invariant is enforced at the serving boundary,
/// not delegated to the derivation staying correct (review df19 F4).
///
/// Returns `Ok(None)` when neither yields a usable issuer — the route
/// then answers `404 discovery_unconfigured`.
///
/// # Errors
///
/// - [`DiscoveryError::Io`] on a filesystem error reading either file.
/// - [`DiscoveryError::Malformed`] if the explicit file is not valid TOML.
/// - [`DiscoveryError::UnsupportedSchema`] / [`DiscoveryError::Degenerate`]
///   if either the explicit file **or** the derived document fails
///   validation.
pub fn load_registry(state_dir: &Path) -> Result<Option<ClientRegistry>, DiscoveryError> {
    let explicit = state_dir.join(REGISTRY_REL_PATH);
    // Read directly rather than `exists()`-then-read: the two-step form is a
    // TOCTOU window. A `client_id` rotation replaces `oauth-clients.toml` by
    // `remove` + `rename`, so `exists()` can observe the file while the
    // subsequent `read_to_string` races the unlink and fails `NotFound` —
    // which the old code mapped to `DiscoveryError::Io` and the route to a
    // spurious `500 discovery_error`, even though the correct answer is to
    // fall through and derive. Treat `NotFound` as "no explicit file"
    // (identical to the absent case), and surface only genuine IO faults.
    match std::fs::read_to_string(&explicit) {
        Ok(text) => {
            let doc: ClientRegistry =
                toml::from_str(&text).map_err(|e| DiscoveryError::Malformed {
                    path: explicit.clone(),
                    reason: e.to_string(),
                })?;
            doc.validate()?;
            return Ok(Some(doc));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            // Fall through to derive from the trusted-issuers allowlist.
        }
        Err(source) => {
            return Err(DiscoveryError::Io {
                path: explicit,
                source,
            });
        }
    }

    let issuers = TrustedIssuers::load(state_dir).map_err(|source| DiscoveryError::Io {
        path: state_dir.join("security/trusted-issuers.toml"),
        source,
    })?;
    match derive_from_trusted_issuers(&issuers) {
        // Same `validate()` gate as the explicit branch: the derived
        // document is served only after it passes, so no code path can
        // hand the route an unvalidated registry (review df19 F4).
        Some(doc) => {
            doc.validate()?;
            Ok(Some(doc))
        }
        None => Ok(None),
    }
}

/// Derive a minimal discovery document from the converged trusted-issuers
/// allowlist. Picks the **first** issuer that carries a non-empty `iss`
/// and at least one non-empty audience — the concrete deployment is a
/// single Forgejo `IdP` with two OAuth apps (A + B), so this is
/// deterministic there. Multiple audience-bearing issuers (a genuine
/// multi-`IdP` federation) is beyond this document version; the operator
/// then provides an explicit `oauth-clients.toml`.
///
/// Each audience becomes a client with `client_id == audience` (Forgejo
/// `aud = client_id`); the authorize/token endpoints are the Forgejo
/// canonical paths joined to the issuer. `redirect_uris`/`scopes` are
/// empty — the explicit file is the way to publish those.
///
/// Returns `None` when no issuer carries a usable audience.
#[must_use]
pub fn derive_from_trusted_issuers(issuers: &TrustedIssuers) -> Option<ClientRegistry> {
    let is_audience_bearing = |i: &&TrustedIssuer| {
        !i.iss.trim().is_empty() && i.audiences.iter().any(|a| !a.trim().is_empty())
    };

    // A genuine multi-IdP federation carries more than one audience-bearing
    // issuer, but this schema version can only publish the *first* — every
    // other issuer's clients are silently dropped, so their tokens have no
    // published `client_id` and login breaks with no on-disk trace. Warn so
    // the operator knows the derivation is lossy and must supply an explicit
    // `oauth-clients.toml`. (delib-20260710-33b7 §C8, review df19.)
    let audience_bearing = issuers.issuers.iter().filter(is_audience_bearing).count();
    if audience_bearing > 1 {
        tracing::warn!(
            event = "oauth_discovery.derive",
            audience_bearing_issuers = audience_bearing,
            "multiple audience-bearing trusted issuers; derivation publishes only the first — \
             supply an explicit oauth-clients.toml to serve the others (their client login \
             will otherwise silently fail)"
        );
    }

    let issuer = issuers.issuers.iter().find(is_audience_bearing)?;

    let clients: Vec<OAuthClient> = issuer
        .audiences
        .iter()
        .filter(|a| !a.trim().is_empty())
        .map(|a| OAuthClient::new(a.clone(), a.clone()))
        .collect();

    Some(ClientRegistry::new(
        issuer.iss.clone(),
        join_endpoint(&issuer.iss, FORGEJO_AUTHORIZE_PATH),
        join_endpoint(&issuer.iss, FORGEJO_TOKEN_PATH),
        clients,
    ))
}

/// Join a Forgejo OAuth path onto an issuer URL, collapsing a trailing
/// slash so `http://h/git/` + `/login/oauth/authorize` yields a single
/// separator.
fn join_endpoint(issuer: &str, path: &str) -> String {
    format!("{}{}", issuer.trim_end_matches('/'), path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> ClientRegistry {
        ClientRegistry::new(
            "https://forgejo.example.ts.net".to_owned(),
            "https://forgejo.example.ts.net/login/oauth/authorize".to_owned(),
            "https://forgejo.example.ts.net/login/oauth/access_token".to_owned(),
            vec![
                OAuthClient::new("cs-rpp-adapter".to_owned(), "cid-a".to_owned())
                    .with_redirect_uris(vec!["http://127.0.0.1:7777/callback".to_owned()])
                    .with_scopes(vec!["cosmon:molecule:read".to_owned()]),
                OAuthClient::new("claude-web".to_owned(), "cid-b".to_owned())
                    .with_redirect_uris(vec!["https://claude.ai/api/mcp/auth_callback".to_owned()]),
            ],
        )
    }

    #[test]
    fn json_shape_matches_the_contract() {
        let v = serde_json::to_value(sample()).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["issuer"], "https://forgejo.example.ts.net");
        assert_eq!(
            v["authorization_endpoint"],
            "https://forgejo.example.ts.net/login/oauth/authorize"
        );
        assert_eq!(
            v["token_endpoint"],
            "https://forgejo.example.ts.net/login/oauth/access_token"
        );
        assert_eq!(v["clients"][0]["audience"], "cs-rpp-adapter");
        assert_eq!(v["clients"][0]["client_id"], "cid-a");
        assert_eq!(v["clients"][1]["audience"], "claude-web");
    }

    #[test]
    fn roundtrips_through_json() {
        let doc = sample();
        let json = serde_json::to_string(&doc).unwrap();
        let back: ClientRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn lookup_is_audience_keyed_not_positional() {
        let doc = sample();
        assert_eq!(doc.lookup("cs-rpp-adapter").unwrap().client_id, "cid-a");
        assert_eq!(doc.lookup("claude-web").unwrap().client_id, "cid-b");
        assert!(doc.lookup("nope").is_none());
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        // Forward-compat: a newer server adds a field; an older parser
        // must ignore it, not error (no deny_unknown_fields).
        let json = r#"{
            "schema_version": 1,
            "issuer": "https://idp.test",
            "authorization_endpoint": "https://idp.test/a",
            "token_endpoint": "https://idp.test/t",
            "future_top_level_field": {"nested": true},
            "clients": [
                {"audience": "a", "client_id": "cid", "client_type": "public+dpop"}
            ]
        }"#;
        let doc: ClientRegistry = serde_json::from_str(json).unwrap();
        assert_eq!(doc.clients[0].client_id, "cid");
    }

    #[test]
    fn client_secret_in_toml_never_reaches_serialized_json() {
        // Load-bearing security property (df19-F5): the served document is a
        // *public* client_id registry — a `client_secret` must NEVER cross the
        // unauthenticated `/.well-known/cosmon-oauth-clients` wire. Today the
        // property holds by type construction (`ClientRegistry`/`OAuthClient`
        // have no secret field, so `Serialize` cannot emit one), but nothing
        // pinned the *serve-time* direction. This test poisons the on-disk
        // registry with a `client_secret` at BOTH the top level and inside a
        // `[[clients]]` entry, loads it, serialises, and asserts the secret is
        // absent from the JSON bytes. A future careless edit — a literal
        // `client_secret` field "for convenience", or a
        // `#[serde(flatten)] extra: Map<String, Value>` passthrough — would
        // start echoing the secret and this test would fail loud.
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("oauth-clients.toml"),
            "schema_version = 1\n\
             issuer = \"https://explicit.test\"\n\
             authorization_endpoint = \"https://explicit.test/login/oauth/authorize\"\n\
             token_endpoint = \"https://explicit.test/login/oauth/access_token\"\n\
             client_secret = \"TOP_LEVEL_SHOULD_NEVER_LEAK\"\n\
             [[clients]]\n\
             audience = \"cs-rpp-adapter\"\n\
             client_id = \"explicit-cid\"\n\
             client_secret = \"PER_CLIENT_SHOULD_NEVER_LEAK\"\n\
             redirect_uris = [\"http://127.0.0.1:7777/callback\"]\n",
        )
        .unwrap();

        // The poisoned file still loads — unknown fields are tolerated on
        // deserialize (forward-compat), they are simply dropped, not an error.
        let doc = load_registry(dir.path()).unwrap().unwrap();
        assert_eq!(
            doc.lookup("cs-rpp-adapter").unwrap().client_id,
            "explicit-cid"
        );

        // The serialised wire document carries no trace of either secret —
        // neither the value nor the key that would smuggle it.
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            !json.contains("SHOULD_NEVER_LEAK"),
            "client_secret value leaked into served JSON: {json}"
        );
        assert!(
            !json.contains("client_secret"),
            "client_secret key leaked into served JSON: {json}"
        );
    }

    #[test]
    fn validate_rejects_newer_schema_version() {
        let mut doc = sample();
        doc.schema_version = 2;
        assert!(matches!(
            doc.validate(),
            Err(DiscoveryError::UnsupportedSchema {
                found: 2,
                supported: 1
            })
        ));
    }

    #[test]
    fn validate_rejects_degenerate_documents() {
        let mut empty_issuer = sample();
        empty_issuer.issuer = "  ".to_owned();
        assert!(matches!(
            empty_issuer.validate(),
            Err(DiscoveryError::Degenerate { .. })
        ));

        let no_clients = ClientRegistry::new(
            "https://idp.test".to_owned(),
            "https://idp.test/a".to_owned(),
            "https://idp.test/t".to_owned(),
            vec![],
        );
        assert!(matches!(
            no_clients.validate(),
            Err(DiscoveryError::Degenerate { .. })
        ));

        let empty_client_id = ClientRegistry::new(
            "https://idp.test".to_owned(),
            "https://idp.test/a".to_owned(),
            "https://idp.test/t".to_owned(),
            vec![OAuthClient::new("a".to_owned(), String::new())],
        );
        assert!(matches!(
            empty_client_id.validate(),
            Err(DiscoveryError::Degenerate { .. })
        ));
    }

    #[test]
    fn explicit_file_is_loaded_and_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        // Also write a trusted-issuers.toml so we can prove the explicit
        // file wins over the derive path.
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\niss = \"https://derived.test\"\naudiences = [\"derived-aud\"]\n",
        )
        .unwrap();
        std::fs::write(
            sec.join("oauth-clients.toml"),
            "schema_version = 1\n\
             issuer = \"https://explicit.test\"\n\
             authorization_endpoint = \"https://explicit.test/login/oauth/authorize\"\n\
             token_endpoint = \"https://explicit.test/login/oauth/access_token\"\n\
             [[clients]]\n\
             audience = \"cs-rpp-adapter\"\n\
             client_id = \"explicit-cid\"\n\
             redirect_uris = [\"http://127.0.0.1:7777/callback\"]\n",
        )
        .unwrap();

        let doc = load_registry(dir.path()).unwrap().unwrap();
        assert_eq!(doc.issuer, "https://explicit.test");
        assert_eq!(
            doc.lookup("cs-rpp-adapter").unwrap().client_id,
            "explicit-cid"
        );
    }

    #[test]
    fn explicit_file_with_bad_schema_is_refused_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("oauth-clients.toml"),
            "schema_version = 99\n\
             issuer = \"https://explicit.test\"\n\
             authorization_endpoint = \"https://explicit.test/a\"\n\
             token_endpoint = \"https://explicit.test/t\"\n\
             [[clients]]\n\
             audience = \"a\"\n\
             client_id = \"cid\"\n",
        )
        .unwrap();
        assert!(matches!(
            load_registry(dir.path()),
            Err(DiscoveryError::UnsupportedSchema { found: 99, .. })
        ));
    }

    #[test]
    fn malformed_explicit_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("oauth-clients.toml"),
            "this is not = valid toml [[[",
        )
        .unwrap();
        assert!(matches!(
            load_registry(dir.path()),
            Err(DiscoveryError::Malformed { .. })
        ));
    }

    #[test]
    fn derives_from_trusted_issuers_when_no_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\n\
             iss = \"http://host/git/\"\n\
             audiences = [\"cs-rpp-adapter\", \"claude-web\"]\n",
        )
        .unwrap();

        let doc = load_registry(dir.path()).unwrap().unwrap();
        assert_eq!(doc.issuer, "http://host/git/");
        // Trailing slash collapsed, Forgejo path joined.
        assert_eq!(
            doc.authorization_endpoint,
            "http://host/git/login/oauth/authorize"
        );
        assert_eq!(
            doc.token_endpoint,
            "http://host/git/login/oauth/access_token"
        );
        // aud == client_id (Forgejo hardcode).
        assert_eq!(
            doc.lookup("cs-rpp-adapter").unwrap().client_id,
            "cs-rpp-adapter"
        );
        assert_eq!(doc.lookup("claude-web").unwrap().client_id, "claude-web");
    }

    #[test]
    fn multi_issuer_derive_publishes_only_the_first_audience_bearing() {
        // A genuine multi-IdP federation: two issuers each carry an
        // audience. This schema version can publish only the first; the
        // second is silently dropped (a `tracing::warn!` flags the loss so
        // the operator supplies an explicit oauth-clients.toml). This test
        // pins the lossy behaviour the warning accompanies.
        let issuers = TrustedIssuers {
            issuers: vec![
                TrustedIssuer {
                    iss: "https://idp-a.test".to_owned(),
                    jwks_uri: None,
                    audiences: vec!["aud-a".to_owned()],
                },
                TrustedIssuer {
                    iss: "https://idp-b.test".to_owned(),
                    jwks_uri: None,
                    audiences: vec!["aud-b".to_owned()],
                },
            ],
        };

        let doc = derive_from_trusted_issuers(&issuers).unwrap();
        assert_eq!(doc.issuer, "https://idp-a.test");
        assert!(doc.lookup("aud-a").is_some());
        assert!(
            doc.lookup("aud-b").is_none(),
            "second issuer's audience must not leak into the first issuer's document"
        );
    }

    #[test]
    fn derive_skips_audienceless_issuer_and_uses_next() {
        // A non-audience-bearing issuer does not count toward the multi-IdP
        // warning and is skipped in favour of the first that carries one.
        let issuers = TrustedIssuers {
            issuers: vec![
                TrustedIssuer {
                    iss: "https://idp-empty.test".to_owned(),
                    jwks_uri: None,
                    audiences: vec![],
                },
                TrustedIssuer {
                    iss: "https://idp-real.test".to_owned(),
                    jwks_uri: None,
                    audiences: vec!["aud-real".to_owned()],
                },
            ],
        };

        let doc = derive_from_trusted_issuers(&issuers).unwrap();
        assert_eq!(doc.issuer, "https://idp-real.test");
        assert!(doc.lookup("aud-real").is_some());
    }

    #[test]
    fn returns_none_when_nothing_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("security")).unwrap();
        // No oauth-clients.toml, no trusted-issuers.toml → inert.
        assert!(load_registry(dir.path()).unwrap().is_none());
    }

    #[test]
    fn missing_explicit_file_falls_through_to_derive() {
        // Regression (review df19): the explicit registry is absent while a
        // trusted-issuers allowlist is present. The `NotFound` on the direct
        // read must fall through to derive — never a spurious `Io` error. This
        // pins the semantics the old `exists()`-then-read raced during a
        // `client_id` rotation (remove + rename of `oauth-clients.toml`).
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\niss = \"http://host/git\"\naudiences = [\"cs-rpp-adapter\"]\n",
        )
        .unwrap();
        // No oauth-clients.toml on disk at all.
        let doc = load_registry(dir.path()).unwrap().unwrap();
        assert_eq!(
            doc.lookup("cs-rpp-adapter").unwrap().client_id,
            "cs-rpp-adapter"
        );
    }

    #[test]
    fn genuine_io_fault_on_explicit_path_still_fails_closed() {
        // A non-`NotFound` IO fault (here: the registry path is a directory,
        // not a file) must still surface as `DiscoveryError::Io` so the route
        // answers `500` fail-closed. Only `NotFound` is swallowed as
        // fall-through; every other IO error is a genuine mis-provisioning.
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        // Make oauth-clients.toml a directory → read_to_string yields a
        // non-NotFound error kind.
        std::fs::create_dir_all(sec.join("oauth-clients.toml")).unwrap();
        assert!(matches!(
            load_registry(dir.path()),
            Err(DiscoveryError::Io { .. })
        ));
    }

    #[test]
    fn derived_document_is_served_only_after_validation() {
        // Invariant (review df19 F4): everything `load_registry` returns —
        // explicit *or* derived — has passed `ClientRegistry::validate`.
        // The explicit branch has always validated; this pins the derived
        // branch to the same gate so no code path can hand the route an
        // unvalidated document. `derive_from_trusted_issuers` builds sound
        // docs by construction, so the assertion is that re-validating the
        // exact object the route serves is `Ok` — the boundary enforces the
        // invariant regardless of the derivation staying correct.
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\niss = \"http://host/git\"\naudiences = [\"cs-rpp-adapter\"]\n",
        )
        .unwrap();

        // No oauth-clients.toml → the derived branch is exercised.
        let doc = load_registry(dir.path()).unwrap().unwrap();
        assert!(
            doc.validate().is_ok(),
            "load_registry must never return a document that fails validate()"
        );
    }

    #[test]
    fn returns_none_when_issuer_has_no_audience() {
        let dir = tempfile::tempdir().unwrap();
        let sec = dir.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\niss = \"http://host/git\"\naudiences = []\n",
        )
        .unwrap();
        assert!(load_registry(dir.path()).unwrap().is_none());
    }

    proptest! {
        /// Any structurally-valid registry roundtrips through JSON
        /// unchanged (the wire type is lossless).
        #[test]
        fn prop_registry_roundtrips(
            issuer in "https://[a-z]{1,12}\\.test",
            auds in prop::collection::vec("[a-z][a-z0-9-]{0,15}", 1..5),
        ) {
            let clients: Vec<OAuthClient> = auds
                .iter()
                .enumerate()
                .map(|(i, a)| OAuthClient::new(a.clone(), format!("cid-{i}")))
                .collect();
            let doc = ClientRegistry::new(
                issuer.clone(),
                format!("{issuer}/a"),
                format!("{issuer}/t"),
                clients,
            );
            let json = serde_json::to_string(&doc).unwrap();
            let back: ClientRegistry = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(doc, back);
        }

        /// Audience-keyed lookup returns the client whose audience matches
        /// and **never** a different audience's client_id — the isolation
        /// selector is exact, not fuzzy or positional.
        #[test]
        fn prop_lookup_never_crosses_audiences(
            auds in prop::collection::hash_set("[a-z][a-z0-9-]{0,15}", 2..6),
        ) {
            let auds: Vec<String> = auds.into_iter().collect();
            let clients: Vec<OAuthClient> = auds
                .iter()
                .enumerate()
                .map(|(i, a)| OAuthClient::new(a.clone(), format!("cid-{i}")))
                .collect();
            let doc = ClientRegistry::new(
                "https://idp.test".to_owned(),
                "https://idp.test/a".to_owned(),
                "https://idp.test/t".to_owned(),
                clients,
            );
            for (i, a) in auds.iter().enumerate() {
                let found = doc.lookup(a).unwrap();
                prop_assert_eq!(&found.client_id, &format!("cid-{i}"));
                prop_assert_eq!(&found.audience, a);
            }
        }
    }
}
