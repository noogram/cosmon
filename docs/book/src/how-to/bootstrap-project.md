# Bootstrap a new project with cs init

**Goal:** make cosmon track the work in an existing repository (a Rust crate, a
research repo, a folder of notes) without vendoring cosmon's source or turning
your project into a cosmon subdirectory. One command does it.

> Cosmon is a **substrate**: your project *embeds* it. Nothing of cosmon's source
> enters your dependency graph. Your project holds only a `.cosmon/` directory;
> a single globally-installed `cs` reaches into it.

## Step 1: Initialise

From the root of the project you want tracked:

```sh
cd ~/path/to/your/project
cs init
```

This creates `.cosmon/`, containing:

- `state/`: the on-disk source of truth for every molecule you create,
- `formulas/`: the canonical formula recipes (`task-work`, `temp-review`, …),
- a project id and default configuration.

`cs init` is strictly idempotent: run it twice and the second run is a no-op, so
it is safe in scripts and CI.

## Step 2: Verify

```sh
cs status
ls .cosmon/formulas/
```

`cs status` reports an empty ensemble (no molecules yet); the `ls` shows the
formulas that shipped with init. That is a healthy fresh project.

## How cosmon finds your project

Once `.cosmon/` exists, every `cs` invocation **walks up** from your current
directory to find it, exactly the way `git` finds `.git/`. So you can run `cs`
from any subdirectory and it resolves to the same project state. If you are inside
a git worktree, cosmon detects it and redirects to the main repo's `.cosmon/`, so
commands behave identically from a worktree and from the main checkout.

## Choosing a mode

Cosmon supports two embedding modes; pick one:

| You need… | Use… |
|-----------|------|
| The full lifecycle: `cs tackle` to spawn workers, `cs run` for DAGs | The globally-installed `cs` + a project `.cosmon/` (what `cs init` sets up). |
| Only to create/read/evolve molecules in-process from Rust, no spawning | The `cosmon-embed` crate (Inert-only facade). |

Most projects want the first. The second is for test harnesses, CI jobs, or
third-party schedulers that run the actual work themselves and use cosmon purely
as a state store. A per-project cosmon *daemon* is neither: it is prohibited by
cosmon's architecture; the `cs` binary is stateless by design.

## A lighter footprint: `--soft`

If you want to propagate cosmon's *conventions* into a project without any
orchestration state at all, generate just a minimal `CLAUDE.md`:

```sh
cs init --soft                    # generic conventions
cs init --soft --template rust    # cargo-based conventions
```

`--soft` writes a single small file any agent can read, and creates **no**
`.cosmon/`: no runtime, no state. Use it when you want an agent to follow the
house style but do not need cosmon to track molecules there yet.

## Upgrading an existing project

When a new cosmon release adds canonical formulas, backfill them without touching
your existing files:

```sh
cs init --upgrade
```

This adds any missing canonical formulas and a `project_id` if absent, and
overwrites nothing you already have.

## Next

- Create your first tracked molecule: [Your first molecule](../tutorials/first-molecule.md).
- Bootstrap a whole galaxy of related projects: see the `galaxy-onboarding`
  workflow (referenced from the project reference).
- Full command surface: [Project commands reference](../reference/project.md).
