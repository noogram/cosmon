# Curate the backlog with temperature tags

**Goal:** keep a growing backlog of pending molecules honest, so it never turns
into sediment that a later `cs run` or patrol accidentally resurrects. Cosmon's
tool for this is a small set of **temperature tags** you attach to pending work.

> A pending molecule that just sits there untagged is the problem. The tag says,
> in one word, how hot the work is: whether to grab it now, park it, or let it
> cool. The cost of stale pendings is not disk space; it is *scope pollution*: a
> greedy runtime can pick them up.

## The four temperatures

| Tag | Meaning |
|-----|---------|
| `temp:hot` 🔥 | Actionable now: tackle soon; often unblocks other work. |
| `temp:warm` 🌡️ | Valid, not urgent: fine to park on the shelf. |
| `temp:cold` ❄️ | Interesting but deprioritised: revisit in a later cycle. |
| `temp:frozen` 🧊 | Blocked on an external decision or a missing prerequisite. |

## Tag a molecule

```sh
cs tag task-20260711-a1b2 --add temp:warm
```

Remove or change a tag the same way:

```sh
cs tag task-20260711-a1b2 --remove temp:warm --add temp:hot
```

## See only the actionable queue

```sh
cs ensemble --tag temp:hot
```

Add `--json` to feed the actionable set into a script.

## The rules that keep it clean

- **Every pending molecule older than ~48h should carry a `temp:*` tag.** If it
  has none, either tag it or `cs collapse` it with a reason. An untagged, aging
  pending is a bug in your backlog, not a neutral state.
- **Drop the tag when you tackle it.** A hot molecule you have started is no
  longer on the shelf; it is in motion. If it bounces back to pending (a
  revision), re-tag it.
- **Promote when a blocker clears.** When a `temp:frozen` molecule's prerequisite
  lands, re-tag it `temp:hot` or `temp:warm` and consider tackling it.
- **Decomposition auto-tags its children.** Any workflow that nucleates child
  molecules should immediately tag each child `temp:warm`, so no child is ever
  born invisible. The periodic sweep is a safety net, not the primary mechanism.

## Periodic hygiene

Every week or so (or after a big session) let the system curate itself. The
`temp-review` formula scans every pending by age and tag, triages the stale
ones, and writes a report:

```sh
cs nucleate temp-review
cs tackle <id>
cs wait <id>
cs done <id>
```

The worker sweeps the backlog and produces a triage report: what it collapsed,
what it tagged, the top priorities, and trends. Because curation is itself just a
molecule, the backlog stays honest with the same `nucleate → tackle → wait →
done` loop you use for everything else.

## See also

- [Fleet management reference](../reference/fleet.md): `cs tag`, `cs ensemble`.
- [Your first molecule](../tutorials/first-molecule.md): the lifecycle loop the
  `temp-review` sweep rides on.
