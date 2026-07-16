---
type: founding
created: 2026-04-01
tags: [cosmon, founding-thesis, agents, rust, physics, open-source]
status: draft
---
# Founding Thesis — Cosmon

## A Rust Framework for Multi-Agent Orchestration

> *"Every agent fleet is a question the organization asks of itself, and the cost of asking is measured in tokens — so ask only what matters."*

> **This is an optional deep-dive, not required first reading.** New here?
> Start with the concise on-ramp: the
> [physics vocabulary](docs/book/src/explanation/physics-vocabulary.md) and
> [crash recovery](docs/book/src/explanation/crash-recovery.md) in the book,
> plus the [North Star](docs/vision/north-star.md) one-page vision (cosmon as
> the AGPL *kernel of an agentic OS*). Come to this THESIS for the full
> reasoning underneath the vocabulary — the *why*, once you want it.
>
> **Note on scope (2026-07-14).** The admitted-costume thermodynamics — the
> Three Laws, Carnot efficiency, the Helmholtz free-energy formula, and the
> cosmological timeline, all of which its own Amendment 3 flagged as decorative
> analogies that fail the Feynman Test — were trimmed from this document. What
> remains is load-bearing: the lifecycle verbs over the real typestate machine,
> the Write-Read Asymmetry, the crash-recovery + git-worktree-per-molecule
> wedge, and the Four Principles. Each demoted construct, with the reason it was
> cut, is recorded in
> [`docs/appendix-physics-inspiration.md`](docs/appendix-physics-inspiration.md).

### Four Founding Principles

These principles are the axioms of the cosmon universe. Every design and configuration choice must be consistent with them.

**0. It from Bit (Self-Reference and Write-Read Asymmetry).** The system operates on itself. Cosmon orchestrates its own development, molecules track ideas about molecules, and workers improve the tools that launch them. A feature that cannot operate on itself has not earned its place: self-reference is how we test that an abstraction is real, not recursion for its own sake. It is the background signal behind every design decision — the CMB of cosmon. **The operational content of this principle is the Write-Read Asymmetry:** `cs evolve` writes state, `cs wait` reads state, and the one-tick lag between them gives the feedback loop its temporal direction and its safety margin (see Amendment 3, §3.1; Part XVIII).

**1. Transport / Cognition.** The framework routes, the agent thinks. Cosmon never reasons: it spawns, monitors, routes, and persists. The agent never manages its own lifecycle; it focuses on the task. This separation is the key to longevity: AI models evolve, the transport layer endures.

**2. Intentions, not Ownership.** Cosmon owns intentions about workers, not the workers themselves. Desired state is persisted; observed state is derived from reality. A reconciliation loop drives convergence. The fleet.json is a blueprint, not a photograph.

**3. Minimum Action.** Every token consumed has a real cost: computational, economic, and environmental. Cosmon seeks the path of least action: the organizational structure that achieves the mission with minimum energy. Don't spawn 6 agents when 3 suffice. Don't use Opus when Haiku works. Make the cost visible. Tend toward equilibrium.

The existing three principles are consequences of Principle 0: Transport/Cognition is the separation that makes self-reference non-circular; Intentions not Ownership is why self-management doesn't create infinite loops; Minimum Action bounds the cost of self-reference.

#### The Seven Manifestations

Principle 0 is derived from observation, not theory imposed on the system. Self-reference is already pervasive:

1. **Cosmon orchestrates its own development.** `cs tackle` creates molecules that improve cosmon itself.
2. **OxyMake and cosmon improve each other.** OxyMake schedules cosmon convoys; cosmon orchestrates OxyMake development.
3. **Neurion registers itself.** The nervous system MCP server holds an entry in its own SQLite database.
4. **Surfaces project the state that creates them.** `STATUS.md` is generated from molecule state, including molecules that improve surface sync.
5. **Molecules track ideas about molecules.** This thesis elevation was itself tracked as a molecule.
6. **Workers improve the tools that launch them.** A `cs tackle` worker can modify `tackle.rs` — the code that constructed its own prompt.
7. **`cs tackle` was manually done before it existed.** The first implementation of tackle was performed through the exact workflow that tackle now automates.

**The Self-Reference Test.** Every new feature should pass: *can this feature operate on itself?* If cosmon adds a new molecule kind, can a molecule of that kind track work on the feature? If cosmon adds a new CLI command, can that command be invoked as part of cosmon's own development? This is a design smell detector, not a CI gate.

---

These principles are **mission-agnostic**. Cosmon orchestrates any work that AI agents can perform: writing code, producing documentation, analyzing data, deploying infrastructure, conducting research, debugging systems. The physics metaphor — molecules, formulas, energy — is deliberately generic. A molecule of work can be an article, a function, a deployment, or an experiment. The framework does not care what the agents think about; it cares how they coordinate.

## Preamble — The Scientific Method

This thesis borrows heavily from physics. That borrowing carries a debt: the same
empirical discipline that makes physics trustworthy must apply here. A beautiful
metaphor that does not survive contact with observation is decoration, not
architecture.

The method is Galilean:

1. **Observe.** Watch the running system. Record what actually happens when agents
   execute, stall, recover, and communicate. Do not begin with theory.
2. **Measure.** Attach numbers to the observations. Token counts, state-transition
   latencies, message volumes, recovery times. If a claim cannot be measured, it
   cannot be tested, and it does not belong in this document.
3. **Model.** Propose the simplest mechanism that explains the measurements. The
   physics vocabulary (entropy, temperature, phase transition) earns its place only
   when it compresses observed data better than a plain description would.
4. **Predict.** Derive a consequence the model has not yet been tested against. Run
   the experiment. If the prediction fails, update the model, not the data.

> *"It doesn't matter how beautiful your theory is, it doesn't matter how smart
> you are. If it doesn't agree with experiment, it's wrong."*
> — Richard Feynman

Every architectural claim in this thesis is subject to that test. When Part I says
agent fleet behavior exhibits phase transitions, that is a prediction: there exists a
measurable threshold (agent count, task complexity, communication volume) beyond which
system behavior changes qualitatively. If no such threshold can be observed, the claim
is wrong and the section must be revised.

The physics metaphor is not the architecture. The architecture is the set of types,
state machines, and protocols that survive the Galilean cycle. The metaphor is
scaffolding — useful during construction, disposable once the structure stands.

### The Informational Foundation

Why reach for physics at all? Because John Archibald Wheeler's *It from Bit*
(1990) proposed that information is the foundation of physics: *"every it —
every particle, every field of force, even the spacetime continuum itself —
derives its existence from apparatus-elicited answers to yes-or-no questions,
binary choices, bits."* That is why Principle 0 (It from Bit) precedes the
other three: it is the operating axiom, not decoration.

Three quantitative links justify borrowing the vocabulary — these are
equalities with units, not analogies:

- **Landauer's bound.** Erasing one bit costs at least *k*<sub>B</sub>*T* ln 2
  joules. Information is physical: reconstructing lost agent context costs
  energy (tokens).
- **Bekenstein bound.** The maximum information in a region scales with its
  boundary area, not its volume — the model for a bounded context window.
- **Shannon channel capacity.** *C* = *B* log₂(1 + S/N) — the context window
  is a noisy channel with finite capacity.

The Galilean method of the Preamble is the safety net: Wheeler's vision is a
hypothesis, not a licence. Every claim that the physics vocabulary "fits" agent
orchestration must survive observation, or the section is wrong and gets
revised. The seven manifestations of self-reference under Principle 0 are the
empirical evidence that this foundation is load-bearing, not ornamental.

---

## Part I — Vision: The Universe Metaphor

### The Name

Cosmon comes from Greek *kosmos* (order, universe) and the particle suffix
*-on* (electron, photon, boson). The name carries three readings at once: a
**universe** whose laws of physics are the type system, state machines, and
channels that govern agents; a **particle** — each agent a discrete entity with
identity and state; and a **field** that permeates the system, agents being its
excitations.

### Recovery Signals

When the universe reboots — a crash, a token limit, a machine restart — agents
resume via short, codified meta-signals briefed into their prompt like pilot
emergency procedures: `⚛ COSMON RESUME` (the local Big Bang — check state and
resume), `📬 NUDGE` (a force carrier — information arrived, react),
`⚛ COSMON FREEZE` (time stops for this particle — state preserved), and
`⚛ COSMON DRAIN` (finish current work, then rest). A system vocabulary distinct
from natural-language instructions.

### Self-Reference and Bootstrap

Cosmon is built by agents orchestrated by a predecessor system, and once mature
it orchestrates the agents that maintain it — the bootstrap paradox (the
compiler that compiles itself). This demands incremental migration, never a
big-bang cutover. Reflexivity operates at the **content** layer (formulas, agent
definitions, skills — editable TOML) but not at the **transport** layer (the
compiled binary and state store), which changes only through the standard build,
test, and review cycle. The Rust type system is the guardrail that keeps
self-modification safe.

> The physics metaphor is deliberately generic and mission-agnostic. The full,
> product-facing vocabulary lives in
> `docs/book/src/explanation/physics-vocabulary.md`; this thesis records the
> reasoning underneath it.

## Part II — The Problem Space

### The Era of Agents

We are entering what Andrej Karpathy calls the "era of agents." The programmer becomes a director: specifying goals, reviewing output, steering course. The code is a by-product of the conversation between human intent and agent execution. The bottleneck shifts from technical capability to the human's ability to formulate goals, review output, and orchestrate parallel streams.

This is not speculation. It is the operational reality of any team running multiple AI coding agents simultaneously. The human supervisors need tools to manage the entities that write code, not to write it themselves.

### The Orchestration Gap

Tools exist for orchestrating workflows. Airflow, Prefect, Temporal, and OxyMake handle DAGs of tasks: dependencies, caching, retries, scheduling. They treat each task as a function: input in, output out, no persistent identity, no state between invocations.

Agents are not functions. An agent has:

- **Identity.** It has a name, a role, capabilities, constraints. It persists across invocations.
- **State.** It is idle, working, stalled, or recovering. Its state changes over time and must be tracked.
- **Context.** It accumulates knowledge within a session and loses it between sessions (decoherence). Context must be explicitly managed.
- **Communication.** It sends and receives messages. Some messages are persistent (formal handoffs), others ephemeral (coordination nudges).
- **Lifecycle.** It is spawned, it works, it may crash, it is restarted, it eventually stops. The lifecycle must be managed.
- **Autonomy.** It makes decisions. It may take unexpected paths. It requires monitoring, not just scheduling.

No existing workflow orchestrator handles these concerns. They treat agents as functions, not as entities with identity and state. The gap is a missing category of tool, not a missing feature.

### Transport and Cognition

The most important architectural distinction in agent orchestration is the separation between transport and cognition.

**Transport** is the mechanical substrate: spawning processes, routing messages, persisting state, checking health, managing sessions. Transport is deterministic, testable, and should be implemented in a systems language with strong type guarantees.

**Cognition** is the agent's intelligence: reasoning about tasks, generating code, writing analysis, making decisions. Cognition is provided by the AI model (Claude, GPT, Gemini, or a local model) and is inherently non-deterministic.

The framework provides transport. The agent provides cognition. Cosmon never reasons: it routes, persists, monitors, and dispatches. The agent never manages its own lifecycle; it focuses on the task at hand, trusting the framework to handle the plumbing.

This separation is the key to framework longevity, not merely clean architecture. AI models evolve rapidly; the transport layer does not need to change when a new model is released. Conversely, the transport layer can be optimized, tested, and hardened independently of the unpredictable behavior of AI cognition.

### Intentions, not Ownership

> "Cosmon does not own workers. It owns intentions about workers."

A worker is a living process — a Claude session, an Ollama instance, a running agent. Cosmon does not control it directly. It declares what it **wants** (desired state) and observes what **is** (observed state). The reconciliation between intention and reality is the core operation of the transport layer.

This follows the Kubernetes declarative model, not the Slurm imperative model:
- **Desired state** is persisted: "this worker should be running" (Running, Paused, Stopped).
- **Observed state** is derived from ground truth: tmux is alive or dead, the terminal shows Working or Idle.
- **Effective status** is computed, never stored: the pure function `reconcile(desired, observed)` produces the status the user sees and the actions the system takes.

The consequences are profound:
- fleet.json records **intentions**, not reality. It is a blueprint, not a photograph.
- When reality diverges from intention (crash, timeout, stuck session), the reconciler detects the gap and produces corrective actions.
- Every command (deploy, kill, patrol, ensemble) calls the same reconciler. No ad-hoc state cross-checking.
- The state model is minimal: 3 desired variants × 2 transport states = 6 meaningful combinations. Not 560.

---

## Part III — Architecture Principles

### Rust as Material

Languages are not interchangeable tools. They are materials, each with properties that shape what can be built. Rust's properties are:

- **Types encode invariants.** An `AgentId` is not a `String`. A `MoleculeStatus::Active` is not a `MoleculeStatus::Failed`. The compiler enforces these distinctions at zero runtime cost.
- **The borrow checker prevents aliasing bugs.** No two parts of the system can hold mutable references to the same state. Race conditions in state management become compile errors.
- **Enums are exhaustive.** When a new molecule status is added, every match statement in the codebase must handle it. Forgotten cases are compile errors, not runtime surprises.
- **Single binary.** `cs` compiles to one static binary with zero runtime dependencies. No Python virtualenv, no Node modules, no JVM. Copy the binary, run it.

Rust is chosen for correctness, not performance (though performance is welcome). In an agent orchestration framework, correctness means: the state machine never enters an invalid state, messages are never lost or duplicated, and lifecycle transitions are never skipped.

### Typestate Pattern

Agent lifecycle and molecule lifecycle are implemented as compile-time state machines using the typestate pattern:

```rust
// Molecule state machine — invalid transitions are compile errors
struct Molecule<S: MoleculeState> {
    id: MoleculeId,
    formula: FormulaId,
    // ...
    _state: PhantomData<S>,
}

struct Initialized;
struct Running;
struct Completed;
struct Failed;

impl Molecule<Initialized> {
    fn start(self, worker: WorkerId) -> Molecule<Running> { /* ... */ }
}

impl Molecule<Running> {
    fn complete(self, evidence: Evidence) -> Molecule<Completed> { /* ... */ }
    fn fail(self, reason: FailureReason) -> Molecule<Failed> { /* ... */ }
}

// This does not compile:
// Molecule<Completed>::fail(...)  — a completed molecule cannot fail
// Molecule<Initialized>::complete(...)  — an unstarted molecule cannot complete
```

The state machine is encoded in the type system and enforced by the compiler, not documented in a wiki and enforced by convention.

### Newtypes Everywhere

Every domain identifier is a distinct type. Mixing them is a compile error:

```rust
pub struct AgentId(Arc<str>);
pub struct MoleculeId(String);
pub struct FormulaId(String);
pub struct SessionId(String);
pub struct WorkerId(String);

// This does not compile:
// fn dispatch(agent: AgentId, molecule: AgentId)  — type error
// fn dispatch(agent: AgentId, molecule: MoleculeId)  — correct
```

Lesson from OxyMake (58,000 lines of Rust, 1,306 tests): every newtype retrofit cost more than creating the newtype upfront. Start with newtypes from day one.

### Trait-Based Extensibility

Core abstractions are traits, not concrete types. Implementations can be swapped without changing domain logic:

```rust
/// Where fleet state is stored.
trait FleetStore {
    fn get_worker(&self, id: &WorkerId) -> Result<WorkerState, StoreError>;
    fn update_worker(&mut self, id: &WorkerId, state: WorkerState) -> Result<(), StoreError>;
    fn list_active(&self) -> Result<Vec<WorkerId>, StoreError>;
}

/// File-based implementation (day one).
struct FileFleetStore { root: PathBuf }

/// SQLite implementation (when queryability matters).
struct SqliteFleetStore { conn: Connection }

/// In-memory implementation (for tests).
struct MemFleetStore { state: HashMap<WorkerId, WorkerState> }
```

The same pattern applies to `StateStore` (molecule persistence), `Dispatcher` (how molecules are assigned to workers), and `EventSink` (where events are recorded). The domain logic is tested against the in-memory implementations in under one second. The file and SQLite implementations are integration-tested separately.

### Pure Core, Impure Shell

The domain crate (`cosmon-core`) keeps its **domain logic** I/O-free: the state machines, molecule transitions, dispatch, and routing make no file-system access, no network calls, no process spawning, and read no system clock. Every external interaction is mediated through an injectable trait (`StateStore`, `CommandRunner`, `PresenceSensor`, `Clock`), and the domain is tested entirely against in-memory implementations that touch nothing. The reference `Real*` implementations that back those traits ship in the same crate and do call `std::fs` / `std::process` / `SystemTime`. They are the injectable seams, not the core, and are being lifted out into the impure shell (`task-20260622-3144`).

This is the lesson that paid for itself most directly in OxyMake: `ox-core` (14,667 lines) has 393 pure unit tests that run in under one second. The entire domain logic — DAG resolution, scheduling, cache invalidation — is tested without touching the file system. The same principle applies to `cosmon-core`: agent lifecycle state machines, molecule transitions, dispatch logic, and communication routing are all pure functions over typed state.

The shell crate (`cosmon-cli`) handles the impure world: reading files, spawning processes, managing tmux sessions, writing to SQLite. It is thin, delegating all decisions to the core.

### Single Binary

`cs` compiles to one static binary. Installation is:

```bash
curl -sSL https://noogram.org/cosmon/install.sh | sh
# or
cargo install cosmon-cli
```

No external runtime to install: `cs` is a self-contained static binary (no Python virtualenv, no Node modules, no JVM). No database *server* to run and no daemon to manage in the core workflow: the file-based JSON state store is the default source of truth. The SQLite used for the local registry is embedded — `rusqlite` with `features = ["bundled"]`, compiled *into* the binary as a linked library, not a server and not a separate install.

---

## Part IV — Domain Model (Ubiquitous Language)

Every concept in Cosmon has a precise definition, a Rust type, and clear invariants. This vocabulary is the contract between the framework, its users, and its agents.

### Agent Definition

A portable, framework-agnostic specification of an AI agent's identity, capabilities, knowledge, and constraints. An Agent Definition is pure cognition: it describes WHO the agent is and WHAT it can do, independent of WHERE or HOW it runs.

```rust
pub struct AgentDefinition {
    pub name: AgentName,
    pub role: Role,
    pub skills: Vec<SkillRef>,
    pub clearance: Clearance,
    pub supervision: SupervisionMode,
    pub knowledge: Vec<KnowledgeSource>,
}
```

**Artefact.** A directory containing `agent.md` (identity and instructions), `agent.yaml` (manifest metadata), and optionally `knowledge/` and `workflows/`.

**Invariants.** An Agent Definition must be deployable to at least two runtime targets. It must contain no runtime-specific configuration (no process paths, no session IDs, no fleet references). It is pure description, not execution.

### Worker

A running instance of an Agent Definition, bound to a specific runtime environment. A Worker is pure transport: a process, a session, or a container. Workers are ephemeral; they are created, they run, and they stop.

```rust
pub struct Worker {
    pub id: WorkerId,
    pub definition: AgentName,
    pub status: WorkerStatus,
    pub session: SessionId,
    pub started_at: Timestamp,
    pub current_molecule: Option<MoleculeId>,
}

pub enum WorkerStatus {
    Idle,
    Working,
    Stalled,
    Stopping,
}
```

**Invariants.** A Worker always references exactly one Agent Definition. Multiple Workers may instantiate the same Agent Definition. A Worker without a valid Agent Definition is a type error.

### Ensemble (Fleet)

The set of all active Workers. The ensemble is the statistical-mechanical object: individual worker trajectories matter less than the distribution of states, the system temperature, and the overall health.

```rust
pub struct Ensemble {
    pub workers: HashMap<WorkerId, Worker>,
}

impl Ensemble {
    pub fn active_count(&self) -> usize { /* ... */ }
    pub fn idle_workers(&self) -> Vec<&Worker> { /* ... */ }
    pub fn temperature(&self) -> f64 { /* ratio of working to idle */ }
}
```

### Formula

A typed workflow template defining a multi-step process with explicit states, transitions, and exit criteria. A Formula is a recipe, not an execution.

```rust
pub struct Formula {
    pub id: FormulaId,
    pub name: String,
    pub steps: Vec<StepDefinition>,
    pub variables: Vec<VariableDefinition>,
}

pub struct StepDefinition {
    pub name: String,
    pub instructions: String,
    pub exit_criteria: Vec<ExitCriterion>,
    pub depends_on: Vec<String>,
}
```

**Artefact.** A TOML file defining steps, variables, and dependencies.

**Invariants.** A Formula is a template, not an instance. It becomes a Molecule when instantiated for a specific task. Every step must have at least one exit criterion.

### Molecule

A running instance of a Formula, bound to a specific task. A Molecule tracks the current step, bound variables, evidence collected, and execution log. It is the fundamental unit of tracked work.

```rust
pub struct Molecule {
    pub id: MoleculeId,
    pub formula: FormulaId,
    pub status: MoleculeStatus,
    pub steps: Vec<MoleculeStep>,
    pub variables: HashMap<String, String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub enum MoleculeStatus {
    Active,
    Paused,
    Completed,
    Failed,
}

pub enum StepState {
    Pending,
    Active,
    Completed { evidence: String },
    Failed { reason: String },
    Skipped { reason: String },
}
```

**Invariants.** A Molecule references exactly one Formula. Its state file is the authoritative record of progress. The execution log is append-only. Step transitions must provide evidence.

### Polymer

A **polymer** is a DAG of molecules linked by `Blocks` edges. It is the
compositional unit above the molecule: where a molecule tracks a single task
through a formula's steps, a polymer tracks a coordinated plan where each
constituent molecule may run independently once its predecessors complete.

A polymer has no steps of its own; its progression is an emergent property
of its constituents' lifecycles. When the last molecule in the DAG reaches
a terminal state, the polymer is complete. `cs run <root>` is the operator
that drives a polymer: it polls the DAG, dispatches ready molecules via
`cs tackle`, and calls `cs done` on completion.

Critically, a polymer is itself a molecule at the next compositional level.
A mission-plan formula nucleates a root molecule whose decay products form
a polymer; that root molecule can in turn be a node in a larger DAG. This
yields the composition hierarchy: **atom** (a single step within a formula),
**molecule** (a task unit driven by a formula), **polymer** (a DAG of
molecules linked by `Blocks` edges). The system is self-similar at each
level: the same lifecycle verbs (`nucleate`, `evolve`, `complete`, `done`)
apply uniformly.

*Origin: chronicle "molecule-polymer-duality" (2026-04-12).*

### Bead

A persistent unit of tracked work in the orchestration system. A Bead is the durable record: it survives session death, agent restarts, and system reboots. While a Molecule tracks *how* work progresses through a Formula's steps, a Bead tracks *that* work exists, who owns it, what its status is, and what has been discovered about it.

```rust
pub struct Bead {
    pub id: BeadId,
    pub title: String,
    pub status: BeadStatus,
    pub owner: Option<AgentName>,
    pub assignee: Option<WorkerId>,
    pub bead_type: BeadType,
    pub priority: Priority,
    pub notes: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub enum BeadStatus {
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
    Pinned,
    Hooked,
}

pub enum BeadType {
    Task,
    Bug,
    Mail,
    MergeRequest,
}
```

**The Bead/Formula/Molecule stack.** These three concepts form a layered abstraction:

- **Bead** — the persistent *what*. An issue, a task, a piece of mail. Stored in durable storage (Dolt, SQLite, or flat files). Beads exist independently of any workflow. A Bead can exist without a Formula or Molecule attached.
- **Formula** — the reusable *how*. A workflow template defining the steps and exit criteria. Formulas are authored once and instantiated many times.
- **Molecule** — the live *execution*. A running instance of a Formula, bound to a Bead. The Molecule is the bridge between the persistent record (Bead) and the procedural template (Formula).

When an orchestrator dispatches work, it attaches a Formula to a Bead, creating a Molecule. The Molecule advances through the Formula's steps. The Bead accumulates the findings. When the Molecule completes, the Bead persists the outcome.

**Invariants.** A Bead's `id` is globally unique within its storage backend. A Bead may have at most one active Molecule attached. Bead status transitions are validated by the storage layer. Notes and design fields are append-friendly; persisting findings to a Bead is the primary defense against context loss at session boundaries.

### Clearance

The maximum level of action a Worker is authorized to perform. Defines the permission boundary.

```rust
pub enum Clearance {
    Read,       // Observe only
    Write,      // Modify files, commit
    Execute,    // Run code, call APIs
}
```

**Invariants.** No Worker may perform actions above its Clearance level. The `Clearance` enum is deliberately extensible: domain-specific applications can define higher levels as needed, but the framework provides only the generic three. Clearance is declared in the Agent Definition, not in the fleet registry.

### Dispatch

The act of assigning a Molecule to a Worker. Dispatch is the bridge between work that needs to be done and agents available to do it.

```rust
pub struct Dispatch {
    pub molecule: MoleculeId,
    pub worker: WorkerId,
    pub dispatched_at: Timestamp,
    pub dispatched_by: DispatchSource,
}

pub enum DispatchSource {
    Human(String),
    Orchestrator,
    AutoDetect,
}
```

### Message

Inter-Worker communication. Two channels, not one:

```rust
pub enum Message {
    /// Persistent message — creates a record, delivered to inbox.
    Mail {
        from: WorkerId,
        to: WorkerId,
        subject: String,
        body: String,
        sent_at: Timestamp,
    },
    /// Ephemeral nudge — no record, immediate delivery, fire-and-forget.
    Nudge {
        from: WorkerId,
        to: WorkerId,
        content: String,
    },
}
```

**Design rationale.** Mail is for formal handoffs that must survive agent restarts. Nudge is for lightweight coordination ("check your inbox," "tests are passing"). The distinction reduces noise: not every inter-agent interaction needs to be recorded.

### Patrol

A health monitoring cycle. The patrol inspects the ensemble, identifies anomalies, and takes corrective action.

```rust
pub struct PatrolReport {
    pub timestamp: Timestamp,
    pub ensemble_size: usize,
    pub idle_count: usize,
    pub stalled_workers: Vec<WorkerId>,
    pub orphaned_molecules: Vec<MoleculeId>,
    pub recommendations: Vec<PatrolAction>,
}

pub enum PatrolAction {
    RestartWorker(WorkerId),
    ReassignMolecule(MoleculeId),
    AlertHuman(String),
    NoAction,
}
```

### Prime

Dynamic context compilation injected at session start. When a Worker boots, Prime assembles the context it needs: its Agent Definition, current molecule state, recent messages, system status, and relevant knowledge.

```rust
pub struct Prime {
    pub agent: AgentDefinition,
    pub current_molecule: Option<MoleculeState>,
    pub inbox: Vec<Message>,
    pub ensemble_summary: EnsembleSummary,
    pub injected_at: Timestamp,
}
```

Prime is the mechanism that fights decoherence. Every session starts with the full context needed to continue work, reducing the information lost at session boundaries.

---

## Part V — The Vocabulary Stack (Physics-Inspired)

> **Note:** The append-only, hash-linked event structure is called a **noogram**
> (νοῦς + γράμμα, "written cognition") in thesis and internal documents, and a
> **signed event log** in product-facing documentation. The term "blockchain" is
> retired — see [ADR-033](docs/adr/033-drop-blockchain-adopt-noogram.md).

The physics terms are organised across three scales — **quantum** (minutes, a
single action), **statistical** (days, the fleet), and **cosmological** (months,
the project). Each term earns a precise operational definition rather than a
poetic resonance: at the quantum scale a *worker* is a fermion (no two occupy
one molecule), a *molecule state* is the wave function, a *checkpoint* is
collapse, *context loss* is decoherence; at the statistical scale the *fleet* is
an ensemble and the orchestrator is Maxwell's demon reducing entropy by sorting
work; at the cosmological scale the *founding documents* are the cosmic
microwave background and the *knowledge graph* is the cosmic web. The complete
term-by-term table is kept as a design-phase artifact in
[`docs/appendix-physics-inspiration.md`](docs/appendix-physics-inspiration.md);
the load-bearing, product-facing subset is taught in
`docs/book/src/explanation/physics-vocabulary.md`.
### Composition Scale (Cross-Cutting)

| Level | Physics | Cosmon |
|-------|---------|--------|
| **Atom** | Indivisible particle | Step — a single action within a formula |
| **Molecule** | Bound group of atoms | Molecule — a task unit driven through a formula's steps |
| **Polymer** | Chain of molecules | Polymer — a DAG of molecules linked by `Blocks` edges |

This hierarchy is self-similar: the same lifecycle verbs apply at each level.

---

## Part VI — Module Architecture

> **Editorial note (2026-06-24).** The crate tree and dependency graph in this
> Part describe the **original day-one seed design** — the eight-crate vision
> cosmon was nucleated from. The live workspace has since grown to **43 crates**
> (see [`crates/`](crates/) and the README's Project Structure section). Several
> seed crates were never split out under the names used here (`cosmon-agent`,
> `cosmon-mol`, `cosmon-dispatch`, `cosmon-patrol`, `cosmon-prime` are conceptual
> concerns that live inside `cosmon-core` / `cosmon-runtime` / `cosmon-cli`
> today), and many new ones were added (`cosmon-daemon`,
> `cosmon-daemon-supervisor`, `cosmon-surface`, `cosmon-api`, `cosmon-remote`,
> `cosmon-runtime`, …). Read this Part as the **architectural intent** (pure core
> / impure shell, all arrows pointing inward to `cosmon-core`) — the principle is
> current; the exact crate names are historical.

The crate structure follows the pure core / impure shell principle, with each crate owning a single concern:

```
cosmon/
├── crates/
│   ├── cosmon-core/      # Pure domain types, state machines, I/O-free domain logic
│   │                     # AgentDefinition, Worker, Molecule, Formula,
│   │                     # Ensemble, Message, Clearance, Dispatch
│   │                     # All lifecycle state machines (typestate)
│   │                     # Trait definitions: FleetStore, StateStore,
│   │                     #   Dispatcher, EventSink
│   │
│   ├── cosmon-state/     # StateStore trait implementations
│   │                     # FileStateStore (JSON files, day one)
│   │                     # SqliteStateStore (feature flag, when queryability matters)
│   │                     # MemStateStore (for tests)
│   │                     # Forward-only migrations
│   │
│   ├── cosmon-agent/     # Agent Definition parsing (Markdown + YAML)
│   │                     # Worker lifecycle management
│   │                     # Session binding (tmux, subprocess, container)
│   │
│   ├── cosmon-mol/       # Formula parsing (TOML)
│   │                     # Molecule instantiation and lifecycle
│   │                     # Step transitions with evidence tracking
│   │                     # Exit criterion evaluation
│   │
│   ├── cosmon-dispatch/  # Dispatch logic (manual, automatic, round-robin)
│   │                     # Communication: Mail (persistent) + Nudge (ephemeral)
│   │                     # Inbox management
│   │
│   ├── cosmon-patrol/    # Health monitoring: transport checks + diagnostics
│   │                     # Stall detection, orphan molecule scan
│   │                     # PatrolReport generation
│   │                     # Recommendation engine (restart, reassign, alert)
│   │
│   ├── cosmon-prime/     # Context compilation for session injection
│   │                     # Agent Definition + Molecule state + inbox +
│   │                     #   ensemble summary → single Prime document
│   │
│   └── cosmon-cli/       # CLI entry point (clap derive)
│                         # One file per subcommand
│                         # Thin shell: parses args, wires traits, delegates
│                         # Subcommands: spawn, quench, ensemble, nucleate, evolve,
│                         #   collapse, observe, patrol, mail, nudge, prime
│
├── formulas/             # Built-in formula templates (TOML)
├── agents/               # Example agent definitions
├── Cargo.toml            # Workspace definition
├── LICENSE               # AGPL-3.0-only (core); LICENSE-APACHE for frontier crates — see ADR-092
└── README.md
```

### Crate Dependency Graph

```
cosmon-cli
  ├── cosmon-core       (domain types, traits)
  ├── cosmon-state      (implements StateStore)
  ├── cosmon-agent      (implements agent lifecycle)
  ├── cosmon-mol        (implements molecule lifecycle)
  ├── cosmon-dispatch   (implements Dispatcher)
  ├── cosmon-patrol     (implements health monitoring)
  └── cosmon-prime      (implements context compilation)

cosmon-state   → cosmon-core
cosmon-agent   → cosmon-core
cosmon-mol     → cosmon-core
cosmon-dispatch → cosmon-core
cosmon-patrol  → cosmon-core
cosmon-prime   → cosmon-core

cosmon-core    → (nothing — zero dependencies beyond std + serde)
```

All dependency arrows point inward toward `cosmon-core`. The core crate depends on nothing external. This is the hexagonal architecture: pure domain at the center, adapters at the periphery.

### Why Not Fewer Crates?

OxyMake grew to 22 crates, some of which were premature. The lesson: start with enough separation to enforce architectural boundaries, but do not create crates speculatively. Eight crates for Cosmon is deliberate:

- `cosmon-core` must be pure; it cannot be merged with anything that does I/O.
- `cosmon-state` is a trait implementation that will have multiple backends, so it earns its own crate.
- `cosmon-cli` is the shell; it must be separate from the core by definition.
- The remaining five (`agent`, `mol`, `dispatch`, `patrol`, `prime`) each own a single lifecycle concern. They could be modules within a single crate, but separate crates enforce that they communicate through `cosmon-core` traits, not through internal APIs.

If a crate stays under 200 lines for more than a month, merge it. If a crate exceeds 2,000 lines, consider splitting.

---

## Part VII — The Two-Layer Patrol

Agent health monitoring operates at two distinct layers that coexist and complement each other.

### Transport Layer: The Safety Net

The transport patrol is mechanical. It runs on a schedule (cron, systemd timer, or built-in interval) and checks:

- **Process liveness.** Is each Worker's process still running?
- **Heartbeat freshness.** Has the Worker updated its state within the expected interval?
- **Resource consumption.** Is the Worker consuming excessive memory or CPU?
- **Orphan detection.** Are there Molecules assigned to Workers that no longer exist?

The transport patrol does not reason. It measures, compares against thresholds, and takes predefined actions (restart, reassign, alert). It is implemented entirely in Rust, runs in milliseconds, and never calls an AI model.

```rust
pub struct TransportPatrol {
    pub interval: Duration,
    pub heartbeat_timeout: Duration,
    pub max_memory_mb: u64,
}

impl TransportPatrol {
    pub fn run(&self, ensemble: &Ensemble, store: &dyn StateStore) -> PatrolReport {
        // Pure function: state in, report out
    }
}
```

### Cognition Layer: The Brain

The cognition patrol is an agent. It reads the PatrolReport, reads the ensemble state, reads recent events, and *thinks* about what is happening:

- "Worker A has been on the same molecule step for 3 hours. The step usually takes 30 minutes. Is the agent stuck, or is the task genuinely harder than expected?"
- "Workers B and C are both idle, but there are 5 pending molecules. Why has dispatch not assigned them? Are the molecules misconfigured, or are the workers lacking the required clearance?"
- "The error rate has doubled in the last hour. Is this a systemic issue (model degradation, API outage) or localized to one worker?"

The cognition patrol is implemented as an Agent Definition with a formula. It is a Cosmon agent that monitors other Cosmon agents. This is reflexivity in action: the system observing itself.

### Why Both Layers

Transport catches the obvious failures: crashed processes, frozen heartbeats, resource exhaustion. It is fast, reliable, and never wrong about what it measures. But it cannot reason about *why* something is wrong.

Cognition catches the subtle failures: agents that are technically alive but effectively stuck, patterns across multiple agents that suggest a systemic issue, situations where the correct action is not "restart" but "change the approach." But it is slow, expensive (it consumes AI tokens), and may itself be wrong.

Together, transport is the safety net and cognition is the brain. The safety net catches you when you fall. The brain prevents you from falling in the first place.

---

## Part VIII — Relationship to OxyMake

Cosmon and OxyMake are siblings, not competitors. OxyMake orchestrates
**workflows** (DAGs of anonymous, transient tasks; data flows through files).
Cosmon orchestrates **agents** (entities with persistent identity and state that
communicate and evolve across sessions). An agent managed by Cosmon can *use*
OxyMake to run a pipeline; OxyMake executions can be *managed* by a Cosmon
agent. They share the Rust ecosystem but neither depends on the other — stronger
together, independently useful. OxyMake works at the molecular scale (build
steps, artifacts); Cosmon at the cosmological scale (workstreams, priorities,
growing scope).

---

## Part IX — Open Source Strategy

### License

Cosmon is a **mixed-licence workspace** ([ADR-092](docs/adr/092-license-bascule-mpl-to-agpl.md)): **AGPL-3.0-only** for the core (runtime, daemons, end-to-end binaries, MCP servers — default for the workspace) and **Apache-2.0** for frontier crates (pure libraries, SDKs, and boundary contracts a third party would `cargo add` to build on top of cosmon). The placement test asks whether a third party would run `cargo add cosmon-X` without wanting to make their whole project AGPL: if yes, Apache-2.0 (frontier); if no, AGPL-3.0-only (core). Across the 43-crate workspace the partition is roughly 28 AGPL-3.0 core crates (21 declare `AGPL-3.0-only` explicitly, the rest inherit the workspace default) and 7 Apache-2.0 frontier crates (see [ADR-092 §3](docs/adr/092-license-bascule-mpl-to-agpl.md#3-partition-test) for the authoritative per-crate placement). This aligns cosmon with the noogram federation invariant (noogram ADR-0001 §7, [noogram.dev](https://noogram.dev)): AGPL-3.0 closes the SaaS hole via §13, Apache-2.0 keeps the frontier composable.

### Development Platform & Documentation

Cosmon is developed on GitHub, in the open — issues, PRs, discussions, and CI
public — using agent orchestration (ideally cosmon's own, the bootstrap
completion). Documentation follows progressive disclosure: start with the
simplest example (`cs demo`, `cs nucleate`), then build complexity.
### Self-Validating Thesis

This thesis makes a testable claim: multi-agent systems need a dedicated orchestration framework that treats agents as entities with identity and state, not as functions in a DAG. If the claim is correct, people will adopt Cosmon. If the claim is wrong, this thesis was wrong, and that is a legitimate outcome.

The thesis validates itself through use. The first user is the team that builds it.

### The Plug Strategy

Cosmon is a library, not a platform: it provides the domain types, state
machines, and trait interfaces, while the host system provides the orchestration
environment (how agents are spawned, where state is stored, how messages are
delivered). A host "plugs in" by implementing the traits — `StateStore`,
`Dispatcher`, `EventSink`, `Transport`. The first operational host was **Gas
Town**, the bootstrap galaxy whose polecat/witness/refinery/mayor patterns the
domain model (Part IV) was extracted from; it is the existence proof, not a
retrofit.

**Design constraint (load-bearing).** `cosmon-core` must keep its domain logic
I/O-free — external interactions only through injectable traits — with zero
host-specific logic and zero runtime assumptions. It pulls in only serialization
and pure-domain helpers (`serde`, `toml`, `chrono`, `thiserror`, `strum`,
`sha2`, `regex`, `rand`, `secrecy`, `async-trait`, and the sibling
`cosmon-graph` / `cosmon-hash` crates) — no database driver, no HTTP or network
stack, no async runtime, no process/tmux crate. Today that boundary is a
maintained convention, not a compiler-enforced allowlist.
### The Implicit Tracking Principle

Work progress is tracked through state transitions, not through explicit status reports.

In a naive orchestration system, agents must explicitly report their status: "I am starting step 3," "I am 50% done," "I have finished." These reports are unreliable (agents forget to send them), expensive (each report consumes tokens), and redundant (the orchestrator can observe state directly).

Cosmon inverts this. The Molecule state machine IS the status report. When a Worker advances a Molecule from step 2 to step 3, the Molecule's state file records the transition with evidence and timestamp. The Patrol reads these state files to determine fleet health. No agent needs to "report in": the act of doing the work IS the report.

The same principle applies to Beads. When a Worker persists findings to a Bead (`bd update <id> --notes "..."`) or closes a Bead (`bd close <id>`), the Bead state is the durable record. The Worker does not need to separately notify anyone that work is complete: the Bead's status transition IS the notification.

This has three consequences:

1. **Zero-overhead monitoring.** The Patrol reads state files, not agent reports. Monitoring does not interrupt working agents.
2. **Session-death resilience.** If an agent dies mid-work, its last Molecule state and Bead notes survive. The next agent picks up where the state indicates, not where a status report claimed.
3. **Auditability by default.** Every state transition is a record. There is no gap between "what happened" and "what was reported" because they are the same thing.

### Visual Identity

The visual identity draws from the void (clean empty space as canvas), the agent
as a minimal figure (a colored dot defined by role and state, not decoration),
and discovery through interaction. Constraints: monochrome, legible at 16×16,
flat vector, at home next to the Rust ferris crab.

---

## Part X — What Cosmon Does NOT Do

Clarity about scope is as important as clarity about features. Cosmon is deliberately thin.

**Cosmon does NOT provide AI models.** Bring your own Claude, GPT, Gemini, Llama, or local model. The framework is transport, not cognition. It does not generate text, write code, or make decisions. It routes, persists, monitors, and dispatches.

**Cosmon does NOT define domain-specific skills.** Skills (operators) are application-layer content. A research lab's skills are different from an engineering team's skills are different from a content creation pipeline's skills. Cosmon provides the mechanism for invoking skills; the skills themselves live outside the framework.

**Cosmon does NOT replace workflow orchestrators.** DAG execution, dependency resolution, caching, and scheduling are OxyMake's job (or Airflow's, or Prefect's, or Temporal's). Cosmon orchestrates the agents that *use* workflow orchestrators, but it does not replicate their functionality.

**Cosmon does NOT store domain data.** It stores agent state (who is running, what are they working on, what is their health) and molecule state (what step is active, what evidence has been collected). Domain data — research results, generated code, processed datasets — lives in the domain's own storage. Cosmon manages the workers, not the work product.

**Cosmon does NOT enforce a specific AI architecture.** It works with single-model agents, multi-model pipelines, tool-using agents, chain-of-thought reasoners, or any other pattern. The framework manages the lifecycle and communication of whatever is inside the agent. It does not care how the agent thinks, only that the agent reports its state honestly.

**Cosmon does NOT micro-manage agents.** It dispatches work and monitors health. It does not tell agents *how* to do their work. The formula defines the steps and exit criteria; the agent decides how to meet those criteria. This is the supervision model: specify the goal, verify the outcome, trust the process.

### What Cosmon Does NOT Ship (and why)

The paragraphs above are scope refusals: the markets cosmon refuses to enter. The bullets below are *architectural* refusals: the mechanisms cosmon refuses to build. These are not omissions. They are wedges. **Each NO defines an architectural commitment**, and most of them are load-bearing: remove one and a different system falls out the other side. Read in 30 seconds; reread when tempted to add "just one" daemon, mailbox, or popup.

- **No daemon.** Stateless CLI, git-composable; crash-recovery is re-reading the disk.
- **No scheduler process.** External clocks (cron, tmux, humans) drive invocations; cosmon owns no loop.
- **No broker, no message queue.** Control plane = DAG, data plane = filesystem. Nothing else crosses workers.
- **No mailboxes.** Workers read predecessors' evidence from disk; inter-worker messaging is forbidden by design.
- **No background bash, no hidden side-channels.** Every state transition is a visible `cs` invocation.
- **No sub-agents inside workers.** Delegation is `cs nucleate` + typed links, never a hidden fork-join.
- **No MCP for workers.** Workers use the `cs` CLI with walk-up discovery ([ADR-020](docs/adr/020-mcp-project-agnostic-cwd-per-call.md), [ADR-021](docs/adr/021-principal-separation-caller-vs-worker.md)).
- **No permission popups.** Clearance is declared once at the fleet level and enforced at `cs tackle`.
- **No plan mode, no to-do list.** Molecules *are* the plan; the DAG *is* the to-do list.
- **No built-in UI dashboard.** `cs peek` is a fractal TUI portal; everything else is a surface projection ([ADR-023](docs/adr/023-cockpit-hexagonal-read-surface.md)).
- **No silent witnesses.** Absent ≠ consented; every participant signs on ([ADR-032-p](docs/adr/032-p-external-witness-axiom.md), [ADR-034](docs/adr/034-witness-charter-v0-protocol.md)).
- **No single point of failure.** Every critical fact is a JSON file on disk ([ADR-001](docs/adr/001-state-storage-json-first.md)).
- **No cognitive contracts.** Contracts are structural (typed links), not prose agreements between agents.
- **No runtime subscription model.** The resident runtime is a *client* of the Transactional Core, not its replacement ([ADR-016](docs/adr/016-autonomy-regimes-and-resident-runtime.md)).
- **No destructive rewrites.** Every evolve appends to the event log and auto-commits a git step.
- **No amendment after completion.** Completed molecules are immutable; revision is a new molecule.
- **No worker self-destroy.** Humans tear down (`cs done`); workers only advance (`cs evolve`/`cs complete`).

If a proposal would reverse any of these bullets, it is a successor thesis, not a feature request. Write an ADR and argue the principle, not the line item.

---

## Appendix A — Lessons Inherited

Cosmon inherits lessons from two predecessor systems. These are scars from operational experience, not theoretical principles.

**From GasTown (Go, 8 days, 42 formulas):** velocity comes from design, not
code (a pre-existing mental framework translated to TOML); a coordinating
background layer (the Deacon) is the missing piece between "cron says fine" and
"actually productive"; two channels beat one (persistent mail for handoffs,
ephemeral nudge for coordination); automatic idle-worker dispatch is a force
multiplier; monitoring must be silent when healthy (the Idle Town Principle);
and 17% infrastructure overhead for state management is too high — cosmon uses
flat files by default, SQLite as an optional upgrade.

**From OxyMake (Rust, 58K LOC, 1,306 tests):** the pure-core / impure-shell
split is the single most important architectural decision (zero-I/O core →
sub-second test suites); newtypes from day one (`AgentId(Arc<str>)`, not
`String`) — every retrofit costs more; plan the SQLite migration path before the
first line of persistence code; budget ~30% for concurrency and process
management (the dominant bug category); and never create crates speculatively —
a crate earns its place when it has real code, not imagined future code.

---

## Appendix B — CLI Preview (historical)

> **Historical sketch, kept for the record.** This was an early guess at the
> command surface, written before the CLI stabilized. Several verbs below were
> never built and do not exist in `cs` today: `spawn`, `stop`, `fleet`, `mail`,
> `nudge`, `inbox`, `up`, `down`, and `events`. The shipped CLI replaced them —
> a worker is put on a molecule with `cs tackle` and torn down with `cs done`;
> the fleet is observed with `cs peek` and `cs ensemble`; a live worker is
> advised with `cs whisper`. For the real, current surface run `cs help`, or
> see the CLI Reference in the README.
*(The illustrative command sketch that once sat here is dropped; run `cs help`
or see the generated CLI reference for the real, current surface.)*

---

## Part XI — The Energy Principle

> *"Energy is the only universal currency."* — Richard Feynman

### Tokens Are the Energy of the System

Every agent action has a cost measured in tokens. Tokens are consumed when an agent reads context, generates output, invokes skills, or communicates with other agents. In the physics vocabulary, tokens are the energy of the Cosmon universe: the conserved quantity that enables all work.

This is an accounting identity, not a metaphor: every dispatch is an irreversible allocation of a finite resource. The system must be aware of this resource at every level, from individual step execution to fleet-wide budgeting.

### Conservation

Tokens spent do not come back. Each dispatch is an irreversible allocation decision, analogous to energy dissipation in a thermodynamic system. A molecule that consumes 50,000 tokens to produce a result cannot un-consume those tokens if the result is discarded. This irreversibility demands discipline: dispatch decisions must be intentional, not speculative.

The implication is operational: the orchestrator must evaluate whether the expected value of a molecule justifies its expected token cost *before* dispatching it.

### Energy Budget

The system operates within finite energy constraints. A weekly or monthly token budget defines the total energy available to the ensemble. This budget is a hard constraint, like the total energy of a closed system. Treat it as binding, not advisory.

When the budget is exhausted, the system must stop or enter a minimal-energy state (only essential patrols, no new molecule dispatch). Exceeding budget is the agent-system equivalent of a thermodynamic violation: it should not happen, and if it does, it signals a governance failure.

### Temperature as Energy Allocation

Temperature controls the trade-off between exploration and exploitation, and this has a direct energy cost:

- **High temperature (hot).** Agents explore freely: new research directions, experimental approaches, speculative investigations. Exploration is expensive: it consumes tokens on paths that may not produce results. High temperature is appropriate when the system has abundant budget and needs discovery.

- **Low temperature (cool/frozen).** Agents converge: finish existing molecules, polish outputs, consolidate results. Convergence is cheap: agents follow known paths with predictable token costs. Low temperature is appropriate when the budget is constrained or when the system is near a deadline.

The orchestrator controls the thermostat. Adjusting temperature is a concrete decision about how aggressively to spend the remaining energy budget, not an abstract parameter change.

### Entropy Tax

Not all tokens produce useful work. Coordination overhead — tokens spent on orchestration, context compilation, patrol reports, inter-agent communication, and prime injection — is the entropy tax of the system. These tokens are necessary (without coordination, the system cannot function) but they do not directly advance any molecule.

**Entropy tax** = tokens spent on orchestration and coordination.

Minimizing entropy tax is a design goal, not an optimization target. Some coordination overhead is irreducible (Landauer's principle applies). But unnecessary layers of abstraction, verbose communication protocols, and redundant health checks inflate the entropy tax beyond the irreducible minimum.

### Free Energy

The productive capacity of the system is the budget minus the entropy tax, not the total budget:

**Free energy** = total budget - entropy tax

**Free energy ratio** = productive tokens / total tokens consumed

A healthy system has a free energy ratio above 0.7 (at least 70% of tokens go to productive work). A system where orchestration consumes more than 30% of the budget has a coordination problem. This ratio is the single most important efficiency metric for an agent fleet.

### Landauer's Principle

In physics, Landauer's principle states that erasing information has an irreducible thermodynamic cost. In Cosmon, the equivalent is: **erasing context (decoherence between sessions) has an energy cost**. When an agent loses its context at a session boundary, the next session must spend tokens to reconstruct that context: reading checkpoints, loading prime, re-establishing the working state.

Preserving context (through checkpoints, molecules, memory files, and prime injection) saves energy. Every token invested in a checkpoint is a token saved at the next session start. The energy cost of decoherence is real and measurable: compare the tokens consumed by a session that starts from scratch versus one that resumes from a rich checkpoint.

### Phase 1 (Pragmatic): Track and Alert

The first implementation of energy consciousness is measurement:

- **Track tokens per molecule.** Every molecule records the tokens consumed across all its steps, by all workers who touched it.
- **Track tokens per worker.** Each worker's cumulative token consumption is recorded per session and per period (weekly, monthly).
- **Track tokens per model.** Different models have different costs. Track consumption by model to understand the cost structure.
- **Compute entropy tax.** Classify token consumption as productive (advancing molecule steps) or overhead (orchestration, patrol, communication). Report the free energy ratio.
- **Alert on budget limits.** When consumption reaches 80% of the period budget, alert the operator. When it reaches 95%, restrict dispatch to essential molecules only.

### Phase 2 (Learned): Predict and Optimize

The second phase uses historical data to make energy-aware decisions:

- **Estimate token cost before executing.** Based on historical data for the same formula and step, predict the expected token consumption. This estimate informs dispatch decisions.
- **Cost-aware dispatch.** When multiple workers are eligible for a molecule, factor in their historical token efficiency. A worker that consistently completes a formula step in 3,000 tokens is more energy-efficient than one that uses 12,000 tokens for the same step.
- **Temperature auto-tuning.** Based on remaining budget and remaining work, automatically suggest temperature adjustments. If the budget is 40% consumed but only 20% of planned molecules are complete, suggest raising temperature. If 80% of budget is consumed with 90% of work complete, suggest lowering temperature to conserve.

### Fleet Review — Metabolism as Observable

The `fleet-review` formula is the first concrete instance of energy observation. It reads `events.jsonl` and molecule state to compute five vital-sign metrics (collapse rate, duration/step, energy utilization, backlog pressure, completed ratio) and produces a human-readable report. v0 is observation-only: the galaxy measures its own metabolism without acting on the measurement. Future versions (v1+) will add parametric suggestions with falsifiable predictions, but only after the observation loop has produced 5+ useful scans. See `.cosmon/formulas/fleet-review.formula.toml` and [handbook §fleet-review](docs/handbook.md#fleet-review).

### Per-Molecule Step Circuit Breaker (`StepBudget`, v0)

Token tracking measures cost after the fact. The circuit breaker prevents the runaway loop *before* it spends. v0 of the named [`EnergyBudget`](#energy-budget) primitive lands as a small typed value attached to every molecule:

```rust
pub struct StepBudget { pub cap: u32, pub remaining: u32 }
```

`cs nucleate` stamps `Some(StepBudget::new(cap))` onto the new molecule, where `cap` is taken from `--energy-budget <N>` (operator override) or from `.cosmon/config.toml` `[energy] default_step_budget` (project default, 100). `cs evolve` checks `remaining` at the top of every step:

- `remaining > 0` → consume one slot, advance the step normally.
- `remaining == 0` → refuse the advance, transition the molecule to `Frozen` with structured reason [`StuckReason::EnergyExhausted`](crates/cosmon-core/src/event_v2.rs), and emit `MoleculeStuck { reason: "energy_exhausted" }` on the event log.

Pass `--energy-budget 0` to opt a molecule out of the breaker (long-running supervisor formulas, infinite watchdog loops). Legacy molecules without the field are also bypass-by-default: `None` means "no breaker installed".

This is the structural answer to the failure mode bycrawl reported on Perplexity Personal Computer (a single page burning ~$200 of compute credits because silent retry sub-agents looped invisibly). Cosmon already had the molecule boundary — sealed `prompt.md`, sealed `briefing.md`, `events.jsonl`, worktree, human-only `cs done`. The breaker makes the protection *explicit* and *audit-friendly*: every molecule carries a visible cap and remaining count surfaced in `cs peek`, every exhaustion lands a typed event, and the only repair is operator action. Never silent retry — exhaustion *is* the signal. (`task-20260427-0bc6`, parent deliberation `delib-20260427-4984`.)

---

## Part XII — The Thermodynamic Extension: Entropy as Computable Observable

> *"Carnot measured entropy to build better steam engines. Shannon measured entropy to build better communication systems. Cosmon measures entropy to build better agent orchestration."*

Part XI established the Energy Principle: tokens are the conserved currency, free energy is the productive fraction, entropy tax is the overhead. This part extends the framework from energy accounting to thermodynamics proper — making entropy a computable observable with units, instruments, and actionable thresholds rather than a metaphor.

### Design Principle: The Feynman Test

Every entropy metric in Cosmon must satisfy four criteria: (a) it is computable from data that exists in the system, (b) it produces a number with units, (c) that number changes when the system changes, and (d) a human or orchestrator looking at the number can decide whether to act. If a metric fails any of these, it is a metaphor, not an observable.

All entropy values are in **bits** (Shannon entropy, log base 2). All energy values are in **tokens** (the existing `TokenCount`). The bridge between them is the **bits-per-token ratio**: how much information does each token carry or destroy?

### Four Sources of Entropy

Entropy in Cosmon has four computable sources, ordered from most concrete to most aspirational.

**1. Message entropy (computable today).** The Shannon entropy of the inter-agent message stream, measured via compression ratio on the JSONL event log:

```
H_msg ≈ compressed_bytes / raw_bytes × 8   (bits per byte, max 8)
```

High message entropy means agents exchange surprising, information-rich content. Low message entropy means repetitive, redundant communication — heartbeats, boilerplate, status pings. This metric makes the nervous tissue's entropy classification (Part XV) continuous and automatic.

**2. Context window entropy (computable today).** The signal-to-noise ratio of each agent's context window, measured as the fraction of tokens that are task-relevant versus stale or redundant. The SNR degrades over a session:

| Session phase | Tokens used | Signal fraction | SNR (dB) |
|---------------|-------------|-----------------|----------|
| Turn 1 | ~15K | ~90% | ~10 dB |
| Mid-session | ~80K | ~40-50% | ~0 dB |
| Pre-compact | ~167K | ~20-30% | -4 dB |
| Post-compact | ~30-50K | ~60-70% | ~3 dB |

When SNR crosses 0 dB, the agent is swimming in noise. This is the trigger for compaction or session restart.

**3. Code entropy (computable today).** The Shannon entropy of the Cosmon codebase itself — the initial entropy of the universe — measured as the compression ratio of source files. Code entropy should grow slowly (new features) and occasionally drop (refactoring extracts patterns). A sudden spike signals a large, complex addition. A steady climb without drops signals accumulating complexity without simplification. The derivative dH_code/dt is the complexity velocity of the project.

**4. State entropy (computable with effort).** The Boltzmann entropy of the fleet state — the logarithm of the number of possible configurations:

```
S_state = log₂(W)

W = (worker_states)^N_workers × (bead_states)^N_beads × (molecule_states)^N_molecules
```

With 6 workers, 5 worker states, 20 beads, and 4 bead states, S_state is approximately 55-70 bits. State entropy grows combinatorially with fleet size. At 50 workers and 200 beads, it reaches ~600 bits — the point at which the orchestrator's stale view becomes a real operational problem. This metric predicts when the system needs better state synchronization or hierarchical routing.

### Demoted: the decorative thermodynamics (Amendment 3, 2026-04-11)

Several sections that once lived here **failed the Feynman Test above** and were
demoted as cargo cult — they mapped real thermodynamic laws onto cosmon but made
no prediction that would fail if the numbers were wrong. They are removed from
this thesis; the honest record of what was tried and why it was cut lives in
[`docs/appendix-physics-inspiration.md`](docs/appendix-physics-inspiration.md).
What was demoted:

| Section | Verdict | Reason |
|---------|---------|--------|
| Three Laws of Cosmon Thermodynamics | **Decorative** | The mapping adds no predictive power |
| Carnot Efficiency of an Agent | **Redundant** | η = productive/total tokens is just the free-energy ratio renamed |
| Agent Work Cycle (4-phase Carnot) | **Decorative** | The four phases neither predict nor measure anything |
| Helmholtz Free Energy F = E − TS | **Non-computable** | "Temperature" (API calls/min) × entropy (bits) has no meaningful unit |
| Cosmological Timeline | **Decorative** | Poetic; "galaxy formation" does not predict team behaviour |

**What survives is load-bearing** because it passes the Feynman Test:

- **Token budgets** and the **free-energy ratio** = productive tokens / total
  tokens (Part XI) — real, measurable, enforced.
- The **four computable entropy sources** above (message, context, code, state)
  — Shannon entropy with instruments.
- **Landauer's principle for context loss** — a genuine, measurable cost (below).
- The **Feynman Test itself** — the discipline that separates observable from
  metaphor.

The lesson: the physics vocabulary earns its place only when it compresses
observed data better than a plain description would. Token budgets and Shannon
entropy do; the Three Laws and the Carnot cycle do not.

### Landauer's Principle Extended

Part XI introduced Landauer's principle: erasing context costs tokens. The thermodynamic extension quantifies this. The Landauer cost of context loss is measurable: compare the token cost of a session that starts from a rich checkpoint versus one that starts cold. The difference is the minimum energy cost of information erasure.

Every irreversible operation in Cosmon has a Landauer cost:

| Irreversible operation | What is erased | Cost to reconstruct |
|-----------------------|---------------|-------------------|
| Session end without checkpoint | Full agent context | Full prime injection cost |
| Lossy compaction | Low-priority context | Tokens to re-derive or reload |
| Force push | Git history | Cannot be recovered |
| Bead deletion | Issue context | Recreation cost |

The design principle follows: **prefer reversible operations.** Event sourcing (append-only logs), git commits (versioned history), and checkpoint creation (state preservation) are thermodynamically cheap because no information is destroyed. Their inverses — log truncation, force push, session end without checkpoint — are thermodynamically expensive.

### Connection to the Energy Principle

The thermodynamic types compose with, not replace, the energy types from Part XI:

- `EnergyReport.free_energy_ratio` IS `ThermodynamicState.efficiency` — same number, richer context
- The entropy tax (Part XI) is now decomposable into message entropy overhead, context window noise, and state complexity
- Temperature (Part XI's exploration/exploitation dial) now has a formal definition: dE/dS, tokens consumed per bit of entropy change

The thermodynamic framework provides the *why* behind the energy metrics: efficiency is bounded by Carnot's theorem, overhead is entropy tax, budget depletion is heat death. The Energy Principle tells you *how much* you are spending. The Thermodynamic Extension tells you *how efficiently* you are spending it, and *what the theoretical limits are*.

---

## Part XIII — Morphological Evolution and the Multiverse

> *"Nothing in biology makes sense except in the light of evolution."* — Theodosius Dobzhansky

### The Plasticity Principle

Every architectural decision in Cosmon must preserve future plasticity. The trait interfaces are the stable skeleton; the implementations are the muscles that can change.

Rust is today's material. It minimizes the free energy of the current system: the type system catches errors at compile time, the borrow checker prevents aliasing bugs, the single binary simplifies deployment. In physics terms, Rust occupies the lowest-energy state of the current fitness landscape. But fitness landscapes change. New constraints emerge. New materials appear. The system that cannot change its material is the system that goes extinct when the environment shifts.

The Plasticity Principle states: **no layer of Cosmon may be designed as permanent.** Every layer — from the programming language to the protocol to the agent definitions — is subject to evolutionary pressure. The architecture must anticipate replacement at every level while providing stability at every interface.

This is not a contradiction. Biological organisms achieve it daily: bones provide structural stability while muscle tissue, skin, and neural connections remodel continuously. The skeleton is the trait system. The muscles are the implementations. The nervous system is the event bus. What evolves is the implementations behind them, not the interfaces.

### Evolution at Every Layer

The following table enumerates what can evolve at each layer of the system, what provides stability during that evolution, and what selection pressure drives the change:

| Layer | What evolves | Stability mechanism | Selection pressure |
|-------|-------------|---------------------|-------------------|
| **Agent Definitions** | New roles, skills, knowledge bases, supervision modes | The `AgentDefinition` struct and its trait contracts | User needs, task complexity, domain expansion |
| **Transport** | New runtimes: tmux today, containers tomorrow, serverless later | The `Runtime` trait -- any backend that implements `spawn`, `stop`, `is_alive` | Cost, scalability, deployment context |
| **Models** | Opus today, future models tomorrow, local models, multi-model dispatch | The `ModelProvider` trait -- any model that accepts messages and returns completions | Model capability, cost per token, latency, privacy requirements |
| **Tools and Protocols** | MCP today, A2A, future protocols | The `ToolProtocol` trait -- any protocol that can discover and invoke tools | Ecosystem adoption, standardization, capability |
| **State Storage** | Files today, SQLite tomorrow, distributed store later | The `StateStore` trait -- any backend that can persist and query state | Scale, queryability, reliability, multi-node deployment |
| **The Framework Itself** | Crate structure, internal architecture, even the CLI surface | The founding thesis (this document) and the trait interfaces | Operational experience, community feedback, ecosystem evolution |
| **The Language** | Rust today, potentially a custom language in a distant future | The domain model and its invariants, expressed independently of any language | Token capacity, AI code generation capability, domain fitness |

The key insight: each row in this table represents an independent axis of evolution. The system can change its transport layer without touching its agent definitions. It can change its model provider without rewriting its dispatch logic. It can even change its programming language without losing its domain model, because the domain model is expressed first in the founding thesis (natural language) and only second in Rust types.

### Why Rust Today

Rust is chosen because it minimizes the free energy of the current system, not out of language loyalty:

- **Type safety eliminates bug classes.** The 8 bug classes documented in the domain types spec (ID confusion, invalid transitions, partial state updates, typo in status strings, missing fields, silent fallthrough, unvalidated formats, clearance escalation) are all compile errors in Rust and silent runtime failures in dynamic languages.
- **Single binary deployment.** Zero runtime dependencies. Copy the binary, run it. This is the lowest-energy deployment model.
- **Performance.** Agent orchestration is I/O-bound, not compute-bound, so raw performance matters less than correctness. But when the fleet scales to hundreds of agents, the orchestrator's overhead must remain negligible. Rust's zero-cost abstractions ensure this.
- **Ecosystem maturity.** `serde`, `clap`, `tokio`, `rusqlite`, `thiserror` — the crate ecosystem provides production-quality building blocks.

In physics terms: Rust minimizes the free energy of the current system because the current environment rewards correctness, type safety, and operational simplicity. But free energy landscapes are not static. If the environment changes — if AI models become capable of generating and maintaining code in novel languages with even stronger guarantees — then a different material may occupy the lowest-energy state. The Plasticity Principle demands that we be ready for this.

### The Multiverse of Forks (a note, not a plan)

Two consequences follow, stated as logical possibilities rather than a roadmap.
First, **the custom-language horizon**: with enough token capacity an AI could
one day design a language purpose-built for agent orchestration (state machines
and lifecycle as primitives, not phantom-type encodings). The Plasticity
Principle only demands that the domain model — expressed first in this thesis,
second in Rust types — remain expressible in *any* language, so that transition
is never foreclosed. Second, **the open-source multiverse**: each fork is a
parallel trajectory adapting to its niche; shared `cosmon-core` traits are the
mechanism for horizontal gene transfer (a novel `StateStore` or patrol heuristic
transplanted across forks), while divergence beyond core compatibility is
speciation — a natural outcome, not a failure. The practical discipline is the
same either way: design for replaceability at every layer, version the
interfaces (not just the implementations), and keep the domain model stable
enough that any fork can trace its decisions back to it.

---

## Part XIV — Observer Regulation: The Anti-Psychosis Principle

The human interacting with a multi-agent system is an **observer** in the quantum mechanical sense. Each observation (prompt, idea, directive) collapses possibilities and perturbs the system. This creates a fundamental design tension.

### The Amplification Trap

A naive system amplifies the observer's creative output. Every idea spawns an agent. Every question triggers a research study. Every "what if" creates a molecule. The result is exponential context explosion — more agents producing more results requiring more human attention triggering more ideas. This is a positive feedback loop that degrades coherence.

Karpathy calls this **"AI Psychosis"**: the cognitive overload from watching agents accelerate faster than the human can absorb. The bottleneck is never system capability. It is human coherence.

### The Quantum Zeno Effect

In quantum mechanics, rapid repeated measurement prevents a system from evolving — the Zeno effect. In agent orchestration, rapid repeated human intervention prevents the fleet from doing deep work. Every context switch from the observer resets the agents' focus. The system needs **quiet time** between observations to produce coherent results.

### The Regulation Principle

Cosmon must be a **regulator**, not an amplifier:

1. **Capture fast, execute slow.** When the observer bursts with ideas, capture them as one-liners. Do not spawn agents immediately. Batch, prioritize, then execute.
2. **Offer collapse moments.** Periodically ask: "We have N active molecules. Which ones matter most? Should we close some before opening new ones?"
3. **Molecule budget.** Like the energy budget (Part XI), impose a maximum number of concurrent active molecules. This forces prioritization.
4. **Daily digest over real-time feed.** The observer should receive a summary, not a stream. Streams amplify psychosis; digests dampen it.
5. **Temperature control.** When the observer is in a creative burst (high temperature), the system captures but does not execute. When the observer is focused (low temperature), the system executes deeply on few items.

### The Physics

The observer is a thermodynamic entity. Each interaction has an entropy cost — it increases the disorder of the system's priority queue. The regulation principle is entropy management: absorb the observer's energy (ideas) into structured state (molecules, backlog) without letting the system's entropy exceed its capacity for coherent work.

In the cosmological metaphor: the observer is the dark energy — the force that drives expansion. Without regulation, expansion accelerates until the system tears apart (the Big Rip). The regulation principle is the cosmological constant: it permits expansion but at a controlled rate.

### The Subtraction Principle

The informative signal is the event that should have arrived and did not.
Heartbeat-gap carries more information than heartbeat. A patrol that reports
"3 workers alive" is noise; a patrol that reports "worker-7 missed its
heartbeat" is signal. The regulation principle (above) tells us to dampen
amplification; the subtraction principle tells us *how*: observe the
absence, not the presence.

This has a direct consequence for observability design. Dashboards that
display every metric continuously are amplifiers — they expand the
observer's attention surface. The correct instrument subtracts: it shows
nothing when the system is healthy, and surfaces only the deviation. In
information-theoretic terms, the steady state has zero surprisal; only
departure from steady state carries bits. Cosmon's observer tooling should
therefore default to silence and emit signal only on absence, anomaly, or
drift. This is observability as subtraction, not as accumulation.

*Origin: chronicle "observability is subtraction" (2026-04-12).*

---

## Part XV — The Nervous Tissue: Communication as Multi-Channel Fabric

> *"The fundamental problem of communication is that of reproducing at one point either exactly or approximately a message selected at another point."* — Claude Shannon, 1948

### Messages are logical, channels are physical

The defining insight, borrowed from OxyMake: a *message* is a logical unit of
communication (identity, type, sender, recipient, payload); the *channel* — IPC
pipe, append-only file, versioned SQL row — is its materialization. The
orchestrator's job is **channel selection, not channel implementation**, exactly
as a nervous system routes a signal through the appropriate medium (fast
myelinated axon vs. slow autonomic fibre) without the conscious brain managing
individual synapses.

Channels occupy a spectrum from fast-and-ephemeral to slow-and-durable, and the
selection rule is a pure function of the message's requirements: must survive
session death and be auditable → the most durable channel; must be queryable →
the structured channel; ephemeral and low-overhead → the append-only event log;
real-time and loss-tolerant → IPC. Add channels **from evidence, not
speculation**.

Three principles fall out of treating the fleet as a communication fabric:

1. **Match channel to message entropy.** A ~1-bit heartbeat uses the cheapest
   channel; a ~10,000-bit architecture decision uses the most durable one. Cost
   proportional to information value.
2. **Accept staleness as physics.** An agent can only react to what has reached
   it; the orchestrator's view is always stale by one channel-latency (the
   *light cone*). Design for eventual consistency between agents, strong
   consistency only within a single agent's own state. This is the standard
   distributed-systems model, for the standard reason.
3. **Redundancy is error correction, not waste.** Critical instructions repeat
   across several injection points (CLAUDE.md, formula, hooks, nudges) because
   the noisy channel — context compaction — may drop any single one. The optimal
   redundancy level is a function of the compaction error rate, not "as much as
   possible."

> **Superseded in practice (Amendment 3 §3.4).** The multi-channel Dolt / Signal
> Bus design once elaborated here was replaced by the two-plane model: the DAG
> carries 1 bit of control (done/not-done) and the filesystem carries all
> content. Mailboxes were eliminated. The principles above survive; the specific
> channel catalogue is history. See the handbook's Channels section for the
> current six-channel model.

---

## Part XV — The Creativity Interface: Advisory Panel as Amplifier

> *"Creativity is just connecting things."* — Steve Jobs

The most important interface is not the task queue but the boundary between the
human creator and the agent fleet. The human using cosmon is a creator
(researcher, designer, strategist, engineer), not a ticket dispatcher, and the
agents form an **advisory panel** — a set of specialized perspectives to
consult, challenge, and synthesize — rather than workers to be managed. This is
realized concretely as the `deep-think` deliberation formula: a molecule of kind
🧠 that runs multiple personas (pragmatist, critic, visionary, historian,
synthesizer, measurer) and produces a `synthesis.md`.

The interface is **dialogue, not dispatch**, and it must obey a handful of
disciplines, most of which are the Anti-Psychosis Principle (Part XIV) applied to
creative work:

1. **Dialogue over dispatch.** The creator poses questions; the panel returns
   perspectives. Molecules and formulas serve execution; the panel serves
   exploration.
2. **Match response depth to question depth.** No comprehensive analysis for a
   casual question; no shallow answer to a deep one.
3. **Preserve ambiguity until the creator resolves it.** The system illuminates
   options and tensions; convergence is the creator's prerogative, never the
   system's optimization target.
4. **Disagree with evidence, not authority.** The critic role is structurally
   required — a panel that only agrees is worse than useless. Disagreement is
   signal.
5. **Adapt to cognitive mode.** Infer high-temperature (divergent) vs.
   low-temperature (convergent) thinking from interaction patterns and match the
   response style; the creator can always override.
6. **Minimize the context tax.** Responses are information-dense — summaries over
   transcripts, conclusions with pointers over exhaustive chains. Value is
   insight per token.
7. **Align to this creator**, not the average user — their vocabulary, depth
   preference, aesthetic judgments, and intellectual honesty (tell them what is
   true, not what they want to hear).

---

## Amendments

### Amendment 1: Noesis Founding Thesis Migration (2026-04-05)

The Noogram founding thesis — the parent document from which this
Cosmon thesis descends — has been migrated from the founders' private vault
into [`docs/founding/`](docs/founding/). This makes the founding thesis
accessible to all agents working on the Cosmon codebase.

The founding thesis comprises four parts:

| Part | Document | Scope |
|------|----------|-------|
| I | [Core Thesis](docs/founding/founding-thesis.md) | Nine immutable principles, why Rust, agentic architecture, security thesis, value proposition |
| II | [Architecture](docs/founding/founding-thesis-architecture.md) | DDD, Hexagonal Architecture, C4/Structurizr, Event Sourcing, ADRs, Second Law of Agentic Systems |
| III | [Ubiquitous Language](docs/founding/founding-thesis-ubiquitous-language.md) | Complete domain type glossary — every bounded context, every Rust type, every invariant |
| — | [POC Roadmap](docs/founding/founding-thesis-poc-roadmap.md) | Three-month plan for initial proof-of-concept pipeline |

**Relationship between the two theses.** The founding thesis defines *what*
Noogram builds and *why* — the business principles, the domain model, the
architectural methods. The Cosmon thesis (this document) defines *how* the
agentic runtime works — the physics metaphor, the typestate machines, the
thermodynamic accounting, the fleet dynamics. The founding thesis is the
cosmic microwave background; the Cosmon thesis is the Standard Model.

This amendment does not modify any existing content. It establishes the
provenance link between the two documents and ensures the founding thesis
is version-controlled alongside the code it governs.

### Amendment 2: Topon Extraction and Ecosystem Boundary Clarification (2026-04-05)

The `cosmon-cfs` crate — Context File System, tree-sitter symbol extraction
with PageRank ranking — has been extracted into an independent project:
**Topon** (an independent project). Topon has its own founding thesis, its own
MCP server, and its own CLI. It is registered as a bearer in Neurion.

This extraction clarifies three boundaries in the ecosystem:

**1. Structural topology belongs to Topon, not Cosmon.**

Cosmon orchestrates agents (transport, lifecycle, communication). It does not
compute the structural shape of knowledge. The `cosmon-cfs` crate had zero
consumers within the Cosmon workspace; it was always a misplaced concern.
Topon owns all deterministic, graph-based structural analysis: tree-sitter
symbol graphs, wikilink graphs, heading hierarchies, schema graphs.

**2. Knowledge access routing belongs to Neurion, not Cosmon.**

The `cosmon-knowledge` crate (7-modality fabric with `KnowledgeFabric` trait,
`Query`, `QueryScope`, `KnowledgeSource`) overlaps with Neurion's semantic
layer (referent → bearer → reach with channel capacity vectors, intent-driven
routing, health metrics). Neurion's model is strictly more general. The
`Modality` enum is useful as shared vocabulary; the routing fabric should
converge with Neurion's reach resolution over time.

**3. Part XV ("Nervous Tissue") scope clarification.**

Part XV defines Cosmon's **inter-agent message routing** — the channels
(IPC, JSONL, Dolt) through which agents communicate with each other. This
is Cosmon's domain: transport between agents. Part XV does NOT define how
agents access knowledge stores: that is Neurion's domain (referent → bearer
routing). Both use the same abstract pattern (logical entity + multiple
physical carriers + intent-driven selection), documented as the `Reachable`
trait in Neurion's thesis (Part VII). Cosmon should reference Neurion for the
shared pattern rather than re-deriving it.

**Ecosystem architecture (post-extraction):**

```
                    NEURION (registry / routing kernel)
                   /          |            \
              TOPON         ALMANAC        [future services]
          (structural      (bibliography)
           topology)
              |              |
              +--- peer MCP services, registered as ---+
              |        bearers in neurion               |
              v              v
                    COSMON (agent orchestration)
                    (agents discover services via neurion,
                     call them directly over MCP protocol)
```

No compile-time dependency between Cosmon and Topon/Almanac. Coupling is
purely at the MCP protocol layer — runtime discovery via Neurion.

This amendment does not modify existing thesis content. It clarifies the
boundaries between Cosmon, Neurion, and Topon as the ecosystem matures.

### Amendment 3: Operational Discoveries — DAG Mechanics, Write-Read Asymmetry, and Thermodynamic Cleanup (2026-04-11)

This amendment crystallizes findings from `delib-20260411-dbbc` and prior
sessions. It documents ten discoveries that emerged from building and
operating the DAG execution system. These are observations about how the
running system actually behaves, not theoretical additions.

#### 3.1 The Write-Read Asymmetry (Principle 0 refinement)

Part XVIII (Coupling Principle) identified the causal asymmetry between
`cs evolve` (write) and `cs wait` (read). This amendment elevates the
insight to its proper status: the Write-Read Asymmetry is the deepest
structural mechanism in cosmon, and the real content of Principle 0
("It from Bit").

Wheeler's strong reading — observation commits the bit — is backwards
for cosmon. The JSON file under `.cosmon/state/` pre-exists the `cs wait`
that reads it. The bit is committed by `cs evolve` (write), not by
`cs wait` (read). The feedback loop is real, but its directionality comes
from the **asymmetry** between write and read, not their identity:

1. `cs evolve` **writes** state (irreversible mutation)
2. `cs wait` **reads** state (pure observation, no side effects)
3. The read constrains the next write
4. The one-tick lag between write and read provides safety

This asymmetry is why the Anti-Psychosis Principle (Part XIV) is
structurally enforced: `cs wait` is mechanically read-only, so the state
machine cannot be advanced inside a wait. The Quantum Zeno safety margin
is a consequence of the write-read asymmetry, not a separate mechanism.

**Architectural invariant:** no command may simultaneously write molecule
state and return a coupling report. See `docs/architectural-invariants.md` §3b.

#### 3.2 DAG-Aligned Git Branching

`cs tackle` branches from the **blocker's branch**, not from `main`. The
git DAG mirrors the cosmon dependency DAG:

- A dependent worker's worktree contains the blocker's committed output
  in its git history
- Content flows via the filesystem (git branch lineage), not via messages
- No mailboxes, no explicit content-passing, no serialization format

This is the practical realization of the Transport/Cognition separation
applied to inter-molecule communication. The transport layer (git) handles
content delivery; cosmon handles control flow (done/not-done).

See `docs/architectural-invariants.md` §3c.

#### 3.3 Merge-Before-Dispatch

When a molecule completes in a DAG context, `on_complete` calls `cs done`
(merge branch, teardown) **before** dispatching dependents via `cs tackle`.
This ordering is an invariant:

1. Worker calls `cs complete` (state transition)
2. Orchestrator calls `cs done` (merge + teardown)
3. Orchestrator calls `cs tackle` for dependents (branch from merged state)

The dependent worker sees the predecessor's output because the branch
was merged before the dependent's worktree was created.

See `docs/architectural-invariants.md` §3d.

#### 3.4 DAG as Communication Protocol

The DAG topology **is** the inter-agent communication protocol. Two channels:

- **Control channel (cosmon):** each edge carries **1 bit** per molecule
  per tick — done or not-done. This is the minimum signal needed to
  unblock a dependent.
- **Data channel (git):** content flows via branch lineage. Files on disk,
  not messages in envelopes.

Shannon's channel coding theorem applies: the control channel has capacity
1 bit/edge/tick, and that is exactly the capacity used. No wasted bandwidth.
The data channel has capacity limited only by disk — effectively infinite
compared to the control channel.

This separation is why mailboxes were eliminated. The original design
(Part XV) assumed inter-agent messaging would carry content. In practice,
the DAG branching strategy makes the filesystem the content channel, and
the control channel needs only the 1-bit done signal.

#### 3.5 CLI Over MCP for Workers

Workers use the `cs` CLI for all cosmon operations, not the MCP `cosmon_*`
tools. Three reasons:

1. **Walk-up discovery.** The CLI resolves `.cosmon/` by walking up from
   the worker's current directory. A worker in `.worktrees/task-xyz/`
   resolves correctly without configuration.
2. **Binary freshness.** The MCP server may run a stale binary if cosmon
   was rebuilt during the session. The CLI always runs the current binary.
3. **Git symmetry.** Workers interact with the state store the same way
   humans do — same binary, same flags, same output format. This mirrors
   the git model where every participant uses the same CLI.

The MCP server exists for external orchestrators and callers (the human
using Claude Code, MCP-connected planners). It is not for workers.

See `docs/architectural-invariants.md` §3e.

#### 3.6 Celestial Mechanics — Runtime Control States

The runtime operates as a control system with four states:

| State | Meaning | Transition |
|-------|---------|------------|
| **Driven** | Active dispatch in progress | → Relaxing (all dispatched) |
| **Relaxing** | Waiting for completions | → Equilibrium (all done) or → Driven (new work) |
| **Equilibrium** | All work complete, at rest | → Driven (new DAG) |
| **Quenched** | Externally halted | → Driven (restored) |

These map to the three regimes: Inert = Equilibrium, Propelled = Driven +
Relaxing (tackle-based), Autonomous = Driven + Relaxing (runtime-based).
Quenched is orthogonal — it can interrupt any regime.

See `docs/architectural-invariants.md` §3f.

#### 3.7 Mission-Plan Formula

The highest-level formula pattern: `mission(goal, fleet_template) → DAG`.
Innovation happens through formulas, not infrastructure. A new kind of work
requires a new formula, not new commands, kinds, or state transitions.

See `docs/architectural-invariants.md` §3g.

#### 3.8 Thermodynamic Cleanup

The Feynman Test (Part XII, line 1131) asks four questions of every metric:
(a) computable from existing data, (b) produces a number with units,
(c) responsive to system changes, (d) actionable. Several sections of
Part XII **fail this test**:

| Section | Verdict | Reason |
|---------|---------|--------|
| Three Laws of Cosmon Thermodynamics | **Decorative** | Mapping to real thermodynamic laws adds no predictive power |
| Carnot Efficiency of an Agent | **Redundant** | η_agent = productive_tokens/total_tokens is just free_energy_ratio renamed |
| Agent Work Cycle (4-phase Carnot) | **Decorative** | Forced mapping; the 4 phases don't predict or measure anything |
| Helmholtz Free Energy F = E − TS | **Non-computable** | T (API calls/minute) × S (total entropy in bits) has no meaningful unit; the product is not actionable |
| Cosmological Timeline | **Decorative** | Poetic, but "galaxy formation" does not predict team behavior |

These sections are **retained** in the thesis for intellectual lineage and
as a record of the project's thinking, but they are **not load-bearing**.
An inline note marks them in the text.

**Load-bearing thermodynamic content** (passes the Feynman Test):

- Token budgets and the free energy ratio (Part XI) — real, enforced
- The four computable entropy sources (Part XII §1–4) — Shannon entropy with instruments
- Landauer's principle for context loss — genuine, measurable
- The Feynman Test itself — the discipline that keeps the metaphor honest
- The Coupling Principle and its bits/token metric (Part XVIII) — computable

The lesson: the physics vocabulary earns its place only when it compresses
observed data better than a plain description (Preamble, line 59). The
Three Laws and Carnot Cycle do not. Token budgets and Shannon entropy do.

#### 3.9 Product Pitch

Added to README.md:

> **Cosmon is a stateless CLI that gives AI agents persistent identity,
> typed lifecycle, and crash-recovery — so humans can run multi-agent
> fleets without building infrastructure.**

This captures the product in one sentence, per the Jobs discipline: what
does it do, for whom, and why should they care?

#### 3.10 Cross-References

This amendment updates three companion files:

- `CLAUDE.md` — added §CLI over MCP, §DAG-aligned branching, §Merge-before-dispatch
- `docs/architectural-invariants.md` — added §3b through §3g and coherence checklist items 8–10
- `README.md` — added one-sentence product pitch

---

## Appendix C — The Entropy Lineage: From Steam Engines to Agent Orchestration

> *"Nature does not know what entropy is. She just does what she does."*
> — Ludwig Boltzmann (attributed)

This appendix once traced the full intellectual lineage of entropy — Carnot,
Clausius, Boltzmann, Gibbs, Shannon, Jaynes, Landauer, Bennett, Bekenstein,
Hawking, Wheeler — mapping each figure's contribution onto a cosmon design
decision. Most of that mapping supported the thermodynamic sections since demoted
as decorative (Part XII, Amendment 3), so the genealogy is no longer
load-bearing and has been removed from the thesis.

The links that **do** survive are stated where they are used: Wheeler's *It from
Bit* under Principle 0; Shannon entropy in the four computable entropy sources
(Part XII §1–4); Landauer's bound for the token cost of context erasure (Part XI
and below); and the Bekenstein bound as the model for a bounded context window
(Part XVI). The demoted thermodynamic constructs — Helmholtz free energy, the
Carnot cycle, the Three Laws, and thermodynamic "temperature" — are recorded,
with the reason each was cut, in
[`docs/appendix-physics-inspiration.md`](docs/appendix-physics-inspiration.md).
The key references whose ideas survive (Shannon, Landauer, Bekenstein, Wheeler)
are retained in the References of Part XVII below.

---

## Part XVI — Surface Observability: The Projection Boundary

> *"One bit of information can have several materializations."*

### The Missing Boundary

Parts I through XV describe a system rich in internal state: molecules with
typestate machines, fleets with worker assignments, energy budgets with token
accounting, entropy metrics, creative panels. This internal richness is
necessary but insufficient. A system that cannot be observed by external
participants does not exist for them.

The founding principles speak to the **internal** observer: the human operator
who runs `cs ensemble`, the agent that calls `cosmon_observe`. But a second
observer class exists: the **non-participant** — a developer who opens the
repository without knowing Cosmon, a CI pipeline that checks project health,
a collaborator who reads `STATUS.md`.

### The Bekenstein Analogy

The Bekenstein bound — the information accessible to an external observer scales
with a system's **surface area**, not its enclosed **volume** — gives the
governing image: cosmon can have arbitrarily rich internal state (molecules,
fleets, budgets), but its value to non-participants is bounded entirely by what
appears at the **projection surface**, the set of files and interfaces through
which internal state becomes externally legible.

### The Corollary: Surface Observability

The three founding principles (Transport/Cognition, Intentions not Ownership,
Minimum Action) acquire a corollary:

> **Surface Observability.** Every piece of internal state that matters to a
> non-participant MUST have a declared surface projection. If it cannot be
> observed externally, it does not exist externally. The framework's value to
> the broader project is bounded by the area of its projection surface, not
> the volume of its internal state.

### Referents and Reaches

The nervous system (neurion) provides the formal model for surface projection.
A **referent** is a logical piece of information: the project status, the issue
list, the architecture decisions. A **reach** is a physical materialization of
that referent: a Markdown file, a GitHub Issue, an MCP tool response, a
dashboard widget.

The principle is: **one referent, many reaches.** The same bit of information
(molecule status) is projected onto multiple surfaces (STATUS.md, GitHub Issue,
`cs ensemble` output, MCP JSON response). Each surface serves a different
audience at a different fidelity/latency tradeoff.

### Mechanical and Cognitive Reconciliation

Surface projection operates in two modes:

1. **Mechanical reconciliation** (default): a deterministic pure function from
   internal state to surface artifact. Zero tokens, zero cognition. Runs after
   every state transition. Idempotent.

2. **Cognitive reconciliation** (on ambiguity): when mechanical projection
   detects conflicts — human edits on projected files, desynchronization from
   external pushes, schema drift — it spawns an ephemeral worker whose mission
   is to analyze the divergence, attempt auto-resolution, and escalate to a
   human if the ambiguity persists.

This follows the founding principle: Transport handles the 95%. Cognition
handles the edge cases. The distinction maps directly to the Transport/Cognition
split of Part I.

### The Standard Interface

The projection surface consists of standard project files that any developer
understands without specialized tools:

| Surface | Content | Non-participant sees |
|---------|---------|---------------------|
| `STATUS.md` | Fleet health, active work | "What is this project doing?" |
| `ISSUES.md` | Tracked issues, blockers | "What needs to be fixed?" |
| `docs/adr/` | Architecture decisions | "Why was this designed this way?" |
| GitHub Issues | Tracked work with labels | Standard issue workflow |
| `cs ensemble` | Fleet dashboard | Operator view (participant) |

The files carry a header. On cosmon-owned surfaces that header reads
`<!-- Generated by cosmon. Source of truth: .cosmon/ -->`. On the default
host-native surfaces it reads `<!-- auto-generated from {dir_name}/ — edit
the source -->`, where `{dir_name}` is the source directory the projection
is derived from (e.g. `docs/adr/`). Both variants signal the same invariant:
these are one-way projections. To change the data, change the source
(create a molecule, evolve a step), not the view.

### Transparency as self-declaration, not tool credit — Wheeler's reframe

The original draft of this part conflated two properties that should
never have been fused. We wrote that the header *"mentions cosmon"* and
treated that as the transparency mechanism. Wheeler's reframe, captured
in the `delib-20260409-f4e1` panel and frozen by
[ADR-017](docs/adr/017-host-native-projection.md), is sharper:

> **Transparency is a property of the surface declaring it is
> auto-generated and pointing at its source — not of the surface
> crediting the rendering engine.**

A `Makefile`-built `.o` file is transparent without being stamped
*"built by GCC"*. A generated Protobuf stub is transparent without
being stamped *"built by protoc"*. The reader learns everything they
need from a single signal: *"this file is derived; the source lives
over there."* The tool name is irrelevant to that signal.

The surface projection system adopts the same discipline. The
**minimum footer** that satisfies the transparency obligation is:

```
<!-- auto-generated from {dir_name}/ — edit the source -->
```

It must contain (a) the words *"auto-generated"* so tools can detect
generated files with a mechanical grep, (b) the source directory so a
human knows where to make a change, and (c) nothing else. Cosmon's
name, version, or involvement is explicitly **not** part of the
transparency contract. A surface that mentions cosmon has chosen to
announce itself (the `attributed` mode, reserved for cosmon-owned
surfaces); it has not become "more transparent".

This refinement shifts the moral weight of the surface observability
corollary. The corollary still holds — every piece of internal state
that matters externally must have a declared projection — but the
projection's job is to declare its own derived status, not to carry a
credit line for the engine that produced it. The host project owns
its surfaces; cosmon is the tool that happens to produce them, and
tools should disappear into the artefacts they produce.

### References

- Bekenstein, J.D. (1973). "Black holes and entropy."
  Physical Review D, 7(8), 2333-2346.
- Wheeler, J.A. (1990). "Information, Physics, Quantum: The Search for Links."
  In Zurek (Ed.), Complexity, Entropy and the Physics of Information.
- [ADR-017: Host-Native Projection and Surface Rendering Invariants](docs/adr/017-host-native-projection.md)
  — operationalises the transparency reframe into rendering modes,
  backref invariants, and mirror schema versioning.
- `delib-20260409-f4e1` — the deep-think deliberation (feynman, jobs,
  wheeler) whose synthesis #7 produced the reframe.

---

## Part XVII — The Attention Conservation Law and Molecule Kinds

> *"Every particle is a claim on the system's attention budget.
> The conservation law the system most needs is attention, not energy."*

### Two Conservation Laws

Part XI introduced the **Energy Principle**: every token has a cost. The energy
budget constrains total token consumption. But energy alone is insufficient.
An agent can nucleate 50 idea-molecules in a single session before any hits
the energy gate. The energy budget catches proliferation *eventually*; the
attention budget catches it *immediately*.

The **Attention Conservation Law**: the total number of alive (non-terminal)
molecules in a fleet must not exceed a configurable attention budget. When the
budget is reached, new nucleation requires either completing or collapsing an
existing molecule first.

```
attention_budget(fleet) >= count(molecules where status ∈ {pending, queued, running, frozen})
```

This is the analog of baryon number conservation in physics: the net count of
"heavy" particles is conserved. You cannot create something from nothing; you
must either transform what exists or release capacity by completing work.

### Molecule Kinds

Molecules have a **cognitive nature** (`MoleculeKind`) orthogonal to their
behavioral template (`Formula`). Kind is WHAT the molecule represents; Formula
is HOW it executes.

| Kind | Nature | Interactions | Surface |
|------|--------|-------------|---------|
| **Idea** | Unstructured insight | Can decay → Tasks, transform → Decision | IDEAS.md |
| **Task** | Actionable work | Can merge → Decision | ISSUES.md |
| **Decision** | Architecture record | Terminal kind | docs/adr/ |
| **Issue** | Tracked problem | Can decay → Tasks | ISSUES.md |
| **Signal** | Ephemeral observation | Zero steps, auto-completes | STATUS.md |

> **Not kinds: `map` / `reduce` / `while`.** Fan-out, fan-in, and bounded iteration are classical control-flow patterns (Dust's block taxonomy, functional `map`/`reduce`, imperative `while`). They are intentionally **not** molecule kinds and **not** new Rust types: they are TOML formulas that compose the existing dynamic-DAG primitives (`cs nucleate --decayed-from`, `cs wait`, typed links). A `map` molecule is a 🔧 Task with the `map` formula; its kind describes cognitive nature, its formula describes behavioral template. See [`docs/handbook.md` §Map / Reduce / While](docs/handbook.md#map-reduce-while) and `.cosmon/formulas/{map,reduce,while}.formula.toml`.

### Interactions

Molecules interact through three operations, each triggered explicitly by an
agent or operator (never automatically — Anti-Psychosis Principle):

1. **Decay**: one molecule produces N child molecules.
   An idea decomposes into implementation tasks.
   The source completes; products are nucleated.

2. **Merge**: N molecules converge into one.
   Research tasks synthesize into an architecture decision.
   Sources complete; the product is nucleated.

3. **Transform**: one molecule changes kind without changing identity.
   An idea is promoted to a task when it becomes actionable.

Each interaction is recorded as a domain event with typed links between
participants (`DecayedFrom`, `MergedInto`, `TransformedFrom`), enabling
full lineage tracking.

### The Minimum Action Constraint

Interactions are subject to the Minimum Action principle (Founding Principle 3):

- **Decay** costs attention: each product claims one slot.
- **Merge** recovers attention: N sources → 1 product frees N-1 slots.
- **Transform** is attention-neutral: the count doesn't change.

This creates an economic incentive to merge rather than decay: synthesis
is cheaper than decomposition. The system naturally tends toward consolidation,
which is the desired behavior: fewer, larger, well-understood work units rather
than a proliferation of small fragments.

### References

The entropy-lineage bibliography that once ran here (Carnot through Hawking,
26 entries) supported the historical lineage since removed (Appendix C). The
references below are those whose ideas remain load-bearing in the surviving text:

1. Shannon, C. E. (1948). A mathematical theory of communication. *The Bell System Technical Journal*, 27(3), 379–423. doi:[10.1002/j.1538-7305.1948.tb01338.x](https://doi.org/10.1002/j.1538-7305.1948.tb01338.x)

2. Landauer, R. (1961). Irreversibility and heat generation in the computing process. *IBM Journal of Research and Development*, 5(3), 183–191. doi:[10.1147/rd.53.0183](https://doi.org/10.1147/rd.53.0183)

3. Bérut, A., Arakelyan, A., Petrosyan, A., Ciliberto, S., Dillenschneider, R. & Lutz, E. (2012). Experimental verification of Landauer's principle linking information and thermodynamics. *Nature*, 483(7388), 187–189. doi:[10.1038/nature10872](https://doi.org/10.1038/nature10872)

4. Bekenstein, J. D. (1973). Black holes and entropy. *Physical Review D*, 7(8), 2333–2346. doi:[10.1103/PhysRevD.7.2333](https://doi.org/10.1103/PhysRevD.7.2333)

5. Wheeler, J. A. (1990). Information, physics, quantum: The search for links. In Zurek, W. H. (Ed.), *Complexity, Entropy and the Physics of Information*, pp. 3–28. Addison-Wesley. doi:[10.1201/9780429502880-2](https://doi.org/10.1201/9780429502880-2)

---

## Part XVIII — The Coupling Principle

> *"Entangle binds molecules to each other. Couple binds a molecule
> to its observer."*

### The unnamed in-breath

Part XVI names **projection symmetry** — one referent, many reaches.
Projection is the *out-breath*. Its counterpart — an observer reading
a surface and the read *causally constraining* the next mutation of
the molecule behind it — is the *in-breath*. Together they form the
full respiratory cycle of a running fleet. Projection has a name;
the loop-closing in-breath does not. This part names it. It adds no
types, no verbs, no kinds, no fields; the mechanism is already in
the code.

### The principle

> **Coupling Principle.** A molecule is **coupled** to its environment
> when its projected state causally constrains the next action taken
> by an observer — human, AI agent, or patrol watchdog. **Coupling
> strength** is the observer-independent ratio of decision-relevant
> bits surfaced per token spent projecting them.

Coupling is relational the way `entangle` is relational. Entangle
binds molecules to one another through typed links; couple binds a
molecule to its observer through the projection surface. A molecule
never `wait`-ed on and on no surface anyone reads is **decoupled**.
One whose metrics flow into every next decision is **strongly
coupled**.

### Coupling channel capacity

Let `M_t` be the metric bundle projected at interaction `t` (for
example, the `WaitMetrics` returned by `cs wait`); let `X_{t+1}` be
the observer's next action. The **realised** coupling strength is

```
η_coupling(t) = I(X_{t+1}; M_t) / tokens_to_project(M_t)    [bits/token]
```

This is observer-dependent. What *is* frame-independent is the
maximum over observer classes, the **coupling channel capacity**
`C_coupling = max_{obs} I(X_{t+1}; M_t) / tokens_to_project(M_t)`.
The physics is the capacity; the realised value is the engineering.
The quantity inherits Part XII's four-criterion Feynman test
(line 1133): (a) computable from data already on disk,
(b) unit-bearing (bits/token), (c) responsive to surface changes,
(d) decidable. It is an observable, not a metaphor.

### Feynman's reverse-causality critique, and the anti-psychosis tie-in

Wheeler's strong *It-from-Bit* reading treats observation as the
commitment of the bit. Feynman's critique survives review: in cosmon,
the JSON file under `.cosmon/state/` **pre-exists** the `cs wait`
that reads it. The bit is committed by `cs evolve` (write), not by
`cs wait` (read). Ontology is exactly backward from strict Wheeler.

The Coupling Principle accepts this and reframes: **cosmon is not
It-from-Bit in Wheeler's strong sense.** The loop is real for a
different reason: because `cs evolve` writes and `cs wait` reads,
the two operations are asymmetric in time. Write precedes read, read
precedes next write, and the next write is *constrained* by what the
read surfaced. **The feedback loop is the asymmetry between write
and read, not their identity.** That asymmetry is what gives the
loop its temporal direction — a property strict It-from-Bit lacks.

This same asymmetry is why Part XIV's Anti-Psychosis Principle is
already structurally enforced, not merely policy. Because `cs wait`
is **mechanically read-only** (`wait.rs` lines 9–16), there is a
**one-tick lag** between observation and the next mutation: the
state machine cannot be advanced *inside* the wait. This one-tick
lag is the Quantum Zeno safety margin of Part XIV expressed as
causal delay. Two parts of the thesis converge on the same
constraint from regulation and projection respectively.

### The canonical coupling report

The first (and so far only) instance of this principle in the
codebase is the `WaitMetrics` bundle returned by `cs wait` — see
[`crates/cosmon-state/src/wait.rs`](crates/cosmon-state/src/wait.rs)
lines 25–113. The module header at line 26 already uses the phrase
*"feedback loop"*, and `transitions` at line 96 is a bit-counter
sitting in the read path. Retrofitting `WaitMetrics` as the
canonical coupling report requires no code changes: the bundle *is*
the projected `M_t` in the formula above.

A constraint follows. Shannon (see
`delib-20260409-b22c/responses/shannon.md`) identifies a **cognitive
SNR ceiling at roughly seven decorrelated scalar fields**: past
that, realised mutual information *decreases* with each added field.
`WaitMetrics` already carries five scalar channels. **This part
forbids widening the bundle.** The current shape is load-bearing.

### The fifth entropy source, and a test Feynman would accept

Part XII enumerates four computable sources of entropy (message,
context, code, state). Coupling supplies a **fifth**:
`H_couple = H(X_{t+1}) − H(X_{t+1} | M_t)`, in bits per interaction,
the only cosmon entropy source with a human *inside* the channel.
Its maximum over observer classes is `C_coupling`.

The full estimator fits in under fifty lines of Rust against
`log/energy.jsonl` and the molecule JSON files:

```rust
// For each (wait_t, nucleate_{t+1}) pair on disk, estimate
// I(X_{t+1}; M_t) / tokens(M_t).
pub fn coupling_capacity(state_dir: &Path) -> Option<f64> {
    let pairs = wait_nucleate_pairs(state_dir)?;     // walk molecule dirs
    let metric_bits = discretise_metrics(&pairs);    // WaitMetrics → buckets
    let scope_bits  = discretise_scopes(&pairs);     // nucleate vars → buckets
    let mi = estimate_mi(&metric_bits, &scope_bits); // plug-in Î
    let tokens: u64 = pairs.iter().map(|p| p.tokens_to_project).sum();
    (tokens > 0).then(|| mi / tokens as f64)
}
```

Every term is on disk today. All four Feynman criteria pass. No new
tracker, no new field, no new kind.

### Cross-references and what this part does not add

- **Part XI — Energy Principle.** `tokens_to_project(M_t)` is the
  Part XI cost of the projection side.
- **Part XII — Thermodynamic Extension.** Adds `H_couple` beside the
  four sources at line 1137; inherits the Feynman test at line 1133.
- **Part XIV — Observer Regulation.** The one-tick lag is the Quantum
  Zeno safety margin expressed causally.
- **Part XVI — Surface Observability.** Projection (out-breath) and
  coupling (in-breath) form the full respiratory cycle; neither half
  stands alone.

No new `MoleculeKind`. No new `cs` verb. No new domain type. No new
field on `WaitMetrics`. No new tracker. **The Coupling Principle is
the recognition of a pattern already implemented**: it names what
`cs wait`, `WaitMetrics`, `events.jsonl`, and the projection surfaces
already do when composed.

### References

- `crates/cosmon-state/src/wait.rs` lines 25–113 — canonical coupling
  report (`WaitMetrics` and the module header that already uses the
  phrase *feedback loop*).
- `delib-20260409-b22c` — deep-think panel (wheeler, hawking, shannon,
  feynman, einstein) whose `synthesis.md` formalises this position:
  naming + Position 3 at lines 280–331; dual-symmetry S1 and
  anti-psychosis tie-in S2 at lines 218–229; Feynman's
  reverse-causality critique S5 at lines 243–247.
- `idea-20260409-02d2` — follow-up idea carrying the `couple` naming
  into the ubiquitous language and Part V Vocabulary Stack.
- Part XII line 1133 — the four-criterion Feynman test.
- Part XVI Wheeler's reframe (lines 2178–2218) — precedent for
  citing a deep-think deliberation inline.

---

## Part XIX — The Unification Principle

> *"Molecules and formulas are the only primitives. Everything else
> is composition."*

### The principle

> **Unification Principle.** Cosmon has exactly two extensible
> primitives: the **molecule** (a typestate work unit persisted on
> disk) and the **formula** (a TOML script that drives a molecule
> through typed steps). Every capability the runtime acquires — new
> workflows, new review loops, new coordination patterns, even
> fleet-level orchestration — is added by writing a new formula,
> never by growing the core.

This is the recognition of a pattern the code has been enforcing all
along. It is stated here, late in the thesis, because the evidence
needed to defend it only accumulated after the mission-plan and
dynamic-DAG iterations landed. The principle is load-bearing for
everything that comes next: it is the guarantee that cosmon can be
extended without being rewritten.

### The mission-plan proof

The sharpest test of the Unification Principle came from fleet
orchestration. The requirement was "plan a multi-worker mission,
dispatch workers, gate on reviewer approval, loop on revision." A
conventional system would answer this with a new subsystem: a
planner service, a dispatcher daemon, a review state machine, a
revision queue. Cosmon answered with **one file**:
[`mission-plan.formula.toml`](.cosmon/formulas/mission-plan.formula.toml),
using the pre-existing `🧠 deliberation` molecule kind.

- **No new crate.** `cargo check --workspace` saw no new targets.
- **No new runtime.** The resident runtime does not exist yet; the
  transactional core handled every step.
- **No new command.** Every transition went through `cs nucleate`,
  `cs evolve`, `cs wait`, `cs complete`.
- **No new kind.** The deliberation kind already existed for panel
  reviews; mission-plan reused its slot.
- **No special-case code.** The Architect panel (C5 of
  `delib-20260411-066c`) verified that
  [`dag_policy.rs`](crates/cosmon-state/src/dag_policy.rs) contains
  no `if formula == "mission-plan"` branches. The generic `Blocks`
  absorption that made dynamic revision loops possible is, by
  construction, indifferent to which formula produced the links.

Fleet orchestration — the feature most likely to have required a
new layer — was added by writing one formula and exercising the
generic link-absorption path that was already there. If the
Unification Principle were false, this test would have exposed it.

### What counts as "the core"

The principle forbids growing the core. The core is defined
extensionally:

| Layer | Contents | Extensible? |
|-------|----------|-------------|
| **Domain types** | `Molecule`, `MoleculeKind`, `MoleculeStatus`, `Formula`, `Step`, `Link`, `WorkerId`, `FleetId` | No — closed set |
| **Transitions** | `nucleate`, `evolve`, `complete`, `collapse`, `freeze`, `thaw`, `decay`, `merge` | No — closed set |
| **Link types** | `Blocks`, `DecayProduct`, `Entangled`, 5 others | Closed + `Entangled` escape hatch |
| **Formulas** | `task-work`, `deep-think`, `mission-plan`, … | **Yes — the only extension point** |

Every time the core grows, the Unification Principle is broken and
the architectural-invariants coherence checklist
(`CLAUDE.md §Architectural Discipline`) must be re-run. Every time a
formula grows, nothing in the core needs to change. This asymmetry
is the point.

### Reframing the founding insight: write-read asymmetry

The Wheeler panel in `delib-20260411-066c` (synthesis S2, responses
`wheeler.md`) concluded that the strong *It-from-Bit* reading — which
Part XVIII already concedes fails under Feynman's reverse-causality
critique — is the wrong summary of what cosmon actually is. The
correct summary is smaller and more defensible.

> **Founding insight (revised).** Cosmon's runtime semantics rest on
> two asymmetries, not on participatory observation:
>
> 1. **Write-read asymmetry.** `cs evolve` *commits* the bit by
>    writing the state file. `cs wait` *reads* the committed bit and
>    thereby *constrains* the next mutation. Write always precedes
>    read; read always precedes the next write; the loop is directed
>    in time because the two halves are mechanically distinct. The
>    feedback loop is the asymmetry, not the identity.
>
> 2. **Observation delegation.** Liveness is not a property of a
>    molecule. It is a *delegation* to an observer. The same
>    molecule file is Inert when no one is watching, Propelled when
>    a patrol watchdog is watching, and Autonomous when a resident
>    runtime is watching. Nothing about the file changes. What
>    changes is who holds the read side of the asymmetry.

The most Wheelerian sentence in the system is not *It from Bit*. It
is **"liveness is not a property; it is a delegation."** Regimes
are defined by the observer, not by intrinsic state. This is the
only genuinely deep claim Part I was reaching for, and it survives
every panel in the deliberation unscathed.

Two structural consequences:

- **The DAG is a control channel, not a data channel.** The Shannon
  panel (synthesis S1, `shannon.md`) measured the DAG edge at *one
  bit per molecule lifetime* — done/not-done. All content flows
  through the filesystem. The DAG orders work; the filesystem
  carries work. The thesis must state this separation explicitly
  because the physics vocabulary previously blurred it.
- **The one-tick lag is structural, not conventional.** Because
  `cs wait` is mechanically read-only
  ([`wait.rs`](crates/cosmon-state/src/wait.rs) lines 9–16), the
  state machine cannot advance *inside* the wait. This is the same
  Quantum Zeno safety margin Part XIV names from the regulation
  side and Part XVIII names from the projection side. Three parts
  converge on the same constraint from three angles: that is
  evidence the constraint is real.

### What the physics vocabulary over-claimed

The Feynman and Shannon panels independently audited the
thermodynamic content of Parts XI and XII and converged on the
same verdict (synthesis C2, responses `feynman.md` and
`shannon.md`):

| Construct | Verdict | Reason |
|-----------|---------|--------|
| `EnergyBudget` (token counter) | **Substance** | Real resource tracking; units consistent |
| `worker_status_entropy()` | **Substance** | Genuine Shannon `H(X)` over worker states |
| `free_energy_ratio` | **Substance** | Useful efficiency metric; dimensionless |
| `Temperature` (LLM sampling) | **Harmless** | Not thermodynamic `T`; labelled honestly |
| `HelmholtzFreeEnergy = U − TS` | **Cargo cult** | Mixes tokens, dimensionless temp, bits; no predictive power |
| **Carnot cycle mapping** | **Cargo cult** | No prediction; decorative analogy |
| **Three Laws of Cosmon Thermodynamics** | **Cargo cult** | Decorative; not load-bearing in any proof |
| `C_coupling` notation (Part XVIII) | **Deferred** | Well-formed but not yet implemented in code |

The rule the panels converged on is Feynman's: **a quantity earns a
physics name only if it makes a prediction that would fail if the
quantity were wrong.** `worker_status_entropy` passes: changing it
moves an observable fleet statistic. `HelmholtzFreeEnergy` fails:
nothing in the code reads it, and its formula is dimensionally
inconsistent (tokens − dimensionless·bits).

**This part demotes the failing constructs to
[`docs/appendix-physics-inspiration.md`](docs/appendix-physics-inspiration.md)
as design inspiration.** They are preserved because the metaphors
were useful during discovery, but they are no longer normative. The
thesis's quantitative commitments are the three substance items
above, plus the five Part XII entropy sources, plus the Part XVIII
coupling capacity. Everything else is acknowledged as decoration.

### Relationship to Parts I, XI, XII, XVIII

- **Part I (Universe Metaphor).** The metaphor remains as narrative
  framing, having earned its place during design, but Principle 0
  (self-reference) is now the load-bearing founding claim, not
  It-from-Bit. Part XIX makes the demotion explicit.
- **Part XI (Energy Principle).** `EnergyBudget` survives untouched.
  The Temperature/Helmholtz decorations around it move to the
  appendix. The token accounting is the real content.
- **Part XII (Thermodynamic Extension).** The four entropy sources
  (message, context, code, state) plus Part XVIII's `H_couple`
  remain — all pass the four-criterion Feynman test at line 1133.
  The Three-Laws framing is demoted; the entropy-as-observable
  discipline is kept.
- **Part XVIII (Coupling Principle).** Unchanged. Coupling is the
  best-developed instance of the Unification Principle: it adds no
  types, no verbs, no kinds, no fields: exactly what Part XIX
  demands of every future extension.

### What this part does not add

No new `MoleculeKind`. No new `cs` verb. No new formula. No new
crate. Part XIX is pure recognition: it names the extension
discipline the code has been enforcing, and it corrects two
framings (founding insight, thermodynamic rigour) that the earlier
parts stated more ambitiously than the code supports. The mechanism
is already in the repository; this part is the acknowledgement.

### References

- `delib-20260411-066c` — "Grand Unification of Cosmon" deep-think
  panel (wheeler, einstein, feynman, hawking, shannon, architect,
  jobs, knuth). Synthesis C1 (composability survives scrutiny), C2
  (thermodynamics is cargo cult), S2 (write-read asymmetry is the
  founding insight), D2 (unification is engineering, not physics).
- [`.cosmon/formulas/mission-plan.formula.toml`](.cosmon/formulas/mission-plan.formula.toml)
  — the canonical Unification Principle proof: fleet orchestration
  added without growing the core.
- [`crates/cosmon-state/src/dag_policy.rs`](crates/cosmon-state/src/dag_policy.rs)
  — generic `Blocks` absorption path; no formula-specific code.
- [`crates/cosmon-core/src/worker_status.rs`](crates/cosmon-core/src/worker_status.rs)
  — `worker_status_entropy()`, the one genuine Shannon entropy in
  the system.
- `docs/appendix-physics-inspiration.md` — demoted home of
  Helmholtz free energy, the Carnot mapping, and the Three Laws.
- Part XVIII — the prototype of an extension that adds no core.

### Self-similarity corollary

The Unification Principle has a geometric consequence: the system is
self-similar across five levels — **step**, **molecule**, **polymer**,
**fleet**, **methodology**. At each level, the same pattern recurs: a
typed unit progresses through states, evidence accumulates, and completion
propagates to the containing level. A step completes inside a molecule; a
molecule completes inside a polymer; a polymer drains inside a fleet; a
fleet cycle closes inside a methodology iteration.

This is a structural invariant, not a metaphor. If a capability
works at one level but is meaningless at the level above or below, it is
a local patch, not a composable primitive. The coherence checklist
(architectural-invariants §5, item 12) encodes this as a gate.

*Origin: chronicle "la fractalité est elle-même fractale" (2026-04-12).*

---

## Part XX — Two-Axis Proof-of-Work: Process and Epistemics

> *"A complete cognitive proof-of-work has two axes: the process by
> which the work arrived, and the epistemic grounding of what the work
> says. Both must be structurally verifiable, or the system is broken."*

### The principle

> **Two-Axis Proof-of-Work.** Every molecule that produces claims
> carries two independent, jointly-necessary verification chains. The
> **process chain** (`prompt.md`, `briefing.md`, `log.md`,
> `synthesis.md`, per-step git commits) attests *how* the worker
> traversed the formula; it is already in place and verified by
> `cs verify` MVP. The **epistemic chain** (`provenance.md` sidecar —
> one entry per factual claim, resolved to a typed source: DOI, URL,
> file path, upstream molecule citekey) attests *why* each claim in
> the synthesis should be believed. A molecule is **fully auditable**
> if and only if both chains are intact. The extended `cs verify`
> checks both axes. A Witness Charter vetoer (Part XIV coupling to
> ADR-034) signs `Ratified` if and only if
> `cs verify --process && cs verify --claims` both pass. A merge-to-main
> is refused when either chain is missing on a claim-producing persona.

The process chain answers *"how did we get here?"*; the epistemic
chain answers *"why believe it?"*. Neither half substitutes for
the other: a valid process chain does not prevent hallucinated
citations in `synthesis.md`, and a populated `provenance.md` without
a traceable sequence of steps is a decorated claim without a worker.
The two chains compose; they do not reduce. This Part names the
compositional structure the repository has been enforcing on the
process side since day one, and declares the epistemic side a
first-class load-bearing mechanism, not a "nice to have", so that
ADR-041 has a thesis foothold to implement from. The doctrine mirrors
the formal-proof side of the ecosystem, where `foundry`'s `kernel.check`
(process) and `kernel-provenance.log` + `golden/hashes.lock`
(epistemic) already enforce the same two-axis structure on Lean-style
proof terms; the convergence between textual claims and formal claims
on identical discipline is the strongest signal that the structure is
fundamental, not idiosyncratic. Its normative instantiation is ADR-041
(pending). Cross-references: ADR-036 (intent+receipt, the process
chain's crash-safety guarantee), ADR-037 (lineage conservation,
the epistemic chain's decidability tiers), ADR-034 (Witness Charter
v0, the external signature terminating the P_external appeals chain).

### What this Part does not add

No new `MoleculeKind`. No new `cs` verb in the core — `cs verify
--claims` is an extension of the existing `cs verify`. No new domain
type. No new field on `Molecule`. The `provenance.md` sidecar is a
markdown artifact alongside `prompt.md`, `briefing.md`, `log.md`, and
`synthesis.md`; it joins the proof-of-work trail defined in
`CLAUDE.md §Molecule artifacts`. Part XX is the thesis-level
recognition that the trail has two columns, not one; the implementation
and schema live in ADR-041.
