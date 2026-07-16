# The distribution mechanism — assembling a kernel + plugins into a runnable image

**Status:** canonical (refines [ADR-132](adr/132-kernel-plugin-catalog-ecosystem.md))
**Date:** 2026-06-24
**Origin:** cosmon-ward feedback from smithy (`task-20260624-107f`) — three
integration gaps found while mapping the *avatar* deployment onto the
kernel/plugin model. The réacteur learns from what it burns.

---

## 1. What a distribution is

[ADR-132](adr/132-kernel-plugin-catalog-ecosystem.md) names the model: cosmon
is a **kernel**, the peripherals are a **plugin catalog**, and a plugin reaches
the kernel only over a **process/network boundary** (the four seams S1–S4), never
by code-linking.

A **distribution** is the act of *assembling* one pinned kernel with a chosen
set of plugins into a thing you can run. The recipe is a single declarative file
— a **distribution profile** (`*.distribution.toml`) — and the assembler is a
build-time script (`build-distribution.sh`), **not** a runtime plugin-loader.
This matters: ADR-132 §4 forbids cosmon from growing a dlopen registry or a
manifest subsystem. The profile is *data the assembler reads at build time*, the
same way a `Cargo.lock` is data, not a daemon.

> **One image is not one process, and one process is not one image.** The
> distribution profile decouples *what is pinned* (kernel commit + plugin revs)
> from *how it runs* (single-process image, or a multi-service compose stack).
> The three sections below pin down that "how it runs" question, which the bare
> kernel/plugin model left open.

The worked example is the **avatar**: kernel (public cosmon, pinned) + the
`llama` plugin (local sovereign inference over seam S1) — see
[`examples/avatar-aria.distribution.toml`](../examples/avatar-aria.distribution.toml).

## 2. The `distribution.toml` schema

A profile pins exactly one kernel and lists zero or more plugins. Each plugin
declares which seam it integrates over (S1–S4) and — **if and only if it binds a
port** — that port, explicitly.

```toml
schema_version = "1"

[kernel]
source = "git"
repo   = "https://github.com/noogram/cosmon"
rev    = "eac7537184598519424497f11b1a9073938c07ab"   # pinned commit

# Reserved kernel ports — informational to a reader, authoritative to the
# assembler: a plugin port that collides with one of these is rejected.
[kernel.reserved_ports]
rpp_ingress = 8080      # cosmon-rpp-adapter — the §8j HTTPS+OIDC door (ADR-080)

[[plugins]]
name        = "llama"             # the plugin's catalog name
seam        = "S1"                # S1 network-provider | S2 subprocess | S3 mcp | S4 formula
source      = "archive"           # git | archive | local — where the plugin bytes come from
archive_rev = "7df0613b3"         # pinned (interim; becomes git+rev at the target regime)
port        = 8081                # REQUIRED for any port-binding plugin — see §3
base_url    = "http://localhost:8081/v1"   # how the kernel reaches it (S1 adapter base_url)
supervision = "compose-sibling"   # how the process runs relative to the image — see §4
```

### Field reference

| Field | Required | Meaning |
|-------|:--------:|---------|
| `schema_version` | yes | `"1"`. Lets the assembler reject a profile it does not understand. |
| `[kernel].source` | yes | `git` (target regime) — the kernel is always pinned by commit, never floating. |
| `[kernel].rev` | yes | The exact kernel commit. The avatar's candidate is re-pinned on the first real build (see the profile's inline reservation). |
| `[kernel.reserved_ports]` | recommended | Kernel-side bindings a plugin must not reuse. The assembler treats them as occupied. |
| `[[plugins]].name` | yes | Catalog name. |
| `[[plugins]].seam` | yes | One of `S1`–`S4` (ADR-132 §2c). Determines whether a `port`/`base_url` is even meaningful. |
| `[[plugins]].source` / `.rev` / `.archive_rev` | yes | Where the plugin bytes come from, pinned. `archive`/`local` are interim; the target regime is `git`+`rev`, like the kernel (ADR-132 §3). |
| `[[plugins]].port` | **yes for any port-binding plugin** | The host/compose port the plugin's server binds. **No implicit default** — see §3. Omitted only for plugins that bind no port (S3 stdio MCP, S4 formula). |
| `[[plugins]].base_url` | for S1 | The URL the kernel's `openai`/`ollama` adapter POSTs to. Must agree with `port`. |
| `[[plugins]].supervision` | for any process plugin | `compose-sibling` (canonical) or `in-image` (downstream choice) — see §4. |

## 3. FLAG 1 — plugin ports are explicit-required; kernel ports are reserved

**The gap.** The first avatar template put the `llama` server on `:8080`. But the
kernel image already binds `:8080` for `cosmon-rpp-adapter` (the RPP ingress
door). Two processes cannot bind the same port. The clash was invisible until a
real co-residence (kernel + a server-plugin) was attempted, because the template
carried `:8080` as an *implicit default* inherited from a standalone-llama
example.

**The resolution.**

1. **A port-binding plugin's `port` is an always-explicit, required field** in
   `distribution.toml`. There is **no implicit `:8080` default.** A profile that
   omits the port of a server-plugin is rejected by the assembler, not silently
   defaulted. (Smithy's profile already moved llama to `:8081` — this makes
   that the rule, not a local patch.)

2. **The kernel declares its reserved ports** in `[kernel.reserved_ports]`. The
   canonical reservation today:

   | Port | Owner | Why it is reserved |
   |------|-------|--------------------|
   | `8080` | `cosmon-rpp-adapter` | The §8j HTTPS+OIDC ingress — cosmon's central secure-delivery door (ADR-080 / ADR-117). Bound by the kernel image's PID 1 whenever the RPP is part of the deployment. |

3. **The assembler validates port disjointness** at build time: every plugin
   `port` must differ from every reserved kernel port and from every other
   plugin port. A collision is a build error with a named conflict, never a
   runtime "address already in use".

The principle: a kernel that co-resides with an HTTP adapter has *already spent*
`:8080`. Making plugin ports explicit means a server-plugin can never silently
inherit a port the kernel owns. Wiring you can see is wiring you cannot clash.

## 4. FLAG 2 — the canonical supervision model: server-plugins are compose siblings

**The gap.** The kernel image already runs `cosmon-rpp-adapter` as **PID 1**.
Running `llama-server` *alongside* it inside the same image needs either (a) a
mini multi-process supervisor as the image `CMD`, or (b) llama as a separate
service. The kernel/plugin model said "separate processes in ONE image" but never
said which of (a)/(b) is canonical when the image already has an adapter at PID 1.

**The resolution — the canonical model is (b): compose siblings.**

> A server-plugin (an S1 process that binds a port) runs as its **own sibling
> service in the compose stack**, on the deployment's private network. The kernel
> **image stays single-process** — one PID 1, the cosmon ingress server. The
> kernel reaches the plugin over `localhost:<port>` (compose network), exactly
> the S1 contract (a `base_url` the adapter POSTs to).

This is canonical for three reasons that are not aesthetic:

- **It honours the no-daemon / stateless-image discipline.** An in-image
  multi-process supervisor (s6, supervisord, a hand-rolled `CMD` fan-out) is a
  small init system — a daemon whose job is to keep other daemons alive. Baking
  one into the kernel image re-introduces exactly the supervision complexity the
  stateless-CLI architecture refuses (CLAUDE.md "never introduce a daemon
  here"). Compose already *is* the process supervisor; we do not ship a second
  one inside the image.
- **It matches what the deployment already does.** Forgejo is already a compose
  sibling of the avatar runtime, not a process inside the kernel image. A
  server-plugin is the same shape — one more sibling service, not a new kind of
  thing.
- **It keeps inference in-VM / sovereign either way.** A compose sibling lives
  on the same VM, same private network; nothing leaves the deployment boundary.
  Sovereignty is a property of the *network boundary*, not of the *process
  table* — co-residence in one image buys no sovereignty that a sibling service
  on the same VM does not already have.

**The one explicitly non-canonical escape hatch.** A downstream distribution
assembler that genuinely cannot run a compose stack (a single-image target with
no orchestrator) MAY use `supervision = "in-image"` and provide its own minimal
supervisor as the image `CMD`. This is a **distribution-assembler's choice,
outside cosmon's shipped closure** — the same directionality as ADR-132 §2d's
"code-linking is a distribution-builder's act." Cosmon neither ships nor blesses
an in-image supervisor; it is named here so the field is honest, not so it is
recommended. **Default and recommendation: `compose-sibling`.**

So the two valid moves, named the way the operator-communication discipline
names valid moves:

| Move | When | What the image looks like |
|------|------|---------------------------|
| **compose-sibling** (canonical) | always, unless an orchestrator is genuinely unavailable | single-process kernel image + sibling plugin service(s) on the compose network |
| **in-image** (downstream, non-canonical) | single-image target, no compose | kernel image whose `CMD` is a minimal supervisor the *assembler* supplies; cosmon ships none |

## 5. FLAG 3 — `cosmon-rpp-adapter` is kernel-side, not a plugin

**The gap.** `cosmon-rpp-adapter` (and the `claude` binary) are baked *with* the
kernel in the cosmon-server image. In ADR-132 vocabulary: is rpp-adapter
kernel-side (the kernel's own adapter surface) or a plugin that should carry its
own `[[plugins]]` line? If it is conceptually a plugin, a profile with no plugin
line for it "lies by omission."

**The resolution — `cosmon-rpp-adapter` is kernel-side. It is not a plugin.**

The word *adapter* is overloaded across cosmon, legitimately, because there are
**two different Ports** that each have adapters (both are hexagonal Layer-B port
adapters, ADR-023):

| | **Worker-spawn Port** (ADR-079) | **Ingress Port** (§8j / ADR-080) |
|--|--------------------------------|----------------------------------|
| Direction | *egress* — spawns the executor that advances a molecule | *ingress* — admits a remote pilot's request into the DAG |
| Adapter example | `claude.rs` (launches the Claude Code CLI) | `cosmon-rpp-adapter` (the RPP HTTPS+OIDC door) |
| Ships where | in the kernel; the *substrate* (claude binary) is external | **in the kernel** (`cosmon-rpp-adapter` is an AGPL `members` crate) |

`cosmon-rpp-adapter` is the **ingress** adapter. It is part of the published AGPL
kernel closure (a `members` crate, SPDX `AGPL-3.0-only`), it is cosmon's central
secure-delivery capability (ADR-117), and it is bound by the kernel image's PID 1.
Therefore:

- It is **not a catalog plugin.** It does not integrate over S1–S4; it *is* the
  kernel's own §8j boundary. It gets **no `[[plugins]]` line**, and the manifest
  does **not** lie by omission — a plugin line would be the lie, asserting an
  out-of-kernel artifact where there is an in-kernel one.
- It is **not a worker-spawn Adapter** (ADR-079). Same word, different Port. The
  ADR-079 four-obligation Adapter contract is about spawning executors; the RPP
  is about admitting ingress. Do not conflate them.
- Its port is therefore a **reserved kernel port** (§3), which is exactly why
  the avatar's llama plugin had to move off `:8080`. FLAG 1 and FLAG 3 are two
  faces of one fact: the kernel owns `:8080` because the kernel — not a plugin —
  ships the thing bound to it.

**The `claude` binary, separately.** The `claude` binary baked alongside is the
*external substrate* of the default worker-spawn Adapter (ADR-079) — not a cosmon
crate, not a catalog plugin, not a kernel crate. It is a baked **runtime
dependency** of the kernel image's worker-spawn path. A distribution may bake it
or not: the local-default container deliberately ships **no** `claude` binary so
the local-inference floor is true by construction
(`docker/local-default-container/Dockerfile`). For an avatar whose inference is
the `llama` plugin and whose worker-spawn adapter is reconfigured accordingly,
the `claude` binary need not be baked at all. The distribution profile may, in a
future schema revision, name baked substrate binaries explicitly; today they are
a property of the kernel image recipe, not of the profile. Note that
`cosmon-rpp-adapter` is **never** baked substrate — it is kernel (§5b).

## 5b. FLAG 6 — a distribution embeds the WHOLE kernel, never a selection (ADR-132 §6 F6)

**The gap.** The POC distribution `Containerfile` built **only `cs`**. The
resulting image was missing `cosmon-rpp-adapter` — the kernel's own §8j ingress
surface — and so could not serve the cosmon-server HTTP contract (`/api/healthz`
+ the tenant API on `:8080`). The image had silently shipped *part* of the kernel
and still called itself a cosmon-server distribution.

**The resolution — the kernel is indivisible.**

> A distribution image **builds and bakes the entire kernel — every one of its
> binary surfaces (`cs`, `cosmon-rpp-adapter`, and any future kernel binary) —
> never a hand-picked subset.** The kernel is one block, pinned once by
> `[kernel].rev`. A `[[plugins]]` entry is a process-separated addition *around*
> that block; it is never a knob for swapping a kernel surface in or out. **An
> image either bakes the whole kernel or it is not a cosmon-server
> distribution.**

This sharpens the manifest taxonomy into exactly three classes:

| Class | Declared by | What it is | Example |
|-------|-------------|-----------|---------|
| **Kernel** | `[kernel].rev` (one pin) | The whole 43-crate block — **including `cosmon-rpp-adapter`** | `cs`, `cosmon-rpp-adapter` |
| **Plugin** | `[[plugins]]` | Process-separated addition *around* the kernel, over a seam S1–S4 | `llama` |
| **Baked substrate** | `[[baked_substrate]]` | A baked binary **genuinely outside the kernel** — neither a `members` crate nor a plugin | `claude` |

**`cosmon-rpp-adapter` is therefore NOT baked substrate.** A *kernel* binary
baked *because it is part of the kernel* is kernel-delivered-with-the-kernel —
its provenance is already the kernel pin. Listing it under `[[baked_substrate]]`
would double-count a kernel surface as an external dependency. FLAG 3 and FLAG 6
are one fact seen twice: rpp-adapter is kernel-side, so the kernel commit *is*
its provenance — it carries no `[[plugins]]` line (FLAG 3) **and** no
`[[baked_substrate]]` line (FLAG 6).

**Corollary — a crate's version number is NOT a freshness signal.** The avatar
POC built an image whose `cosmon-rpp-adapter` reported `version 2.2.0` even though
the JWKS-fetch had already landed; the version field had simply never been bumped
(corrected to the `2.5.0` series in `task-20260627-9e4d`). A version string is a
*label a human maintains*, not a *measurement of the bytes*. **The reliable,
falsifiable signal that a pinned kernel tree contains the JWKS-fetch is the
presence of the file `crates/cosmon-rpp-adapter/src/jwks_fetch.rs`** in that tree
— not the adapter's reported version. When pinning a kernel commit for a
distribution, grep for that file; do not trust the version number.

## 6. Summary — the flags, resolved

| Flag | Question | Resolution |
|------|----------|-----------|
| **1 — port wiring** 🔴 | Should a server-plugin's port be always-explicit, no implicit `:8080`? | **Yes.** `port` is required for any port-binding plugin; the kernel declares `[kernel.reserved_ports]` (rpp-adapter `:8080`); the assembler validates disjointness at build time. |
| **2 — supervision** 🟠 | Multi-process supervisor in one image, or compose sibling? | **Compose sibling is canonical.** The kernel image stays single-process; server-plugins are sibling services. `in-image` is a non-canonical downstream escape hatch cosmon does not ship. |
| **3 — rpp-adapter placement** 🟡 | Is `cosmon-rpp-adapter` kernel-side or a plugin? | **Kernel-side.** It is the §8j ingress Port adapter (ADR-080/ADR-117), an AGPL kernel `members` crate — not a catalog plugin, not a worker-spawn Adapter. No `[[plugins]]` line; no omission-lie. Its `:8080` is a reserved kernel port. |
| **6 — whole-kernel rule** 🟢 | Does a distribution bake the whole kernel or a selection? | **The whole kernel, always.** An image bakes every kernel binary surface (`cs` + `cosmon-rpp-adapter` + …), pinned once by `[kernel].rev`; plugins are process-separated additions *around* it. `cosmon-rpp-adapter` is kernel, **not** `[[baked_substrate]]`. A version number is not a freshness signal — `jwks_fetch.rs`'s presence is. |

## 7. References

- [ADR-132](adr/132-kernel-plugin-catalog-ecosystem.md) — kernel / plugin
  catalog model, the four seams S1–S4, §8 (this doc's parent, the distribution
  deployment shape).
- [ADR-079](adr/079-worker-spawn-port-and-adapter-contract.md) — the worker-spawn
  Port and its Adapter contract (the *other* adapter).
- [ADR-080](adr/080-remote-pilot-port-https-oidc.md) /
  [ADR-117](adr/117-rpp-central-security-capability.md) — the RPP ingress
  adapter (the kernel-side adapter of FLAG 3).
- `docs/architectural-invariants.md` §8j — ingress bindings; every Port is one.
- [`examples/avatar-aria.distribution.toml`](../examples/avatar-aria.distribution.toml)
  — the canonical worked profile (kernel + llama, explicit ports).
