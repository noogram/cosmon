# Agent adapters: a harness over harnesses

Cosmon does not replace the agentic systems you already use. It sits *above* them.
An **adapter** is the plug that names which system actually does the work: Claude
Code, aider, codex, an OpenAI or Anthropic endpoint, `llama-cpp`, a local model.
Cosmon is a **harness over harnesses**: it gives a piece of work an identity, a
lifecycle, and crash-recovery, then hands the actual agent loop to whichever
harness you picked.

## What an adapter is

When you `cs tackle` a molecule, cosmon creates the worktree, the tmux pane, and
the fleet bookkeeping, and then it has to launch *something* inside that pane to
be the agent. The adapter is that named choice: `claude`, `aider`, `openai`,
`anthropic`, `llama-cpp`, `local`. The kernel stays agnostic about which one runs;
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

## The seam is typed, not stringly

The choice of adapter and the harness that actually launches are guaranteed to be
the same thing. Cosmon's spawn seam refuses to compile if handed a bare string:
the validated adapter name is the only value the launch site accepts, so "adapter
`aider` selected" and "worker spawned with `aider`" are the same bytes by
construction, not by a hopeful runtime check. A smoke test once showed a worker
report `aider` and then route through Claude; that class of bug is now a type
error rather than a possibility. The lineage is recorded in ADR-099 and its
successors; the [Execution commands reference](../reference/execution.md)
documents the public dispatch behavior.

## What ships today, and what is roadmap

Per-molecule adapter and model choice ships **today**: you can point any molecule
at any registered harness, hosted or local, right now. The stronger reading
(long, unattended runs entirely on local models with no human in the loop) reaches
into the **Autonomous** regime, which is roadmap, not shipping. See
[The three regimes](./regimes.md) for where present capability ends and the
roadmap begins.

## See also

- [Execution commands reference](../reference/execution.md): `--adapter`,
  `--model`, and the full resolution chain.
- [Fleets: many agents, one portal](./fleets.md): a fleet's config picks adapters
  for its workers.
- [Noogram & the Cosmon kernel](./cosmon-and-noogram.md): why an agnostic kernel
  is what a distribution composes above.
