# cosmon-mcp — remote-MCP transport substrate

**Status: active library, single consumer. The legacy standalone MCP
*server* (stdio) was retired 2026-07-12 (decision C14,
`task-20260712-74a1/outcomes.md`).**

## What this crate is (today)

`cosmon-mcp` is a **transport-only** library. Its one public entry point,
[`streamable_http_service()`], mints an MCP *Streamable-HTTP* tower service
that [`cosmon-rpp-adapter`] nests behind its bearer/OIDC gate
(`.nest_service("/mcp", …)`) to serve the remote-tenant MCP endpoint. The
service exposes only the remote-safe tool partition (`CosmonService::new_remote`,
with the worker-internal and teardown verbs in `DENY_REMOTE_TOOLS` absent from
`tools/list`). It holds **no** opinion about authentication, tenancy, or
ingress — those live one layer up in the host adapter (cosmon is the Resource
Server; the AS is elsewhere). See delib-20260709-943e (torvalds/turing M3).

The crate is not an explicit `[workspace] members` entry, but it is **not**
dead code: `cosmon-rpp-adapter` (a live member) depends on it, so
`cargo build --workspace` compiles it as a first-class transitive dependency.

## History — the stdio surface, deprecated then removed

The crate was **deprecated 2026-04-11** as a *standalone worker/pilot MCP
surface*. That surface exposed cosmon commands as MCP tools over **stdio**,
in parallel with the `cs` CLI. It had no real consumer:

- **Workers** use the `cs` CLI exclusively (CLI-first invariant,
  `docs/architectural-invariants.md`): `cs evolve`, `cs complete`,
  `cs observe` from inside the git worktree, walk-up discovery hitting the
  correct `.cosmon/`.
- **The human pilot** uses the same `cs` CLI.

Maintaining two parallel surfaces caused real drift bugs — a stale
`cosmon-mcp` process answered `molecule not found` while the `cs` CLI,
rebuilt from the same tree, saw the molecule perfectly.

**The git analogy is definitive.** Git has no MCP server, yet LLMs use it
fluently via shell every day. Cosmon follows the same path for local use:
**one surface**, the `cs` CLI, shared by humans and agents.

The 2026-04-11 note gave the crate a "~1-month grace window" then deletion.
That window lapsed without action. Decision **C14** (2026-07-12) audited the
stale window and found the premise had **inverted**: between deprecation and
the audit, the crate had been repurposed as the transport for the RPP remote
surface above. So instead of deleting the crate, C14 retired the genuinely
dead **stdio** surface and kept the live **remote** one:

- **Removed 2026-07-12:** `serve_stdio()`, the standalone `cosmon-mcp`
  binary (`[[bin]]` + `src/main.rs`), the `cs mcp` CLI subcommand, and the
  `cosmon-cli → cosmon-mcp` path dependency (its only use was `cs mcp`).
- **Kept:** `streamable_http_service()` and the `CosmonService` /
  `HttpStatePin` / `DENY_REMOTE_TOOLS` remote partition.

## What NOT to do

- **Do not re-add a stdio server or a `cs mcp` command.** Local pilot/worker
  operation is the `cs` CLI's job. If an agent workflow needs a new
  operation, add a `cs <verb>` subcommand.
- **Do not add worker-internal or teardown verbs to the remote partition.**
  A remote caller is the *requester* of work, never its executor — see
  `DENY_REMOTE_TOOLS` and the deny-remote rationale in `src/tools.rs`.

## Migration (for anyone still on the old stdio tools)

If you were invoking `cosmon_*` MCP tools over the standalone stdio server,
switch to the equivalent `cs` subcommand:

| MCP tool              | CLI equivalent        |
|-----------------------|-----------------------|
| `cosmon_nucleate`     | `cs nucleate`         |
| `cosmon_evolve`       | `cs evolve`           |
| `cosmon_complete`     | `cs complete`         |
| `cosmon_observe`      | `cs observe`          |
| `cosmon_get`          | `cs observe --json`   |
| `cosmon_list`         | `cs list --json`      |
| `cosmon_stats`        | `cs stats --json`     |
| `cosmon_fleet_templates` | `cs fleet templates --json` |

Every `cs` command supports `--json` for agent consumption.

[`streamable_http_service()`]: src/lib.rs
[`cosmon-rpp-adapter`]: ../cosmon-rpp-adapter
