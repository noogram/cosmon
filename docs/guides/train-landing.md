# Landing a train with `cs land-train`

A train is a batch of completed molecules merged together on a side branch and
gated once, instead of each being merged and gated by its own `cs done`
(issue #213). ADR-190 D3 splits the work: the operator's tool builds and gates
the train; cosmon moves the base branch and closes the members. This page is the
contract between the two.

## What the operator's tool must build

Start the train branch from the current base and record that commit; it is the
`--train-base` the landing compares against.

For each member, in order:

- Pin the member's branch head. The merge must take exactly that commit, and
  the same id is passed to `cs land-train` and then to `cs done --expect-head`.
- Merge it with `git merge --no-ff`, never a fast-forward, squash or rebase. The
  merge commit carries the member's `Mol-Id` and `Mission-Id` trailers; without
  it the member has no trailer carrier and its worker commits sit on the base's
  first-parent chain.
- Use the message `cs done` would write, verbatim:

  ```sh
  git merge --no-ff -m "$(cs done <id> --print-merge-message)" <pinned head>
  ```

  Pass the text with `-m "$(...)"` rather than `-F <file>`. A
  `prepare-commit-msg` sign-off hook, such as the one this repository installs,
  then gets the same input it gets for `cs done`'s own merge, and the trailer
  lands in the same paragraph. With `-F` the hook adds a blank line first: the
  merge is still accepted, but its message no longer matches what two serial
  `cs done` merges would have written byte for byte.

Nothing else may sit on the train's first-parent chain: no operator commit, no
fix-up, no merge of the train into itself. Gate the tip once, then land it.

## The command

```sh
cs land-train --train-base <rev> --train-tip <rev> \
              --member <id>=<pinned head> --member <id>=<pinned head>
```

Name the members in the order the train merged them.

## What cosmon checks

It takes the trunk lock once. Every check runs before the base moves, and a
failure moves nothing, closes nothing, and names the offending commit or member:

1. `[harvest_authority] required` is false. When it is true, this increment
   refuses the train; close the members one at a time with `cs done`.
2. The base branch still equals `--train-base`. Otherwise the refusal reads
   "`<base>` moved after the gate": rebuild the train on the current base and
   gate it again. (A base that already has the tip on its first-parent chain is
   a rerun, described below.)
3. Each first-parent commit of `train-base..train-tip` is a two-parent merge.
   Its first parent is the previous commit (the train base for the oldest).
   Its second parent is the next member's pinned head, and its message is that
   member's `--print-merge-message` text, with only trailer lines appended
   after it. There are as many merges as members.
4. Each member is Completed (not collapsed), integrates into this base, and its
   branch is still at its pinned head.
5. If the train changes a file of the trusted shell surface
   (`.cosmon/config.toml`, the formulas, and the scripts they invoke), an
   existing trust grant must already cover the surface the train will leave on
   disk. `cs land-train` never grants trust.
6. If the base is checked out in the main checkout, no local change may overlap
   a file the train changes.

The base then moves by `git update-ref <base> <tip> <train-base>`, a
compare-and-swap. A checked-out index and working tree are moved first, with
`git read-tree -m -u`, and moved back if the swap fails. The lock is released
before any member is closed: `cs done` takes the trunk lock itself, and a child
blocked on a lock its parent holds would wait forever.

## Closing the members

Each member is then closed, in train order, by `cs done <id> --expect-head
<pinned head>`, run as a child process. `cs done` stays the only writer of
`merged_at`. Because each member is already on the base, it takes the
already-landed path: no merge, no post-merge gate.

The first failing closure stops the run. The message names the members already
closed and the ones still open. Fix the cause and rerun the same `cs land-train`
command. A kill between the fast-forward and the closures leaves the same kind
of state. The base is then at the train tip, or past it: in a galaxy that
tracks harvest state each closure commits that state to the base, and another
pilot may land once the lock is released. When the tip is on the base's
first-parent chain, the rerun checks the train's shape again, skips the landing,
and closes only the members whose `merged_at` is still empty. A member is never
closed twice.

## When the shell-surface check refuses

A trust grant pins the surface that is on disk when the grant is made. The
train's surface is not on disk until the landing, so review it and grant it from
a checkout of the tip, then rerun:

```sh
git diff <train-base> <train-tip> -- <named paths>
git switch --detach <train-tip>
cs trust
git switch <base>
cs land-train ...            # same arguments
```

Until the train lands, that grant reads stale for the base's surface, so a
`cs done` that needs trust in the meantime is refused. The comparison reads the
tip's tracked files. A surface file that exists in the main checkout but is not
tracked would make the two differ; a train cannot change such a file anyway.

## Out of scope

Building, gating or bisecting a train inside cosmon, syncing members from a
train, queues and gate slots (ADR-190 §6). Pushing stays the operator's
decision.
