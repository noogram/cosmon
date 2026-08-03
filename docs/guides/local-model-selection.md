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

## Some formulas will not dispatch here at all

Choosing a good model does not make a chat loop into a coding agent. The
local adapter runs an in-process model loop over a confined tool registry:
it has **no shell, no git, and no `cs` command**. A formula whose steps
*are* shell work — run the gate toolchain, execute a producer script,
resolve a merge conflict — cannot be satisfied here however the prompt is
worded.

So a formula can say what it needs of its worker:

```toml
# in <formula>.formula.toml
requires_capabilities = ["shell", "vcs"]
```

and `cs tackle` refuses the pairing up front:

```
cs tackle: refusing dispatch — formula `producer-work` requires worker
capabilities [shell, vcs] that adapter 'local' does not have. …
```

Exit code **17**, no worktree, no pane, no model call — the molecule stays
pending and re-tacklable. Re-run with a coding-agent adapter
(`--adapter claude`), or set `COSMON_SKIP_CAPABILITY_GATE=1` to dispatch
anyway if you are deliberately experimenting on the floor.

The vocabulary is `shell`, `vcs`, `cs-cli`. It is opt-in per formula: a
formula that declares nothing dispatches everywhere it did before, which is
every formula the quickstart touches. Today every non-local adapter has all
three and every local one has none, so the gate draws exactly one line —
chat loop versus coding agent. Details, and why the vocabulary is
three-valued rather than a `requires_shell` bit, are in
`crates/cosmon-core/src/adapter_capability.rs`.

## Provenance

Filed as COSMON #23 by an external tester who ran `cs demo` repeatedly,
got `qwen3:8b` every time, and concluded the model was hardcoded. The
resolution chain existed; nothing ever said so. A capability nobody can
find is, from the user's chair, a capability that does not exist — which
is why the fix is a flag, a printed line, and this page, not a new knob.

The capability gate above comes from a second report, COSMON #4: a
shell-shaped mission dispatched to the local floor ran its machinery end to
end and produced nothing, because *"the worker briefing assumes a full
coding agent"*. The briefing was made adapter-aware first; the reporter's
own suggestion — *gate formulas on adapter capabilities* — is what closes
the rest, because a briefing cannot lend a chat loop a shell.
