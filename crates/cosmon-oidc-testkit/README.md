# cosmon-oidc-testkit

Test infrastructure for the §8j HTTPS+OIDC ingress adapters defined by
[ADR-080]. The crate ships three primitives:

1. **[`OidcMock`]** — an in-memory `IdP`. Spins up a tokio task hosting
   an `axum` JWKS endpoint at a random local port, and provides
   [`OidcMock::issue_jwt`] for signing tokens with the embedded RSA-2048
   test key.
2. **[`tenant_workspace`]** / **[`TenantWorkspaces`]** — TempDir
   factories that lay out the per-noyau `/srv/cosmon/<noyau>/.cosmon/state/`
   tree expected by the subprocess envelope (ADR-080 §3.5 clause (e)).
3. **`fake-cs`** — a minimal stand-in binary that mimics
   `cs --json observe <id>`. Bundled as `[[bin]]` so the path is
   reachable via `cosmon_oidc_testkit::fake_cs_path()` in tests.

The crate is **dev-only**. Adapters consume it through
`[dev-dependencies]`; nothing from this crate ever links into a
production binary.

## Why this exists

Concern 5 of [`delib-20260503-8c2b`] (innocuité) hinges on a single
invariant: a JWT scoped to `noyau=A` cannot — through any path the RPP
exposes — read state owned by `noyau=B`. The structural defence is the
per-tenant subprocess `cwd`: a `cs` invocation in `/srv/cosmon/A/` cannot
resolve a molecule that lives under `/srv/cosmon/B/.cosmon/state/`. This
crate makes that test cheap to write, fast to run, and impossible to
forget.

## Quickstart

```rust,no_run
use cosmon_oidc_testkit::{OidcMock, TenantWorkspaces, fake_cs_path};

#[tokio::test]
async fn jwt_for_noyau_a_cannot_read_noyau_b() {
    // 1. Spin up a mock IdP.
    let oidc = OidcMock::start().await;

    // 2. Build a two-tenant galaxies/ tree.
    let mut workspaces = TenantWorkspaces::new();
    let tenant_a = workspaces.add("a");
    let tenant_b = workspaces.add("b");

    // 3. Plant identical molecule ids in both tenants.
    tenant_a.insert_molecule("task-secret",
        &serde_json::json!({"owner": "noyau-a"})).unwrap();
    tenant_b.insert_molecule("task-secret",
        &serde_json::json!({"owner": "noyau-b"})).unwrap();

    // 4. Issue a JWT for sub-a (mapped to noyau-a in the adapter's
    //    NucleonMap).
    let jwt_a = oidc.issue_jwt("sub-a", &["cosmon:molecule:read"]);

    // 5. Wire the adapter so its subprocess envelope shells out to
    //    `fake-cs` from this crate. The cwd is fixed by the noyau
    //    resolved from sub-a — therefore the request reads from
    //    galaxies/a/, not galaxies/b/.
    //
    // ... build cosmon_rpp_adapter::AppState with:
    //   cs_path = fake_cs_path(),
    //   galaxies_root = workspaces.galaxies_root().to_owned(),
    //   jwks loaded from oidc.write_jwks_file(state_dir),
    //   nucleon_map binding (sub-a, noyau-a)
    //
    // The response payload contains owner=noyau-a, never noyau-b.
}
```

A complete worked example lives at
`crates/cosmon-rpp-adapter/tests/tenant_isolation_test.rs`.

## What the testkit deliberately does **not** do

- **No HTTP fetch of JWKS at adapter boot.** V0 of `cosmon-rpp-adapter`
  loads JWKS from disk only ([`OidcMock::write_jwks_file`] projects the
  in-memory key into that on-disk format). The HTTP endpoint is for
  V1+ adapters and tests that exercise discovery directly.
- **No DPoP / par / pkce.** ADR-080 §6.5 lists DPoP as Posture::Active
  V1+ work. The mock issues bearer tokens only.
- **No live token rotation.** The embedded key is committed in
  plaintext. Rotation tests use successive `OidcMock` instances with
  different `kid` values.

## Endpoints

The runnable `cs-oidc-mock` binary exposes four HTTP endpoints. None
of them require client registration; the embedded RSA-2048 key signs
every JWT, and the audience allow-list is fixed at boot via
`--audience` (repeatable).

### `GET /jwks.json` (alias: `GET /jwks`)

Returns the JWK record for the embedded test key in RFC 7517 shape.
Consumed by `cosmon_rpp_adapter::JwksStore` (currently disk-pinned via
`--write-jwks-out`; live HTTP fetch is V1+).

No query parameters. Response: `application/json`, status 200.

### `POST /issue`

Mints a signed RS256 JWT. No body — every parameter rides the query
string. The handler is generous on shape: pass either the OIDC-spec
singular (`scope`, space-separated) or the cosmon historical plural
(`scopes`, comma-separated) — both produce the same scope set.

| Param | Required | Default | Notes |
|---|---|---|---|
| `sub` | yes | — | Subject claim (e.g. `"sub-tenant-demo"`). |
| `aud` | no | first `--audience` from the CLI | Must be in the configured allow-list; otherwise 400. |
| `scope` | no¹ | `cosmon:molecule:read` | RFC 8693 / OIDC singular, space-separated. |
| `scopes` | no¹ | `cosmon:molecule:read` | Cosmon historical plural, comma-separated. |
| `lifetime` | no | `600` | JWT lifetime in seconds (`exp - iat`). |
| `jti` | no | `jti-<unix>-<sub>` | Override the `jti` claim. |

¹ Either `scope` or `scopes` may be passed (not both required). When
both are present, `scope` wins — the OIDC-spec spelling is canonical.

Response (200): `{"access_token": "...", "token_type": "Bearer",
"expires_in": <secs>, "jti": "...", "iss": "...", "aud": "...",
"scopes": ["..."]}`.

### `GET /healthz`

Liveness probe. Returns the literal string `ok` with status 200. No
authentication, no body. Used by docker-compose's healthcheck and any
external probe (ALB, k8s readiness).

[ADR-080]: ../../docs/adr/080-remote-pilot-port-https-oidc.md
[`delib-20260503-8c2b`]: ../../docs/architectural-invariants.md
[`OidcMock`]: src/mock.rs
[`OidcMock::issue_jwt`]: src/mock.rs
[`OidcMock::write_jwks_file`]: src/mock.rs
[`tenant_workspace`]: src/workspace.rs
[`TenantWorkspaces`]: src/workspace.rs
