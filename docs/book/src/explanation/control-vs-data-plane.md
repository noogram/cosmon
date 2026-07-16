# Control plane vs data plane

> These commands use physics-inspired names (nucleate, evolve, decay, …). New to
> the vocabulary? See [The physics vocabulary](./physics-vocabulary.md).

When people first meet a multi-agent system, they reach for the obvious picture:
agents send each other messages. Agent A finishes, mails its result to Agent B,
B reads the mail and starts. That picture needs a mailbox, a queue, a broker:
another running thing to keep alive, another place state can be lost.

**Cosmon has no mailboxes.** It separates two questions that the mailbox picture
tangles together:

- **When** should the next piece of work start? (the *control plane*)
- **What** does it read to do that work? (the *data plane*)

These two flow through completely different channels.

## The control plane is the DAG: one yes/no per molecule

Cosmon links molecules with typed edges: `Blocks`, `BlockedBy`, `DecayProduct`,
`Refines`, `Entangled`. Together they form a directed graph: the DAG. But look
at how little information an edge carries. A `Blocks` edge between molecule A and
molecule B says exactly one thing: *is A done yet, yes or no?* A single yes/no
signal — done or not-done. Information theory has a precise name for a yes/no
answer, and that name is **one bit**.

That is the entire control signal. `cs evolve` writes the bit (A moved forward);
`cs wait` and the ready-frontier computation read it (is B allowed to start?).
Ordering, the *when*, is the only thing that travels on this channel. No
payload, no content, no message body. Just done / not-done, edge by edge.

**"One bit" describes the signal, never the delivery.** This is worth spelling
out, because a `cs done` clearly hands the next worker far more than a yes/no: a
merged branch, a report, evidence files, however many megabytes of code. All of
that is real — it just travels on the *other* channel. The data plane is
arbitrarily large; the control plane is one bit. The point of the split is
exactly that contrast: the DAG says *go*, and the filesystem holds *everything
you go and read*.

## The data plane is the filesystem: all the content

Everything a downstream worker actually *reads* (the predecessor's report, its
code changes, its evidence files) flows through shared state on disk:

- `.cosmon/state/` JSON files,
- git worktrees and branch lineage,
- a molecule's response and synthesis files,
- evidence attachments.

Workers read and write these files directly. The DAG never carries the content;
it only tells a worker *when* it is allowed to go look. When molecule B becomes
ready, it reads molecule A's output straight off the disk, because A's branch
was merged into B's worktree base before B was dispatched (this is
*merge-before-dispatch*, and it is why the git history a worker sees already
contains its predecessor's work).

## Why split them this way

Collapsing the two planes into one messaging channel is where distributed
systems get their hardest bugs. Keeping them separate buys three concrete things:

- **It survives crashes.** State is on disk, never in a broker's RAM. Kill
  everything, restart, and the DAG bit plus the files on disk fully reconstruct
  where you were. (See [Crash recovery](./crash-recovery.md).)
- **It needs no broker.** There is no queue process to run, secure, scale, or
  lose messages in. One fewer moving part, one fewer failure mode.
- **It makes reconciliation a pure projection.** Because the authoritative
  content is all on disk, `cs reconcile` can rebuild every derived surface
  (status files, issue lists, dashboards) as a deterministic function of the
  files. Run it twice, get the same result.

## The rule of thumb

When you feel the urge to add "messaging" between molecules, stop and ask the two
questions instead:

1. **Which typed link expresses this dependency?** (control plane: the *when*)
2. **Which file on disk carries the payload?** (data plane: the *what*)

Every real need answers cleanly in those two terms. If it seems to need a third
thing, a mailbox, that is almost always a dependency edge and a file wearing a
disguise.

> **The channels at a glance.** Cosmon actually distinguishes six channels: the
> service registry, the DAG (1 bit, authoritative ordering), the filesystem
> (authoritative content), the artifact chain (proof of work), propulsion (a
> zero-byte wake-up from pilot to worker), and *whisper* (advisory text a human
> pilot can send to a live worker). The first three are the load-bearing pair
> described above plus their registry; the rest are thin signals layered on top.
