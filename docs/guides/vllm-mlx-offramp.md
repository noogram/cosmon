# vllm-mlx — local-inference offramp (operator runbook)

**Purpose.** Run a local LLM server on the operator's M4 Max so cosmon
stops paying Claude Code rent. This is **Path B** of the
`delib-20260519-f6c3` synthesis: the 2026-06-15 ship target, zero new
Rust code, configuration change only. See
an internal chronicle
for the principle and the four-observable test that gates any future
re-internalization.

`vllm-mlx` is a Python sidecar that exposes both OpenAI `/v1/*` and
Anthropic `/v1/messages` from a single process. `cosmon-provider::openai`
and `cosmon-provider::anthropic` already speak those schemas; pointing
them at `http://127.0.0.1:8000` is a TOML edit or two `export` lines.

## What you get

- A local LLM server bound to loopback only (`127.0.0.1:8000`).
- A LaunchAgent that respawns it on crash and starts it at login.
- A `cs vllm-mlx health` subcommand that returns the resolved base URL,
  the loaded model id, and the round-trip latency.
- Two adapters, one process — OpenAI envelope on `/v1/chat/completions`,
  Anthropic envelope on `/v1/messages`.

## Install the sidecar binary

vllm-mlx is a pure-Python wheel; `uv` keeps it in an isolated tool venv
so it never touches the system Python:

```sh
uv tool install vllm-mlx --python 3.13
```

Verify:

```sh
vllm-mlx --help        # subcommands: serve, bench, download, model, …
```

Python 3.14 is **not** supported upstream as of 0.3.0; pin to 3.13.

## Pick a model

The default in the LaunchAgent template is **Qwen3-7B-Instruct-4bit**
(MLX-native, ~4 GB). Alternatives that exercise the same envelope on
the same harness:

| Model                                 | Approx. size | Why                                |
| ------------------------------------- | -----------: | ---------------------------------- |
| `mlx-community/Qwen3-7B-Instruct-4bit`| ~4 GB        | v0 default — strong tool-calling   |
| `mlx-community/Meta-Llama-3.1-8B-Instruct-4bit` | ~4.5 GB | Llama family, OpenAI-style tools  |
| `mlx-community/Mistral-7B-Instruct-v0.3-4bit` | ~4 GB | Mistral family, tight latency     |
| `mlx-community/Qwen3-0.6B-4bit`       | ~330 MB      | Smoke-test model; ~63 tok/s        |

Download up-front (saves 60+ seconds on first launch):

```sh
vllm-mlx download mlx-community/Qwen3-7B-Instruct-4bit
```

Models cache to `$HF_HOME/hub/` — defaults to `~/.cache/huggingface/`.
If you'd rather isolate cosmon model assets, set `HF_HOME` in the
LaunchAgent (the install script exposes `--hf-home`).

## Install the supervisor

The LaunchAgent template lives at
[`docker/launchd/dev.cosmon.vllm-mlx.plist`](../../docker/launchd/dev.cosmon.vllm-mlx.plist).
The install script renders it with operator-specific paths and loads
it via `launchctl`:

```sh
scripts/install-vllm-mlx-launchagent.sh \
    --model mlx-community/Qwen3-7B-Instruct-4bit \
    --port 8000 \
    --extra-arg "--auto-unload-idle-seconds=900"
```

`--auto-unload-idle-seconds=900` releases GPU memory after 15 minutes
of idle — important on an M4 Max where every gigabyte unified-memory
released is one less gigabyte the rest of the desktop fights for.

The agent runs as `dev.cosmon.vllm-mlx`:

```sh
launchctl list dev.cosmon.vllm-mlx
tail -f ~/Library/Logs/cosmon-vllm-mlx.err.log
```

Uninstall:

```sh
scripts/uninstall-vllm-mlx-launchagent.sh
```

## Point cosmon at the local endpoint

Two equivalent paths. **Pick one** — don't mix.

### Path 1 — `.cosmon/config.toml` (recommended, durable)

```toml
[adapters.openai]
base_url = "http://127.0.0.1:8000"
api_key_env = "OPENAI_API_KEY"           # any non-empty value will do
default_model = "mlx-community/Qwen3-7B-Instruct-4bit"

[adapters.anthropic]
base_url = "http://127.0.0.1:8000"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "mlx-community/Qwen3-7B-Instruct-4bit"
```

The sidecar accepts any non-empty bearer token (it does no real auth).
Set `OPENAI_API_KEY=local-llama` in your shell so the providers'
required-key check passes.

### Path 2 — environment variables (ephemeral, good for trying it out)

```sh
export OPENAI_BASE_URL=http://127.0.0.1:8000
export OPENAI_API_KEY=local-llama
export ANTHROPIC_BASE_URL=http://127.0.0.1:8000
export ANTHROPIC_API_KEY=local-llama
```

Precedence (mirrored by `cs config show adapters`):

```
[adapters.X].base_url  >  ENV_X_BASE_URL  >  vendor default
```

## Pre-flight check

Before tackling a molecule that depends on local inference:

```sh
cs vllm-mlx health
```

Output (success):

```
vllm-mlx: OK  base_url=http://127.0.0.1:8000 (config)  latency=4ms
  models:
    - mlx-community/Qwen3-7B-Instruct-4bit (owned_by=vllm-mlx)
```

Exit code `0` on healthy endpoint, `2` on any failure. Use it in
shell pipelines:

```sh
cs vllm-mlx health && cs tackle <mol_id>
```

Useful flags:

- `--base-url URL` — override the resolution chain (one-off probes).
- `--timeout 2.5` — shorter timeout when you expect a fast answer.
- `--json` — NDJSON output for scripting.

## Smoke-test the envelopes

OpenAI:

```sh
curl -s http://127.0.0.1:8000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "mlx-community/Qwen3-7B-Instruct-4bit",
    "messages": [{"role":"user","content":"hello"}]
  }'
```

Anthropic:

```sh
curl -s http://127.0.0.1:8000/v1/messages \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
  -d '{
    "model": "mlx-community/Qwen3-7B-Instruct-4bit",
    "messages": [{"role":"user","content":"hello"}],
    "max_tokens": 64
  }'
```

## Latency reference (Qwen3-0.6B-4bit on M4 Max, May 2026)

Recorded as the cat-test baseline for the future `cosmon-llama` Rust
adapter (Path A v0, `task-20260519-fedc`) to match or beat:

| Metric                          | Value      |
| ------------------------------- | ---------- |
| `/v1/models` round-trip         |    2–5 ms  |
| TTFT (streaming chat-complete)  |    32 ms   |
| 60-token completion total       |   956 ms   |
| Throughput                      | ~63 tok/s  |

7B-Instruct-4bit will be 3–5× slower per token but still well above
the agent-loop floor (5 s/turn). See chronicle for the full numbers.

## Troubleshooting

- **`launchctl list` shows the agent but `/v1/models` 404s**
  Model still loading. Cold loads on a fresh HF cache can take 60+ s;
  watch `~/Library/Logs/cosmon-vllm-mlx.err.log`.

- **`cs vllm-mlx health` says `DOWN`, base_url is the vendor default**
  The config / env tier did not resolve. Either run the install
  script, or `export OPENAI_BASE_URL=http://127.0.0.1:8000`.

- **`cs tackle --adapter openai` works in shell but fails under LaunchAgent**
  The LaunchAgent inherits a minimal env. Set `OPENAI_API_KEY` (and
  any other adapter env vars) in your shell rc — `cs tackle` runs in
  *your* shell, not the LaunchAgent's.

- **Continuous batching causes OOM on first request**
  Drop `--continuous-batching` from the LaunchAgent args
  (`--no-continuous-batching` on the install script), or pass
  `--max-num-seqs 1`.

## Companion artifacts

- LaunchAgent template: [`docker/launchd/dev.cosmon.vllm-mlx.plist`](../../docker/launchd/dev.cosmon.vllm-mlx.plist)
- Install script: [`scripts/install-vllm-mlx-launchagent.sh`](../../scripts/install-vllm-mlx-launchagent.sh)
- Chronicle: an internal chronicle
- Parent deliberation: `delib-20260519-f6c3` (synthesis §C1, §C5)
- Patterns for future Rust port: internal research notes
