# cosmon-pilot — manual smoke test

The Definition of Done for the cs-pilot walking skeleton
(`task-20260531-67f5`, delib `2026-05-31-cs-pilot-external-cognitive-pilot`)
requires **one molecule observed end-to-end via a tool call against a local
model**. The automated `tests/repl_end_to_end.rs` proves the loop wiring
with a *scripted* provider; this manual smoke proves it against a **real
local Ollama model** that emits native `tool_calls`.

## Prerequisites

- Ollama running on `http://localhost:11434` with a model that supports
  **native OpenAI tool-calling** through its `/v1/chat/completions`
  endpoint. Verified emitters (2026-06-01): `qwen3:8b`, `qwen2.5:32b`,
  `llama3.2`. **Non-emitters** (they narrate the call as plain text and the
  harness sees a `Turn::Stop`, so no dispatch happens): `qwen2.5-coder:7b`,
  `qwen2.5-coder:14b`. Pick an emitter.
- A cosmon project on disk reachable by walk-up from the working directory
  (a `.cosmon/state/` with at least one molecule).

## Procedure

```sh
cargo build -p cosmon-pilot --bin cosmon-pilot

printf 'What is the status of molecule <MOL_ID>? Use the observe tool to look it up, then tell me in one sentence.\n/quit\n' \
  | COSMON_PILOT_MODEL=qwen3:8b \
    COSMON_PILOT_TRANSCRIPT=/tmp/pilot-smoke.md \
    ./target/debug/cosmon-pilot
```

Knobs (env): `COSMON_PILOT_MODEL`, `COSMON_PILOT_BASE_URL`,
`COSMON_PILOT_TRANSCRIPT`.

## Observed result (2026-06-01, `qwen3:8b`)

Observing the completed molecule `task-20260420-5ad6`:

1. The model emitted a native `observe` tool call with the correct
   `{"molecule_id": "task-20260420-5ad6"}` argument.
2. The harness dispatched it through the read-only `cosmon-ops-tools`
   registry; the `observe` verb read `cosmon-state` **directly** (no `cs`
   subprocess) and returned the molecule's JSON state.
3. The tool result was folded back into the conversation as a `## TOOL`
   transcript entry (the full `ObserveJson`: `status: "completed"`,
   `completed_steps: ["implement","verify"]`, the escalation history, …).
4. The model produced a final, accurate answer:

   > The molecule task-20260420-5ad6 is **completed** with escalations
   > including 4 conflict retries and 2 exhausted retries during its
   > execution.

5. `/quit` exited cleanly; the transcript was kept on disk.

The IFBDD bit — *one molecule observed end-to-end via a tool call against a
local model* — is green.

## Wiring note (the gap this smoke caught)

The first smoke run advertised the **filesystem worker tools**
(`read_file` / `edit_file` / `exec_command`) to the model rather than the
ops tools, because `OpenAIProvider` hardcoded
`default_registry().declarations()` as its advertised schema. The model was
never told `observe` existed and narrated a guessed call as text. The fix:
`OpenAIProvider::with_tools(...)` lets the driver advertise the **same**
registry the `InteractiveSession` dispatches against — advertisement and
dispatch must agree. The pilot binary now passes
`read_only_registry().declarations()` to both.

## Increment 2 — remote tool backend (`task-20260601-4997`)

`cs pilot --remote [--profile <name>]` keeps the *same* REPL and the *same*
client-side model, but swaps the tool backend: instead of calling
`cosmon-state` in-process, the tools call the avatar's `cosmon-rpp-adapter`
over the ADR-080 §8p wire via `cosmon-remote` (JWT `sub → nucleon_id`). The
model never learns which backend it drives — the remote `observe`/`ensemble`
tools carry the *identical* name + declaration as the local ones (asserted by
`remote::tests::remote_declarations_match_local_for_observe_and_ensemble`).

- **Read-only by default:** `observe` (`GET /v1/molecules/:id`) + `ensemble`
  (`GET /v1/molecules`). `peek` is **absent remotely** — it has no RPP route.
- **`--write`:** adds `nucleate` (`POST /v1/molecules`) + `tackle`
  (`POST /v1/molecules/:id/tackle`).
- **Never on the wire:** `done` / `evolve` / `complete` — absent *by
  construction*, not gated (ADR-080 §5; ADR-115 §5).
- **No new RPP route** is introduced — the backend is a *client* of the
  existing §8p surface, so the `api_surface_freeze` test is unchanged.

### Manual remote smoke

```sh
# 1. Stand up (or reach) an avatar's cosmon-rpp-adapter and configure a
#    cosmon-remote profile (host/sub/aud/oidc_url) — see cosmon-remote docs.
cosmon-remote --profile example config set host https://avatar.example.ts.net
cosmon-remote --profile example config set sub operator
cosmon-remote --profile example config set aud cosmon-rpp
cosmon-remote --profile example config set oidc-url https://avatar.example.ts.net/oidc

# 2. Drive the remote fleet with a client-side Ollama model. The JWT is
#    minted from the profile (or pass $COSMON_REMOTE_TOKEN to skip the mint).
printf 'List the temp:hot backlog with the ensemble tool, then summarise.\n/quit\n' \
  | COSMON_PILOT_MODEL=qwen3:8b \
    cs pilot --experimental --remote --profile example
```

Add `--write` to expose `nucleate`/`tackle`. The wire contract (route +
bearer header + decoded envelope, including the 404→Io mapping) is covered
automatically by `tests/remote_wire.rs` against a `wiremock` adapter.
