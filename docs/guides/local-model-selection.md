# Choosing the local (Ollama) model

The `local` adapter is cosmon's floor: no API key, no spend, everything on
your own machine. It talks to an Ollama daemon over the OpenAI-compatible
endpoint. Out of the box it runs **`qwen3:8b`** — but that is a *default*,
not a fixture. This guide is the one page that says how to change it.

## TL;DR

```bash
# once, per invocation
cs demo --model qwen2.5:32b --prompt "…"
cs tackle <molecule-id> --adapter local --model qwen2.5:32b

# durably, for this galaxy — .cosmon/config.toml
[adapters.local]
default_model = "qwen2.5:32b"

# for the shell session
export COSMON_LOCAL_MODEL=qwen2.5:32b
```

The model must already be pulled: `ollama pull qwen2.5:32b`. Cosmon
verifies this **before** dispatch and refuses with a named repair rather
than spawning a worker that dies mid-flight.

## How to see what is actually running

Every dispatch to the `local` (or `ollama`) adapter prints one line on
**stderr**, before any worker is spawned — including under
`cs tackle --dry-run`:

```
local adapter: model qwen2.5:32b (from [adapters.local].default_model), backend http://localhost:11434
  change it with `--model <id>`, `[adapters.local].default_model = "<id>"` in .cosmon/config.toml, or COSMON_LOCAL_MODEL=<id> (default qwen3:8b; see docs/guides/local-model-selection.md)
```

It names both the model **and its origin**, because the model alone is not
actionable: reading `qwen3:8b` does not tell you whether your config row
lost the race or was never read. The line goes to stderr so that
`cs tackle --dry-run` keeps a clean stdout for the bootstrap prompt and
`--json` envelopes.

`cs tackle --dry-run --adapter local` is the cheapest way to check your
configuration: it walks the whole resolution chain and returns before
dialling any backend, so it works with Ollama stopped.

## Precedence

Highest first. The first tier that names a model wins:

| # | Mechanism | Scope |
|---|-----------|-------|
| 1 | `--model <id>` on `cs tackle` / `cs demo` | one dispatch |
| 2 | `model = "<id>"` on the formula step | one workflow |
| 3 | `$COSMON_DEFAULT_MODEL` | shell session, all adapters |
| 4 | `[adapters.local].default_model` in `.cosmon/config.toml` | this galaxy |
| 5 | `[adapters.local].default_model` in `~/.config/cosmon/config.toml` | this machine |
| 6 | `$COSMON_LOCAL_MODEL` | shell session, local floor only |
| 7 | built-in `qwen3:8b` | the floor |

Tiers 1–5 are the generic model axis shared with every adapter (a model id
only has meaning inside its adapter, which is why the config rows are
scoped per adapter). Tier 6 is specific to the local floor. Tier 7 is the
compile-time default.

## Choosing the daemon, not just the model

The endpoint is a separate axis, resolved the same way:

| # | Mechanism |
|---|-----------|
| 1 | `[adapters.local].base_url` in `.cosmon/config.toml` |
| 2 | `$COSMON_LOCAL_BASE_URL` |
| 3 | `$OLLAMA_HOST` (Ollama's own variable — `gpu-box`, `127.0.0.1:11434`, `http://host:11434`, all accepted) |
| 4 | `$OPENAI_BASE_URL` |
| 5 | built-in `http://localhost:11434` |

So a GPU box on the LAN serving a big model is two lines:

```toml
[adapters.local]
base_url = "http://gpu-box:11434"
default_model = "qwen2.5:32b"
```

## Which models actually work

Cosmon's local loop needs the model to emit **structured `tool_calls`** on
`/v1/chat/completions`, not a tool call pasted into `content`. That is why
`qwen3:8b` is the default: it was measured to do so. `qwen2.5-coder:7b`
was measured *not* to, and is a poor choice however capable it looks
otherwise. Verified emitters and their measured behaviour live in
[`crates/cosmon-pilot/SMOKE.md`](../../crates/cosmon-pilot/SMOKE.md) and
the parity-cliff measurements under [`docs/measurements/`](../measurements/).

Bigger is not automatically better here: a model that reasons well but
cannot emit a tool call will loop and collapse, while a smaller
tool-calling model finishes.

## When a dispatch is refused

```
refusing to dispatch: the local adapter's backend at http://localhost:11434
serves no model named 'qwen2.5:32b' …
```

That is the preflight, not a crash. The molecule is untouched and still
tacklable — nothing was spawned and nothing collapsed. Either
`ollama pull` the model, or pick one the daemon already serves. To
dispatch anyway (at the risk the preflight exists to prevent), set
`COSMON_SKIP_ADAPTER_PREFLIGHT=1`.

## Provenance

Filed as COSMON #23 by an external tester who ran `cs demo` repeatedly,
got `qwen3:8b` every time, and concluded the model was hardcoded. The
resolution chain existed; nothing ever said so. A capability nobody can
find is, from the user's chair, a capability that does not exist — which
is why the fix is a flag, a printed line, and this page, not a new knob.
