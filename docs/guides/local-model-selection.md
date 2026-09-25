# OpenAI-compatible servers for the `local` adapter

The `local` adapter is cosmon's floor: no API key required, no spend, an
in-process chat loop with a confined tool registry. It dials any backend
that speaks the OpenAI `/v1/chat/completions` envelope — Ollama out of the
box, but also vLLM and llama-server. "Just change the port" is not enough
for the last two: each server needs its own launch flags before it emits
*structured* `tool_calls` instead of pasting a tool call into `content`,
plus the exact model id it registered itself under. This guide is the one
page that has all three.

If you already run [`vllm-mlx`](vllm-mlx-offramp.md) on Apple Silicon, that
is a different path: it fronts the `openai` / `anthropic` **remote**
adapters (a conscious egress opt-in) with a local server standing in for
the vendor API. This guide is about the `local` adapter itself — strict
egress by construction, covered below.

## TL;DR

```bash
# once, per invocation
cs tackle <molecule-id> --adapter local --model <served-id>

# durably, for this galaxy — .cosmon/config.toml
[adapters.local]
base_url = "http://localhost:11434"   # or wherever the server listens
default_model = "<served-id>"
api_key_env = "LOCAL_INFERENCE_TOKEN"  # only if the server checks a key

# for the shell session
export COSMON_LOCAL_MODEL=<served-id>
```

The model must already be served under that exact id. Cosmon verifies
this **before** dispatch and refuses with a named repair rather than
spawning a worker that dies mid-flight.

## Ollama

```bash
ollama serve
ollama pull qwen3:8b
```

No extra flags: Ollama's OpenAI-compat endpoint emits structured
`tool_calls` for the models this guide recommends (below), and the
sentinel credential `"ollama"` is accepted without configuration.

```toml
[adapters.local]
default_model = "qwen3:8b"   # base_url defaults to http://localhost:11434
```

## vLLM

vLLM's OpenAI server does not turn on tool calling by default — without
the flags below it either 400s on a `tools` payload or echoes the call as
plain text, and cosmon's loop never sees a `tool_calls` entry to act on.

```bash
vllm serve Qwen/Qwen2.5-7B-Instruct \
  --served-model-name qwen2.5-7b \
  --enable-auto-tool-choice \
  --tool-call-parser hermes \
  --port 8000
```

- `--served-model-name` is the id the `/v1/models` and
  `/v1/chat/completions` endpoints expect — pin it to something short and
  stable, because it is what you put in `default_model` / `--model`.
  Without it vLLM serves under the full HF repo path.
- `--enable-auto-tool-choice --tool-call-parser <family>` is what
  produces structured `tool_calls`. The parser is model-family specific
  (`hermes` for Qwen/Hermes-tuned checkpoints, `mistral` for Mistral,
  `llama3_json` for Llama-3 instruct, etc.) — pick the one matching the
  checkpoint, not the one that merely compiles.

```toml
[adapters.local]
base_url = "http://localhost:8000"
default_model = "qwen2.5-7b"          # the --served-model-name, not the HF path
api_key_env = "VLLM_API_KEY"          # only if you started vLLM with --api-key
```

## llama-server (llama.cpp)

```bash
llama-server -m ./qwen2.5-7b-instruct-q4_k_m.gguf \
  --jinja \
  --port 8080
```

- `--jinja` makes llama-server apply the GGUF's own chat template
  (including its tool-call grammar) instead of a built-in generic one.
  Without it, tool calls routinely land as text in `content` — the same
  failure mode as an unflagged vLLM.
- llama-server's `/v1/models` id is derived from the loaded file, not a
  name you choose; read it back with `curl localhost:8080/v1/models`
  before setting `default_model`, or the preflight below will refuse a
  guessed id that does not match.

```toml
[adapters.local]
base_url = "http://localhost:8080"
default_model = "qwen2.5-7b-instruct-q4_k_m"   # copy this from /v1/models
```

llama-server checks no key unless started with `--api-key <key>`. Without it,
leave `api_key_env` unset — the sentinel credential is sent and ignored.
With it, export the same key and set `api_key_env` to that variable's name.

## A remote host: reachable, but still `StrictLocal`

Nothing above requires the server to run on the same machine — `base_url`
takes any host:

```toml
[adapters.local]
base_url = "http://gpu-box:8000"
default_model = "qwen2.5-7b"
```

Two things this does **not** change:

1. **Reachability is on you.** The preflight dials `base_url` directly
   over HTTP before spawning; a firewalled or unreachable host surfaces
   as `LocalPreflightError::Unreachable`, not a hang. There was no remote
   OpenAI-compatible inference host registered and reachable from the
   worker that wrote this guide to measure a live cross-host round trip
   against — this section documents the mechanism from the code path
   (`resolve_local_base_url`, `preflight_local_adapter_model` in
   `crates/cosmon-cli/src/cmd/tackle.rs`), not a captured latency number.
   If you have a candidate host, `cs tackle --dry-run --adapter local`
   against it is the cheapest way to get a real verdict — it walks the
   whole preflight and returns before any worker spawns.
2. **The egress posture does not follow `base_url`.** `cs tackle`
   resolves autonomy posture from the **adapter name**, not from where
   `base_url` points: `local` is always `StrictLocal`, which denies the
   in-process `exec_command` tool any outbound network (kernel-enforced
   netns where available). That jail wraps the *tool* the model can
   call — it does **not** wrap the HTTP client that dials `base_url`.
   So a `local` dispatch pointed at `gpu-box:8000` sends every prompt and
   response to that host over the open network, while the worker itself
   still cannot `curl` anything from inside a tool call. If the remote
   host is outside a trust boundary you care about, that asymmetry is the
   thing to know before pointing `base_url` off-box — not the strictness
   of the sandbox, which is unchanged and does not cover this path.

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
dialling any backend, so it works with the server stopped.

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

## Choosing the endpoint, not just the model

The endpoint is a separate axis, resolved the same way and normalized once
(so the preflight and the worker never dial two different URLs — a
`base_url` with or without a trailing `/v1` resolves identically):

| # | Mechanism |
|---|-----------|
| 1 | `[adapters.local].base_url` in `.cosmon/config.toml` |
| 2 | `$COSMON_LOCAL_BASE_URL` |
| 3 | `$OLLAMA_HOST` (Ollama's own variable — `gpu-box`, `127.0.0.1:11434`, `http://host:11434`, all accepted) |
| 4 | `$OPENAI_BASE_URL` |
| 5 | built-in `http://localhost:11434` |

## The credential, if the server checks one

Ollama and a bare llama-server accept anything (cosmon sends the sentinel
`"ollama"` when nothing is configured). vLLM started with `--api-key`, or
any server behind a reverse-proxy token check, needs:

```toml
[adapters.local]
api_key_env = "LOCAL_INFERENCE_TOKEN"   # the env var *name*, not the value
```

Cosmon reads the named variable at dispatch time and sends it as a bearer
token on every request, including the preflight probe. A server that
rejects it fails the preflight as `Unauthorized` (HTTP 401/403) — a
distinct diagnosis from `Unreachable`, because the fix is different:
changing `base_url` cannot repair a rejected credential.

## Which models actually work

Cosmon's local loop needs the model to emit **structured `tool_calls`** on
`/v1/chat/completions`, not a tool call pasted into `content`. That is why
`qwen3:8b` is the Ollama default: it was measured to do so. `qwen2.5-coder:7b`
was measured *not* to, and is a poor choice however capable it looks
otherwise. Verified emitters and their measured behaviour live in
[`crates/cosmon-pilot/SMOKE.md`](../../crates/cosmon-pilot/SMOKE.md) and
the parity-cliff measurements under [`docs/measurements/`](../measurements/).
The same requirement holds under vLLM and llama-server — it is what
`--tool-call-parser` and `--jinja` exist to satisfy — but the specific
measurements on record are against the Ollama backend.

Bigger is not automatically better here: a model that reasons well but
cannot emit a tool call will loop and collapse, while a smaller
tool-calling model finishes.

## When a dispatch is refused

Unreachable backend:

```
refusing to dispatch: the local adapter's backend at http://gpu-box:8000
is not reachable (…). Check that the server is running or point the
adapter elsewhere with [adapters.local].base_url / COSMON_LOCAL_BASE_URL.
```

Rejected credential:

```
refusing to dispatch: the local adapter's backend at http://gpu-box:8000
rejected its credential (HTTP 401). Check [adapters.local].api_key_env and
the named environment variable.
```

Model not served:

```
refusing to dispatch: the local adapter resolved to model 'qwen2.5-7b',
but the backend at http://gpu-box:8000 cannot serve it — it serves:
qwen2.5-7b-instruct-q4_k_m. Set COSMON_LOCAL_MODEL=… or pin one that
exists via --model / [adapters.local].default_model / COSMON_LOCAL_MODEL.
```

All three are the preflight, not a crash. The molecule is untouched and
still tacklable — nothing was spawned and nothing collapsed. To dispatch
anyway (at the risk the preflight exists to prevent), set
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

The vLLM and llama-server sections above and the remote-host reachability
note were added by `delib-20260925-4832` §C2/C5 (Q5: "il suffit de
changer le port" is false without the tool-calling flags and the exact
served model id).
