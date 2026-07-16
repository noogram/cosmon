# ADR-066 — Surfaces Cluster Config (`cluster.toml`)

**Status:** Accepted (2026-04-23)
**Date:** 2026-04-23
**Authoring molecule:** `task-20260423-bb9c`
**Scope:** a single TOML file at `~/.config/cosmon/cluster.toml`, a
`GET /cluster` endpoint in `cosmon-api`, a `cs cluster` CLI verb, and
an `init-cluster-config.sh` bootstrap script.

**Related ADRs:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — Transactional
  Core vs Resident Runtime; `cs-api` is an adapter and stays outside the
  runtime boundary.
- [ADR-030](030-cosmon-archive-model.md) — selective gitignore; cluster
  config secrets stay out of the file (references only).
- [ADR-038](038-whisper-perturbation-port.md) — whisper channel (Matrix
  room IDs are topology, promoted out of `credentials.toml`).
- ADR-064 — matrix-echo-tick
  credentials (the thing we stop hardcoding in service files).

**Related commits:**
- `96194171e` — the debt we repay: ios-pilot hardcoded Tailscale IP.
- `d4b7c620e` — cs-api v1 (the adapter that grows the endpoint).

---

## 1 · Context

### 1.1 · The hardcode that forced the question

`apps/ios-pilot/ios-pilot/CosmonAPI.swift` carries a fallback URL
`http://192.0.2.10:4222` — the Tailscale IP of the operator's
primary MacBook Pro. Three separate deployments are now planned over
the same cluster:

1. **A collaborator's iPad** joins the Tailscale orbit (tenant_auditor,
   Tenant-Demo). Their device cannot reach the operator's MBP without a
   code patch.
2. **A secondary operator Mac** (AWS or a spare laptop). Same
   rewrite-and-rebuild loop as #1.
3. **An AWS-hosted Synapse** (`task-20260422-1916`) for cosmon-whispers
   routes through a *different* Tailscale node from the MBP.

Beyond the iOS URL, two other couplings are hardcoded in separate
files: the `cosmon-whispers` Matrix room ID lives in
`~/.config/cosmon-matrix-tick/credentials.toml` on the MBP; the list
of LaunchAgent bundle identifiers is duplicated between installer
scripts and inline documentation.

There is no single file that answers: **what is the shape of this
cluster?**

### 1.2 · Why a file, not a daemon

The obvious shape would be: a daemon serves topology, devices ask the
daemon. ADR-016 rules that out — cosmon has a transactional core; new
services must have a strong justification. Topology changes by the
operator's hand at human speed (minutes, not seconds). A file on disk
is the simpler, more recoverable substrate. `cs-api` already exists as
the HTTP adapter for native pilots (ADR-016 §4 caveat); it can expose
`GET /cluster` by reading the file.

### 1.3 · Why `~/.config/cosmon/` and not `/srv/cosmon/`

Three options were considered:

| Location | Pro | Con |
|---|---|---|
| `/srv/cosmon/cluster.toml` | Co-located with galaxies | Cluster ≠ galaxies; mixing concerns |
| `/srv/cosmon/.cluster/cluster.toml` | Gitignored, hidden | Still couples cluster to galaxy tree |
| `~/.config/cosmon/cluster.toml` | XDG standard, single cosmon home | New dir (minor) |

The cluster is **a property of the machine, not of the galaxies**.
Two operators can share one `/srv/cosmon/` (via rclone) but have
different Tailscale IPs — the cluster file must be machine-local.
XDG (`~/.config/cosmon/`) is the idiomatic home: it is where a future
`~/.config/cosmon/cockpit.toml` or `~/.config/cosmon/patrols.toml`
would also live. We adopt it.

---

## 2 · Decision

### 2.1 · The file

A single TOML file at `~/.config/cosmon/cluster.toml` (overridable via
`$COSMON_CLUSTER_CONFIG`). Schema:

```toml
schema_version = 1

[cluster]
name = "you-local"
owner_nucleon_id = "you"
tailnet_domain = "tail-XXXX.ts.net"  # optional, enables MagicDNS
updated_at = "2026-04-23T10:30:00Z"

[host.mbp]
tailscale_ip = "192.0.2.10"
tailscale_hostname = "host.example"
role = "primary"

[host.ipad-tenant_auditor]
tailscale_ip = "192.0.2.11"
tailscale_hostname = "ipad-tenant_auditor"
role = "collaborator"

[surfaces.cs_api]
host = "mbp"
port = 4222
launchagent = "dev.noogram.cosmon.cs-api"

[surfaces.matrix_echo_tick]
host = "mbp"
launchagent = "dev.noogram.cosmon.matrix-tick"
credentials_file = "~/.config/cosmon-matrix-tick/credentials.toml"
room_id = "!room:matrix.org"

[apps]
mac_pilot_bundle_id = "dev.noogram.cosmon.mac-pilot"
ios_pilot_bundle_id = "dev.noogram.cosmon.ios-pilot"
```

Three invariants hold on the schema:

1. **References, not secrets.** `credentials_file` names a path; the
   credentials themselves never enter `cluster.toml`. A file with
   plaintext credentials is a different ADR.
2. **Read-only for surfaces.** Only the operator edits it (`cs
   cluster edit` or a text editor). Surfaces read and cache; they
   never write.
3. **Versioned.** `schema_version = 1` is the first integer; any
   breaking change bumps it. Older readers ignore unknown sections
   and log a warning.

### 2.2 · The endpoint

```
GET /cluster           → 200 application/json  (the TOML, as JSON)
                      → 200 { "error": "not_configured" }  when the file is missing
```

No 500. A missing file is a valid state (e.g. fresh install, before
bootstrap). The iOS pilot treats `not_configured` as "fall back to the
compile-time default" — the old hardcoded IP stays as a safety net.

`cs-api` gains a `--cluster-config <path>` flag for testing and
override; by default it reads `$COSMON_CLUSTER_CONFIG` then
`~/.config/cosmon/cluster.toml`.

### 2.3 · The CLI verb

`cs cluster` is a top-level cosmon CLI verb with three sub-commands:

- `cs cluster show [--json]` — pretty-print the resolved config
  (a human-readable table by default, JSON with `--json`).
- `cs cluster edit` — open `$EDITOR` on the file, creating a
  template first if absent.
- `cs cluster bootstrap <primary_url>` — fetch `<primary_url>/cluster`
  and write it locally. Used on a new device joining the cluster.

### 2.4 · The bootstrap script

`scripts/init-cluster-config.sh` is a shell-level fallback that runs
when `cs` is not yet installed on the new device. It detects
`tailscale ip -4`, `tailscale status --json`, and `/srv/cosmon/*/`
entries to produce a valid `cluster.toml` seed.

### 2.5 · Migration path

iOS v0:
- Default fallback stays `http://192.0.2.10:4222` (backward
  compat).
- At first launch, if `UserDefaults` has no `cs_api_url`, probe the
  fallback URL, fetch `/cluster`, apply the `cs_api` surface's
  `host` + `port`, and cache. On `not_configured`, keep the hardcoded
  fallback.

QR-code pairing (v1) is deferred. The v0 mechanism is strictly: the
operator types the primary URL once (or ships the app with the
compile-time default), and the app self-configures from there.

---

## 3 · Consequences

### 3.1 · Adds

- One TOML file on disk, one HTTP endpoint, one CLI verb, one script.
- A single `schema_version`-gated extension point for every future
  topology need (additional hosts, additional surfaces, non-Tailscale
  transport).
- A clear seam between **topology** (this file) and **credentials**
  (referenced, never inlined).

### 3.2 · Removes

- Hardcoded Tailscale IP in Swift (moved to a self-configuring
  lookup with a safety-net default).
- Duplication of Matrix room ID between credentials and installer
  comments.
- The ambient assumption that there is always exactly one operator
  Mac.

### 3.3 · Does **not** change

- `cs-api`'s transactional-core role. It is still a thin HTTP adapter
  over shell-outs; reading a file does not make it stateful.
- Credential flow. Secrets remain where they are today (Keychain,
  credentials files, env vars).
- The role of `.cosmon/config.toml` (per-project) — `cluster.toml` is
  strictly per-*machine cluster*.

### 3.4 · Future extensions (explicit non-goals for v0)

- **Multi-cluster** (e.g. `perso` + `pro`). Reserved via a future
  `$COSMON_CLUSTER` env var and `~/.config/cosmon/default-cluster`
  marker; out of scope here.
- **QR-code pairing** for iOS. v1 feature, depends on a
  `cs cluster qr` renderer and a small iOS scanner.
- **Neurion integration** (`task-20260422-d430`). `cluster.toml`
  may eventually become a neurion-backed view; the TOML-on-disk
  substrate stays authoritative for now.

---

## 4 · Compliance with the coherence checklist

1. **Stateless?** Yes — reading a file per request, no background loop.
2. **Idempotent?** Yes — `GET /cluster` twice = once; `cs cluster
   show` is pure read.
3. **Regime-aware?** Yes — pure Inert read path on the core; the
   adapter side lives in `cs-api`, outside the runtime boundary.
4. **Single perimeter?** Yes — `cs cluster` does topology-read; it
   does not overlap with `cs init`, `cs neurion`, or `cs spark`.
5. **Symmetric undo?** Not applicable — config file; operator edits
   directly.
6. **Runtime-compatible?** Yes — the runtime could later become a
   reader of `cluster.toml` without touching this ADR.
7. **Worker/human boundary respected?** Yes — `cs cluster edit` is
   human-callable; workers never edit.
8. **Write-read asymmetry preserved?** Yes — write by editor only,
   read by CLI/API.
9. **Merge-before-dispatch respected?** Not applicable (no dispatch).
10. **CLI-first for workers?** Workers do not use cluster.toml
    today; if they ever do, they will use `cs cluster show --json`,
    consistent with the invariant.

---

## 5 · One-line summary

> *The map of the cosmon village. Everyone reads the same copy; we
> distribute it when a new habitant arrives.*
