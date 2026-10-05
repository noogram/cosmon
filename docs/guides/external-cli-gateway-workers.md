# External CLI workers through an OpenAI-compatible gateway

The `codex` and `opencode` adapters can use one per-galaxy gateway row without
changing either harness's global configuration. `cs tackle` translates the
same three cosmon fields into each harness's native provider configuration,
injects the named key only into that worker's tmux session, and keeps the key
out of the molecule state, event journal, and pane command.

Use a dedicated variable name per person. The TOML records the variable name,
never its value.

## Codex

```toml
[adapters.codex]
base_url      = "https://gateway.example/v1"
api_key_env   = "ALICE_GATEWAY_KEY"
default_model = "publisher/coder-model"
mode          = "exec" # optional; interactive remains the default
```

```bash
export ALICE_GATEWAY_KEY=...
cs tackle <molecule-id> --adapter codex
```

Cosmon renders an ephemeral `cosmon_gateway` provider through Codex's `-c`
channel: `model_provider`, `model_providers.<id>.base_url`, `env_key`, and the
Responses wire protocol. A `--model` or formula model pin still wins over
`default_model`. An explicit `--harness key=value` remains the final Codex
override and can replace one of the generated provider settings for that
dispatch.

The gateway must implement the Responses endpoint and its streamed event
format. Codex supplies its normal tool declarations and consumes tool calls;
the selected model and gateway must support that contract. WebSocket transport
is disabled for this route, so streaming uses HTTP server-sent events.

Codex writes its own rollout under the molecule-scoped Codex home. Cosmon can
read cumulative token counts from that rollout and emit `usage_observed` while
`cs peek --no-tui` runs with a nonzero energy-tick interval. API-equivalent
cost is an estimate from cosmon's price manifest when the model has a matching
price card; otherwise cost stays unavailable. The gateway remains the billing
authority.

## OpenCode

```toml
[adapters.opencode]
base_url      = "https://gateway.example/v1"
api_key_env   = "ALICE_GATEWAY_KEY"
default_model = "publisher/coder-model"
```

```bash
export ALICE_GATEWAY_KEY=...
cs tackle <molecule-id> --adapter opencode
```

Cosmon supplies `OPENCODE_CONFIG_CONTENT` only to the worker process. The
inline configuration defines a `cosmon-gateway` provider using the
OpenAI-compatible package, refers to the key as
`{env:ALICE_GATEWAY_KEY}`, and declares the selected model. The launched model
name is `cosmon-gateway/publisher/coder-model`; the gateway receives the
configured model id. Neither `~/.config/opencode` nor its credential store is
written.

This route uses OpenAI-compatible chat completions and the harness's streaming
client. The gateway must support streamed chat-completion chunks. Tool calling
is owned by OpenCode: the model, gateway, and compatible provider package must
preserve tool-call fields. A successful loopback wiring test does not establish
that an arbitrary hosted model is a capable coding agent.

Cosmon does not currently parse OpenCode's session database or statistics into
`usage_observed`. Absence of a cosmon usage record means unobserved, not zero;
inspect the gateway's metering for billing totals.

## Failure and confidentiality behavior

`base_url` opts into gateway mode. When it is present, `api_key_env` must be a
valid environment-variable name, the variable must be nonempty, and either a
resolved model pin or `default_model` must exist. `cs tackle` refuses before
spawn when any of those conditions is missing.

The loopback regression test uses fabricated credentials and isolated
`HOME`/`CODEX_HOME`/OpenCode data roots. It asserts the endpoint, bearer key,
and model at the HTTP boundary, then scans the molecule state and captured
output for the fabricated key. It never contacts a paid model.

For the in-process alternative, see
[Reaching a hosted model through an API gateway](gateway-worker.md).

Harness configuration references:

- [Codex configuration reference](https://developers.openai.com/codex/config-reference)
- [OpenCode providers](https://opencode.ai/docs/providers) and
  [configuration](https://opencode.ai/docs/config)
