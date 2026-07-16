# HTTP-on-Tailscale Runbook

> **Status**: standard transport for native apps in the local cluster of galaxies (Verdict, Mur du Matin, Cosmon, future).
> **Decision date**: 2026-04-26 (operator).
> **Trust boundary**: Tailscale. **Wire format**: HTTP+JSON, snake_case keys, `seconds_since_1970` dates, `/v1/<resource>` URLs.

This runbook is the operational complement to the
[`apps-transport-http`](../../crates/apps-transport-http) Rust crate
and the
[`AppsTransportHTTP`](../../apps/AppsTransportHTTP) Swift package.
Read it once when adding a new daemon; come back when something breaks.

---

## 1. Why HTTP, why Tailscale, why this crate

Three earlier decisions converge here:

1. **Single-observer, single-trust-boundary** (Noogram compliance) — the
   tailnet is the network perimeter; nothing inside it crosses a
   second mTLS layer. We do not duplicate authentication.
2. **The wire is observable** — HTTP+JSON is `curl`-debuggable and `tcpdump`-readable.
   UNIX sockets and ad-hoc framings (cf. legacy `cockpit-store/wire.rs`)
   are not.
3. **One canonical shape per language** — Rust apps speak through the
   `apps-transport-http` axum helpers; Swift apps speak through the
   `AppsTransportHTTP` actor. Anything else is a bug.

The crate is **not** a daemon. It is a thin layer over `axum` and
`URLSession` that enforces the conventions every cluster app should
share:

- Bind only on the Tailscale interface (never `0.0.0.0`).
- Tag every request with an `x-request-id` header.
- Map handler errors to `{error, code, detail}` JSON with stable status
  codes (400/404/409/422/500).
- URL-version every public route at `/v1/<resource>`.

---

## 2. Find your Tailscale IP

```bash
tailscale ip --4
# 192.0.2.10
```

This is the address `apps-transport-http` will bind on by default. To
override (e.g. inside a CI sandbox without `tailscale`), set
`APPS_TRANSPORT_HTTP_TAILSCALE_IP`:

```bash
APPS_TRANSPORT_HTTP_TAILSCALE_IP=127.0.0.1 cargo run -p mydaemon
```

Or pass an explicit `COCKPIT_HTTP_BIND`:

```bash
COCKPIT_HTTP_BIND=192.0.2.10:8789 cargo run -p mydaemon
```

The crate refuses to bind on `0.0.0.0` / `::` and surfaces the refusal
as a `BindError::UnspecifiedAddress`. This is intentional — keeping the
daemon Tailscale-scoped is non-negotiable.

---

## 3. Standard ports (cluster registry)

| Port | Daemon | Galaxy | Notes |
|------|--------|--------|-------|
| 8787 | mural-server (tiny_http legacy) | mailroom | Pre-crate; migrating to 8788 (other molecule). |
| 8788 | mural-daemon (HTTP) | mailroom | Le Mur du Matin — JSON API + static. |
| 8789 | verdict-daemon (HTTP) | verdict | Replaces UNIX socket from `cockpit-daemon` (other molecule). |
| 8790 | cosmon-app-daemon | cosmon | Reserved for future. |

Ports above 8800 are free; pick the lowest available when adding a new daemon.

---

## 4. Add a new daemon (Rust)

```rust
// src/main.rs of `mydaemon`
use apps_transport_http::{
    access_log_layer, request_id_layer, routing::v1,
    serve_http_on_tailscale, ApplicationError, TailscaleBind,
};
use axum::middleware::from_fn;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

#[derive(Serialize)]
struct Health { ok: bool, service: &'static str }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let app = Router::new()
        .route(v1::HEALTH, get(|| async {
            Json(Health { ok: true, service: "mydaemon" })
        }))
        .route("/v1/things", post(handle_thing))
        .layer(from_fn(access_log_layer))
        .layer(from_fn(request_id_layer));

    let shutdown = async {
        tokio::signal::ctrl_c().await.ok();
    };
    serve_http_on_tailscale(
        TailscaleBind::auto_with_port(8800),
        app,
        shutdown,
    ).await?;
    Ok(())
}

async fn handle_thing(
    Json(req): Json<MyReq>
) -> Result<Json<MyResp>, ApplicationError> {
    if req.field.is_empty() {
        return Err(ApplicationError::BadRequest("field required".into()));
    }
    // ... do work ...
    Ok(Json(MyResp { /* ... */ }))
}
```

The `from_fn` layers are **order-sensitive**: `request_id_layer` must
run before `access_log_layer`, otherwise the access log misses the
request id. Apply them in *reverse* of desired execution (axum stacks
them inside-out): outermost layer first → `access_log_layer`,
innermost → `request_id_layer`.

---

## 5. Add a new client (Swift)

`Info.plist`:

```xml
<key>CockpitHost</key>
<string>host.example</string>
<key>CockpitPort</key>
<integer>8800</integer>
```

`MyClient.swift`:

```swift
import AppsTransportHTTP

@MainActor
final class MyClient {
    let transport = HTTPTransport(config: .fromInfoPlist())

    func health() async throws -> Health {
        try await transport.get("/v1/health")
    }

    func createThing(_ req: MyReq) async throws -> MyResp {
        try await transport.post("/v1/things", body: req)
    }

    func pollWithRetry() async throws -> Health {
        try await transport.withReconnect(maxAttempts: 5) {
            try await transport.get("/v1/health")
        }
    }
}
```

Errors fan out into typed `HTTPTransportError` cases:

- `.daemonOffline` — daemon process not running, switch UI to read-only.
- `.protocolMismatch` — version skew, surface a banner.
- `.applicationError(status, body)` — switch on `body.code`.
- `.unexpectedStatus(status, raw)` — fallback for non-canonical errors.

---

## 6. Tailscale ACL stanza (Noogram compliance)

Cluster daemons should only be reachable by Noogram's own devices and
by tagged worker hosts. The ACL stanza below restricts the cluster
ports to the operator's tailnet user.

```jsonc
// tailscale-acl.json — extract
{
    "tagOwners": {
        "tag:cluster-daemon": ["you@"]
    },
    "acls": [
        // Cluster daemons are reachable only from operator devices.
        {
            "action": "accept",
            "src":    ["you@"],
            "dst":    ["tag:cluster-daemon:8780-8800"]
        }
    ],
    "ssh": []
}
```

Apply the tag to the host:

```bash
sudo tailscale up --advertise-tags=tag:cluster-daemon
```

If you skip the ACL, the wire is still inside the tailnet (i.e. not on
the public internet) — Tailscale's default ACL accepts traffic only
from your own tailnet members. The stanza above is defence in depth,
not a load-bearing security layer.

---

## 7. Curl-debug a cluster daemon

```bash
# Liveness probe
curl -sS http://host.example:8789/v1/health
# {"ok":true,"service":"verdict"}

# With request id propagation
curl -sS -H 'X-Request-Id: dbg-001' \
    http://host.example:8789/v1/health \
    -i | grep -iE '^(http|x-request-id)'
# HTTP/1.1 200 OK
# x-request-id: dbg-001

# POST with snake_case body
curl -sS -X POST -H 'Content-Type: application/json' \
    -d '{"surface_name":"deal-aws","x":10,"y":20}' \
    http://host.example:8788/v1/pins
```

Application errors arrive as JSON:

```bash
$ curl -sS -X POST -H 'Content-Type: application/json' \
       -d '{"surface_name":""}' \
       http://host.example:8788/v1/pins
{"error":"bad request: surface_name required","code":"bad_request","detail":"surface_name required"}
```

---

## 8. Health-checking the cluster

```bash
cs apps health
```

Pings every known cluster daemon (verdict, mural, cosmon-app, …) at
`/v1/health` and reports green/red. See `cs apps health --help` for
flags. The list of daemons lives in
[`crates/cosmon-cli/src/cmd/apps.rs`](../../crates/cosmon-cli/src/cmd/apps.rs).

---

## 9. v1.1 — push channel (planned, not implemented)

The current crate is request/response only. For push (e.g. Verdict's
"new molecule" event), we will add Server-Sent Events on
`/v1/events` (SSE). Design notes:

- SSE keeps the wire HTTP — no WebSocket framing, no second protocol.
- Re-uses the same axum router and middleware stack.
- Swift side: `URLSession.bytes(for:)` consumes the SSE stream as
  `AsyncSequence<String>`, then `JSONDecoder` per line.

Inscribed for traceability; implementation lives in a future molecule
(`apps-transport-http-sse-v11`, not yet nucleated).

---

## 10. Operational checklist (new daemon)

- [ ] Reserve a port in §3 above; PR the table update.
- [ ] Bind via `serve_http_on_tailscale` — no manual `axum::serve`.
- [ ] Apply `request_id_layer` and `access_log_layer` in that order.
- [ ] Expose `/v1/health` with `{ok: true, service: "<name>"}`.
- [ ] Add a `LaunchAgent` plist that runs the binary as a normal user.
- [ ] Smoke-test with `curl`. Verify the access log emits one line per request.
- [ ] Update `cs apps health` to include the new daemon.
- [ ] Mention the daemon in the relevant galaxy CLAUDE.md.

---

## 11. Forbidden

- Binding to `0.0.0.0` or `::` (the crate refuses).
- Skipping `/v1/` prefix on public routes.
- Returning errors as plain text (must be `{error, code, detail}` JSON).
- Hard-coding the Tailscale IPv4 in the binary (use the auto-discover path).
- Adding a second authentication layer (Tailscale is the trust boundary).
