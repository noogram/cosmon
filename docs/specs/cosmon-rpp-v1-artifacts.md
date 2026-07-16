# Cosmon RPP v1 — Artifact endpoints

**Spec hash:** e653 — `task-20260522-ef4f` (impl), `task-20260522-14fc` (CLI),
ADR-080 (RPP framing), ADR-0018 (smithy noogram v1).

**Status:** v1.0.0-rc, wired in `cosmon-rpp-adapter` 2026-05-22.

## What and why

Tenant workers (Claude Code via `cs tackle`) produce outputs while they
run — generated files, screenshots, logs, signed artefacts. Without a
typed wire surface, the only way to retrieve these is to SSH into the
container and `cat` them, which violates the §8j *no-direct-shell*
invariant. The three routes in this spec close that gap.

The endpoints are filesystem-mediated, not state-mediated: artifacts
live on disk under
`<artifact_root>/<noyau>/<molecule_id>/` and the adapter scans the
directory on demand. There is no `cosmon-state::ops::artifact*`
operation — adding one would have promoted artifacts into the briefing
seal, which they should not be (a worker may write a 50 MB
screenshot and we would not want to seal it).

## The three routes

| Method | Path | Scope | Body / response |
|--------|------|-------|-----------------|
| `GET`  | `/v1/molecules/{id}/artifacts` | `cosmon:artifact:read`  | `200 { request_id, molecule_id, artifacts: [ArtifactEntry] }` |
| `GET`  | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:read`  | `200 <binary>` + `Content-Type`, `Content-Length`, `ETag` |
| `PUT`  | `/v1/molecules/{id}/artifacts/{name}`  | `cosmon:artifact:write` | `201 { request_id, artifact: ArtifactEntry }` |

`{name}` and `{token}` share the same axum route pattern
(`/artifacts/{token}`) — the method disambiguates and the path
parameter is the filename for PUT and the manifest token for GET.

## ArtifactEntry — wire schema

```json
{
  "name": "haiku.txt",
  "content_type": "text/plain",
  "size_bytes": 42,
  "integrity": {"algo": "blake3", "hex": "deadbeef…"},
  "created_at": "2026-05-22T11:00:00+00:00",
  "token": "art_01234567890123456789ABCD"
}
```

- **`token`** is `art_<24 base32 chars>` deterministically derived from
  `SHA-256(molecule_id || "/" || name)`. Stable across reads, opaque
  to clients, URL-safe.
- **`integrity.algo`** is `"blake3"` for V0 — open by contract for
  future algos. The hex is the file's full blake3 digest.
- **`size_bytes`** is the on-disk size at scan time.

## Convention — `COSMON_ARTIFACT_DIR`

At `cs tackle` time, the adapter:

1. `mkdir -p <artifact_root>/<noyau>/<molecule_id>/` (best-effort).
2. Exports `COSMON_ARTIFACT_DIR=<that path>` into the subprocess env.
3. Strips any inherited `COSMON_ARTIFACT_DIR` first (§3.5 strip list)
   so a stale value from one tenant can never leak to another.

The worker writes its outputs under that directory using normal
filesystem APIs (`tee output.json > $COSMON_ARTIFACT_DIR/output.json`,
or whatever it prefers). The adapter's GET routes list and stream
whatever ends up there. Dotfiles are skipped (convention for
"do not surface").

Default `artifact_root` is `/tmp/cosmon`. Operators can relocate it
via `rpp.toml [artifact_root]` — useful for putting artifacts on a
persistent volume rather than `/tmp`.

## RFC 9530 `Digest` header (PUT)

On PUT, the client may include `Digest: blake3=<hex>`. If present, the
adapter computes the blake3 of the body and compares (constant-time)
against the declared value; a mismatch yields `400 digest_mismatch`.
If absent, the upload is accepted and the response carries the
adapter-computed `integrity.hex` so the client can verify after the
fact.

## `If-Match` idempotence (PUT)

A PUT with `If-Match: "<blake3-hex>"` succeeds only if the file already
exists at `name` and its current digest equals the supplied tag. A
mismatch yields `412 if_match_failed`. Absence is fine — the PUT
creates the file (or overwrites unconditionally).

## Path-traversal hardening

Both `molecule_id` and `name` are rejected by the handler when they
contain `..`, `/`, or `\` — defence-in-depth even though axum's path
extractor already disallows the slashes. A failed segment check is
`400 invalid_path_segment`, never a silent fallthrough.

## Scopes

Two new constants in `crate::auth::scopes`:

- `cosmon:artifact:read` — required by the two GETs.
- `cosmon:artifact:write` — required by the PUT.

Distinct from `cosmon:molecule:{read,write}` because artifact payloads
may have a different blast radius than molecule state (a tenant
granting `:molecule:read` to a dashboard does not have to also grant
`:artifact:read`).

The admin-nucleon binding can grant these scopes implicitly, same
union logic as the molecule routes (T23 — `task-20260513-3a9e`).

## Cross-references

- **Server impl:** `crates/cosmon-rpp-adapter/src/routes/artifacts.rs`.
- **Client (CLI):** `crates/cosmon-remote/src/main.rs` (`artifact get`,
  `artifact push` subcommands, `task-20260522-14fc`).
- **Subprocess env:** `crates/cosmon-rpp-adapter/src/subprocess.rs`
  (`SystemInvoker::with_artifact_root`).
- **AppState root:** `crates/cosmon-rpp-adapter/src/lib.rs`
  (`AppState::artifact_root`).
- **OpenAPI:** `crates/cosmon-rpp-adapter/openapi/v1.yaml`
  (`/v1/molecules/{id}/artifacts*` paths + `ArtifactManifest`,
  `ArtifactEntry`, `IntegrityHash`, `ArtifactPushedEnvelope` schemas).
- **Frozen surface:** `crates/cosmon-rpp-adapter/src/lib.rs#frozen_api_surface()`
  carries the three new routes. The bijection check in
  `tests/api_surface_freeze.rs` exempts them via the `/artifacts`
  path segment (mirroring the `/v1/auth/` exemption).

## What is intentionally **not** in this spec

- **No `cs artifact` CLI verb.** Artifacts are filesystem-mediated,
  not state-mediated. The cosmon-remote CLI talks to the routes
  directly; there is no `cs` counterpart, by design.
- **No size cap.** The adapter inherits the global
  `RequestBodyLimitLayer(DEFAULT_BODY_LIMIT_BYTES = 1 MB)` on PUT.
  Larger artifacts will need a streaming variant — out of scope for
  this iteration.
- **No artifact deletion route.** Worker outputs accumulate until
  the operator garbage-collects (or `/tmp` is cleared on reboot).
  Adding `DELETE /v1/molecules/{id}/artifacts/{token}` is additive
  and can be done in a successor iteration if a use case lands.
