# The physics vocabulary

Cosmon names its commands after physics: you `nucleate` a molecule, `evolve` it
one step, let it `decay` into children, `freeze` and `thaw` a worker. This page
is the one place the metaphor is explained in full. Every generated Reference
page links back here through a banner, and every tutorial glosses a term the
first time it appears, but the *why* lives here, once.

## Why physics names

A unit of tracked work in cosmon really does behave like a physical object. It
is **created** out of a template, it **changes state** one step at a time, it
can be **suspended** and later resumed, it can **split** into smaller pieces, and
when it is finished it leaves a **trace** on disk that you can inspect long
after. Those are the same verbs physics uses for a particle: create, evolve,
freeze, decay, observe. So instead of inventing bland words like *create-task*
and *advance-task*, cosmon borrows the words that already fit, and the borrowed
word carries an intuition that transfers for free. When you read `decay`, you
already expect "one thing becomes several," and that is exactly what it does.

**The names are the model, not decoration.** This is the important part. The
command *names* are how cosmon describes what a piece of work *is*. You cannot
strip the metaphor out and keep the meaning, because the meaning is the
metaphor. That is why the vocabulary is taught, not hidden away in an appendix.

**Naming, not physics.** One honest caveat: cosmon borrows physics *words*, not
physics *equations*. Early design notes tried to run real thermodynamic formulas
(free energy, Carnot cycles, three "laws of cosmon thermodynamics") and those
were later demoted as cargo cult: they made no prediction that would fail if the
numbers were wrong. What survives is genuine on its own (a token budget is a real
resource tracker; worker-status entropy is a real Shannon measure of how spread
out the fleet's states are). So read the vocabulary as a *naming scheme with good
intuitions*, not as a claim that a molecule obeys Schrödinger's equation.

## Two registers

Not every command has a physics name, and that is deliberate. Cosmon's verbs
split into two registers:

- **Lifecycle verbs (physics register)**: `nucleate`, `evolve`, `complete`,
  `collapse`, `decay`, `freeze`, `thaw`, `merge`. These act *on* a molecule's
  state and follow the physics model above.
- **Operator verbs (vernacular register)**: `tackle`, `done`, `wait`, `peek`,
  `patrol`, `run`, `reconcile`. These are the human's toolkit for steering the
  fleet. They are plain CLI words on purpose: `peek` is your window into the
  system, not a physics act on a molecule.

The split resolves what looks like inconsistency. `nucleate` is physics because
it transforms a molecule; `peek` is vernacular because it is *you* looking. Once
you know which register a verb lives in, the naming stops feeling arbitrary.

## The core glossary

| Term | What it means in cosmon |
|------|-------------------------|
| **molecule** | The fundamental unit of tracked work: one running instance of a formula, bound to a task. It has a state (`Active` / `Frozen` / `Completed` / `Collapsed`), a current step, and a durable trace on disk. |
| **formula** | The recipe a molecule follows: a TOML template of ordered steps with exit criteria. A formula is a template; a molecule is the running instance of it. |
| **nucleate** | Create a new molecule from a formula. Pure creation; nothing runs yet. |
| **evolve** | Advance a molecule one step along its formula, recording evidence. On the last step it auto-completes. |
| **complete** | Move a molecule from Active to Completed. Idempotent; running it twice is the same as once. |
| **collapse** | Terminate a molecule permanently, recording a final reason. Cannot be undone by `complete`. |
| **decay** | One molecule spawns N child molecules mid-flight (e.g. a plan splitting into tasks). The parent does not block on them. |
| **merge** | Combine several molecules' outputs into one synthesis. |
| **freeze** / **thaw** | Suspend a worker with its state preserved / resume it later. |
| **ensemble** | The whole fleet of molecules and workers, seen at a glance (`cs ensemble`). |
| **worker** | A running agent instance bound to a molecule: a process in a tmux pane. Ephemeral; the molecule's state on disk outlives it. |
| **spore** | A shareable template of an *entire* wired DAG of molecules (formulas + fleet config + a proof), not just one. It **germinates** into a running polymer. Where a formula nucleates one molecule, a spore germinates the whole set. |
| **polymer** | The running DAG of linked molecules a spore germinates into, also called a *mission*. |
| **tackle** / **done** | Start a worker on a molecule / tear it down after merging its branch. Operator verbs, not physics. |
| **peek** | The operator's TUI window into the fleet. Vernacular. |

If you come from knowledge representation, this table is cosmon's domain
ontology in Gruber's sense — an explicit specification of a conceptualization:
a controlled set of entity types (molecule, formula, worker, spore, polymer) and
the generative relations between them (nucleate, germinate, decay, merge).

## The template/instance table

The clearest way to see how `formula`, `molecule`, `spore`, and `polymer` fit
together is a two-by-two:

|              | template (immutable) | instance (lives / dies) |
|--------------|----------------------|-------------------------|
| **one unit** | `formula`            | `molecule`              |
| **whole DAG**| `spore`              | `polymer` / `mission`   |

The verb for the top row is **nucleate** (`formula ─nucleate→ molecule`); the
verb for the bottom row is **germinate** (`spore ─germinate→ polymer`). Same
generative relation, one scale up.

---

For the full command reference grouped by role, see the
[CLI overview](../reference/overview.md). For the honest record of which physics
metaphors were kept and which were demoted, see the design-phase appendix in the
repository (`docs/appendix-physics-inspiration.md`).
