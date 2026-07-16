# Dev MCP Harness — smoke-test `/mcp` from Claude Desktop

> **Script**: [`scripts/dev-mcp-harness.sh`](../../scripts/dev-mcp-harness.sh)
> **Purpose**: stand up the cosmon `/mcp` connector on a real port (tailnet)
> behind the full admission chain, minted with a throwaway test JWT, so you
> can point Claude Desktop at it and watch the handshake.
> **⚠️ DEMO ONLY** — the IdP signs with a plaintext RSA key committed to this
> repo. Never leave it on outside your own tailnet.

---

## What it is

The production `/mcp` surface is the **third projection** of the cosmon core,
nested on the rpp-adapter's single listener behind a bearer gate
(delib-20260709-943e). It is not reachable without the whole admission chain
live. This harness assembles that chain against a scratch state dir:

```
Claude Desktop ──HTTP+Bearer──▶ cosmon-rpp-adapter   (tailnet :8443)
                                     │  validate JWT vs JWKS
                                     │  (iss,sub,aud) → noyau  (nucleon binding)
                                     │  pin MCP statedir to the tenant
                                     ▼
                                cs-oidc-mock          (localhost :8444)
                                     JWKS + POST /issue  (embedded RSA test key)
```

The script provisions the trust seam the two processes need — a
`security/trusted-issuers.toml` allowlist (which JWKS to fetch) plus one
sealed `nucleons/<noyau>/oidc-identity.toml` binding rendered by the
adapter's own audited `nucleon render` path — mints a JWT, curl-smokes the
`/mcp initialize` handshake (anonymous → 401, authenticated → 200), then
prints the Claude Desktop connector config.

## Run it

```bash
scripts/dev-mcp-harness.sh          # build, boot, mint, print, then serve
# Ctrl-C tears both processes down and removes the scratch state dir.
```

First run builds three debug binaries (`cs`, `cosmon-rpp-adapter`,
`cs-oidc-mock`); pass `--no-build` on reboots. The bind address auto-resolves
to your Tailscale IPv4 (`tailscale ip -4`); it falls back to `127.0.0.1` and
warns when no tailnet is found.

### Flags & env

| Flag / env | Default | Effect |
|---|---|---|
| `--bind <addr>` | `<tailnet-ip>:8443` | override the adapter bind address |
| `--keep` | off | preserve the scratch state dir across runs |
| `--no-build` | off | skip `cargo build` (fast reboot) |
| `COSMON_DEV_MCP_HOME` | `$TMPDIR/cosmon-dev-mcp` | scratch state root |
| `ADAPTER_PORT` / `MOCK_PORT` | `8443` / `8444` | listener ports |
| `NOYAU` / `SUB` | `dev` / `dev-operator` | tenant slot & JWT subject |
| `TOKEN_LIFETIME` | `86400` | JWT lifetime (seconds) |

## Point Claude Desktop at it

The script writes a ready `claude_desktop_config.json` fragment (path printed
on boot). It uses the `mcp-remote` npx bridge to attach the `Authorization`
header — Claude Desktop's remote-MCP transport:

```json
{
  "mcpServers": {
    "cosmon-dev": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "http://<bind>/mcp",
               "--allow-http", "--transport", "http-only",
               "--header", "Authorization: Bearer <token>"]
    }
  }
}
```

Two flags earn their place here. `mcp-remote` refuses any non-HTTPS URL that
is not `localhost` (`Non-HTTPS URLs only allowed for localhost`), so a tailnet
HTTP endpoint needs **`--allow-http`**. And because the adapter speaks
Streamable HTTP rather than the legacy SSE transport, **`--transport
http-only`** pins the bridge to the right transport instead of probing SSE
first. Both are harmless for a localhost bind, so the printer always emits
them.

Merge that into `~/Library/Application Support/Claude/claude_desktop_config.json`
and restart Claude Desktop. The connector exposes the **remote-safe tool
partition** only — `cosmon_nucleate`, `cosmon_observe`, `cosmon_ensemble`, …
The worker-internal verbs (`evolve`, `complete`, `nudge`, `declare`,
`energy_log`) are absent by construction (`DENY_REMOTE_TOOLS`).

If Claude Desktop runs on a **different** tailnet device, the default tailnet
bind already reaches it. For a public HTTPS front:
`tailscale serve --bg 8443`.

## Debug by hand

The boot banner prints a copy-paste `curl` that re-drives the `initialize`
handshake. To walk the full session:

```bash
TOKEN=$(curl -sf -X POST 'http://127.0.0.1:8444/issue?sub=dev-operator&aud=cosmon-rpp-test&lifetime=600' | jq -r .access_token)

# initialize → capture Mcp-Session-Id from the response headers
curl -sS -D /tmp/h.txt -X POST http://<bind>/mcp \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"dev","version":"0"}}}'
SID=$(awk 'tolower($1)=="mcp-session-id:"{print $2}' /tmp/h.txt | tr -d '\r')

# list the exposed tools
curl -sS -X POST http://<bind>/mcp -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -H "mcp-session-id: $SID" -d '{"jsonrpc":"2.0","id":3,"method":"tools/list"}' \
  | grep '^data:' | sed 's/^data: //' | jq -r '.result.tools[].name'
```

Adapter and IdP logs land under `$COSMON_DEV_MCP_HOME/logs/`.

## Common failures

| Symptom | Cause |
|---|---|
| `mcp-remote`: **Non-HTTPS URLs only allowed for localhost** | the bridge is missing `--allow-http` for the tailnet HTTP endpoint. Copy the current printed fragment — it carries `--allow-http` and `--transport http-only`. |
| authenticated `/mcp` → **401** | JWT rejected. Check `iss`/`aud`/`kid` in the adapter log match the mock's compiled defaults; confirm the adapter fetched the mock's JWKS (`boot.jwks mode=http-fetch`). |
| adapter never comes up | port in use, or `trusted-issuers.toml` unreadable — see `logs/rpp-adapter.log`. |
| `no Tailscale IPv4 found` | tailscale not up; bind is localhost. Pass `--bind` for an explicit address. |

## Not for production

This harness is the dev-loop complement to the real deploy
(`crates/cosmon-rpp-adapter/deploy/`). The embedded RSA key makes every token
forgeable; `posture=prepared` warns rather than enforces short-lived JWTs.
For a real IdP, wire Keycloak (or the avatar's Forgejo AS) and flip to
`posture=active`.
