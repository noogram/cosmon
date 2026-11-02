# Agent adapters: a harness over harnesses

Cosmon does not replace the agentic systems you already use. It sits *above* them.
An **adapter** is the plug that names which system actually does the work: Claude
Code, aider, codex, opencode, agy (Google Antigravity CLI), an OpenAI or
Anthropic endpoint, `llama-cpp`, a local model.
Cosmon is a **harness over harnesses**: it gives a piece of work an identity, a
lifecycle, and crash-recovery, then hands the actual agent loop to whichever
harness you picked.

## What an adapter is

When you `cs tackle` a molecule, cosmon creates the worktree, the tmux pane, and
the fleet bookkeeping, and then it has to launch *something* inside that pane to
be the agent. The adapter is that named choice: `claude`, `codex`, `aider`,
`opencode`, `agy`, `openai`, `anthropic`, `llama-cpp`, `ollama`, `local`. `cs config
adapters` lists the ones your build registers. The kernel stays agnostic about which one runs;
the adapter is the seam where a concrete system attaches.

This is what makes cosmon **provider-agnostic**. Cosmon is not a competitor to
Claude Code or aider; it is the layer that composes above any of them and lets you
run several at once under one identity and one lifecycle.

## Why this is the wedge

Because the choice is per-molecule, your context, your data, and your model choice
stay yours. One mission can route to a hosted frontier model; the next molecule
can run entirely on a local model, decided by changing a single word. You are not
locked to one vendor's loop, and you are not rewriting anything to switch. The
molecule, its state, and its trace are identical whichever harness ran it.

## How an adapter is chosen

At tackle time cosmon resolves the adapter through a fixed order, highest priority
first:

1. the `--adapter <name>` flag on `cs tackle`;
2. a formula step's `adapter = "<name>"` pin;
3. the `$COSMON_DEFAULT_ADAPTER` environment variable;
4. the per-galaxy `.cosmon/config.toml` default;
5. the global `~/.config/cosmon/config.toml` default;
6. the built-in `local` adapter.

An unknown name **aborts the dispatch**; it never silently falls back to some
other harness. The exact chain, the `--model` sibling that pins a model within an
adapter, and the events each dispatch emits are documented in the
[Execution commands reference](../reference/execution.md).

## The `opencode` and `agy` workers

Both run as a one-shot command in a tmux pane under `cs tackle`. When the
command exits, the pane ends, tmux fires the `pane-died` hook, and the hook
records the worker's exit. What happens next is set by `[harvest]
on_pane_died` (see [Harvest after the pane ends](#harvest-after-the-pane-ends)).

### opencode

```sh
cs tackle <molecule-id> --adapter opencode [--model <provider/model>]
```

Cosmon runs `opencode run [--model <provider/model>] '<prompt>'` in the
worktree. `opencode` must be on `PATH`; you configure its provider and
credentials yourself, unless you bind a per-galaxy gateway row
(`docs/guides/external-cli-gateway-workers.md` in the repository).

- **Readiness.** `cs tackle` waits up to 30 seconds for evidence that opencode
  printed its own output. Evidence is the run preamble line
  `> <mode> · <model>` (opencode 2.2.5 and later), or one of the older
  preamble markers. The mere existence of the tmux session does not count. If
  no evidence appears, `cs tackle` terminates the session and fails the spawn.
- **Model.** The model is carried by opencode's `--model` flag, passed as
  given. Cosmon adds no provider prefix, so use opencode's `provider/model`
  form. With no pin, no flag is passed and opencode's own default applies.
- **Recorded model.** Through `cs tackle`, a pin is both recorded as
  `ModelSelected` and passed to the process. The in-process executor used by
  library embedders and the RPP tackle route has no channel for the flag.
  opencode is the one adapter listed as lacking it
  (`ADAPTERS_WITHOUT_MODEL_CARRIER`), so that executor refuses a pinned
  opencode dispatch before any effect (`501 tackle_unsupported_model` on the
  RPP route) instead of recording a pin the process never received. An
  unpinned opencode dispatch is not refused. Dispatch a pinned opencode
  molecule through `cs tackle`.

### agy

```sh
cs tackle <molecule-id> --adapter agy [--model <model>]
```

Cosmon runs this pipeline in the pane:

```text
rm -f '<mol dir>/agy-exit.code'; { env -u ANTIGRAVITY_CONVERSATION_ID agy \
  --print-timeout 0 --output-format stream-json [--model <model>] \
  --dangerously-skip-permissions -p '<prompt>' 2>&1; \
  echo $? > '<mol dir>/agy-exit.code'; } | tee -a '<mol dir>/agy-stream.jsonl'; \
  exit "$(cat '<mol dir>/agy-exit.code')"
```

Each part has a reason:

- `echo $? > …/agy-exit.code` and the final `exit "$(cat …)"` make the pane's
  exit status agy's, not `tee`'s. The command avoids `set -o pipefail`, which
  dash rejects, so it runs under any POSIX `sh`.
- `env -u ANTIGRAVITY_CONVERSATION_ID` keeps a conversation id inherited from
  the tmux server environment from reaching the worker, so the worker does not
  take over a caller identity that belongs to another agy session. An explicit
  entry with that name in the session environment is dropped as well.
- `--print-timeout 0 --output-format stream-json` runs agy non-interactively
  with no print timeout and emits one JSON event per line.
- `--model <model>` is present only when a model is pinned.
- `--dangerously-skip-permissions` approves tool permissions automatically.
  An unattended agy worker therefore runs tools without asking. Run it only
  on work and in a worktree where that is acceptable.
- `| tee -a …/agy-stream.jsonl` shows progress in the pane and appends the
  stream to a receipt in the **molecule directory**, not in the worktree, so
  the receipt survives `cs done` removing the worktree.

Further points:

- **Readiness.** `cs tackle` waits up to 30 seconds for agy's `init` event
  from the stream. `capture-pane` hard-wraps agy's long init line, so a
  fragment that starts with `{"event":"init"` counts. If it never appears,
  the spawn fails and the session is terminated.
- **Recorded model.** Cosmon records the pin, if any. It does not yet read the
  model agy actually used from the stream, so for an unpinned run the
  realized model is unrecorded.
- **Credentials.** You log in to agy yourself. Cosmon never handles agy
  credentials, and an `[adapters.agy]` gateway row is not translated (the
  gateway binding exists for `codex` and `opencode` only).
- **Quota.** agy quota is shared with interactive use of the same account.
  Several concurrent agy workers, or workers running while you use agy
  yourself, can hit rate limits. Keep agy fan-out small.

### Harvest after the pane ends

The `opencode` and `agy` panes end when the run ends. With the default `on_pane_died = "merge"`, the
`pane-died` hook records the exit and then runs `cs done`: the molecule is
merged with no pilot review. With `"mark"` the hook records the exit and
stops. The molecule stays completed and unmerged, listed by `cs status` and
`cs peek --phase harvestable` until a pilot runs `cs done`.

```toml
# .cosmon/config.toml
[harvest]
on_pane_died = "mark"   # "merge" (default) | "mark"
```

Use `mark` in a galaxy where a pilot reviews workers before they merge. An
unknown value is refused when the config loads, never treated as `merge`.

## The seam is typed

The choice of adapter and the harness that actually launches are guaranteed to be
the same thing. Cosmon's spawn seam refuses to compile if handed a bare string:
the validated adapter name is the only value the launch site accepts, so "adapter
`aider` selected" and "worker spawned with `aider`" are the same bytes by
construction. It does not rely on a runtime check. A smoke test once showed a worker
report `aider` and then route through Claude; that class of bug is now a type
error rather than a possibility. The lineage is recorded in ADR-099 and its
successors; the [Execution commands reference](../reference/execution.md)
documents the public dispatch behavior.

## What ships today, and what is roadmap

Per-molecule adapter and model choice ships **today**: you can point any molecule
at any registered harness, hosted or local, right now. The stronger reading
(long, unattended runs entirely on local models with no human in the loop) reaches
into the **Autonomous** regime. That capability remains on the roadmap. See
[The three regimes](./regimes.md) for where present capability ends and the
roadmap begins.

## See also

- [Execution commands reference](../reference/execution.md): `--adapter`,
  `--model`, and the full resolution chain.
- [Fleets: many agents, one portal](./fleets.md): a fleet's config picks adapters
  for its workers.
- [Noogram & the Cosmon kernel](./cosmon-and-noogram.md): why an agnostic kernel
  is what a distribution composes above.
