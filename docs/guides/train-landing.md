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
fix-up, no merge of the train into itself. Run the landing checks, gate the
tip once, then land it.

```sh
cs land-train --check --train-base <rev> --train-tip <rev> \
              --member <id>=<pinned head> --member <id>=<pinned head>
```

`--check` runs the landing checks without taking the trunk lock or changing
refs, the index, the checkout, or Cosmon state. Pass the same members and
`--exclude` pins when landing. Landing rechecks the current state under the
trunk lock. The gate runs separately on the exact train tip.

## Supervising the gate

Run the gate in a checkout of the train tip. For a Cargo gate, set
`CARGO_HOME` to a private directory for that train so Cargo processes outside
the train cannot hold its package-cache lock. Choose a runtime limit and
keep its verdict file on storage that remains available if the build
directory disconnects.

```sh
cs gates slot --timeout 900 --require-dir "$build_dir" \
  --verdict-file "$verdict_file" -- just gates
```

The timeout starts when the command starts, after it acquires a slot.
`cs gates slot` forwards SIGTERM, SIGINT and SIGHUP sent to the wrapper to
the command's process group. After a short grace period it kills any
remaining processes in the group. At the timeout it kills the group
immediately.

Use the verdict file to decide whether to bisect. `ran:0` is green and
`ran:<nonzero>` reports the command's status. `timed_out` and
`could_not_run:<reason>` leave the train ungated. A missing verdict leaves
the result unknown. Never eject a member on one of those outcomes. The
wrapper exits 124 on timeout and 125 when the gate could not run. A gate
command can itself return either code. When `--require-dir` is given,
`cs` probes the directory before execution and again after the run, so a
directory that became unwritable can invalidate a command result.

To serialize controllers across wrapper crashes, declare a named pool of
size one under `[gate_slots.pools]` and run the controller through
`cs gates slot --pool <name> --hand-descriptor -- <controller>`. The slot
remains held until the last inherited copy of its descriptor closes. A
daemon that retains a copy can keep the pool occupied after the controller
ends. The `full_gate` pool always closes its descriptor on exec.

## The command

```sh
cs land-train --train-base <rev> --train-tip <rev> \
              --member <id>=<pinned head> --member <id>=<pinned head>
```

Name the members in the order the train merged them.

## What cosmon checks

The landing checks run before the base moves. A refusal leaves the base and
members in place and names the offending commit or member.

1. `[harvest_authority] required` is false. When it is true, this increment
   refuses the train; close the members one at a time with `cs done`.
2. The local base branch still equals `--train-base`. Otherwise the refusal reads
   "`<base>` moved after the gate": rebuild the train on the current base and
   gate it again. (A base that already has the tip on its first-parent chain is
   a rerun, described below.)
3. Each first-parent commit of `train-base..train-tip` is a two-parent merge.
   Its first parent is the previous commit (the train base for the oldest).
   Its second parent is the next member's pinned head, and its message is that
   member's `--print-merge-message` text, with only trailer lines appended
   after it. There are as many merges as members.
4. Each member is Completed (not collapsed), integrates into this base, and its
   branch is still at its pinned head. Each local blocker has already merged or
   appears earlier in the train (ADR-192 I11).
5. Each `--exclude` head resolves to a commit, the excluded ID is absent from
   the member list, and no pinned member head contains the excluded member's
   first own commit, if it has one above the train base.
6. If the train changes a file of the trusted shell surface
   (`.cosmon/config.toml`, the formulas, and the scripts they invoke), an
   existing trust grant must already cover the surface the train will leave on
   disk. `cs land-train` never grants trust.
7. No other worktree has the base checked out. If the base is checked out in
   the main checkout, no local change may overlap a file the train changes.

The base then moves by `git update-ref <base> <tip> <train-base>`, a
compare-and-swap. A checked-out index and working tree are moved first, with
`git read-tree -m -u`, and moved back if the swap fails. The lock is released
before any member is closed: `cs done` takes the trunk lock itself, and a child
blocked on a lock its parent holds would wait forever.

## Rebuilding a red train and checking for carriers

1. Run the same gate against the current base by itself before dropping any
   members. If the base fails the gate, investigate the base failure first.
2. Find a suspect and confirm it with a witness pair (below), then record the
   suspect's pinned head and rebuild the train without it.
3. When checking and landing the rebuilt train, add `--exclude <id>=<head>`
   for each removed member. Use the head it contributed to the earlier train
   or a later head on the same first-parent history. Run the gate after the
   checks pass and before landing.

### The witness rule (ADR-192 I6′)

A probe proposes a suspect and proves nothing. `git blame` on the failing
line, or `git log -S` on the failing symbol, names a member that may be the
author of the fault. Eject that member only with a witness pair: a gate run on
the train prefix just before the suspect that is green, and a gate run on the
prefix that includes the suspect that is red. Both runs must actually have
happened, in the same environment (same checkout method, `CARGO_HOME`, build
directory, toolchain and gate command), and each verdict must be `ran:0` and
`ran:<nonzero>` respectively.

- When the probe names no member, or the pair does not hold (the prefix before
  the suspect is red, or the prefix with it is green), the probe missed.
  Bisect the member list by prefix over a base that passed the gate, and eject
  the first member whose prefix turns red.
- A `timed_out` or `could_not_run` verdict is not red. A prefix with that
  outcome is unknown: rerun it, or repair the environment, and do not eject on
  it. A missing verdict is unknown as well.
- Blame alone is not enough. The assertion line of a catalogue-wide test
  points at the file that registers the entry, not at the member that broke
  the invariant, and a carrier (below) defeats both blame and bisection,
  because the prefix that turns red is the carrier while the faulty commit
  belongs to another member. Refuse carriers before gating rather than
  expecting the probe to find them.

A late red costs more per probe (a failure that appears only after the
platform build or the longest suite finishes can take tens of minutes), but it
does not change what the pair has to show.

### Conflicting members and re-admission

A member that conflicts when the train merges it is ejected from this train.
It syncs from the landed base, by merging or rebasing the base into its branch
and re-running its own gates, and returns as a candidate for the next train.
Order the next train with the ejected members first, oldest ejection first.
Each member then reaches the front of the queue after a bounded number of
trains, so a member that keeps conflicting with newer arrivals is eventually
admitted.

### Build environment and gate order

Give the train's gate a private `CARGO_HOME`. This is project policy, not
something cosmon sets: a Cargo process outside the train that holds
`$CARGO_HOME/.package-cache` blocks the gate inside the command, where neither
the slot queue nor `cs pulse` shows it, and a timeout is the only bound.

Run the platform build first in the project's `[gates]` order, before the
suites that are quick to fail on other grounds. A reporter's trains showed
their reds roughly 50 minutes in, after the platform build; putting that build
first moves a red caused by it to the start of the run, where a probe is
cheapest.

A carrier is a member whose pinned head contains the excluded member's first
own commit. Git finds it as the oldest entry in
`git rev-list --first-parent <head> ^<train-base>`. An empty range has no
carrier. A branch can carry the first own commit even if it merged the removed
member before that member reached its pinned head. `cs land-train` refuses the
landing and names the carrier, the excluded member and the first own commit.
Rebuild the carrier without the excluded commits or remove it as well, then
gate the new train again. Keep the excluded commits available to Git until
landing. The check misses a rebased culprit when the rebase replaces the
commit IDs that a carrier contains.

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

If the train already landed upstream, switch to the local base branch and run
`git pull --ff-only`. Rerun `cs land-train` with the same arguments. Once the
train tip is on the local base's first-parent chain, `cs land-train` skips the
landing trust check and closes the open members. `cs done` warns about changed
trusted shell files, and configured `pre_done` hooks still require trust.

To land the train locally, review the named files and grant trust while the
gated tip is checked out. Then switch back to the base branch and rerun:

```sh
git diff <train-base> <train-tip> -- <named paths>
git switch --detach <train-tip>
cs trust
git switch <base>
cs land-train ...            # same arguments
```

The check compares the grant with the surface of the tip's tracked files. A
grant made from a checkout matches it only when that checkout's surface files
are exactly the tracked ones. While a surface file is untracked in the main
checkout, or a tracked one is locally modified, no train that changes the
surface can pass. Run `git status -- .cosmon/config.toml .cosmon/formulas` and
the scripts they name, and commit, remove or restore what it lists before
granting.

## Leaving a dependent chain to a train

A node that a train lands, not the runtime, carries the tag `no-auto-harvest`
from its nucleation. Its dependents wait for the train to stamp `merged_at`, and
each is dispatched with the model pinned on it:

```sh
cs nucleate … --blocked-by <id> --tag no-auto-harvest --var cosmon_model=<id>
cs run --resident _
```

`cs run` does not need a mode for this. The tag is on disk, so a relaunch of
`cs run` reads it again and holds the node the same way.

- A `Completed` molecule with the tag is never merged by an automatic path: the
  resident loop, the legacy `cs run`, the tmux pane-died hook and
  `cs patrol --harvest` all land through `cs done --if-completed`, which answers
  `reserved` and changes nothing. The resident loop writes a `hold` line, "held
  for external landing", once per molecule on `runtime-trace.jsonl`.
- `cs land-train` closes a tagged member with `cs done --expect-head`, and an
  explicit `cs done <id>` or `cs harvest --molecule <id>` typed by the operator
  lands it too. Only the automatic entry honours the tag.
- A dependent is dispatched only when each blocker is `Completed` with
  `merged_at` set, so it starts from a tree that holds its predecessors' work.
- `harvest_to:<branch>` reserves a merge in the same way: an automatic path can
  only merge to the trunk, so any routing intent is held.

## Out of scope

Choosing when to build, gate or bisect a train, syncing members from a train,
and managing train queues stay with the operator's tool (ADR-190 §6).
Pushing stays the operator's decision.
