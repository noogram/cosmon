# Fleets: many agents, one portal

A **fleet** is the set of workers running together and the `fleet.toml` that
configures them. It is what makes "ten agents in parallel" a single first-class
object rather than ten unrelated terminals you have to remember by hand.

## What a fleet is

The whole reason cosmon exists is to run many agents at once and keep track
of who is doing what. The fleet is that "many agents" made into one tracked thing:
the running set of workers and the molecules they are bound to, held together so
you can start them, watch them, and tear them down as a group. Each worker still
gets its own worktree, its own tmux pane, and its own git branch; they never
collide, but the fleet is the roster that knows they all belong together.

## `fleet.toml`: the config

One file declares the fleet's shape: which adapters its workers use, how many, and
what roles they play. This is the file the spore how-to leans on when it bundles
"fleet config" alongside recipes; a fleet's config is what pins the
[adapters](./adapter.md) a germinated polymer's workers will run under.

You do not write `fleet.toml` from scratch. `cs fleet init <template>` scaffolds
one from a named template, and `cs fleet resolve` flattens a composable
`fleet.toml` (one that includes others) into the effective configuration. Both are
documented in the [Fleet management commands](../reference/fleet.md).

## Fleet versus ensemble

Two words sit close together and are worth separating:

- The **fleet** is the *configured set*: the roster of workers and the
  `fleet.toml` that shapes it.
- The **ensemble** is that set *seen at a glance*: the dashboard read of it, what
  `cs ensemble` prints and `cs peek` shows.

One is the roster, the other is the live view of the roster. You configure a
fleet; you observe an ensemble.

## Cross-examination

A fleet is not only agents working *beside* each other; it is agents working
*on* each other's output. Reviewing is itself molecule-shaped, so the reviewer
is a different molecule, run by a different worker, in a different worktree —
never the author grading its own homework.

Three shapes recur:

- A **panel** frames one question, dispatches several personas in parallel who
  never see each other's drafts, and synthesizes where they converge and where
  they disagree. That is the `deep-think` formula.
- A **pre-mortem** is an independent audit molecule ordered behind the work. It
  reads the merged code against its spec and returns NO-GO or GO with numbered
  findings. NO-GO does not revert; it nucleates the remediation, which faces
  another round.
- A **verification molecule** re-checks a specific claim the gates cannot —
  what a surface actually renders, whether a bug is closed everywhere.

Each is an ordinary `cs nucleate --blocked-by` edge. There is no reviewer
registry and no privileged molecule kind.

[Adversarial review](./adversarial-review.md) works all three through, with the
artifacts they leave on disk, the structural guard that stops a panel dodging
its own question, a four-round NO-GO→GO example, and the honest limit: a panel
of personas over one provider is channel-independent, not error-independent.

## A naming footgun, said once

The config file is `fleet.toml`, singular. The on-disk state directory is
`.cosmon/state/fleets/`, plural. Same word, two different objects: one is the
configuration you edit, the other is where cosmon keeps the running fleet's state.
Knowing this once means no page trips you later.

## See also

- [Fleet management commands](../reference/fleet.md): `cs fleet init`,
  `cs fleet resolve`, and the ensemble commands.
- [Running a fleet of agents](../tutorials/first-fleet.md): three workers in
  parallel, watched from one portal.
- [Monitor the fleet with cs peek](../how-to/monitor-with-peek.md): reading the
  ensemble in depth.
- [Agent adapters](./adapter.md): what a fleet's config selects for each worker.
- [Adversarial review](./adversarial-review.md): how a fleet's agents
  cross-examine each other's findings.
