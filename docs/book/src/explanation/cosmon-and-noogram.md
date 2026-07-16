# Noogram & the Cosmon kernel

> **Noogram is the distribution; Cosmon is its kernel.**

That one sentence is the whole relationship. This page unpacks it: what a
distribution is, what the kernel is on its own, and why "kernel" is the right
word.

## What Noogram is: the distribution

Noogram is an open-source **distribution** for composing, piloting, and auditing
missions you hand to AI systems. A distribution is not a single tool; it is a
curated whole built around a core: the kernel, plus the adapters that let it
drive different agentic systems, the fleets that run many agents together, the
spores that package whole mission shapes, and the pilot surfaces you watch it
through. Noogram is the larger system: the place where work is delegated to AI,
wired together, watched over, and later reviewed with its full reasoning intact.

## What Cosmon is: the kernel

Cosmon is the kernel: the load-bearing core, and the subject of most of this
book. It is the part you install today: a stateless command-line tool that gives
AI agents an **identity** (this worker, on this task), a **typed lifecycle** (work
moves through well-defined states, and only valid transitions are allowed), and
**crash-recovery** (state lives on disk, so a crashed agent resumes rather than
restarts). No daemon, no server, no scheduler: just a binary and a directory of
files.

**The kernel runs fully standalone.** You do not need the Noogram distribution to
get value from Cosmon. If all you ever want is to run several AI coding agents in
parallel on one codebase and keep track of who is doing what, Cosmon does
that fully, by itself. It is not a trial edition or a client stub. Like SQLite, it
is complete on its own *and* embeddable in something larger; both readings are
true at once.

## The analogy: a kernel and its distribution

The cleanest way to see the relationship is the one the words already name: a
kernel and the distribution built around it.

The Linux kernel is a complete, load-bearing core. Debian and Ubuntu are
distributions that compose that kernel with everything a usable system needs.
**The kernel runs without the distribution**: it is not incomplete; the
distribution is a convenience built on top, not a requirement. Cosmon is the
kernel; Noogram is the distribution built around it. Adopt the kernel alone and it
is complete. Add the distribution and the kernel becomes the core inside it: the
part that handles identity, lifecycle, and recovery while the larger system
composes, pilots, and audits missions around it.

"Kernel" means exactly this: the essential core others build upon, that also
stands on its own.

## Boundary honesty

A few deliberate limits on how this page talks:

- **No private names.** Cosmon and Noogram are attributed to Noogram
  (noogram.dev). No company, fund, or individual names appear here.
- **Mechanism over adjectives.** Cosmon earns its description by what it *does*
  (identity, typed lifecycle, crash-recovery), not by adjectives like "powerful"
  or "revolutionary."
- **The distribution does not complete the kernel.** Naming Cosmon a "kernel" must
  never be read as "Cosmon needs Noogram to be useful." It does not; the disarm is
  written into the analogy above, not left to inference.
- **No claim of a running autonomous engine.** Today Cosmon is a stateless,
  one-shot CLI: it does not run itself. A resident runtime that walks work
  unattended is on the roadmap (see [The three regimes](./regimes.md)), not
  something shipping finished. This page will not imply otherwise.
- **The lineage claim travels with the reader, not the stranger.** "Kernel of
  Noogram" is a crisply falsifiable statement, and it belongs where the reader has
  already run `cs` and can see Cosmon standing on its own. It carries **no
  outbound link** while the public Noogram site is not yet live; a dead link would
  refute the claim on first contact. The sentence and any link to it go public
  together, gated on the site actually resolving, not on a date.

For the physics vocabulary that names cosmon's commands, see
[The physics vocabulary](./physics-vocabulary.md). For the design bet underneath
the "kernel," see [Why a stateless CLI](./stateless-cli.md). For the seam that
lets one kernel drive many agentic systems, see [Agent adapters](./adapter.md).
