# ADR-141 — Auto-provisioning images (Forgejo + cosmon-server), no external init script

- **Status:** Accepted
- **Date:** 2026-07-03
- **Context molecules:** task-20260703-56d5 (this ADR + implementation),
  delib-20260701-8a19 round-2 blockers (auth-B1: merge-preserving seed,
  fail-closed parse-back), delib-20260702-93ac round-3 re-review,
  the 2026-07-02 parc incident (`admin` is a Forgejo *reserved* username —
  `sign_up` silently refused it, surfacing later as an incomprehensible 401).
- **Supersedes:** the external `bootstrap-forgejo-init.sh` gesture (smithy)
  and the `cosmon-seed` init-container of the v3.0 recipe (absorbed).

## Context

Tenant-Demo asked to abandon the external `bootstrap-forgejo-init.sh` (they are
not autonomous to modify it, local/parc behaviours diverge, and some actions
are not automatable from a separate init-container) in favour of images that
**configure themselves at boot**, idempotently, under the LEAN.md working
agreement: read-only rootfs, writes to volumes/tmpfs only, one
`volumes-<CONTAINER>.csv` per container, global config via `rpp.toml`,
per-galaxy config via API only, minimal Compose handed to Tenant-Demo.

The provisioning problem has three parts, split A/B/C along ownership lines.

## Decision

### A — Forgejo custom image owns the IdP side

`dist/avatar-tenant-demo/forgejo/` builds a thin layer over the upstream
`forgejo:<major>-rootless` image. A wrapper entrypoint starts a background
provisioner and `exec`s the untouched upstream entrypoint (Forgejo itself is
byte-identical). The provisioner, **inside the container**:

1. waits until the local Forgejo serves a signing key
   (`/login/oauth/keys` contains a `kid`);
2. creates the admin account **via the internal Forgejo CLI**
   (`forgejo admin user create --admin`) — the REST API cannot create the
   first admin. The username is guarded against Forgejo's reserved names
   (`admin` is reserved; default is `cosmon`) and the guard fails **loudly**;
3. generates the admin password once and persists it at
   `<data>/.cosmon-provision/admin-pass` (mode 600, on the `forgejo-data`
   volume) — idempotence across reboots;
4. creates the OAuth2 application (default name `cs-rpp-adapter`,
   confidential client) via the REST API authenticated with that admin, or
   re-reads its `client_id` if it already exists (idempotent by name);
5. derives `iss` from the OIDC discovery document (authoritative when
   `ROOT_URL` is fixed; an internal-looking issuer is **refused**, never
   pinned), derives `sub` from `/api/v1/user` (never assumed to be `1`);
6. writes the **handoff file** (contract C) atomically.

Ownership: everything Forgejo-side (admin, password, OAuth2 app, client_id)
belongs to image A. Nothing else creates Forgejo state.

### B — cosmon-server owns its trust state (trust bootstrap at boot)

The adapter gains a boot-time **trust bootstrap** step
(`cosmon-rpp-adapter::trust_bootstrap`), replacing the `cosmon-seed`
init-container. Before arming the JWKS fetch, the server converges

- `<state_dir>/security/trusted-issuers.toml` (authn allowlist,
  canon `jwks_fetch.rs`: `[[issuer]]` with `iss` / `jwks_uri` / `audiences`),
- `<state_dir>/nucleons/<id>/oidc-identity.toml` (authz binding, rendered by
  the same audited `nucleon_map` renderer as the operator path — one writer
  schema),

from three declaration sources (highest precedence first):

1. **handoff files** `*.toml` in the configured `handoff_dir` (contract C);
2. the **env trio** `TRUSTED_ISS` / `TRUSTED_JWKS_URI` / `TRUSTED_AUDIENCES`
   (compat with the v3.0 seed contract, for external-IdP deployments);
3. static `[[trust_bootstrap.issuer]]` entries in `rpp.toml`.

Convergence semantics carry the auth-B1 round-2 fixes forward, in Rust:

- **merge-preserving**: a declared entry is matched by `iss` and rewritten on
  drift; every *foreign* `[[issuer]]` block on the volume is preserved
  verbatim (a legitimately-enriched multi-issuer allowlist survives reboots);
- **fail-closed parse-back**: the merged content is parsed with the *same*
  serde shape the fetcher consumes, plus shape checks (≥1 issuer, non-empty
  `iss`, ≥1 non-empty audience) **before** it atomically replaces the file;
  on failure the server refuses to boot (restart policy retries);
- **reset gesture**: `TRUSTED_FORCE=1` rewrites the whole allowlist from the
  declaration (cross-tenant volume reuse, corrupt file recovery);
- **bounded wait**: when a `handoff_dir` is configured with
  `handoff_wait_secs > 0` and nothing is declared or on the volume yet, boot
  polls for the handoff and, if it never lands, **exits non-zero** — under
  `restart: unless-stopped` this is a self-healing crash-loop, not a silent
  deny-all. No `depends_on` ordering is needed (survives the engine-level
  restart-after-host-reboot where compose ordering does not apply).

The operator one-shot `cosmon-rpp-adapter trust converge` runs the same code
and exits — the validation bench and hosts use it exactly like the old seed
entrypoint.

Ownership: `trusted-issuers.toml` and the bindings have exactly **one
writer** (the server itself, plus the pre-existing audited provisioner API);
the seed container and its root chown pass are deleted. Volume ownership is
covered by Docker's copy-ownership-on-first-mount of the image's
`/cosmon/.cosmon/state` (pre-created `1000:999` in the image) or by the
parc's out-of-band provisioning, as before.

### C — client_id transfer via a dedicated handoff volume, not env

Inter-container transfer of the OAuth2 `client_id` (and issuer facts) uses a
**small dedicated named volume** (`provision-handoff`):

- mounted **rw** in Forgejo at `/handoff` — image A is the only writer;
- mounted **ro** in cosmon-server at `/cosmon/handoff` — image B can only
  read it (a compromised server cannot rewrite its own trust declaration).

No environment variables cross the container boundary; no API call is needed
(an unauthenticated config route on the server would be a hole in the §8p
frozen surface, and any authenticated route has a bootstrap circularity).

Handoff file contract (`/handoff/forgejo-issuer.toml`, schema
`cosmon-issuer-handoff/v1`, written atomically via rename):

```toml
schema = "cosmon-issuer-handoff/v1"

[issuer]                # → merged into security/trusted-issuers.toml
iss       = "https://<external-host>/git"   # external, discovery-derived
jwks_uri  = "http://forgejo:3000/login/oauth/keys"  # internal fetch target
audiences = ["<client_id>"]

[binding]               # → nucleons/<nucleon_id>/oidc-identity.toml
noyau      = "tenant-demo-sandbox"
nucleon_id = "cosmon-forgejo"
sub        = "1"                  # real uid from /api/v1/user, never assumed
audience   = "<client_id_A>"      # defaults to audiences[0] when omitted
# scopes optional; defaults to molecule:read/write, worker:spawn, artifact:read

# Additional bindings — the two-audience provisioner (task-20260710-6ffc).
# One self-provisioning gesture publishes N habilitations, one per audience
# (aud=A = the CLI/API app cs-rpp-adapter, aud=B = the MCP connector claude-web).
# Each [[bindings]] entry MUST carry a DISTINCT effective nucleon_id (the file is
# keyed by nucleon_id, not by audience) — a collision is refused fail-closed. A
# token carrying aud=A can only ever resolve binding A: audience isolation is
# structural. Additive + back-compat: a server predating this field seals only
# the legacy [binding] above.
[[bindings]]            # → nucleons/cosmon-forgejo-mcp/oidc-identity.toml
noyau      = "tenant-demo-sandbox"
nucleon_id = "cosmon-forgejo-mcp"
sub        = "1"
audience   = "<client_id_B>"
# scopes optional; tighten to harden the third-party-held MCP token (turing-T1)
```

The `audiences` list is the CLOSED audience allowlist — exactly the provisioned
client_ids (A, and B when the MCP app is created), never a wildcard.

## Idempotence across reboots and stale volumes

| Scenario | A (forgejo) | B (cosmon-server) |
|---|---|---|
| Fresh volumes, first boot | create admin + app, write handoff | wait for handoff, converge, boot |
| Reboot (volumes intact) | auth with persisted pass, re-derive client_id, handoff rewritten only on drift | converge is a byte-identical no-op |
| `forgejo-data` reused, our admin present | reuse | own entry converged (client_id rotation covered) |
| `forgejo-data` reused, foreign users, no persisted pass | **fail loudly** (reset required — never guess) | unchanged (fail-closed: no new declaration) |
| `rpp-state` reused across tenants | n/a | stale foreign issuers persist by design; `TRUSTED_FORCE=1` is the documented reset |
| Handoff never appears (first boot) | n/a | bounded wait then non-zero exit → restart-policy crash-loop until provisioned |

## Consequences

- The v3.0 recipe loses the `seed` service, `seed/init-seed.sh`, and
  `volumes-seed.csv`; gains `forgejo/` (image A), a `forgejo` compose service,
  `volumes-forgejo.csv`, and the `provision-handoff` volume rows.
- `validate-local.sh` S1/S2/S3 exercise `cosmon-rpp-adapter trust converge`
  instead of the seed entrypoint; a new
  `dist/avatar-tenant-demo/forgejo/test-provision-local.sh` proves provisioning
  against a **real virgin Forgejo**, including the negative test that would
  have caught the reserved `admin` username.
- `TRUSTED_ISSUERS_FILE` (seed Path A) is retired: complete-allowlist
  declarations move to `[[trust_bootstrap.issuer]]` in `rpp.toml`
  (the LEAN-canonical global-config surface) combined with `TRUSTED_FORCE=1`
  for the reset gesture.
- The security posture is unchanged: deny-by-default, split-DNS two-field
  issuer entries, RS256/ES256 whitelist — this ADR only moves *who writes the
  declaration files*, never what the verifier accepts.
