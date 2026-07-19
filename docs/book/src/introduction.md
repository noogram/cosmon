# cosmon

Noogram is an open system for running long AI-agent missions inside your own
perimeter, keeping every step as plain files you own. Its kernel, **cosmon**,
is a stateless CLI that gives each agent an identity, a lifecycle, and crash
recovery.

The code lives at [github.com/noogram/cosmon](https://github.com/noogram/cosmon),
open-core: the kernel, the CLI, and the runtime are AGPL-3.0-only, and the
network SDK third parties link to talk to an instance over the wire is
Apache-2.0 ([LICENSE](https://github.com/noogram/cosmon/blob/main/LICENSE) maps
the split crate by crate). It is early-stage but functional — the core types,
the `cs` CLI, state persistence, tmux transport, and the DAG engine all ship
today, and the API is still moving, so expect breaking changes before 1.0.

- **A social organization of agents.** A long mission becomes small tasks, each
  with a precise goal. Agents take them on side by side, wait on one another
  when one task depends on another, and
  [cross-examine each other's findings](./explanation/fleets.md) — panels,
  reviews, counter-arguments — before anything reaches you. Work at scale, the
  way a good team organizes itself.
- **Runs inside your perimeter.** The agent works on your corpus, on your
  machine. Every step, every decision, and every result is written to plain
  files on a disk you control. Only the model call leaves your machine — no
  server in the loop, no database, no Cosmon account.
- **You stay the judge.** The agent hands you a draft on its own branch, with
  its reasoning and evidence alongside; what the gates couldn't verify is marked
  unverified, never silently accepted. Nothing lands until it passes your gates.
- **The story can't be quietly rewritten.** Every mission leaves a complete,
  replayable work record. If anyone — human or agent — edits it after the fact
  without resealing it, `cs verify` notices. It catches a careless edit, not a
  determined forger.
- **A crash is a pause, not a loss.** Everything the agents record lives in
  those same plain files, so a fresh worker picks up where a crashed one
  stopped.
- **A harness over the harnesses you already use:** Claude Code, Codex, Aider,
  and other CLI agents; hosted APIs from Anthropic, OpenAI, Google Gemini,
  Mistral AI, Qwen, DeepSeek, GLM, Kimi, and more; or local models through
  llama.cpp and Ollama.
- **Federate** *(under construction)*. Several machines cooperating on one
  mission, with no central owner.

<!-- Four-band illustration (pilot → control plane → disk → federation), pitch
     spec 2026-07-15. The asset lives at docs/book/src/intro-how-cosmon-runs.svg
     so mdBook copies it into the built book alongside this page; the ./ ref
     below resolves against the rendered introduction.html. -->
<figure>
  <img class="diag diag-light" src="./intro-synthetic-light.svg"
       alt="Long AI-agent missions run on your machine: a pilot drives a chain of agents that write every step to the .cosmon/ folder as plain files you own; only the model call leaves your machine.">
  <img class="diag diag-dark" src="./intro-synthetic.svg" alt="">
</figure>

## noogram and its cosmon kernel

On a single machine, the tool you run is **cosmon**; its command is `cs`. You
`cs tackle` a piece of work to start an agent on it, and `cs done` to close it
out; the record of both lands under `.cosmon/`.

Noogram's ambition is a **federated agentic system**: many cosmon instances
cooperating, each keeping its own record, with no single owner in the middle.
The first brick is already here: the **remote mode**, where a `cosmon-remote`
client talks to a `cosmon-rpp-adapter` service over HTTP(S) on another host.
Broader peer-to-peer federation between instances is on the roadmap; the shape of
that link is still being explored. That larger, cooperating whole is **noogram**;
**cosmon** is its kernel. Today, run several agents in parallel on one machine and
cosmon does that by itself, complete and standalone. See
[Noogram & the Cosmon kernel](./explanation/cosmon-and-noogram.md) for the
relationship, and [Agent adapters](./explanation/adapter.md) for how a concrete
agent or model plugs in.

## Where to go next

- **Want it running now?** [Install cosmon](./getting-started/install.md), then
  [Ten minutes to cosmon](./getting-started/ten-minutes.md) takes you from an
  empty terminal to one merged piece of agent work.
- **New here?** The [Explanation](./explanation/physics-vocabulary.md) section is
  the conceptual on-ramp: the physics vocabulary, the kernel/distribution split,
  formulas and adapters and fleets, and how cosmon survives crashes. Start there.
- **Learning by doing?** The tutorials walk you from
  [your first molecule](./tutorials/first-molecule.md) to
  [your first fleet](./tutorials/first-fleet.md) to
  [composing a DAG](./tutorials/first-dag.md).
- **Looking up a command?** The [CLI reference](./reference/overview.md) is
  generated from the tool itself, grouped by role. The full
  `nucleate → tackle → wait → done` cycle is covered in
  [The three regimes](./explanation/regimes.md).

> This site is built by mdBook and rendered by the same pipeline that builds
> cosmon's own documentation.
