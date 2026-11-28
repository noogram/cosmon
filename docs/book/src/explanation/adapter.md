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

Both run in a tmux pane under `cs tackle`. opencode runs a one-shot command;
agy keeps an interactive session open (see [agy](#agy)). When the process
exits, the pane ends, tmux fires the `pane-died` hook, and the hook records
the worker's exit. What happens next is set by `[harvest] on_pane_died` (see
[Harvest after the pane ends](#harvest-after-the-pane-ends)).

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
cs tackle <molecule-id> --adapter agy [--model <model>] [--harness <key>=<value>]…
```

Cosmon runs this command in the pane:

```text
exec env -u ANTIGRAVITY_CONVERSATION_ID cs agy-pane --molecule-dir '<mol dir>' \
  --prompt '<prompt>' -- agy --print-timeout 0 --input-format stream-json \
  --output-format stream-json [--model <model>] [--<key> <value>]… \
  --dangerously-skip-permissions -p ''
```

`cs agy-pane` is cosmon's pane driver, a hidden internal command. It sits
between the terminal and agy:

- It sends the prompt as the session's first message, then each line typed
  into the pane, or pasted by `cs whisper`, as the next one. A multi-line
  whisper stays one message. Blank lines are not sent.
- It appends every line agy writes, stdout and stderr, to
  `<mol dir>/agy-stream.jsonl`, the receipt `cs peek` reads for usage and a
  quota stop. The receipt lives in the **molecule directory**, so it survives
  `cs done` removing the worktree.
- It shows the same lines readably in the pane: agent text as it streams, one
  line per tool call, and after each turn its status and the session's token
  counts so far.
- When agy exits, it writes agy's exit status to `<mol dir>/agy-exit.code`
  and exits with it, so the pane's exit status is agy's.

Each part of the agy command has a reason:

- `--input-format stream-json` with `-p ''` is agy's multi-turn print mode:
  one JSON message per input line, one turn each, and the same `stream-json`
  events on output as a one-shot run. Measured on agy 1.5.19: one `init`
  event, then one `result` per turn whose usage covers the whole session so
  far. agy exits when its input closes or a turn fails; a quota refusal exits
  with status 3, which ends the pane as before.
- `exec` makes the driver the pane's process, so tmux reports `cs` as the
  pane's command (the whisper gate accepts it for agy) and no shell is left in
  the pane to receive typed text after agy ends.
- `env -u ANTIGRAVITY_CONVERSATION_ID` keeps a conversation id inherited from
  the tmux server environment from reaching the worker, so the worker does not
  take over a caller identity that belongs to another agy session. An explicit
  entry with that name in the session environment is dropped as well.
- `--model <model>` is present only when a model is pinned.
- `--<key> <value>` comes from `--harness <key>=<value>` or from the step's
  `[steps.harness]` table, carried verbatim (ADR-177): cosmon keeps no list of
  agy's keys, and an unknown key fails in agy's own parser at launch. Each
  resolved key is recorded on `events.jsonl` as `harness_setting_selected`
  with the channel `agy:flag` and the output of `agy --version`.
- `--dangerously-skip-permissions` approves tool permissions automatically.
  An unattended agy worker therefore runs tools without asking. Run it only
  on work and in a worktree where that is acceptable.

Why not agy's interactive TUI (`agy -i`)? It is readable and accepts typed
input, but during the session it records token usage only in its own
conversation store, as unnamed fields in a binary format, and its `/usage`
view shows the account's quota windows, not the session's tokens. Running it
would lose the usage and quota stop `cs peek` reads from the receipt, and a
quota error would leave the TUI waiting for input instead of ending the pane.

Further points:

- **Interaction.** The session stays open after the brief's turn, like a
  claude or codex worker. Type into the pane (`cs peek`, then attach) or use
  `cs whisper <id> --message …`. The worker ends its molecule with
  `cs complete`; the pane stays open until `cs done` ends the session. Ctrl-D
  in the pane closes agy's input, and agy exits after its current turn.
- **Effort.** `--harness effort=<low|medium|high|xhigh|max>` renders
  `--effort <value>`. agy also lists model ids that carry an effort suffix
  (`gemini-3.8-flash-high`). Measured on agy 1.5.19, a pinned model id whose
  suffix contradicts `--effort` is refused at launch with exit status 1 and no
  turn spent (`--model gemini-3.8-flash-high --effort low`: "conflicts with
  --effort=low"); agy applies neither. Pin a model without a suffix, or a
  suffix that agrees with the effort.
- **Typing.** The driver takes the pane's terminal out of its line mode and
  edits lines itself, because that mode drops any input line longer than 1024
  bytes on macOS (measured), which would lose a long whisper. Backspace,
  Ctrl-U (clear the line), Ctrl-D (end the session) and Enter work; arrow keys
  are ignored. Ctrl-C still stops agy and the driver.
- **Readiness.** `cs tackle` waits up to 30 seconds for the driver's
  `agy session <conversation id> · model <model>` line, which it prints only
  when agy's own `init` event arrives. If it never appears, the spawn fails
  and the session is terminated.
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

The `opencode` pane ends when the run ends; the `agy` pane ends when agy
exits (its input closed, or a failed turn such as a quota stop). With the
default `on_pane_died = "merge"`, the
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
