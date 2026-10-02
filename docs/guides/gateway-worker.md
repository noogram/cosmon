# Reaching a hosted model through an API gateway (`openai` adapter)

A pay-per-token gateway that speaks the OpenAI chat-completions envelope can
serve any model it lists, from any publisher, to a cosmon worker. No new
adapter is involved: the built-in `openai` adapter runs the in-process agent
loop (ADR-100) and only needs to be repointed. This guide gives the
configuration, the model id form, what the route can and cannot do, and where
its usage shows up. The executable version of every claim below is
`crates/cosmon-cli/tests/gateway_worker_recipe.rs`, which runs the recipe
against a loopback responder and never touches the network.

This is the metered path. Subscription (account-login) access is a different
mechanism and is not covered here.

## Configure

`.cosmon/config.toml`:

```toml
[adapters.openai]
base_url      = "https://gateway.example/api"   # the gateway root, see below
api_key_env   = "GATEWAY_API_KEY"               # NAME of the variable, never the key
default_model = "publisher/model-name"          # the gateway's own model id
```

```bash
export GATEWAY_API_KEY=...            # in the shell that runs `cs tackle`
cs tackle <molecule-id> --adapter openai
cs tackle <molecule-id> --adapter openai --model publisher/other-model   # per-molecule pin
```

Never write a key into `config.toml`, a formula or a briefing; the config
holds only the variable name.

### `base_url`

The worker appends `/v1/chat/completions` to `base_url`. Give the gateway root
including any path prefix it uses (`https://gateway.example/api`), so the
request lands on `https://gateway.example/api/v1/chat/completions`. A trailing
`/v1` is stripped with a warning, so the vendor-documented
`https://gateway.example/api/v1` resolves to the same URL; it is never doubled.

### Model id

The id is passed to the gateway verbatim, on every turn. Gateways name models
`publisher/model-name`; copy the exact string from the gateway's model catalogue
and prefer a concrete version over a floating alias, because the id is what the
molecule records as intended. Precedence, highest first: `cs tackle --model`
(or a formula pin), `[adapters.openai].default_model`, `OPENAI_MODEL`,
`gpt-4o-mini`. The last default is an OpenAI model id, so on a gateway always
set one of the first two.

### Credential

When `api_key_env` is declared, that variable is the only credential source. If
it is unset or empty, `cs tackle` refuses and names it; it does not fall back to
`OPENAI_API_KEY`, `XAI_API_KEY` or `MOONSHOT_API_KEY`, and nothing is sent. Without
`api_key_env`, the historical scan of those three variables applies, which can
pick the wrong vendor's key. Declare `api_key_env` for any gateway.

## What the route provides

The worker is the same harness as every other in-process adapter: one turn is
one POST, tool calls returned by the model are executed against the worktree and
their results sent back, until the model stops (at most eight turns). The model
sees the default tool set: `read_file`, `edit_file`, `write_file`, `list_dir`,
`grep`, `find_file` and `exec_command`. It is therefore a working coder, but it
has no terminal UI and no pane to steer, so it is not a drop-in for a full CLI
agent.

Limits that depend on the gateway and the model, not on cosmon:

- the model must support function calling through the gateway; a model that
  pastes tool calls into plain text will complete with no tool work, and
  cosmon collapses a loop that did none rather than sealing it;
- requests set `stream: true`; a gateway that ignores it and returns a single
  JSON body is accepted;
- context window, rate limits, quota and model availability are the gateway's.
  Rate-limit and 5xx responses are retried in place with backoff; a quota or
  authentication error ends the worker with the gateway's message;
- the egress posture is remote opt-in. The `remote_egress_opt_in` audit event
  names the host parsed from `base_url`, not the adapter name.

A passing test establishes wiring only. Whether a given model is available and
tool-capable on the live gateway is established by running one bounded molecule
against it and keeping its `events.jsonl`.

## What is recorded

- `worker_spawn_attempted` with `adapter_name = "openai"`, and the
  `remote_egress_opt_in` line above;
- `model_observed` (source `provider_response`): the model id the gateway put in
  its response, scoped to the worker, emitted on first observation and on
  change. The intended id (what you pinned) and the observed id are separate
  fields; a gateway that routes or falls back to another model shows up as a
  difference between them;
- `synthesis.md` in the molecule directory: the model's final message, headed by
  the intended model id.

**Token usage is not recorded for this route.** The response `usage` block is
not read, so `cs peek` shows no token counters or API-equivalent cost for these
workers, and absence there means unobserved, not zero. Read spend from the
gateway's own dashboard.
