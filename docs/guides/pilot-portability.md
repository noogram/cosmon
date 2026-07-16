# Pilot portability — the minimal contract, the context-pack decision, and the 3-access-mode test matrix

> **Axis 2 (Aperture) made concrete.** This guide is the portability deliverable
> of `task-20260615-c643`, child of `delib-20260615-73f9` (the Valence/Aperture
> two-axis model, [ADR-125](../adr/125-valence-and-aperture-two-axes.md)).
> **Valence** = *who does the work* (the execution adapter). **Aperture** =
> *who talks to cosmon* (the pilot surface). This document is entirely about the
> Aperture axis: what the smallest harness must know and do to pilot cosmon in
> natural language, across **three access modes** — `cs` local CLI,
> `cosmon-remote` CLI, and direct rpp-v1 REST.
>
> The two axes are **structurally decoupled** (a property of the code's shape, not
> an aspiration): the piloting harness is provably *not* an input to
> `resolve_adapter_selection`. See ADR-125 §4 and `responses/turing.md` for the
> falsification. Nothing in this guide couples the aperture you pilot through to
> the valence that executes.

Three parts:

1. **The minimal pilot contract** — the irreducible set a harness must *know* and
   *do*, per surface (the subtraction test applied).
2. **The context-pack decision** — how a foreign harness is taught the surface:
   point at existing artifacts first; generate only if they drift.
3. **The 3-access-mode portability matrix** — a runnable verb × access-mode grid
   with a concrete one-line gesture and an observable pass criterion per cell,
   plus the harness-applicability row.

---

## Part 1 — The minimal pilot contract

A *harness* (codex, opencode, aider, gemini-cli, claude-code, or a bare shell
script) **pilots** cosmon when it can drive the molecule lifecycle in natural
language. The contract is the smallest set it must **KNOW** (the mental model)
and **DO** (the verbs), after every droppable item has been subtracted.

The contract splits because the two surfaces sit at different positions in the
dependency stack:

- **`cs` (local)** — git-like, stateless, files on disk are truth. The pilot
  owns the *full* lifecycle, **including teardown**.
- **`cosmon-remote` / rpp-v1 (remote)** — the §8p frozen wire surface. The
  *server* owns teardown; the pilot owns intent and result-fetch. There is **no
  client `done`**.

### 1.1 The irreducible spine — MANDATORY verbs

**`cs` local — four writes + one read:**

| Verb | Role | Droppable? |
|------|------|-----------|
| `cs nucleate <formula> --kind <k> --var topic=…` | create the molecule (intent → entity) | **No** — nothing else creates a molecule. |
| `cs tackle <id>` | Inert→Propelled: spawn one worker (worktree + transport + adapter). **Human-only.** | **No** — sole sanctioned spawn route (ADR-079 §4). |
| `cs wait <id> &` | block-in-background until terminal | **Only if replaced** — it is the *correct* primitive; hand-polling is the anti-pattern. |
| `cs done <id>` | Propelled→Inert: merge + teardown. **Human-only.** | **No** — without it the branch never merges and the worktree/tmux leak. Workers cannot call it. |
| `cs observe <id> --json` | single-molecule state dump | the minimal **read** the loop parses. |

Local spine: **nucleate → tackle → wait → done**, with **observe** as the read.
Four writes, one read. Everything else is optional.

**Remote (`cosmon-remote` CLI) — the fused, shorter spine:**

| Verb | Role | Maps to local |
|------|------|---------------|
| `cosmon-remote auth login --email <you>` | pose the worker badge (**once**) | no local analogue — local trusts the ambient install |
| `cosmon-remote do <formula> --topic … --kind …` | one gesture: nucleate + tackle + follow-until-ready | **fuses** `nucleate`+`tackle`+`wait` (a client-side composition of `POST /v1/molecules` and `POST /v1/molecules/{id}/tackle`; zero new routes) |
| `cosmon-remote molecule result <id>` | fetch the deliverable | reads the data plane remotely (no local worktree to `cat`) |

**The load-bearing per-surface delta is teardown.** On `cs` local, `cs done` is
a *mandatory pilot verb* (human-only). On the remote surface there is **no
`done`** — teardown is server-side, and the structural absence of a destroy verb
*enforces* the human-only invariant. The remote steady-state spine is just
**`do` → `result`** (after a one-time `auth login`).

### 1.2 Invariants the harness MUST respect

Not verbs — rules whose violation breaks the system *silently* (the worst class):

1. **CLI-first for workers.** A *worker* (the process the harness spawns inside
   the worktree) uses `cs evolve` / `cs complete` / `cs observe` via walk-up
   discovery from its cwd — never MCP, never `cosmon_*` tools. The *pilot* uses
   the spine above. Do not confuse the roles: the pilot tackles; the worker
   self-advances. (`cs mcp`, the local stdio MCP server, was **removed**
   2026-07-12 (decision C14) — git has no MCP; LLMs drive git via shell, same
   here. The *remote* `/mcp` endpoint served by `cosmon-rpp-adapter` is a
   separate, live surface — not this local command.)
2. **`cs done` is human-only.** Workers cannot self-destroy. A pilot standing in
   for the human MAY call it; a worker process MUST NOT. On the remote surface
   this is enforced structurally — there is no teardown verb to misuse.
3. **`cs tackle` is always one node.** It spawns exactly one leaf worker; it does
   not walk a DAG. Walking N≥1 nodes is `cs run`'s job only. A pilot that wants a
   DAG uses `cs run` in a detached session — it does **not** loop `cs tackle`.
4. **Never block the pilot session.** `cs wait <id> &` in background; never
   foreground `cs run`; never `watch cs observe` / shell poll loops; never
   `tmux attach` to a worker. On remote, `do` follows for you — do not poll
   `events` in a hot loop to simulate it.
5. **`--json` is on every command.** A harness parses JSON, not human prose.
   Knowing this exists is part of the contract; using it is the only robust way
   to parse state.
6. **Frozen-surface + formula-opacity (remote only).** The rpp-v1 surface is
   §8p-frozen. Treat the subcommand/route set as fixed and **do not depend on
   formula internals** — *"this help documents the call, never the formula."*
   What a formula *does* is content of the targeted deployment, discovered on the
   instance, **not** part of the frozen API. An unknown formula is refused with
   `404`/`409`. A pilot that hard-codes a formula's step structure has coupled to
   private state.
7. **Two badges on remote.** The **API badge** (JWT, scopes
   `cosmon:molecule:read|write`, `cosmon:worker:spawn`, `cosmon:events:subscribe`)
   and the **worker badge** (the Anthropic/Claude credential posed via
   `auth login`) are *independent*. Without the worker badge a tackled worker has
   nothing to spend. `tackle` requires **both** `:write` AND `:worker:spawn` — a
   `:write`-only token gets `403`.
8. **Effect markers are derived, trust them.** `[coûteux]` / `[irréversible]`
   are derived from the OAuth scope a route requires, not hand-written. A pilot
   gates costly operations by reading the marker, not by re-deriving cost from
   prose. (`tackle` is `[coûteux]` — every success burns real Anthropic credit.)

### 1.3 Required KNOWLEDGE — the mental model

A verb checklist without the model produces a pilot that issues correct calls in
the wrong order. The irreducible knowledge:

1. **The lifecycle states.** `Pending/Inert → Active/Propelled → Completed →
   (harvested via done) → Inert`, plus terminal `stuck` / `collapse`. `tackle`
   only moves Inert→Propelled; `done` only Completed→Inert. Knowing the order is
   what lets the pilot *not* hand-poll.
2. **Control plane vs data plane.** The DAG carries ~1 bit per molecule
   (done/not-done); the filesystem carries all content. **There are no
   mailboxes.** A pilot does not message a molecule — it reads `MOLECULE_DIR`
   artifacts (`synthesis.md`, `responses/`, `briefing.md`) from disk (local) or
   via `molecule result` / `GET …/artifacts` (remote). This is *the* model that
   explains why `wait` works (it reads the bit) and why output lands on disk.
3. **Worktree vs state-dir.** The worktree (`<repo>/.worktrees/<mol>`) is where
   the worker runs and where `cs` resolves via walk-up; `.cosmon/state/` is the
   authoritative JSON. The pilot tackles from the repo, the worker lives in the
   worktree, content merges back via `cs done`. A **remote** pilot has *neither*
   exposed — it only sees `molecule result`. Knowing this asymmetry stops a
   remote pilot from trying to `cat` a worktree that does not exist on its
   machine.

### 1.4 What can be DROPPED (the minimization)

Everything below is reachable-but-not-required for a single-molecule, happy-path
pilot:

- **Droppable local verbs:** `cs peek`, `cs ensemble`, `cs reconcile`, `cs tag`,
  `cs freeze`/`thaw`, `cs collapse`/`stuck`, `cs patrol`, `cs run`.
- **Droppable remote diagnostics** (the man page tags these *"(diagnostic) —
  when something breaks, not before"*): `healthz`, `quota`, `workers`, `noyaux`,
  `events`, `doctor`, `auth me`, `converse`. (`events` SSE is a *richer*
  alternative to `do`'s follow, not a requirement; `converse` is a separate
  capability — talking to a bound avatar — not part of the dispatch spine.)
- **Droppable knowledge:** `temp:*` tags (backlog curation only), the
  artifact-chain seal mechanism (`prompt_seal`/`briefing_seals` — a verifier's
  concern), the whisper channel (human-pilot-only, advisory, Propelled-only).

**The irreducible core after full subtraction:**

```
LOCAL pilot (minimum viable):
  KNOW:  lifecycle order (nucleate→tackle→wait→done) ;
         data lands on disk in MOLECULE_DIR ; workers self-evolve, never call done
  DO:    cs nucleate <formula> --kind <k> --var topic=…
         cs tackle <id>
         cs wait <id> &            # background ; never foreground-block
         cs done <id>              # human-only, mandatory to merge + teardown
         cs observe <id> --json    # the one read it parses

REMOTE pilot (minimum viable):
  KNOW:  two badges (API + worker) ; teardown is server-side (no done) ;
         formula internals are NOT part of the frozen surface
  DO:    cosmon-remote auth login --email <you>          # once
         cosmon-remote do <formula> --topic … --kind … --json
         cosmon-remote molecule result <id> --json
```

If a harness can satisfy this checklist it can pilot cosmon in NL. Everything
not on these two blocks is debt the pilot author can defer until a concrete use
case demands it.

---

## Part 2 — The context-pack decision (tension T2)

**Question.** How is a foreign harness *taught* the surface above? Two options:

- **(a) Point harnesses at existing artifacts** — `man cs` /
  `man cosmon-remote` / `crates/cosmon-rpp-adapter/openapi/v1.yaml`, plus the
  curated CLAUDE.md sections (*Pilot patterns*, *Communication Model*,
  *Monitoring*, *Command perimeters*) and this guide. **Zero new code.**
- **(b) A generated `cs pilot-pack --surface cs|remote` projection** that
  assembles the live man page + curated CLAUDE.md sections + `.cosmon/skills`
  index + worked examples, framed for the target harness's prompt slot
  (`AGENTS.md` / `GEMINI.md` / `CLAUDE.md` / aider conventions) — a
  `cs reconcile`-style projection, drift-free by construction.

### Decision: **start at (a). Build (b) only if (a) demonstrably drifts.**

**Rationale.**

- **Composability.** CLAUDE.md's principle is *"before adding a command, ask: can
  this be a formula? … resist single-step commands."* Option (a) adds **no
  extension point** — it reuses Markdown + man pages + ADR-042 skills that
  already exist. A `cs pilot-pack` command is a new perimeter; it must earn its
  keep against a demonstrated drift problem, not a hypothetical one.
- **The HTTP context-pack already exists, for free.** For the shell-less /
  HTTP-only harness, `openapi/v1.yaml` *is already* the context-pack — a
  machine-readable, versioned, drift-gated description of every route. There is
  nothing to generate.
- **Per-surface drift risk is already managed.** `man/cs.1` is the clap tree
  rendered; `man/cosmon-remote.1` is **canon-projected** from
  `crates/cosmon-remote/src/canon.rs` (`ROUTES_USED`) and pinned by the
  surface-bijection test, so the remote man page *cannot* drift from the wire.
  Pointing at these is pointing at the source of truth, not a paraphrase.
- **The trigger for (b) is observable, not speculative.** Build the projection
  *only* when manual assembly proves to drift or to be too much friction in
  practice — e.g. operators repeatedly hand-assemble the same four files and they
  fall out of sync, or a harness needs the four artifacts framed into one prompt
  slot often enough that copy-paste becomes a maintenance seam. Until then, (a)
  honors *"resist new commands"* and costs nothing.

**MCP is not a third option — it is dominated.** Every named harness shells, so
the CLI is the tool; for the no-shell case, the harness speaks HTTP and hits
rpp-v1 directly. **HTTP-capable ⊃ MCP-capable**, so the last edge case for MCP
(no-shell-but-MCP-capable) is dominated by no-shell-but-HTTP-capable → direct
REST. The falsifier *"does AXE-3 ever need MCP the CLI/REST can't give?"* is
**NO**: every candidate (streaming, conversational channel, tool discovery,
no-shell harness, session state) resolves to a frozen verb (`events` SSE,
`converse`, `--json`) or to direct REST. The local `cs mcp` stdio server is
gone (removed 2026-07-12, C14) — the remote-tenant `/mcp` endpoint is a
distinct RPP surface, not a `cs` verb. (Full falsification: ADR-125 §5,
`responses/architect.md` §3.)

**Per-surface pack contents (whichever option is in force), for reference:**

| Pack | Verb truth (source) | Cognition truth | Auth / cost model |
|------|---------------------|-----------------|-------------------|
| **`cs` local** | `man/cs.1` (clap tree) / `cs help --json` | CLAUDE.md *Pilot patterns* + *Monitoring* + *Command perimeters*; `.cosmon/skills/*` | — (ambient install) |
| **`cosmon-remote`** | `man/cosmon-remote.1` (canon-projected, frozen) | the §8p golden path; formula-opacity rule | two-badge model + effect markers (`[coûteux]`/`[irréversible]`) |
| **direct rpp-v1 REST** | `crates/cosmon-rpp-adapter/openapi/v1.yaml` (*is already* the pack) | the same §8p / formula-opacity rules in prose | `bearerAuth` JWT + the scope table |

---

## Part 3 — The 3-access-mode portability matrix

**What this matrix proves.** Strip away the harness personalities: each harness
(codex, opencode, aider, gemini-cli) reduces, *for piloting*, to one capability —
**can it issue a gesture (shell command or HTTP request) and observe the
result?** All four can. So piloting is **behavioral**: issue the gesture, observe
the proof. The internal mechanism of the harness is irrelevant — which is *why*
the gesture string is identical across all four harness rows. The only
per-harness variable is the NL prompt that makes the harness emit the gesture.

So the matrix is given as **verb × access-mode** (the falsifiable instrument),
followed by a compact **harness-applicability** row showing all four clear the
same bar. Duplicating one gesture four times would be noise, not signal.

**The three access modes:**

| Mode | Transport | Locality | Context-pack |
|------|-----------|----------|--------------|
| **M1 — `cs` local CLI** | shell | local; files on disk are truth | `man cs` + curated CLAUDE.md |
| **M2 — `cosmon-remote` CLI** | shell | remote; thin client over rpp-v1 | `man cosmon-remote` (canon-projected) |
| **M3 — direct rpp-v1 REST** | HTTP (shell-less) | remote | `openapi/v1.yaml` *is* the pack |

> **The remote modes do NOT map 1:1 to the local verbs.** `cosmon-remote do`
> *fuses* nucleate+tackle+wait; standalone `wait` becomes the `GET /v1/events`
> SSE stream (client decides terminality); **there is no client `done`**
> (teardown is server-side). Every cell where the map differs is marked **⚠**.

REST gestures assume the server base `https://rpp.<domain>` and a validated JWT
in `$JWT` (`Authorization: Bearer $JWT`). The matrix covers the **dispatch
spine** only (nucleate/tackle/wait/done/observe); avatar `converse` and the
diagnostic ladder are out of scope per the minimal contract (§1.4).

### 3.1 Verb × access-mode

#### nucleate — *create the molecule (intent → entity)*

| Mode | One-line gesture | Pass criterion (observable proof) |
|------|------------------|-----------------------------------|
| **M1 `cs`** | `cs nucleate task-work --kind task --var topic="portability probe" --json` | exit 0; stdout JSON carries `molecule_id` (e.g. `task-20260615-xxxx`); on disk `.cosmon/state/.../molecules/<id>/prompt.md` exists with a `PromptSealed` event |
| **M2 `cosmon-remote`** | `cosmon-remote molecule nucleate task-work --kind task --topic "probe" --json` | exit 0; JSON returns server-side molecule id. **The granular route — the "advanced" path; the headline verb is fused `do` (§3.2).** |
| **M3 REST** | `curl -fsS -X POST https://rpp.$DOMAIN/v1/molecules -H "Authorization: Bearer $JWT" -H 'content-type: application/json' -d '{"formula":"task-work","kind":"task","variables":{"topic":"probe"}}'` | `201 Created`; `Location: /v1/molecules/<id>` header; body is a `MoleculeEnvelope` with `molecule.id`+`kind`+`status`. **⚠ does NOT auto-tackle** (ADR-080 §5.1) — REST nucleate maps cleanly to `cs nucleate`, *not* to `do`. Needs scope `cosmon:molecule:write`. |

#### tackle — *Inert→Propelled, spawn one worker* `[coûteux]`

| Mode | One-line gesture | Pass criterion |
|------|------------------|----------------|
| **M1 `cs`** | `cs tackle <id> --json` | exit 0; a git worktree `.worktrees/<id>` appears; a tmux session is created (or in-process loop for the `local` adapter); `events.jsonl` gains `AdapterSelected` + worker-spawn lines; status → `Active` |
| **M2 `cosmon-remote`** | `cosmon-remote molecule tackle <id> --json` | exit 0; marked **`[coûteux]`** — burns real credit; proof = a `quota` decrement + a worker in `cosmon-remote workers list` |
| **M3 REST** | `curl -fsS -X POST https://rpp.$DOMAIN/v1/molecules/<id>/tackle -H "Authorization: Bearer $JWT"` | `200`; body is a `TackleEnvelope` with `tackle.molecule_id` + `tackle.worker_session` + `tackle.spawned_at`. **⚠ requires BOTH scopes `cosmon:molecule:write` AND `cosmon:worker:spawn`** (a `:write`-only token gets `403`); every success burns Anthropic credit (one `anthropic.call` audit event) |

#### nucleate + tackle + wait FUSED (`do`) — *remote modes only* ⚠

| Mode | One-line gesture | Pass criterion |
|------|------------------|----------------|
| **M1 `cs`** | *(no equivalent)* | — local keeps the three verbs separate. |
| **M2 `cosmon-remote`** | `cosmon-remote do task-work --topic "probe" --kind task --json` | exit 0; **single gesture fuses nucleate+tackle+follow-until-ready** (composition of the two POSTs, zero new routes); the credit guard is shown once before first spend. **⚠ This is the remote surface's headline verb — no `cs`-local equivalent.** |
| **M3 REST** | *(no single route — compose the two POSTs above, then read `GET /v1/events`)* | the fusion is a **client-side composition**; at the wire there are only the granular routes. An HTTP harness implements `do` itself by chaining `POST /v1/molecules` → `POST …/tackle` → `GET /v1/events`. |

#### wait — *block until terminal* ⚠ (map differs on both remote modes)

| Mode | One-line gesture | Pass criterion + how the map differs |
|------|------------------|--------------------------------------|
| **M1 `cs`** | `cs wait <id> --json &` | process blocks, returns exit 0 only when the molecule reaches a terminal state (`Completed`/`Stuck`/`Collapsed`); returned JSON shows the terminal status. **Proof: it returns only *after* a status transition, never on a fixed timer.** |
| **M2 `cosmon-remote`** | `cosmon-remote events --json` (SSE) **or** let `do` block | exit-0 stream; each lifecycle transition arrives as an SSE line. **⚠ there is no standalone `wait` route** — you either let `do` block, or read the event stream and decide terminality client-side |
| **M3 REST** | `curl -fsS -N https://rpp.$DOMAIN/v1/events -H "Authorization: Bearer $JWT"` (SSE, `-N` = no buffering) | `200` `text/event-stream`; a `molecule.state_changed` event arrives with `{old_state,new_state}` reaching a terminal `new_state`. **⚠ live tail, no replay** — history is on the filesystem, not re-served; reconnect with `Last-Event-ID:<n>` filters `id<=n` but does not back-fill. Needs scope `cosmon:events:subscribe`. The client decides terminality. |

#### observe — *read current state / fetch deliverable* ⚠ (splits on remote)

| Mode | One-line gesture | Pass criterion + how the map differs |
|------|------------------|--------------------------------------|
| **M1 `cs`** | `cs observe <id> --json` | exit 0; JSON state dump with `status` + `current_step`; **idempotent** (twice = same answer; re-running does not append to `events.jsonl`) |
| **M2 `cosmon-remote`** | live: `cosmon-remote molecule get <id> --json` · terminal deliverable: `cosmon-remote molecule result <id> --json` | exit 0; current state, or the finished artifact. **⚠ "observe" splits into a *state read* (live) and a *result/artifact fetch* (terminal)** |
| **M3 REST** | state: `curl -fsS https://rpp.$DOMAIN/v1/molecules/<id> -H "Authorization: Bearer $JWT"` · artifacts: `curl -fsS https://rpp.$DOMAIN/v1/molecules/<id>/artifacts -H "Authorization: Bearer $JWT"` | `200`; `MoleculeView` with `id`+`kind`+`status` (idempotent, scope `cosmon:molecule:read`), or the artifact list/blob. **⚠ same split as M2** — there is no local worktree to `cat`; the data plane is read over HTTP |

#### done — *Propelled→Inert, merge + teardown* ⚠ (the sharpest divergence)

| Mode | One-line gesture | Pass criterion + how the map differs |
|------|------------------|--------------------------------------|
| **M1 `cs`** | `cs done <id> --json` | exit 0; branch merged (`git log --first-parent main` shows the molecule's commits); worktree removed; tmux gone; molecule harvested → Inert. **Human-only.** Proof of teardown = the *absence* of `.worktrees/<id>` afterward |
| **M2 `cosmon-remote`** | **No client gesture.** | **⚠ there is no `cosmon-remote done`.** Teardown is server-side. The client's terminal observation is `molecule result <id>` returning a finished artifact, or a terminal line on the `events` stream |
| **M3 REST** | **No route.** | **⚠ there is no `done` route in `openapi/v1.yaml`.** The §8p frozen surface deliberately withholds the human-only destroy verb from the remote client — merge/worktree teardown happens on the noyau, invisible to the wire. The decoupling is structural: the absence of a verb enforces the human-only invariant. |

### 3.2 Harness applicability (the four rows clear the same bar)

The matrix is **uniform across harnesses** precisely because piloting never
inspects the harness. Each harness needs exactly one capability: emit the gesture
(shell or HTTP) and observe the result.

| Harness | Emits shell gesture? (M1/M2) | Emits HTTP gesture? (M3) | NL prompt that produces it (example) | Verdict |
|---------|:---------------------------:|:------------------------:|--------------------------------------|---------|
| **codex** | yes (shell tool) | yes (shell `curl`, or its HTTP tool) | "run `cs nucleate task-work --kind task --var topic=… --json` and give me the id" | drives all 3 modes |
| **opencode** | yes (shell/bash tool) | yes (`curl` via shell) | same | drives all 3 modes |
| **aider** | yes (`/run` shell command) | yes (`/run curl …`) | "/run cs tackle <id> --json" | drives all 3 modes |
| **gemini-cli** | yes (shell execution) | yes (`curl` via shell) | same | drives all 3 modes |

The uniformity is itself the evidence for decoupling (ADR-125 §4): the gesture
string carries no information about who emitted it, so the piloting harness
**cannot** leak into adapter selection — there is no parameter through which such
a leak could travel.

### 3.3 The 1:1-break summary (read this if you read nothing else)

| Local verb | `cosmon-remote` CLI (M2) | direct REST (M3) |
|------------|--------------------------|-------------------|
| `cs nucleate` | `molecule nucleate` *or* fused into `do` | `POST /v1/molecules` (⚠ **no auto-tackle**) |
| `cs tackle` | `molecule tackle` *or* fused into `do` | `POST /v1/molecules/{id}/tackle` `[coûteux]`, ⚠ needs `:write`+`:worker:spawn` |
| *(none)* | **`do`** = nucleate+tackle+wait fused (headline) | client-side composition of the POSTs |
| `cs wait &` | `events` SSE *or* `do`'s blocking follow | `GET /v1/events` SSE (⚠ no replay, client decides terminality) |
| `cs observe` | `molecule get` (live) + `molecule result` (terminal) | `GET /v1/molecules/{id}` + `GET …/artifacts` |
| `cs done` | **⚠ none** (server-side teardown) | **⚠ none** (no route — invariant enforced by absence) |

---

## Grounding (relative to repo root)

- `docs/adr/125-valence-and-aperture-two-axes.md` — the ratified two-axis model;
  §4 decoupling, §5 the MCP-dominated portable surface.
- `.cosmon/state/.../molecules/delib-20260615-73f9/synthesis.md` (C2, T2) and
  `responses/{turing,tolnay,architect}.md` — the deliberation this guide
  operationalizes (turing's two-surface matrix, widened here to three access
  modes; tolnay's minimal contract; architect's context-pack).
- `crates/cosmon-rpp-adapter/openapi/v1.yaml` — the rpp-v1 REST surface: routes,
  request/response envelopes, scope table, SSE wire format, `bearerAuth` scheme,
  server `https://rpp.{domain}`. *Is already* the M3 context-pack.
- `crates/cosmon-remote/man/cosmon-remote.1` — the M2 canon-projected, frozen
  surface: `do` fusion, two-badge auth, effect markers, formula-opacity.
- `crates/cosmon-cli/man/cs.1` + `CLAUDE.md` — the M1 surface and the pilot
  patterns / monitoring / command-perimeter cognition.
- `docs/adr/079-worker-spawn-port-and-adapter-contract.md`,
  `docs/adr/099-dispatch-site-stability.md`,
  `docs/adr/119-adapter-exit-code-contract.md` — the Valence-axis parity bar
  (orthogonal to this guide; cited for the decoupling claim).
