# Surfaces cluster config (`cluster.toml`)

**Status:** v0 shipped 2026-04-23 (ADR-066). Schema version `1`.

This document describes the **machine-level** configuration file that
every cosmon surface reads to discover the shape of the local cluster:
which devices are in the Tailscale orbit, which ports the cs-api and
matrix-echo-tick services listen on, which credentials file holds the
Matrix bot token, and which bundle identifiers apply to the Mac and iOS
pilot apps.

The goal is simple: **adding a new device to the cluster must not
require rebuilding code**. A collaborator plugging their iPad in, a
second operator laptop, a future AWS Synapse — each of them should
discover the topology from the same file.

---

## 1. Where the file lives

| Precedence | Path |
|---|---|
| 1 | `--cluster-config <path>` (CLI / `cs-api` flag) |
| 2 | `$COSMON_CLUSTER_CONFIG` |
| 3 | `$HOME/.config/cosmon/cluster.toml` *(default)* |

The file is **machine-local**: it describes *this particular host's view
of the cluster*. Two operators can share a `/srv/cosmon/` tree via
rclone and still have different `cluster.toml` files. XDG placement
(`~/.config/cosmon/`) keeps it alongside the future
`~/.config/cosmon/cockpit.toml` and `~/.config/cosmon/patrols.toml`.

---

## 2. Full schema

```toml
schema_version = 1

[cluster]
name              = "you-local"
owner_nucleon_id  = "you"
tailnet_domain    = "tail-XXXX.ts.net"   # optional (MagicDNS)
updated_at        = "2026-04-23T10:30:00Z"

[host.mbp]
tailscale_ip       = "192.0.2.10"
tailscale_hostname = "host.example"
role               = "primary"

[host.ipad-tenant_auditor]
tailscale_ip       = "192.0.2.11"
tailscale_hostname = "ipad-tenant_auditor"
role               = "collaborator"

[surfaces.cs_api]
host        = "mbp"                          # must match a [host.*] key
port        = 4222
launchagent = "dev.noogram.cosmon.cs-api"

[surfaces.matrix_echo_tick]
host             = "mbp"
launchagent      = "dev.noogram.cosmon.matrix-tick"
credentials_file = "~/.config/cosmon-matrix-tick/credentials.toml"
room_id          = "!room:matrix.org"

[apps]
mac_pilot_bundle_id = "dev.noogram.cosmon.mac-pilot"
ios_pilot_bundle_id = "dev.noogram.cosmon.ios-pilot"
```

All fields are optional except `schema_version` (which defaults to the
reader's compile-time `CURRENT_SCHEMA_VERSION` when missing). Unknown
fields are ignored with a log warning so that a v2 file still opens
under a v1 reader.

### Three invariants (ADR-066 §2.1)

1. **References, not secrets.** `credentials_file` names a path. The
   plaintext credential never enters `cluster.toml`.
2. **Read-only for surfaces.** Only the operator edits the file
   (manually, via `$EDITOR`, or via `cs cluster edit`). Surfaces read
   and cache.
3. **Versioned.** `schema_version = 1` today. Bumping breaks older
   readers on purpose — they must emit a warning and continue.

---

## 3. Consumers

| Surface | How it reads |
|---|---|
| `cs-api` (`GET /cluster`) | Reads the file on each request and emits JSON (or `{"error":"not_configured"}` when absent). No cache. |
| `cs cluster show` | Pretty table (or `--json`). |
| `cs cluster edit` | Opens `$EDITOR` on the file, seeding a template when absent. |
| `cs cluster bootstrap <primary_url>` | `curl <primary_url>/cluster` and write the result locally. |
| `ios-pilot` | At first launch, if `UserDefaults` has no `cs_api_url`, probes `http://<fallback-ip>:4222/cluster`, applies the cs-api host+port if present, otherwise keeps its compile-time default. |
| `scripts/install-*-launchagent.sh` | Can read `cluster.toml` for default values (room_id, launchagent labels). v0 still accepts `--room-id` flags, just with smarter defaults. |

None of them write.

---

## 4. First-device bootstrap

On the operator's primary Mac:

```bash
# Option A — shell script (no cs binary yet)
./scripts/init-cluster-config.sh

# Option B — Rust-side (once cs is installed)
cs cluster edit
```

Either path produces a commented template. Fill in the `<TO_FILL>`
placeholders, save, and verify:

```bash
cs cluster show
```

---

## 5. Adding a second device

### 5.1 · Secondary Mac / Linux host with `cs` installed

```bash
cs cluster bootstrap http://<primary-tailscale-ip>:4222
cs cluster show
```

`bootstrap` fetches `/cluster` from the primary's cs-api, validates the
payload as a `ClusterConfig`, and writes it to
`~/.config/cosmon/cluster.toml`. If a file already exists locally, the
command refuses unless `--force` is passed.

### 5.2 · iPad / iPhone

v0 flow: the operator types the primary URL once in Settings. The app
fetches `/cluster`, applies the `cs_api` surface's host+port, and caches
it. If `GET /cluster` returns `not_configured`, the app keeps the
compile-time fallback (today `http://192.0.2.10:4222`) so nothing
breaks during rollout.

QR-code pairing is v1 — deferred.

### 5.3 · AWS host (future)

When the AWS Synapse lands (`task-20260422-1916`), adding it is one
block in `cluster.toml`:

```toml
[host.aws-synapse]
tailscale_ip       = "100.99.x.y"
tailscale_hostname = "aws-synapse"
role               = "homeserver"
```

Plus whatever `[surfaces.synapse_homeserver]` block we define at that
time. No code changes to existing surfaces — they simply gain a new
optional field to read.

---

## 6. HTTP endpoint reference

```
GET /cluster
```

**200 OK** with a JSON body that mirrors the TOML structure:

```json
{
  "schema_version": 1,
  "cluster": {"name": "you-local", "owner_nucleon_id": "you", ...},
  "host": {"mbp": {"tailscale_ip": "192.0.2.10", ...}},
  "surfaces": {"cs_api": {"host": "mbp", "port": 4222, ...}},
  "apps": {"ios_pilot_bundle_id": "dev.noogram.cosmon.ios-pilot"}
}
```

**200 OK** with `{"error":"not_configured","path":"/Users/.../cluster.toml"}`
when the file is absent. This is **not** a 500: the caller's correct
behavior is to fall back to its compile-time default, which is semantic
information, not a transport error.

---

## 7. Common pitfalls

| Pitfall | Symptom | Fix |
|---|---|---|
| Surface `host` key doesn't match any `[host.*]` table | iOS app sees `cs_api_base_url = null` | Check `cs cluster show` — the `→ http://…` line is missing |
| Operator edits the file while cs-api is running | Next request picks up the change | None needed — reads are un-cached |
| Credentials leaked into `cluster.toml` | Secrets in a file that gets `cs cluster bootstrap`'d to peers | Remove. Invariant #1 is non-negotiable |
| Schema bump to 2 with an old reader | Reader logs a warning, ignores new fields | Expected; not a bug |

---

## 8. See also

- ADR-066 — the decision record
- ADR-016 — adapter-vs-runtime boundary (`cs-api` is an adapter)
- ADR-038 — whisper channel (room id moved here from credentials)
- ADR-064 — matrix-echo-tick isolation (credentials file path format)
- `scripts/init-cluster-config.sh` — shell-level bootstrap
- An internal chronicle — the fy8 telling
