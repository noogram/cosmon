# `cosmon-remote` — Phase 1 CLI for the cosmon-rpp v1 surface

Thin Rust CLI binary distributed by the cosmon-rpp-adapter at install
time. Replaces the served justfile of Phase 0 (smithy
`task-20260520-4431`, v1.2/v1.2.1) with a single binary that mirrors
the v1 wire surface one-to-one.

## Why not the justfile?

The Phase 0 served justfile worked, but a live deployment test
surfaced three structural problems:

1. **Templating was only partial.** `install.sh` pinned `COSMON_HOST`
   but every other deployment-specific value (`sub`, `aud`, `oidc_url`,
   `noyau`) used generic defaults. A real deployment used a non-default
   `sub` / `aud` / `oidc_url`. Result: `just mol-list` returned
   a null ensemble with no useful diagnostic.
2. **The placeholder safety-check was fragile.** v1.2 added a guard
   that aborted if `__COSMON_HOST__` was still present; v1.2.1 had to
   remove it because the templating replaced the guard's own check
   string. The CLI removes the placeholder game entirely — values are
   set explicitly, not inferred.
3. **Scheme was assumed HTTPS.** Some deployments are served over HTTP
   (e.g. behind a Tailscale Serve proxy); `install.sh` had to be patched
   in flight.

`cosmon-remote` reads a TOML profile per deployment that captures the
full four-tuple — no placeholder substitution at the binary level.

## Profile layout

```text
~/.config/cosmon-remote/
├── config.toml                       # default_profile = "example"
└── profiles/
    ├── example.toml
    └── local.toml
```

A profile file:

```toml
host = "https://cosmon.example.ts.net"
sub = "operator"
aud = "cosmon-rpp"
oidc_url = "https://cosmon.example.ts.net/oidc"
noyau = "default"
scopes = ["cosmon:molecule:read", "cosmon:molecule:write"]
timeout_secs = 30
```

The Phase 1 `install.sh` (out of scope for this crate; lands in
`cosmon-rpp-adapter` follow-up) templates this file server-side so the
operator never types the four-tuple by hand.

## Surface

The CLI mirrors the v1 OpenAPI surface (the §8p frozen list of nine
molecule routes, the five auth-claude routes, the three artifact
routes from `smithy/docs/specs/cosmon-rpp-v1-artifacts.md`, and
`/healthz`).

```text
cosmon-remote config init <name> <host>
cosmon-remote config set <key> <value>
cosmon-remote config show
cosmon-remote config use <name>
cosmon-remote config list

cosmon-remote auth login --email <addr> [--code <pasted-code>]
cosmon-remote auth status <session_id>
cosmon-remote auth logout <session_id>

cosmon-remote molecule nucleate <formula> [--topic …] [--description …] [--var k=v] [--tag T] [--kind K]
cosmon-remote molecule list [--status …] [--kind …] [--tag …] [--fleet …]
cosmon-remote molecule get <id>
cosmon-remote molecule tackle <id>
cosmon-remote molecule collapse <id> --reason <r> [--cause <c>]
cosmon-remote molecule freeze <id> --reason <r>
cosmon-remote molecule thaw <id> --reason <r>
cosmon-remote molecule stuck <id> --reason <r>
cosmon-remote molecule tag <id> [--add T] [--remove T]

cosmon-remote artifact list <mol-id>
cosmon-remote artifact get <mol-id> <token> [--out PATH]
cosmon-remote artifact push <mol-id> <name> --file <PATH> [--content-type T] [--if-match <etag>]

cosmon-remote healthz
```

Global flags:
- `--profile <name>` — override the active profile for one call.
- `--json` — emit machine-readable JSON on stdout.
- `--token <jwt>` — supply a JWT directly (otherwise minted via the
  profile's `oidc_url`). Also honours `$COSMON_REMOTE_TOKEN`.

## Examples

```sh
# First-time setup against a deployment
cosmon-remote config init example https://cosmon.example.ts.net
cosmon-remote config set sub operator
cosmon-remote config set aud cosmon-rpp
cosmon-remote config set oidc-url https://cosmon.example.ts.net/oidc
cosmon-remote config use example

# Mirror of `just mol-nucleate task-work …`
cosmon-remote molecule nucleate task-work \
    --topic "test-e2e" \
    --description "Écris un haiku sur les smithys dans /tmp/haiku.txt"

# Mirror of `just mol-list`
cosmon-remote molecule list --json

# Pull the artefact the worker produced (no more docker exec)
cosmon-remote artifact list task-20260522-xxxx
cosmon-remote artifact get task-20260522-xxxx art_01234567890123456789ABCD --out ./haiku.txt
```

## Out of scope (follow-up molecules)

This crate ships the CLI core. The Phase 1 `dist` molecule will add:

- Multi-arch cross-compile (macOS arm64/x86_64, Linux arm64/x86_64).
- Serving the binaries through the adapter so `install.sh` can
  download `cosmon-remote` instead of installing `just` + a recipe file.
- Self-update verb.
- New `install.sh` template that drops a templated profile.

## Test strategy

- Unit tests (`src/config.rs`, `src/client.rs`, `src/pkce.rs`) — pure
  in-process logic.
- Integration tests (`tests/wire_contract.rs`) — every endpoint is
  exercised against a `wiremock` mock server. The body shapes,
  bearer header, query parameters and digest header are asserted by
  matchers, so a drift between the CLI and the OpenAPI surface fails
  the test.
- A smoke harness against the live cosmon-server docker stack is
  intended as the Phase 1 e2e step (`cargo run -p cosmon-remote --
  …` against `dist-cosmon-server-1`). Out of scope for this crate's
  unit-test target.
