// SPDX-License-Identifier: AGPL-3.0-only

//! Formula and workflow template types.
//!
//! A formula is a TOML-defined workflow template with ordered steps and
//! typed variables. This module parses `.formula.toml` files into the
//! [`Formula`] domain type with validation, including [`Tier`] enforcement
//! (see ADR-032).
//!
//! # Examples
//!
//! ```
//! use cosmon_core::formula::Formula;
//!
//! let toml = r#"
//! formula = "deploy-pipeline"
//! version = 1
//! description = "Build, test, and deploy"
//!
//! [[steps]]
//! id = "build"
//! title = "Build"
//! description = "Compile the project."
//!
//! [[steps]]
//! id = "test"
//! title = "Test"
//! description = "Run the test suite."
//! needs = ["build"]
//! "#;
//!
//! let formula = Formula::parse(toml).unwrap();
//! assert_eq!(formula.name.as_str(), "deploy-pipeline");
//! assert_eq!(formula.steps.len(), 2);
//! // Steps are topologically sorted: build before test
//! assert_eq!(formula.steps[0].id, "build");
//! assert_eq!(formula.steps[1].id, "test");
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Deserialize;

use crate::id::{FormulaId, MoleculeId};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when parsing or validating a formula.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set will grow; external callers must keep a `_ =>` arm
pub enum FormulaError {
    /// The TOML source text is syntactically invalid.
    #[error("TOML parse error: {0}")]
    Toml(String),

    /// The formula declares zero steps.
    #[error("formula must have at least one step")]
    NoSteps,

    /// Two or more steps share the same identifier.
    #[error("duplicate step id: {0}")]
    DuplicateStepId(String),

    /// A step's `needs` list references a step that does not exist.
    #[error("step \"{step}\" depends on unknown step \"{dependency}\"")]
    UnknownDependency {
        /// The step that declared the dependency.
        step: String,
        /// The missing dependency target.
        dependency: String,
    },

    /// The dependency graph contains a cycle.
    #[error("circular dependency detected involving step \"{0}\"")]
    CircularDependency(String),

    /// The formula name failed ID validation.
    #[error("invalid formula name: {0}")]
    InvalidName(String),

    /// A step declares both `command` and `native` (mutually exclusive).
    #[error("step \"{0}\" declares both `command` and `native` — choose one")]
    CommandAndNative(String),

    /// A step declares more than one of `command` / `native` / `[steps.query]`
    /// / `[steps.llm]`. The four execution kinds are mutually exclusive — a
    /// step is either a Claude worker (none set), a shell gate (`command`),
    /// a native function (`native`), a query over the event store (`query`),
    /// or a checkpointed LLM call (`llm`).
    #[error("step \"{step}\" declares multiple execution kinds: {kinds} — choose exactly one")]
    MultipleStepKinds {
        /// The offending step id.
        step: String,
        /// Comma-joined list of kinds that were declared simultaneously.
        kinds: String,
    },

    /// A `[steps.query]` table is present but missing a required field.
    #[error("step \"{step}\" declares `[steps.query]` but is missing required field `{field}`")]
    QueryMissingField {
        /// The offending step id.
        step: String,
        /// The missing field name (one of: `expr`, `source`, `output_var`).
        field: &'static str,
    },

    /// A `[steps.query] source` value did not match the supported scheme.
    /// Recognised forms today: `molecule:<id>`, `molecule:current`,
    /// `state` (alias for `molecule:current`), `prompt`, `briefing`,
    /// `events`. The grammar is intentionally tiny — extensions land via
    /// new schemes, not free-form strings.
    #[error("step \"{step}\" declares `[steps.query] source = \"{raw_source}\"`: unrecognised scheme; expected `molecule:<id>`, `molecule:current`, `state`, `prompt`, `briefing`, or `events`")]
    QueryUnknownSource {
        /// The offending step id.
        step: String,
        /// The raw `source` string as written in the formula.
        raw_source: String,
    },

    /// A `[steps.llm]` table is present but missing a required field.
    #[error("step \"{step}\" declares `[steps.llm]` but is missing required field `{field}`")]
    LlmMissingField {
        /// The offending step id.
        step: String,
        /// The missing field name.
        field: &'static str,
    },

    /// A `[steps.llm]` table declared both `prompt` and `prompt_file`.
    #[error("step \"{0}\" declares both `[steps.llm] prompt` and `[steps.llm] prompt_file` — choose one")]
    LlmPromptAmbiguous(String),

    /// A step's `parallel_limit` declares a zero `max` — meaningless, likely
    /// a typo. Use `1` for serial execution or omit the field for unbounded.
    #[error("step \"{0}\" declares parallel_limit.max = 0; use 1 for serial or omit the field")]
    ParallelLimitZero(String),

    /// A step's `parallel_limit.mode` is not one of the recognized values
    /// (`"static"`, `"smart"`). Unknown modes are rejected at parse time so
    /// a typo fails loudly instead of silently disabling the limit.
    #[error("step \"{step}\" declares parallel_limit.mode = \"{mode}\"; expected \"static\" or \"smart\"")]
    ParallelLimitUnknownMode {
        /// Offending step id.
        step: String,
        /// The unrecognized mode value.
        mode: String,
    },

    /// A step's `[steps.validation]` table declares an unrecognised mode.
    #[error("step \"{step}\" declares unknown validation mode \"{mode}\" (expected mtime|blake3|sha256|keyed_blake3)")]
    UnknownValidationMode {
        /// The step id that declared the invalid mode.
        step: String,
        /// The raw mode string as it appeared in the TOML.
        mode: String,
    },

    /// The declared formula tier violates its structural contract.
    #[error("tier violation: {0}")]
    Tier(#[from] TierError),
}

/// Errors raised by [`validate_tier`] when a formula breaks its declared
/// tier contract.
///
/// Tier levels encode the nucleation surface of a formula (see ADR-032):
/// Tier 0 is a leaf (no child molecules); Tier 1 nucleates but is
/// well-founded via a declared measure; Tier 2 is a general-recursion
/// formula gated by a trusted signature.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set will grow; external callers must keep a `_ =>` arm
pub enum TierError {
    /// Declared `[tier]` table is missing required fields.
    #[error("formula declares `[tier]` with an invalid level ({level}); expected 0, 1, or 2")]
    InvalidLevel {
        /// The out-of-range level value encountered.
        level: u8,
    },

    /// A Tier 0 formula has a step whose body mentions a nucleation verb.
    /// Lift the formula to Tier 1 (with a measure) or remove the nucleation.
    #[error(
        "Tier 0 formula step \"{step}\" contains a nucleation call; \
         declare `level = 1` with a measure or remove the call"
    )]
    Tier0Nucleates {
        /// Step id that violates Tier 0 purity.
        step: String,
    },

    /// A Tier 1 formula omits its required `measure` field.
    #[error(
        "Tier 1 formula is missing the `measure` field in `[tier]`; \
         declare a finite structural bound (e.g. `measure = \"count\"`)"
    )]
    Tier1MissingMeasure,

    /// A Tier 1 formula declares a measure that is not referenced anywhere
    /// in any step description (syntactic presence check).
    #[error(
        "Tier 1 formula declares measure \"{measure}\" but no step body \
         references it; document the guard/decrement in the step that \
         nucleates children"
    )]
    Tier1MeasureNotReferenced {
        /// The declared measure name.
        measure: String,
    },

    /// Tier 2 is not yet available: it depends on `cosmon-sign` which has
    /// not shipped.
    #[error(
        "Tier 2 support is pending `cosmon-sign`; this formula declares \
         `level = 2` and cannot be loaded until signing primitives land"
    )]
    Tier2Unsupported,

    /// Tier 2 declared but the signature manifest is missing or unreadable.
    #[error("Tier 2 formula's signature manifest `{path}` is missing or invalid")]
    Tier2SignatureInvalid {
        /// The signature path that failed validation.
        path: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// Raw TOML shapes (deserialization targets)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawFormula {
    formula: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    description: String,
    #[serde(default)]
    id_prefix: String,
    #[serde(rename = "type")]
    #[serde(default)]
    formula_type: Option<String>,
    #[serde(default)]
    freeze_on_last_step: bool,
    /// ADR-140 D5: declares the formula a pure function of its inputs.
    /// Absent defaults to `false` (every agentic formula today).
    #[serde(default)]
    deterministic: bool,
    #[serde(default)]
    steps: Vec<RawStep>,
    #[serde(default)]
    tier: Option<RawTier>,
    #[serde(default)]
    vars: HashMap<String, RawVar>,
    // Fields we parse but don't need in the domain model:
    #[serde(default)]
    inputs: HashMap<String, RawVar>,
    // Allow unknown fields (squash, prompts, output, legs, synthesis, etc.)
    #[serde(flatten)]
    _extra: HashMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct RawStep {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    acceptance: Option<String>,
    /// Optional list of artifacts the step MUST have produced under the
    /// molecule's canonical `molecule_dir` by the time it advances. TOML
    /// form: `acceptance_artifacts = ["synthesis.md", "responses/"]`. Threads
    /// into [`Step::expected_artifacts`] and is enforced as a HARD gate by
    /// `cs evolve` (see that field's doc). Default empty = no-op, so legacy
    /// steps are unaffected.
    #[serde(default)]
    acceptance_artifacts: Vec<String>,
    #[serde(default)]
    needs: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    verification: Option<RawVerificationSpec>,
    #[serde(default)]
    validation: Option<RawValidationSpec>,
    /// Shell command to execute instead of launching a Claude worker.
    #[serde(default)]
    command: Option<String>,
    /// Native Rust function path to execute instead of launching a Claude worker.
    ///
    /// The string is a registry key (e.g. `"cosmon::smoke::cargo_check"`).
    /// When present, `cs tackle` calls the registered function directly —
    /// no shell, no tmux, no worktree. Mutually exclusive with `command`.
    #[serde(default)]
    native: Option<String>,
    /// Timeout in seconds for shell command execution (default: 300).
    #[serde(default)]
    timeout: Option<u64>,
    /// Wall-clock budget for the step itself, in minutes. Honored by the
    /// `Stalled` health computation in `cs peek` / `cs patrol --nudge`:
    /// once the worker's `last_progress_at` exceeds this budget the row is
    /// classified as `Stalled` regardless of tmux activity. Absence falls
    /// back to the global default (30 minutes).
    #[serde(default)]
    timeout_minutes: Option<u32>,
    /// When `true`, a `cs nucleate` call made while this step is running
    /// (i.e. from a worker whose `COSMON_PARENT_MOL_ID` points at the
    /// parent molecule) MUST carry an explicit `--blocks` or
    /// `--blocked-by` edge. Protects against the b22c failure mode where
    /// children nucleated from a decomposition step were linked only by
    /// `DecayedFrom` (an information edge) and remained invisible to
    /// `cs deps --transitive`.
    #[serde(default)]
    requires_parent_link: bool,
    /// Optional concurrency cap for this step (see [`ParallelLimit`]).
    /// Absence = unbounded (the default; preserves pre-ADR-043 behavior).
    #[serde(default)]
    parallel_limit: Option<RawParallelLimit>,
    /// Optional `[steps.query]` table — a typed query over the event store
    /// or molecule state. Mutually exclusive with `command`, `native`, and
    /// `llm`. See [`QuerySpec`].
    #[serde(default)]
    query: Option<RawQuerySpec>,
    /// Optional `[steps.llm]` table — a checkpointed LLM call. Mutually
    /// exclusive with `command`, `native`, and `query`. See [`LlmSpec`].
    #[serde(default)]
    llm: Option<RawLlmSpec>,
    /// Optional per-step Worker-Spawn Port Adapter override. TOML form:
    /// `adapter = "claude"`. Threads into [`Step::adapter`] and ranks below
    /// `--adapter` but above `[adapters.default]` in
    /// `resolve_adapter_selection`.
    #[serde(default)]
    adapter: Option<String>,
    /// Optional per-step model pin. TOML form: `model = "claude-fable-5"`.
    /// Threads into [`Step::model`] and ranks below `--model` but above
    /// every default in `resolve_model_selection` (delib-20260704-b476 C1).
    #[serde(default)]
    model: Option<String>,
    #[serde(flatten)]
    _extra: HashMap<String, toml::Value>,
}

/// Raw TOML shape for `[steps.query]`.
///
/// All three fields are required. `source` is parsed into [`QuerySource`]
/// at validation time; `expr` is a tiny dot-path (e.g. `.variables.versions`)
/// evaluated by the runtime against the resolved JSON document.
#[derive(Deserialize, Debug, Clone)]
struct RawQuerySpec {
    expr: String,
    source: String,
    output_var: String,
}

/// Raw TOML shape for `[steps.llm]`.
///
/// `prompt` and `prompt_file` are mutually exclusive; one is required.
/// Defaults are conservative — a 30 s checkpoint cadence, 120 s per-checkpoint
/// timeout, 30 minutes total budget — designed for streamed reasoning rather
/// than a one-shot completion.
#[derive(Deserialize, Debug, Clone)]
struct RawLlmSpec {
    provider: String,
    model: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    prompt_file: Option<PathBuf>,
    output_path: PathBuf,
    #[serde(default = "default_checkpoint_every")]
    checkpoint_every: u64,
    #[serde(default = "default_timeout_per_checkpoint")]
    timeout_per_checkpoint: u64,
    #[serde(default = "default_max_total_minutes")]
    max_total_minutes: u64,
    #[serde(default = "default_llm_max_retries")]
    max_retries: u32,
}

fn default_checkpoint_every() -> u64 {
    30
}

fn default_timeout_per_checkpoint() -> u64 {
    120
}

fn default_max_total_minutes() -> u64 {
    30
}

fn default_llm_max_retries() -> u32 {
    3
}

/// Raw TOML shape for a `parallel_limit` table on a step.
///
/// TOML form: `parallel_limit = { max = 8, mode = "static" }`. The `mode`
/// field is opt-in — absence defaults to `"static"`. Future modes (e.g.
/// `"smart"`) are accepted syntactically so forward-compat formulas parse
/// on older binaries, but only `"static"` is enforced by the runtime today
/// (see ADR-044 for the smart-limit roadmap).
#[derive(Deserialize, Debug, Clone)]
struct RawParallelLimit {
    /// Maximum concurrent molecules permitted at this step.
    max: u32,
    /// Selector for the limit policy. Omitted = `"static"`.
    #[serde(default)]
    mode: Option<String>,
    /// Smart-mode policy name (ignored in `"static"` mode).
    #[serde(default)]
    policy: Option<String>,
}

#[derive(Deserialize)]
struct RawVerificationSpec {
    criteria: String,
    #[serde(default = "default_max_retries")]
    max_retries: u32,
}

fn default_max_retries() -> u32 {
    3
}

/// Raw TOML shape for `[steps.validation]` (ADR-043).
#[derive(Deserialize)]
struct RawValidationSpec {
    mode: String,
}

#[derive(Deserialize, Default)]
struct RawTier {
    level: u8,
    #[serde(default)]
    measure: Option<String>,
    #[serde(default)]
    signature: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct RawVar {
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    var_type: Option<String>,
    #[serde(default)]
    required: Option<bool>,
    #[serde(default)]
    default: Option<String>,
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Formula tier — the declared nucleation contract.
///
/// See ADR-032 and `idea-20260413-62f5`. Tiers stratify formulas by their
/// ability to create child molecules, which is what the verifier layer
/// needs for a decidable completion predicate.
///
/// - [`Tier::Zero`] — leaf formula, no nucleation.
/// - [`Tier::One`] — nucleates children, but well-founded via a declared
///   structural measure (e.g. a finite fan-out `count`).
/// - [`Tier::Two`] — general-recursion formula gated by a trusted
///   signature. Unavailable until `cosmon-sign` lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// Leaf formula — contains no nucleation step.
    Zero,
    /// Nucleating formula with a declared structural measure.
    One {
        /// Name of the measure (e.g. `"count"`, `"panelists"`).
        measure: String,
    },
    /// General-recursion formula gated by a cryptographic signature.
    Two {
        /// Path to the signature manifest (resolved relative to the formula).
        signature: PathBuf,
    },
}

impl Tier {
    /// Numeric level (0, 1, or 2).
    #[must_use]
    pub fn level(&self) -> u8 {
        match self {
            Tier::Zero => 0,
            Tier::One { .. } => 1,
            Tier::Two { .. } => 2,
        }
    }

    /// Short badge label suitable for CLI rendering (`T0`, `T1`, `T2`).
    #[must_use]
    pub fn badge(&self) -> &'static str {
        match self {
            Tier::Zero => "T0",
            Tier::One { .. } => "T1",
            Tier::Two { .. } => "T2",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tier::Zero => f.write_str("Tier 0"),
            Tier::One { measure } => write!(f, "Tier 1 (measure={measure})"),
            Tier::Two { signature } => write!(f, "Tier 2 (signature={})", signature.display()),
        }
    }
}

/// A workflow template parsed from a `.formula.toml` file.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Formula {
    /// Formula name (e.g. `"mol-polecat-work"`).
    pub name: FormulaId,
    /// Schema version.
    pub version: u32,
    /// Human-readable description.
    pub description: String,
    /// Optional ID prefix for generated beads.
    pub id_prefix: String,
    /// Optional formula type (e.g. `"workflow"`, `"convoy"`).
    pub kind: Option<String>,
    /// Ordered steps (topologically sorted by `depends_on`).
    pub steps: Vec<Step>,
    /// Declared variables.
    pub variables: HashMap<String, Variable>,
    /// When `true`, the runtime transitions a molecule to `Frozen` after it
    /// completes the last step instead of leaving it in `Completed`.
    ///
    /// Defaults to `false` so existing formulas are unaffected.
    pub freeze_on_last_step: bool,
    /// Declared nucleation tier (see [`Tier`] and ADR-032).
    ///
    /// Absence of `[tier]` in the TOML source defaults to [`Tier::Zero`].
    /// The tier is validated at parse time; a Tier 0 formula that contains
    /// a nucleation call fails to load.
    pub tier: Tier,
    /// ADR-140 D5 — the `deterministic` trait.
    ///
    /// When `true`, the formula declares itself a **pure function of its
    /// inputs**: the same resolved variables plus the same upstream input
    /// artifacts yield the same output bytes (a build, a schema regen, a
    /// deterministic transform). Such a molecule is **cachable by content**
    /// (see [`crate::det_cache`]) and its
    /// `verify_requires_execution` bit
    /// is `false`.
    ///
    /// When `false` (the default, every agentic formula today), the
    /// molecule's work is an LLM session that is not byte-reproducible: it is
    /// sealed and re-executed, never content-skipped.
    ///
    /// Absent in the TOML source defaults to `false`, so existing formulas
    /// are unaffected.
    pub deterministic: bool,
}

/// A single step within a formula.
#[derive(Debug, Clone)]
pub struct Step {
    /// Unique step identifier within the formula.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Markdown description with instructions.
    pub description: String,
    /// Exit criteria / acceptance criteria.
    pub exit_criteria: Option<String>,
    /// Artifacts the step MUST have produced under the molecule's canonical
    /// `molecule_dir` for the advance to succeed. Paths/filenames are
    /// relative to that directory; a trailing `/` (e.g. `"responses/"`)
    /// marks a directory that must exist *and be non-empty*.
    ///
    /// **This is a HARD gate** (the gate-step family — `Step::command` /
    /// `Step::native` that FAIL on non-zero exit), *not* the defensive
    /// briefing-seal family. The seal model (CLAUDE.md §8b) deliberately
    /// *proposes* verification — any seal failure is logged and swallowed
    /// and never blocks the hot path, because a seal guards against
    /// *retrospective tampering* that a later audit can still catch. The
    /// artifact-presence guard blocks instead, because a **missing
    /// acceptance artifact is a contemporaneous, irreversible
    /// data-loss-before-`cs done`-teardown**: the misplaced or absent file
    /// is gone the moment the worktree is torn down. Blocking the advance
    /// *is* the recovery window — it keeps the molecule on its current step
    /// (no state mutation) so the worker can move the misplaced copy and
    /// re-run `cs evolve`. A reviewer should therefore read this as a gate
    /// step, not a §8b breach.
    ///
    /// Default empty = no-op; legacy steps that omit `acceptance_artifacts`
    /// are unaffected. The check is read-only and idempotent. Enforced in
    /// `cs evolve` (`crates/cosmon-cli/src/cmd/evolve.rs`).
    pub expected_artifacts: Vec<String>,
    /// IDs of steps that must complete before this one.
    pub depends_on: Vec<String>,
    /// Skills required or invoked by this step.
    pub skills: Vec<String>,
    /// Position in topological order (0-based).
    pub order: usize,
    /// Content-validation mode for this step (ADR-043).
    ///
    /// Controls how the step's inputs are hashed for memoization and drift
    /// detection. `None` means the formula did not declare a mode and the
    /// caller should fall back to the project default (typically
    /// [`cosmon_hash::ValidationMode::MTime`]).
    pub validation_mode: Option<cosmon_hash::ValidationMode>,
    /// Optional structured verification configuration for automated retry loops.
    pub verification: Option<VerificationSpec>,
    /// Shell command to execute instead of launching a Claude worker.
    ///
    /// When present, `cs tackle` runs this command via `sh -c` and uses the
    /// exit code to determine success (0) or failure (non-zero). The step
    /// bypasses `TransportBackend` entirely — no tmux session, no worktree
    /// session. This is the "gate" execution path.
    pub command: Option<String>,
    /// Native Rust function path to execute instead of launching a Claude worker.
    ///
    /// The string is a registry key (e.g. `"cosmon::smoke::cargo_check"`).
    /// When present, `cs tackle` calls the registered function directly —
    /// no shell, no tmux, no worktree. Sub-millisecond dispatch overhead.
    /// Mutually exclusive with `command`.
    pub native: Option<String>,
    /// Timeout in seconds for gate/native step execution.
    ///
    /// Only meaningful when `command` or `native`
    /// is `Some`. Defaults to 300 seconds.
    pub timeout: Option<u64>,
    /// Wall-clock budget for the *worker* progressing through this step,
    /// expressed in minutes. Consumed by the stall-detection layer
    /// (`cs peek` heartbeat, `cs patrol --nudge`) to classify a row as
    /// `MoleculeHealth::Stalled` once `now - last_progress_at` exceeds
    /// this budget. Absence means "use the project default" (30 minutes).
    /// Distinct from `timeout`, which only applies to
    /// gate/native steps.
    pub timeout_minutes: Option<u32>,
    /// When `true`, child molecules nucleated while this step is active
    /// must be linked back to the parent with an explicit `--blocks` or
    /// `--blocked-by` edge. Enforced at the CLI boundary by `cs nucleate`
    /// via the `COSMON_PARENT_MOL_ID` contract.
    pub requires_parent_link: bool,
    /// Optional concurrency cap for this step (ADR-043).
    ///
    /// Absence means unbounded — the pre-ADR-043 default behavior. When
    /// present, the resident runtime (`cs run`) caps the number of
    /// molecules simultaneously `Running` at this `(formula, step_order)`
    /// coordinate. See [`ParallelLimit`].
    pub parallel_limit: Option<ParallelLimit>,
    /// Optional structured query over the event store or molecule state.
    ///
    /// When present, the step replaces what would historically have been a
    /// `command = "cs --json observe … | jq …"` shell-out. The runtime
    /// resolves [`QuerySpec::source`], evaluates [`QuerySpec::expr`] over
    /// the JSON document, and binds the result into the molecule's variable
    /// map under [`QuerySpec::output_var`]. Mutually exclusive with
    /// `command`, `native`, and
    /// `llm`.
    pub query: Option<QuerySpec>,
    /// Optional checkpointed LLM call.
    ///
    /// When present, the step streams a completion from a registered
    /// provider into [`LlmSpec::output_path`], persisting partial output
    /// to disk on a checkpoint cadence. On per-checkpoint timeout, the
    /// runtime emits an `ExternalChannelTimeout` event and retries from
    /// the last checkpoint up to [`LlmSpec::max_retries`] times before
    /// failing the step. Mutually exclusive with `command`,
    /// `native`, and `query`.
    pub llm: Option<LlmSpec>,
    /// Worker-Spawn Port Adapter this step pins, overriding the galaxy
    /// default.
    ///
    /// A workflow step may legitimately demand a *specific* adapter
    /// regardless of `[adapters.default]` — e.g. a `deep-think` panel step
    /// pins `adapter = "claude"` because it needs frontier reasoning even
    /// in a galaxy whose default is the local Ollama-backed loop. This is
    /// the per-workflow **override** in the four-level resolution chain:
    /// it ranks *above* `[adapters.default]` (config policy) and the
    /// built-in `"local"` floor, but *below* an explicit `--adapter` flag
    /// (the operator's in-the-moment choice always wins).
    ///
    /// Only meaningful for worker-spawn steps. Gate / native / query / llm
    /// steps bypass the Adapter seam entirely, so a value here is inert on
    /// those kinds (no error — formulas evolve, and a step may change kind).
    pub adapter: Option<String>,
    /// Model this step pins, overriding every default (delib-20260704-b476 C1).
    ///
    /// A workflow step may legitimately demand a *specific* model — e.g. a
    /// `deep-think` panel step pins `model = "claude-fable-5"` because it
    /// needs frontier reasoning. This is the per-workflow **override** in
    /// the model resolution chain: it ranks *above* `$COSMON_DEFAULT_MODEL`
    /// and the config `default_model`, but *below* an explicit `--model`
    /// flag (the operator's in-the-moment choice always wins). The pin is
    /// carried opaquely — cosmon does not validate that the id is legal for
    /// the resolved adapter; the backend rejects an invalid pair at launch
    /// (composition validation is C5).
    ///
    /// Only meaningful for worker-spawn steps, and does **not** propagate
    /// across nucleation: a child molecule resolves from its own formula,
    /// never inheriting a parent step's pin (C4 Ghost D).
    pub model: Option<String>,
}

/// Where a [`QuerySpec`] resolves its JSON document from.
///
/// The grammar is intentionally tiny so the surface is auditable. A
/// `source = "molecule:<id>"` reads the named molecule's `state.json`;
/// `state` and `molecule:current` read the *currently executing*
/// molecule's `state.json` (the one tackling the formula). `prompt` and
/// `briefing` read the molecule's `prompt.md` / `briefing.md` parsed as
/// JSON when possible (e.g. front-matter), and `events` reads the
/// molecule's `events.jsonl` as an array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuerySource {
    /// Read the currently executing molecule's `state.json`. TOML form:
    /// `source = "state"` or `source = "molecule:current"`.
    CurrentMoleculeState,
    /// Read another molecule's `state.json`. TOML form: `source =
    /// "molecule:<id>"`.
    MoleculeState(MoleculeId),
    /// Read the currently executing molecule's `prompt.md`. TOML form:
    /// `source = "prompt"`.
    Prompt,
    /// Read the currently executing molecule's `briefing.md`. TOML form:
    /// `source = "briefing"`.
    Briefing,
    /// Read the currently executing molecule's `events.jsonl` as an array.
    /// TOML form: `source = "events"`.
    Events,
}

/// A typed query step.
///
/// Replaces shell-outs of the form `cs --json observe ${id} | jq …` with a
/// Rust-evaluated query whose error mode is a typed event, not a swallowed
/// pipe failure. The `expr` is a small dot-path subset of `JSONPath` — see
/// the `dotpath` module in `cosmon-cli` for the supported grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySpec {
    /// Dot-path expression to evaluate (e.g. `.variables.versions`,
    /// `.id`, `.steps[0].name`).
    pub expr: String,
    /// Where the JSON document is loaded from.
    pub source: QuerySource,
    /// Variable name to bind the evaluation result into. The runtime writes
    /// it back to the molecule's `variables` map (and serialises arrays /
    /// objects as JSON strings — variables are typed `String` today).
    pub output_var: String,
}

/// A checkpointed LLM call.
///
/// The runtime streams the response into `output_path`, flushing every
/// `checkpoint_every` seconds. A per-checkpoint timeout
/// (`timeout_per_checkpoint`) bounds silence on the wire; an aggregate
/// `max_total_minutes` caps total wall time. Failure modes (timeout,
/// retry, total budget exhausted) emit `ExternalChannelTimeout` events
/// instead of being swallowed by an opaque `curl`-style failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSpec {
    /// Provider key (e.g. `"anthropic"`, `"mock"`). Resolved against the
    /// CLI's provider registry at execution time.
    pub provider: String,
    /// Model identifier passed verbatim to the provider.
    pub model: String,
    /// Inline prompt text. Mutually exclusive with `prompt_file`.
    pub prompt: Option<String>,
    /// Path (relative to the molecule directory) to a file containing the
    /// prompt. Mutually exclusive with `prompt`.
    pub prompt_file: Option<PathBuf>,
    /// Path (relative to the molecule directory) where the streamed
    /// completion is written. Already-emitted bytes are flushed on every
    /// checkpoint so a crash leaves a partial-but-valid file.
    pub output_path: PathBuf,
    /// Checkpoint cadence in seconds. Smaller values mean more frequent
    /// flushes (lower data-loss window) at the cost of more I/O.
    pub checkpoint_every_secs: u64,
    /// Wall-clock timeout per checkpoint, in seconds. If no new tokens are
    /// observed within this window the runtime aborts the current attempt
    /// and may retry from the last checkpoint.
    pub timeout_per_checkpoint_secs: u64,
    /// Hard ceiling on total wall time, in minutes. Once exceeded the step
    /// fails irrespective of progress.
    pub max_total_minutes: u64,
    /// Maximum number of retry attempts before the step is collapsed.
    /// Each retry resumes from the last checkpoint (the on-disk prefix).
    pub max_retries: u32,
}

impl Step {
    /// Whether this step is a shell gate (has a command to execute).
    #[must_use]
    pub fn is_gate(&self) -> bool {
        self.command.is_some()
    }

    /// Whether this step is a native gate (has a Rust function to call).
    #[must_use]
    pub fn is_native(&self) -> bool {
        self.native.is_some()
    }

    /// Whether this step bypasses the Claude worker (shell or native gate).
    #[must_use]
    pub fn is_automated(&self) -> bool {
        self.is_gate() || self.is_native() || self.is_query() || self.is_llm()
    }

    /// Whether this step is a typed query (has a `[steps.query]` block).
    #[must_use]
    pub fn is_query(&self) -> bool {
        self.query.is_some()
    }

    /// Whether this step is a checkpointed LLM call (has `[steps.llm]`).
    #[must_use]
    pub fn is_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// Effective timeout for a gate/native step, in seconds.
    ///
    /// Returns 300 (5 minutes) when no explicit timeout is set.
    #[must_use]
    pub fn gate_timeout_secs(&self) -> u64 {
        self.timeout.unwrap_or(300)
    }

    /// Effective stall budget for the *worker* running this step, in minutes.
    ///
    /// Used by `cs peek` and `cs patrol --nudge`. Falls
    /// back to 30 minutes when the formula step does not declare an explicit
    /// `timeout_minutes`.
    #[must_use]
    pub fn stall_timeout_minutes(&self) -> u32 {
        self.timeout_minutes.unwrap_or(30)
    }
}

/// Concurrency limit declared on a formula step (ADR-043).
///
/// Opt-in: absence of `parallel_limit` on a step means **unbounded**
/// concurrency (the historical default). Declaring a limit instructs the
/// resident runtime / DAG policy to cap the number of molecules
/// simultaneously `Running` at that `(formula, step_order)` coordinate.
///
/// Two variants are defined:
///
/// - [`ParallelLimit::Static`] — a fixed integer cap enforced now. Corresponds
///   to `parallel_limit = { max = N }` (or `{ max = N, mode = "static" }`).
/// - [`ParallelLimit::Smart`] — placeholder for future observation-driven
///   limits (energy, entropy, backlog pressure). Currently **parsed but not
///   enforced** — see ADR-044 for the roadmap. The runtime treats `Smart`
///   as unbounded until the policy surface lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParallelLimit {
    /// Fixed concurrency cap. Enforced by the resident runtime.
    Static {
        /// Maximum concurrent molecules at this step (must be ≥ 1).
        max: u32,
    },
    /// Observation-driven concurrency cap — forward-looking placeholder.
    /// Parsed for forward compatibility but not yet enforced (ADR-044).
    Smart {
        /// Policy name (e.g. `"energy-aware"`, `"entropy-backoff"`).
        policy: String,
    },
}

impl ParallelLimit {
    /// Return the static cap if this is a [`ParallelLimit::Static`] variant.
    /// `Smart` variants return `None` (no enforcement today).
    #[must_use]
    pub fn static_max(&self) -> Option<u32> {
        match self {
            ParallelLimit::Static { max } => Some(*max),
            ParallelLimit::Smart { .. } => None,
        }
    }
}

/// Structured verification specification for a formula step.
///
/// When present on a [`Step`], this enables automated verification loops:
/// the orchestrator can re-check `criteria` up to `max_retries` times before
/// marking the step as failed. Without this, steps rely on free-text
/// `exit_criteria` interpreted by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationSpec {
    /// Machine-evaluable criteria (e.g. `"cargo test --workspace"`).
    pub criteria: String,
    /// Maximum number of retry attempts before the step fails.
    pub max_retries: u32,
}

/// A declared variable in a formula.
#[derive(Debug, Clone)]
pub struct Variable {
    /// Human-readable description.
    pub description: Option<String>,
    /// Type hint (e.g. `"string"`).
    pub var_type: Option<String>,
    /// Whether the variable must be provided.
    pub required: bool,
    /// Default value if not provided.
    pub default: Option<String>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

impl Formula {
    /// Parse a formula from TOML text.
    ///
    /// # Errors
    /// Returns [`FormulaError`] if the TOML is malformed, steps are invalid,
    /// dependencies reference unknown steps, or a cycle exists.
    #[allow(clippy::too_many_lines)]
    pub fn parse(toml_text: &str) -> Result<Self, FormulaError> {
        let raw: RawFormula =
            toml::from_str(toml_text).map_err(|e| FormulaError::Toml(e.to_string()))?;

        let name =
            FormulaId::new(&raw.formula).map_err(|e| FormulaError::InvalidName(e.to_string()))?;

        if raw.steps.is_empty() {
            return Err(FormulaError::NoSteps);
        }

        // Check for duplicate step IDs.
        let mut seen = HashSet::new();
        for step in &raw.steps {
            if !seen.insert(&step.id) {
                return Err(FormulaError::DuplicateStepId(step.id.clone()));
            }
        }

        // Check that command, native, query, and llm are mutually exclusive.
        // The legacy `CommandAndNative` error is preserved for the two-kind
        // case so existing callers / messages stay stable; multi-kind clashes
        // surface as `MultipleStepKinds` with a comma-joined inventory.
        for step in &raw.steps {
            let mut kinds = Vec::with_capacity(4);
            if step.command.is_some() {
                kinds.push("command");
            }
            if step.native.is_some() {
                kinds.push("native");
            }
            if step.query.is_some() {
                kinds.push("query");
            }
            if step.llm.is_some() {
                kinds.push("llm");
            }
            match kinds.len() {
                0 | 1 => {}
                2 if kinds == ["command", "native"] => {
                    return Err(FormulaError::CommandAndNative(step.id.clone()));
                }
                _ => {
                    return Err(FormulaError::MultipleStepKinds {
                        step: step.id.clone(),
                        kinds: kinds.join(", "),
                    });
                }
            }
        }

        // Check that all depends_on references exist.
        for step in &raw.steps {
            for dep in &step.needs {
                if !seen.contains(dep) {
                    return Err(FormulaError::UnknownDependency {
                        step: step.id.clone(),
                        dependency: dep.clone(),
                    });
                }
            }
        }

        // Topological sort (Kahn's algorithm) to detect cycles and assign order.
        let ordered = topological_sort(&raw.steps)?;

        // Build step order map.
        let order_map: HashMap<&str, usize> = ordered
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();

        let mut steps: Vec<Step> = raw
            .steps
            .into_iter()
            .map(|s| {
                let order = order_map[s.id.as_str()];
                let parallel_limit = build_parallel_limit(&s.id, s.parallel_limit)?;
                let validation_mode = s
                    .validation
                    .as_ref()
                    .map(|v| {
                        cosmon_hash::ValidationMode::parse(&v.mode).map_err(|_| {
                            FormulaError::UnknownValidationMode {
                                step: s.id.clone(),
                                mode: v.mode.clone(),
                            }
                        })
                    })
                    .transpose()?;
                let query = build_query_spec(&s.id, s.query)?;
                let llm = build_llm_spec(&s.id, s.llm)?;
                Ok::<_, FormulaError>(Step {
                    id: s.id,
                    title: s.title,
                    description: s.description,
                    exit_criteria: s.acceptance,
                    expected_artifacts: s.acceptance_artifacts,
                    depends_on: s.needs,
                    skills: s.skills,
                    order,
                    validation_mode,
                    verification: s.verification.map(|v| VerificationSpec {
                        criteria: v.criteria,
                        max_retries: v.max_retries,
                    }),
                    command: s.command,
                    native: s.native,
                    timeout: s.timeout,
                    timeout_minutes: s.timeout_minutes,
                    requires_parent_link: s.requires_parent_link,
                    parallel_limit,
                    query,
                    llm,
                    adapter: s.adapter,
                    model: s.model,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        steps.sort_by_key(|s| s.order);

        // Merge vars and inputs (design.formula.toml uses `[inputs]` instead of `[vars]`).
        let mut all_vars = raw.vars;
        for (k, v) in raw.inputs {
            all_vars.entry(k).or_insert(v);
        }

        let variables = all_vars
            .into_iter()
            .map(|(k, v)| {
                let var = Variable {
                    description: v.description,
                    var_type: v.var_type,
                    required: v.required.unwrap_or(false),
                    default: v.default,
                };
                (k, var)
            })
            .collect();

        let tier = build_tier(raw.tier)?;

        let formula = Formula {
            name,
            version: raw.version,
            description: raw.description,
            id_prefix: raw.id_prefix,
            kind: raw.formula_type,
            steps,
            variables,
            freeze_on_last_step: raw.freeze_on_last_step,
            tier,
            deterministic: raw.deterministic,
        };

        validate_tier(&formula)?;
        Ok(formula)
    }

    /// Whether `cs verify` must re-execute the molecule to certify it
    /// (ADR-140 D5).
    ///
    /// This is the inverse of `deterministic`: a
    /// deterministic formula is a pure function of its inputs, so its output
    /// is certifiable by a content hash and never needs re-execution; an
    /// agentic formula (`deterministic = false`) is an LLM session whose
    /// output is not byte-reproducible, so verification must re-run it.
    #[must_use]
    pub fn verify_requires_execution(&self) -> bool {
        !self.deterministic
    }

    /// Return the sorted list of *effectively-required* variables that the
    /// `provided` bindings leave missing or empty.
    ///
    /// A variable is *effectively required* when it is declared `required`
    /// **and** carries no `default` — a required variable with a default is
    /// always satisfiable without operator input, so it can never make a
    /// molecule briefless. For each effectively-required variable, the
    /// binding fails the check when it is either absent from `provided` or
    /// present but blank after trimming whitespace.
    ///
    /// This is the single source of truth for the "briefless molecule"
    /// question, consumed by two seams:
    ///
    /// - **nucleation** ([`crate::nucleate::nucleate`]) rejects an empty
    ///   required variable at birth (`--var topic=""` fails fast).
    /// - **dispatch** (`cs tackle`'s guard) refuses to spawn a worker for a
    ///   molecule whose required brief content was lost — e.g. after a
    ///   `cs reconcile` cleared `state.json` variables, the observed
    ///   pathology where empty-topic `task-work` molecules were dispatched
    ///   with no operator intent. Corollary of the frontier stuck-frozen
    ///   fix (task-20260711-9b86): the DAG says "ready", but a ready
    ///   molecule with no brief must still not be dispatched.
    ///
    /// An empty return value means every effectively-required variable is
    /// present and non-blank — the molecule carries a brief.
    #[must_use]
    pub fn missing_required_vars(
        &self,
        provided: &std::collections::HashMap<String, String>,
    ) -> Vec<String> {
        let mut missing: Vec<String> = self
            .variables
            .iter()
            .filter(|(_, var)| var.required && var.default.is_none())
            .filter(|(key, _)| provided.get(*key).is_none_or(|val| val.trim().is_empty()))
            .map(|(key, _)| key.clone())
            .collect();
        missing.sort();
        missing
    }
}

/// Translate the raw TOML `parallel_limit` table into a [`ParallelLimit`].
///
/// `step_id` is the owning step's id, used for error messages. Mode default
/// is `"static"`; `"smart"` requires a `policy` string (caller-supplied
/// selector — enforcement is deferred per ADR-044).
fn build_parallel_limit(
    step_id: &str,
    raw: Option<RawParallelLimit>,
) -> Result<Option<ParallelLimit>, FormulaError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.max == 0 {
        return Err(FormulaError::ParallelLimitZero(step_id.to_owned()));
    }
    let mode = raw.mode.as_deref().unwrap_or("static");
    match mode {
        "static" => Ok(Some(ParallelLimit::Static { max: raw.max })),
        "smart" => Ok(Some(ParallelLimit::Smart {
            policy: raw.policy.unwrap_or_default(),
        })),
        other => Err(FormulaError::ParallelLimitUnknownMode {
            step: step_id.to_owned(),
            mode: other.to_owned(),
        }),
    }
}

/// Translate the raw TOML `[steps.query]` table into a [`QuerySpec`].
///
/// Validates that all three required fields (`expr`, `source`, `output_var`)
/// are present and non-empty, and that `source` matches one of the known
/// schemes (`molecule:<id>`, `molecule:current`, `state`, `prompt`,
/// `briefing`, `events`). Empty fields surface as
/// [`FormulaError::QueryMissingField`] so the operator gets a precise
/// remediation hint rather than a downstream evaluator error.
fn build_query_spec(
    step_id: &str,
    raw: Option<RawQuerySpec>,
) -> Result<Option<QuerySpec>, FormulaError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.expr.trim().is_empty() {
        return Err(FormulaError::QueryMissingField {
            step: step_id.to_owned(),
            field: "expr",
        });
    }
    if raw.source.trim().is_empty() {
        return Err(FormulaError::QueryMissingField {
            step: step_id.to_owned(),
            field: "source",
        });
    }
    if raw.output_var.trim().is_empty() {
        return Err(FormulaError::QueryMissingField {
            step: step_id.to_owned(),
            field: "output_var",
        });
    }
    let source = parse_query_source(step_id, &raw.source)?;
    Ok(Some(QuerySpec {
        expr: raw.expr,
        source,
        output_var: raw.output_var,
    }))
}

/// Translate the raw TOML `[steps.llm]` table into an [`LlmSpec`].
///
/// Enforces the prompt source contract: exactly one of `prompt` / `prompt_file`
/// must be set. Other fields fall back to conservative defaults (30 s
/// checkpoint, 120 s per-checkpoint timeout, 30 min total budget, 3 retries).
fn build_llm_spec(step_id: &str, raw: Option<RawLlmSpec>) -> Result<Option<LlmSpec>, FormulaError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.provider.trim().is_empty() {
        return Err(FormulaError::LlmMissingField {
            step: step_id.to_owned(),
            field: "provider",
        });
    }
    if raw.model.trim().is_empty() {
        return Err(FormulaError::LlmMissingField {
            step: step_id.to_owned(),
            field: "model",
        });
    }
    match (raw.prompt.as_deref(), raw.prompt_file.as_ref()) {
        (None, None) => {
            return Err(FormulaError::LlmMissingField {
                step: step_id.to_owned(),
                field: "prompt",
            });
        }
        (Some(_), Some(_)) => {
            return Err(FormulaError::LlmPromptAmbiguous(step_id.to_owned()));
        }
        _ => {}
    }
    if raw.output_path.as_os_str().is_empty() {
        return Err(FormulaError::LlmMissingField {
            step: step_id.to_owned(),
            field: "output_path",
        });
    }
    Ok(Some(LlmSpec {
        provider: raw.provider,
        model: raw.model,
        prompt: raw.prompt,
        prompt_file: raw.prompt_file,
        output_path: raw.output_path,
        checkpoint_every_secs: raw.checkpoint_every,
        timeout_per_checkpoint_secs: raw.timeout_per_checkpoint,
        max_total_minutes: raw.max_total_minutes,
        max_retries: raw.max_retries,
    }))
}

/// Parse the `source` string of a `[steps.query]` block into a [`QuerySource`].
///
/// Recognised forms:
/// - `state` or `molecule:current` → [`QuerySource::CurrentMoleculeState`]
/// - `molecule:<id>` → [`QuerySource::MoleculeState`]
/// - `prompt` → [`QuerySource::Prompt`]
/// - `briefing` → [`QuerySource::Briefing`]
/// - `events` → [`QuerySource::Events`]
fn parse_query_source(step_id: &str, raw: &str) -> Result<QuerySource, FormulaError> {
    let trimmed = raw.trim();
    if trimmed == "state" || trimmed == "molecule:current" {
        return Ok(QuerySource::CurrentMoleculeState);
    }
    if trimmed == "prompt" {
        return Ok(QuerySource::Prompt);
    }
    if trimmed == "briefing" {
        return Ok(QuerySource::Briefing);
    }
    if trimmed == "events" {
        return Ok(QuerySource::Events);
    }
    if let Some(rest) = trimmed.strip_prefix("molecule:") {
        let id = MoleculeId::new(rest).map_err(|_| FormulaError::QueryUnknownSource {
            step: step_id.to_owned(),
            raw_source: raw.to_owned(),
        })?;
        return Ok(QuerySource::MoleculeState(id));
    }
    Err(FormulaError::QueryUnknownSource {
        step: step_id.to_owned(),
        raw_source: raw.to_owned(),
    })
}

/// Translate the raw TOML `[tier]` table into a [`Tier`] domain value.
///
/// Absent `[tier]` defaults to [`Tier::Zero`]. An out-of-range `level` is
/// rejected here — higher-level semantic checks live in [`validate_tier`].
fn build_tier(raw: Option<RawTier>) -> Result<Tier, TierError> {
    let Some(raw) = raw else {
        return Ok(Tier::Zero);
    };
    match raw.level {
        0 => Ok(Tier::Zero),
        1 => {
            let measure = raw.measure.ok_or(TierError::Tier1MissingMeasure)?;
            Ok(Tier::One { measure })
        }
        2 => {
            // Tier 2 requires a signature. Until cosmon-sign lands we reject
            // any Tier 2 declaration; once it ships the signature path will
            // be verified against the trusted key set.
            let Some(signature) = raw.signature else {
                return Err(TierError::Tier2Unsupported);
            };
            // Conservative: refuse to admit Tier 2 until signing verification
            // exists. We surface the declared path for operator feedback.
            Err(TierError::Tier2SignatureInvalid { path: signature })
        }
        other => Err(TierError::InvalidLevel { level: other }),
    }
}

/// Post-parse validation of a formula's tier contract (see [`Tier`]).
///
/// This is the parse-time enforcement of ADR-032:
/// - Tier 0 formulas must not contain a nucleation call in any step body.
/// - Tier 1 formulas must declare a `measure` *and* reference it in at
///   least one step description (syntactic presence check — it signals the
///   guard/decrement discipline without inspecting the step action AST,
///   which does not yet exist).
/// - Tier 2 is handled at construction time via `build_tier` and is
///   unreachable here until `cosmon-sign` ships.
///
/// # Errors
/// Returns the specific [`TierError`] that identifies the offending step
/// and a one-line remediation hint.
pub fn validate_tier(formula: &Formula) -> Result<(), TierError> {
    match &formula.tier {
        Tier::Zero => {
            for step in &formula.steps {
                if step_nucleates(step) {
                    return Err(TierError::Tier0Nucleates {
                        step: step.id.clone(),
                    });
                }
            }
            Ok(())
        }
        Tier::One { measure } => {
            if measure.trim().is_empty() {
                return Err(TierError::Tier1MissingMeasure);
            }
            let referenced = formula.steps.iter().any(|s| step_mentions(s, measure))
                || formula.description.contains(measure.as_str());
            if !referenced {
                return Err(TierError::Tier1MeasureNotReferenced {
                    measure: measure.clone(),
                });
            }
            Ok(())
        }
        Tier::Two { .. } => Err(TierError::Tier2Unsupported),
    }
}

/// Syntactic check: does this step's structured body perform a nucleation?
///
/// We inspect the `command` field (the shell gate action) only. Step
/// descriptions are human-readable prose and may legitimately mention
/// `cs nucleate` in documentation (e.g. `absorb`, `temp-review`). The
/// `command` field is the true structural action surface, so restricting
/// the substring check to it keeps the rule precise.
fn step_nucleates(step: &Step) -> bool {
    fn mentions_nucleate(s: &str) -> bool {
        // Match either the CLI verb or the MCP port name. Use whitespace-
        // aware match to reduce false positives on names like
        // `cs-nucleate-test`.
        s.contains("cs nucleate ") || s.contains("cs nucleate\n") || s.contains("cosmon_nucleate")
    }
    step.command.as_deref().is_some_and(mentions_nucleate)
}

/// Whether a step body (description, title, or exit criteria) syntactically
/// mentions the given measure name. Used to enforce Tier 1's syntactic
/// presence check (see [`validate_tier`]).
fn step_mentions(step: &Step, measure: &str) -> bool {
    step.description.contains(measure)
        || step.title.contains(measure)
        || step
            .exit_criteria
            .as_deref()
            .is_some_and(|s| s.contains(measure))
}

// ---------------------------------------------------------------------------
// Topological sort
// ---------------------------------------------------------------------------

/// Delegates to [`cosmon_graph::toposort`], mapping `RawStep` edges to
/// owned `String` pairs and converting `CycleError` to `FormulaError`.
fn topological_sort(steps: &[RawStep]) -> Result<Vec<String>, FormulaError> {
    // Build edge list from step dependency declarations.
    let edges: Vec<(String, String)> = steps
        .iter()
        .flat_map(|step| {
            step.needs
                .iter()
                .map(move |dep| (dep.clone(), step.id.clone()))
        })
        .collect();

    // cosmon_graph only sees nodes that appear in edges. Steps with no
    // edges (isolated nodes) must be appended separately.
    let mut result =
        cosmon_graph::toposort(&edges).map_err(|e| FormulaError::CircularDependency(e.0))?;

    // Any steps not in edges (isolated nodes) need to be included.
    let in_graph: HashSet<&str> = result.iter().map(String::as_str).collect();
    let mut isolated: Vec<String> = steps
        .iter()
        .filter(|s| !in_graph.contains(s.id.as_str()))
        .map(|s| s.id.clone())
        .collect();
    isolated.sort();
    result.extend(isolated);

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_formula() {
        let toml = r#"
formula = "minimal"
version = 1
description = "A minimal formula"

[[steps]]
id = "only-step"
title = "Do the thing"
description = "Just do it."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "minimal");
        assert_eq!(f.version, 1);
        assert_eq!(f.steps.len(), 1);
        assert_eq!(f.steps[0].id, "only-step");
        assert_eq!(f.steps[0].order, 0);
        assert!(f.steps[0].depends_on.is_empty());
    }

    #[test]
    fn test_parse_step_adapter_pin() {
        // Q5a (task-20260530-c089): a step may pin a specific adapter,
        // threaded into Step::adapter. Absence is None (the common case).
        let toml = r#"
formula = "panel-demo"
version = 1
description = "deep-think-style panel pinning a frontier adapter"

[[steps]]
id = "panel"
title = "Run the panel"
description = "Needs frontier reasoning regardless of galaxy default."
adapter = "claude"

[[steps]]
id = "synthesise"
title = "Synthesise"
description = "Local is fine here."
needs = ["panel"]
"#;
        let f = Formula::parse(toml).unwrap();
        let panel = f.steps.iter().find(|s| s.id == "panel").unwrap();
        assert_eq!(panel.adapter.as_deref(), Some("claude"));
        let synth = f.steps.iter().find(|s| s.id == "synthesise").unwrap();
        assert_eq!(
            synth.adapter, None,
            "no pin → None, falls to galaxy default"
        );
    }

    #[test]
    fn test_parse_step_model_pin() {
        // delib-20260704-b476 C1: a step may pin a specific model, threaded
        // into Step::model. Independent of the adapter pin — a step may pin
        // both, one, or neither. Absence is None (the common case).
        let toml = r#"
formula = "model-pin-demo"
version = 1
description = "a step pinning a frontier model"

[[steps]]
id = "panel"
title = "Run the panel"
description = "Needs frontier reasoning."
adapter = "claude"
model = "claude-fable-5"

[[steps]]
id = "synthesise"
title = "Synthesise"
description = "No model pin — resolves from the chain."
needs = ["panel"]
"#;
        let f = Formula::parse(toml).unwrap();
        let panel = f.steps.iter().find(|s| s.id == "panel").unwrap();
        assert_eq!(panel.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(panel.adapter.as_deref(), Some("claude"));
        let synth = f.steps.iter().find(|s| s.id == "synthesise").unwrap();
        assert_eq!(
            synth.model, None,
            "no pin → None, resolves from the model chain"
        );
    }

    #[test]
    fn test_parse_full_formula() {
        let toml = r#"
formula = "full-example"
version = 3
description = "A formula with multiple steps and dependencies"

[[steps]]
id = "setup"
title = "Setup"
description = "Initialize."

[[steps]]
id = "build"
title = "Build"
description = "Compile."
needs = ["setup"]
acceptance = "Code compiles"
skills = ["rust"]

[[steps]]
id = "test"
title = "Test"
description = "Run tests."
needs = ["build"]
acceptance = "All tests pass"

[[steps]]
id = "deploy"
title = "Deploy"
description = "Ship it."
needs = ["build", "test"]

[vars.target]
description = "Deploy target"
required = true

[vars.verbose]
description = "Enable verbose output"
type = "bool"
default = "false"
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "full-example");
        assert_eq!(f.version, 3);
        assert_eq!(f.steps.len(), 4);

        // Steps should be topologically ordered.
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        let setup_pos = ids.iter().position(|&id| id == "setup").unwrap();
        let build_pos = ids.iter().position(|&id| id == "build").unwrap();
        let test_pos = ids.iter().position(|&id| id == "test").unwrap();
        let deploy_pos = ids.iter().position(|&id| id == "deploy").unwrap();
        assert!(setup_pos < build_pos);
        assert!(build_pos < test_pos);
        assert!(test_pos < deploy_pos);

        // Check exit_criteria maps from acceptance.
        let build_step = f.steps.iter().find(|s| s.id == "build").unwrap();
        assert_eq!(build_step.exit_criteria.as_deref(), Some("Code compiles"));
        assert_eq!(build_step.skills, vec!["rust"]);

        // Check variables.
        assert_eq!(f.variables.len(), 2);
        let target = &f.variables["target"];
        assert!(target.required);
        assert_eq!(target.description.as_deref(), Some("Deploy target"));

        let verbose = &f.variables["verbose"];
        assert!(!verbose.required);
        assert_eq!(verbose.var_type.as_deref(), Some("bool"));
        assert_eq!(verbose.default.as_deref(), Some("false"));
    }

    #[test]
    fn test_acceptance_artifacts_parses_into_expected_artifacts() {
        // A step declaring `acceptance_artifacts` threads the list verbatim
        // into `Step::expected_artifacts`; a step omitting it defaults to an
        // empty Vec (legacy steps are unaffected — backward-compatible).
        let toml = r#"
formula = "artifact-gate"
version = 1
description = "exercise acceptance_artifacts parsing"

[[steps]]
id = "frame"
acceptance_artifacts = ["frame.md"]

[[steps]]
id = "dispatch"
needs = ["frame"]
acceptance_artifacts = ["responses/", "synthesis.md"]

[[steps]]
id = "legacy"
needs = ["dispatch"]
"#;
        let f = Formula::parse(toml).unwrap();
        let frame = f.steps.iter().find(|s| s.id == "frame").unwrap();
        assert_eq!(frame.expected_artifacts, vec!["frame.md".to_owned()]);

        let dispatch = f.steps.iter().find(|s| s.id == "dispatch").unwrap();
        assert_eq!(
            dispatch.expected_artifacts,
            vec!["responses/".to_owned(), "synthesis.md".to_owned()]
        );

        // Legacy step that omits the key → empty (no-op gate).
        let legacy = f.steps.iter().find(|s| s.id == "legacy").unwrap();
        assert!(legacy.expected_artifacts.is_empty());
    }

    #[test]
    fn test_parse_rejects_no_steps() {
        let toml = r#"
formula = "empty"
version = 1
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert_eq!(err, FormulaError::NoSteps);
    }

    #[test]
    fn test_parse_rejects_duplicate_step_ids() {
        let toml = r#"
formula = "dupes"
version = 1

[[steps]]
id = "step-a"
title = "First"
description = "First instance."

[[steps]]
id = "step-a"
title = "Second"
description = "Duplicate id."
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert_eq!(err, FormulaError::DuplicateStepId("step-a".to_owned()));
    }

    #[test]
    fn test_parse_rejects_circular_deps() {
        let toml = r#"
formula = "circular"
version = 1

[[steps]]
id = "a"
title = "A"
description = "Step A."
needs = ["b"]

[[steps]]
id = "b"
title = "B"
description = "Step B."
needs = ["a"]
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(err, FormulaError::CircularDependency(_)));
    }

    #[test]
    fn test_parse_rejects_missing_dep_reference() {
        let toml = r#"
formula = "bad-ref"
version = 1

[[steps]]
id = "step-a"
title = "A"
description = "Depends on ghost."
needs = ["nonexistent"]
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert_eq!(
            err,
            FormulaError::UnknownDependency {
                step: "step-a".to_owned(),
                dependency: "nonexistent".to_owned(),
            }
        );
    }

    #[test]
    fn test_parse_real_formula_mol_polecat_work() {
        let toml = include_str!("../tests/fixtures/mol-polecat-work.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "mol-polecat-work");
        assert!(!f.steps.is_empty());
        assert!(!f.variables.is_empty());
    }

    #[test]
    fn test_parse_real_formula_shiny() {
        let toml = include_str!("../tests/fixtures/shiny.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "shiny");
        assert!(!f.steps.is_empty());
    }

    #[test]
    fn test_parse_real_formula_towers_of_hanoi() {
        let toml = include_str!("../tests/fixtures/towers-of-hanoi.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "towers-of-hanoi");
        assert_eq!(f.steps.len(), 9); // setup + 7 moves + verify
    }

    #[test]
    fn test_parse_real_formula_convoy_cleanup() {
        let toml = include_str!("../tests/fixtures/mol-convoy-cleanup.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "mol-convoy-cleanup");
    }

    #[test]
    fn test_parse_real_formula_gastown_release() {
        let toml = include_str!("../tests/fixtures/gastown-release.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "gastown-release");
    }

    #[test]
    fn test_parse_real_formula_witness_patrol() {
        let toml = include_str!("../tests/fixtures/mol-witness-patrol.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "mol-witness-patrol");
    }

    #[test]
    fn test_parse_real_formula_operator_attention_patrol() {
        // delib-20260509-18df §D-B Mach proxy. The formula must
        // load cleanly so a future operator can `cs nucleate
        // operator-attention-patrol` without fighting parser drift.
        let toml = include_str!("../tests/fixtures/operator-attention-patrol.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "operator-attention-patrol");
        assert_eq!(f.steps.len(), 2);
        assert_eq!(f.steps[0].id.as_str(), "scan");
        assert_eq!(f.steps[1].id.as_str(), "report");
        // Step 2 depends on step 1 — the report cannot run before
        // the scan has produced stall-report.md.
        assert_eq!(f.steps[1].depends_on.len(), 1);
        assert_eq!(f.steps[1].depends_on[0].as_str(), "scan");
    }

    #[test]
    fn test_parse_real_formula_query_demo() {
        let toml = include_str!("../tests/fixtures/query-demo.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "query-demo");
        assert_eq!(f.steps.len(), 2);
        assert!(f.steps[0].is_query());
        assert!(f.steps[1].is_query());
        assert_eq!(
            f.steps[0].query.as_ref().unwrap().output_var.as_str(),
            "my_id"
        );
        assert_eq!(
            f.steps[1].query.as_ref().unwrap().output_var.as_str(),
            "my_topic"
        );
    }

    #[test]
    fn test_parse_real_formula_beads_release() {
        let toml = include_str!("../tests/fixtures/beads-release.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "beads-release");
    }

    #[test]
    fn test_topological_order_preserved() {
        let toml = r#"
formula = "order-test"
version = 1

[[steps]]
id = "c"
title = "C"
description = "."
needs = ["a", "b"]

[[steps]]
id = "a"
title = "A"
description = "."

[[steps]]
id = "b"
title = "B"
description = "."
needs = ["a"]
"#;
        let f = Formula::parse(toml).unwrap();
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_step_verification_spec_parsed() {
        let toml = r#"
formula = "verified"
version = 1

[[steps]]
id = "build"
title = "Build"
description = "Compile."

[steps.verification]
criteria = "cargo check --workspace"
max_retries = 5

[[steps]]
id = "test"
title = "Test"
description = "Run tests."
needs = ["build"]
"#;
        let f = Formula::parse(toml).unwrap();
        let build = f.steps.iter().find(|s| s.id == "build").unwrap();
        let spec = build.verification.as_ref().unwrap();
        assert_eq!(spec.criteria, "cargo check --workspace");
        assert_eq!(spec.max_retries, 5);

        let test = f.steps.iter().find(|s| s.id == "test").unwrap();
        assert!(test.verification.is_none());
    }

    #[test]
    fn test_step_validation_mode_parsed() {
        let toml = r#"
formula = "hash-modes"
version = 1

[[steps]]
id = "dev"
title = "Dev"
description = "."

[steps.validation]
mode = "mtime"

[[steps]]
id = "release"
title = "Release"
description = "."
needs = ["dev"]

[steps.validation]
mode = "blake3"

[[steps]]
id = "attest"
title = "Attest"
description = "."
needs = ["release"]

[steps.validation]
mode = "sha256"
"#;
        let f = Formula::parse(toml).unwrap();
        let dev = f.steps.iter().find(|s| s.id == "dev").unwrap();
        let release = f.steps.iter().find(|s| s.id == "release").unwrap();
        let attest = f.steps.iter().find(|s| s.id == "attest").unwrap();
        assert_eq!(
            dev.validation_mode,
            Some(cosmon_hash::ValidationMode::MTime)
        );
        assert_eq!(
            release.validation_mode,
            Some(cosmon_hash::ValidationMode::Blake3)
        );
        assert_eq!(
            attest.validation_mode,
            Some(cosmon_hash::ValidationMode::Sha256)
        );
    }

    #[test]
    fn test_step_validation_mode_default_none() {
        let toml = r#"
formula = "no-mode"
version = 1

[[steps]]
id = "s"
title = "S"
description = "."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.steps[0].validation_mode, None);
    }

    #[test]
    fn test_step_validation_mode_rejects_unknown() {
        let toml = r#"
formula = "bad-mode"
version = 1

[[steps]]
id = "s"
title = "S"
description = "."

[steps.validation]
mode = "md5"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::UnknownValidationMode { ref step, ref mode }
                if step == "s" && mode == "md5"
        ));
    }

    #[test]
    fn test_verification_spec_default_max_retries() {
        let toml = r#"
formula = "default-retries"
version = 1

[[steps]]
id = "lint"
title = "Lint"
description = "Run linter."

[steps.verification]
criteria = "cargo clippy"
"#;
        let f = Formula::parse(toml).unwrap();
        let spec = f.steps[0].verification.as_ref().unwrap();
        assert_eq!(spec.criteria, "cargo clippy");
        assert_eq!(spec.max_retries, 3);
    }

    #[test]
    fn test_freeze_on_last_step_defaults_false() {
        let toml = r#"
formula = "no-freeze"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "Do something."
"#;
        let f = Formula::parse(toml).unwrap();
        assert!(!f.freeze_on_last_step);
    }

    #[test]
    fn test_freeze_on_last_step_parsed_when_true() {
        let toml = r#"
formula = "freeze-me"
version = 1
freeze_on_last_step = true

[[steps]]
id = "work"
title = "Work"
description = "Do something."
"#;
        let f = Formula::parse(toml).unwrap();
        assert!(f.freeze_on_last_step);
    }

    #[test]
    fn test_step_command_and_timeout_parsed() {
        let toml = r#"
formula = "gate-test"
version = 1

[[steps]]
id = "check-dois"
title = "DOI verification gate"
command = "just doi-check wiki/*.md"
timeout = 60
acceptance = "All DOIs resolve correctly (exit code 0)"

[[steps]]
id = "work"
title = "Do the work"
description = "Agent step, no command."
needs = ["check-dois"]
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.steps.len(), 2);

        let gate = f.steps.iter().find(|s| s.id == "check-dois").unwrap();
        assert!(gate.is_gate());
        assert_eq!(gate.command.as_deref(), Some("just doi-check wiki/*.md"));
        assert_eq!(gate.timeout, Some(60));
        assert_eq!(gate.gate_timeout_secs(), 60);

        let work = f.steps.iter().find(|s| s.id == "work").unwrap();
        assert!(!work.is_gate());
        assert!(work.command.is_none());
        assert!(work.timeout.is_none());
        assert_eq!(work.gate_timeout_secs(), 300); // default
    }

    #[test]
    fn test_step_command_without_timeout_uses_default() {
        let toml = r#"
formula = "gate-default"
version = 1

[[steps]]
id = "lint"
title = "Lint gate"
command = "cargo clippy"
"#;
        let f = Formula::parse(toml).unwrap();
        let step = &f.steps[0];
        assert!(step.is_gate());
        assert_eq!(step.timeout, None);
        assert_eq!(step.gate_timeout_secs(), 300);
    }

    #[test]
    fn test_self_referencing_dep_is_circular() {
        let toml = r#"
formula = "self-ref"
version = 1

[[steps]]
id = "loop"
title = "Loop"
description = "."
needs = ["loop"]
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(err, FormulaError::CircularDependency(_)));
    }

    // -----------------------------------------------------------------------
    // Tier system tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tier_defaults_to_zero_when_absent() {
        let toml = r#"
formula = "leaf"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "Do the thing."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
        assert_eq!(f.tier.level(), 0);
        assert_eq!(f.tier.badge(), "T0");
    }

    #[test]
    fn test_tier_zero_explicit() {
        let toml = r#"
formula = "explicit-zero"
version = 1

[tier]
level = 0

[[steps]]
id = "work"
title = "Work"
description = "Leaf step."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_tier_one_parsed() {
        let toml = r#"
formula = "nucleating"
version = 1

[tier]
level = 1
measure = "count"

[[steps]]
id = "decompose"
title = "Decompose"
description = "Nucleate children; count bounded at plan time."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(
            f.tier,
            Tier::One {
                measure: "count".to_owned()
            }
        );
        assert_eq!(f.tier.level(), 1);
        assert_eq!(f.tier.badge(), "T1");
        assert_eq!(f.tier.to_string(), "Tier 1 (measure=count)");
    }

    #[test]
    fn test_tier_one_missing_measure_rejected() {
        let toml = r#"
formula = "no-measure"
version = 1

[tier]
level = 1

[[steps]]
id = "work"
title = "Work"
description = "Step."
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::Tier(TierError::Tier1MissingMeasure)
        ));
    }

    #[test]
    fn test_tier_one_measure_not_referenced_rejected() {
        let toml = r#"
formula = "orphan-measure"
version = 1

[tier]
level = 1
measure = "fanout"

[[steps]]
id = "work"
title = "Work"
description = "This step says nothing about the declared measure."
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::Tier(TierError::Tier1MeasureNotReferenced { .. })
        ));
    }

    #[test]
    fn test_tier_two_rejected_until_cosmon_sign() {
        let toml = r#"
formula = "signed"
version = 1

[tier]
level = 2
signature = "signed.manifest"

[[steps]]
id = "work"
title = "Work"
description = "Step."
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::Tier(TierError::Tier2SignatureInvalid { .. })
        ));
    }

    #[test]
    fn test_tier_two_no_signature_rejected() {
        let toml = r#"
formula = "unsigned"
version = 1

[tier]
level = 2

[[steps]]
id = "work"
title = "Work"
description = "Step."
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::Tier(TierError::Tier2Unsupported)
        ));
    }

    #[test]
    fn test_tier_invalid_level_rejected() {
        let toml = r#"
formula = "bad-level"
version = 1

[tier]
level = 5

[[steps]]
id = "work"
title = "Work"
description = "Step."
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::Tier(TierError::InvalidLevel { level: 5 })
        ));
    }

    #[test]
    fn test_tier_zero_rejects_nucleation_in_command() {
        let toml = r#"
formula = "sneaky-nucleate"
version = 1

[tier]
level = 0

[[steps]]
id = "spawn"
title = "Spawn"
command = "cs nucleate task-work --var topic=hidden"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::Tier(TierError::Tier0Nucleates { .. })
        ));
    }

    #[test]
    fn test_tier_zero_allows_nucleation_in_description() {
        // Descriptions are human-readable prose; mentioning cs nucleate
        // in a doc context is legitimate for Tier 0 (e.g. absorb, temp-review).
        let toml = r#"
formula = "doc-mention"
version = 1

[tier]
level = 0

[[steps]]
id = "verify"
title = "Verify"
description = "Run cs nucleate --dry-run to check TOML syntax."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_tier_one_measure_referenced_in_description_passes() {
        let toml = r#"
formula = "ref-in-desc"
version = 1

[tier]
level = 1
measure = "count"

[[steps]]
id = "decompose"
title = "Decompose"
description = "Nucleate a bounded count of children."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(
            f.tier,
            Tier::One {
                measure: "count".to_owned()
            }
        );
    }

    #[test]
    fn test_tier_one_measure_referenced_in_formula_description_passes() {
        let toml = r#"
formula = "ref-in-formula-desc"
version = 1
description = "Deliberation with bounded panelists"

[tier]
level = 1
measure = "panelists"

[[steps]]
id = "dispatch"
title = "Dispatch"
description = "Send prompts to the panel."
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(
            f.tier,
            Tier::One {
                measure: "panelists".to_owned()
            }
        );
    }

    #[test]
    fn test_tier_display() {
        assert_eq!(Tier::Zero.to_string(), "Tier 0");
        assert_eq!(
            Tier::One {
                measure: "count".to_owned()
            }
            .to_string(),
            "Tier 1 (measure=count)"
        );
    }

    #[test]
    fn test_tier_serde_roundtrip_via_formula() {
        // Verify that each tier variant survives a parse roundtrip.
        let t0 = r#"
formula = "rt-zero"
version = 1
[tier]
level = 0
[[steps]]
id = "a"
title = "A"
description = "."
"#;
        assert_eq!(Formula::parse(t0).unwrap().tier, Tier::Zero);

        let t1 = r#"
formula = "rt-one"
version = 1
[tier]
level = 1
measure = "count"
[[steps]]
id = "a"
title = "A"
description = "Bounded count of children."
"#;
        assert_eq!(
            Formula::parse(t1).unwrap().tier,
            Tier::One {
                measure: "count".to_owned()
            }
        );
    }

    // -----------------------------------------------------------------------
    // Live formula tier tests — verify the 8 annotated formulas parse
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // ParallelLimit tests (ADR-043)
    // -----------------------------------------------------------------------

    #[test]
    fn test_parallel_limit_absent_defaults_to_none() {
        let toml = r#"
formula = "no-limit"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "No limit."
"#;
        let f = Formula::parse(toml).unwrap();
        assert!(f.steps[0].parallel_limit.is_none());
    }

    #[test]
    fn test_parallel_limit_static_parsed() {
        let toml = r#"
formula = "limited"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "Bounded fan-out."
parallel_limit = { max = 4, mode = "static" }
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(
            f.steps[0].parallel_limit,
            Some(ParallelLimit::Static { max: 4 })
        );
        assert_eq!(
            f.steps[0].parallel_limit.as_ref().unwrap().static_max(),
            Some(4)
        );
    }

    #[test]
    fn test_parallel_limit_mode_defaults_to_static() {
        let toml = r#"
formula = "default-mode"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "Bounded."
parallel_limit = { max = 2 }
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(
            f.steps[0].parallel_limit,
            Some(ParallelLimit::Static { max: 2 })
        );
    }

    #[test]
    fn test_parallel_limit_smart_parsed_but_not_enforced() {
        let toml = r#"
formula = "future-smart"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "."
parallel_limit = { max = 16, mode = "smart", policy = "energy-aware" }
"#;
        let f = Formula::parse(toml).unwrap();
        assert_eq!(
            f.steps[0].parallel_limit,
            Some(ParallelLimit::Smart {
                policy: "energy-aware".to_owned()
            })
        );
        // Smart mode yields no enforceable static cap today (ADR-044).
        assert_eq!(
            f.steps[0].parallel_limit.as_ref().unwrap().static_max(),
            None
        );
    }

    #[test]
    fn test_parallel_limit_zero_rejected() {
        let toml = r#"
formula = "zero-cap"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "."
parallel_limit = { max = 0 }
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(err, FormulaError::ParallelLimitZero(ref s) if s == "work"));
    }

    #[test]
    fn test_parallel_limit_unknown_mode_rejected() {
        let toml = r#"
formula = "bad-mode"
version = 1

[[steps]]
id = "work"
title = "Work"
description = "."
parallel_limit = { max = 4, mode = "turbo" }
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::ParallelLimitUnknownMode { ref mode, .. } if mode == "turbo"
        ));
    }

    #[test]
    fn test_live_formula_task_work_is_tier0() {
        let toml = include_str!("../../../.cosmon/formulas/task-work.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_live_formula_temp_review_is_tier0() {
        let toml = include_str!("../../../.cosmon/formulas/temp-review.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_live_formula_oversee_is_tier0() {
        let toml = include_str!("../../../.cosmon/formulas/oversee.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_live_formula_absorb_is_tier0() {
        let toml = include_str!("../../../.cosmon/formulas/absorb.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_live_formula_idea_to_plan_is_tier1() {
        let toml = include_str!("../../../.cosmon/formulas/idea-to-plan.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier.level(), 1);
    }

    #[test]
    fn test_live_formula_deep_think_is_tier1() {
        let toml = include_str!("../../../.cosmon/formulas/deep-think.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier.level(), 1);
    }

    #[test]
    fn test_live_formula_mission_controller_is_tier1() {
        let toml = include_str!("../../../.cosmon/formulas/mission-controller.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier.level(), 1);
    }

    #[test]
    fn test_live_formula_mission_plan_is_tier1() {
        let toml = include_str!("../../../.cosmon/formulas/mission-plan.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier.level(), 1);
    }

    // -----------------------------------------------------------------------
    // Map / Reduce / While — IDÉE-2 (delib-6b9d): canonical dynamic-DAG
    // patterns absorbed as formulas (TOML-only, no Rust type), see
    // docs/handbook.md §"Map / Reduce / While".
    // -----------------------------------------------------------------------

    #[test]
    fn test_live_formula_map_is_tier1_items() {
        let toml = include_str!("../../../.cosmon/formulas/map.formula.toml");
        let f = Formula::parse(toml).unwrap();
        match &f.tier {
            Tier::One { measure } => assert_eq!(measure, "items"),
            other => panic!("expected Tier::One {{ measure: items }}, got {other:?}"),
        }
    }

    #[test]
    fn test_live_formula_reduce_is_tier0() {
        let toml = include_str!("../../../.cosmon/formulas/reduce.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_live_formula_while_is_tier1_iterations() {
        let toml = include_str!("../../../.cosmon/formulas/while.formula.toml");
        let f = Formula::parse(toml).unwrap();
        match &f.tier {
            Tier::One { measure } => assert_eq!(measure, "iterations"),
            other => panic!("expected Tier::One {{ measure: iterations }}, got {other:?}"),
        }
    }

    #[test]
    fn test_live_formula_map_has_expected_steps() {
        let toml = include_str!("../../../.cosmon/formulas/map.formula.toml");
        let f = Formula::parse(toml).unwrap();
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["plan", "fanout", "collect"]);
    }

    #[test]
    fn test_live_formula_while_has_expected_steps() {
        let toml = include_str!("../../../.cosmon/formulas/while.formula.toml");
        let f = Formula::parse(toml).unwrap();
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["init", "loop", "report"]);
    }

    #[test]
    fn test_live_formula_reduce_has_expected_steps() {
        let toml = include_str!("../../../.cosmon/formulas/reduce.formula.toml");
        let f = Formula::parse(toml).unwrap();
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["resolve", "collect", "reduce"]);
    }

    // -----------------------------------------------------------------------
    // Fleet Review — metabolism scan (delib-8c50)
    // -----------------------------------------------------------------------

    #[test]
    fn test_live_formula_fleet_review_is_tier0() {
        let toml = include_str!("../../../.cosmon/formulas/fleet-review.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.tier, Tier::Zero);
    }

    #[test]
    fn test_live_formula_fleet_review_has_expected_steps() {
        let toml = include_str!("../../../.cosmon/formulas/fleet-review.formula.toml");
        let f = Formula::parse(toml).unwrap();
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["scan", "report"]);
    }

    #[test]
    fn test_live_formula_fleet_review_has_expected_vars() {
        let toml = include_str!("../../../.cosmon/formulas/fleet-review.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "fleet-review");
        assert_eq!(f.id_prefix, "review");
        assert!(!f.freeze_on_last_step);

        // All three variables are optional with defaults.
        assert!(f.variables.contains_key("galaxy_path"));
        assert!(f.variables.contains_key("lookback_days"));
        assert!(f.variables.contains_key("max_observations"));

        let lookback = &f.variables["lookback_days"];
        assert!(!lookback.required);
        assert_eq!(lookback.default.as_deref(), Some("7"));

        let max_obs = &f.variables["max_observations"];
        assert!(!max_obs.required);
        assert_eq!(max_obs.default.as_deref(), Some("5"));
    }

    // -----------------------------------------------------------------------
    // missing_required_vars — the briefless-molecule predicate
    // (task-20260711-919a). Single source of truth shared by nucleation and
    // the `cs tackle` dispatch guard.
    // -----------------------------------------------------------------------

    fn formula_requiring_topic() -> Formula {
        Formula::parse(
            r#"
formula = "needs-topic"
version = 1
id_prefix = "nt"

[vars.topic]
description = "The task to execute."
required = true

[vars.note]
description = "Optional note."
default = "n/a"

[[steps]]
id = "s"
title = "S"
description = "."
"#,
        )
        .unwrap()
    }

    #[test]
    fn test_missing_required_vars_flags_absent_required() {
        let f = formula_requiring_topic();
        let provided = HashMap::new();
        assert_eq!(
            f.missing_required_vars(&provided),
            vec!["topic".to_string()]
        );
    }

    #[test]
    fn test_missing_required_vars_flags_blank_required() {
        let f = formula_requiring_topic();
        for blank in ["", "   ", "\t\n"] {
            let mut provided = HashMap::new();
            provided.insert("topic".to_string(), blank.to_string());
            assert_eq!(
                f.missing_required_vars(&provided),
                vec!["topic".to_string()],
                "blank {blank:?} should be flagged"
            );
        }
    }

    #[test]
    fn test_missing_required_vars_empty_when_present() {
        let f = formula_requiring_topic();
        let mut provided = HashMap::new();
        provided.insert("topic".to_string(), "do the thing".to_string());
        assert!(f.missing_required_vars(&provided).is_empty());
    }

    #[test]
    fn test_missing_required_vars_ignores_optional_and_defaulted() {
        // A blank optional variable and a missing required-with-default one
        // must NOT be flagged — only effectively-required (required && no
        // default) blanks count.
        let f = formula_requiring_topic();
        let mut provided = HashMap::new();
        provided.insert("topic".to_string(), "x".to_string());
        provided.insert("note".to_string(), String::new());
        assert!(f.missing_required_vars(&provided).is_empty());
    }

    #[test]
    fn test_live_formula_task_work_topic_is_required() {
        // Pins the briefless-molecule guard contract: `task-work` must declare
        // `topic` as a required, default-free variable so an empty-topic
        // molecule fails at nucleation AND is refused at dispatch.
        let toml = include_str!("../../../.cosmon/formulas/task-work.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "task-work");
        let topic = f
            .variables
            .get("topic")
            .expect("task-work must declare a `topic` variable");
        assert!(topic.required, "task-work `topic` must be required");
        assert!(
            topic.default.is_none(),
            "task-work `topic` must have no default (else it is trivially satisfiable)"
        );
        // A molecule with no topic is briefless.
        assert_eq!(
            f.missing_required_vars(&HashMap::new()),
            vec!["topic".to_string()]
        );
    }

    // -----------------------------------------------------------------------
    // Drift patrols — the five broken-promise signals
    // (task-20260419-c565, delib-20260419-5168 §4 Q8).
    // Each patrol is Tier 1 because the `detect` step may nucleate up to
    // five `issue` molecules per run; the count is bounded by the number
    // of galaxies of the relevant kind in neurion. The tests below pin
    // the contract so a future edit that breaks the tier, the step
    // shape, or the formula name fails fast.
    // -----------------------------------------------------------------------

    #[test]
    fn test_live_formula_drift_hub_to_project() {
        let toml = include_str!("../../../.cosmon/formulas/drift-hub-to-project.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "drift-hub-to-project");
        assert_eq!(f.id_prefix, "drift-hub");
        assert_eq!(f.tier.level(), 1);
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["scan", "detect", "report"]);
    }

    #[test]
    fn test_live_formula_drift_editorial_introspection() {
        let toml =
            include_str!("../../../.cosmon/formulas/drift-editorial-introspection.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "drift-editorial-introspection");
        assert_eq!(f.id_prefix, "drift-edit");
        assert_eq!(f.tier.level(), 1);
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["scan", "detect", "report"]);
    }

    #[test]
    fn test_live_formula_drift_infra_imposition() {
        let toml = include_str!("../../../.cosmon/formulas/drift-infra-imposition.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "drift-infra-imposition");
        assert_eq!(f.id_prefix, "drift-infra");
        assert_eq!(f.tier.level(), 1);
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["scan", "detect", "report"]);
    }

    #[test]
    fn test_live_formula_drift_vanity_family() {
        let toml = include_str!("../../../.cosmon/formulas/drift-vanity-family.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "drift-vanity-family");
        assert_eq!(f.id_prefix, "drift-vanity");
        assert_eq!(f.tier.level(), 1);
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["scan", "detect", "report"]);
    }

    #[test]
    fn test_live_formula_drift_project_frozen() {
        let toml = include_str!("../../../.cosmon/formulas/drift-project-frozen.formula.toml");
        let f = Formula::parse(toml).unwrap();
        assert_eq!(f.name.as_str(), "drift-project-frozen");
        assert_eq!(f.id_prefix, "drift-frozen");
        assert_eq!(f.tier.level(), 1);
        let ids: Vec<&str> = f.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["scan", "detect", "report"]);
    }

    /// Step `timeout_minutes` round-trips through the parser and the
    /// `Step::stall_timeout_minutes()` accessor falls back to 30 minutes
    /// when the field is absent.
    #[test]
    fn test_step_timeout_minutes_round_trip_and_default() {
        let toml = r#"
formula = "stall-test"
version = 1
description = "test"
id_prefix = "stall"

[[steps]]
id = "fast"
title = "Fast step"
description = "Short budget"
timeout_minutes = 5

[[steps]]
id = "default"
title = "Default step"
description = "No explicit budget"
"#;
        let f = Formula::parse(toml).unwrap();
        let by_id = |id: &str| f.steps.iter().find(|s| s.id == id).unwrap();
        assert_eq!(by_id("fast").timeout_minutes, Some(5));
        assert_eq!(by_id("fast").stall_timeout_minutes(), 5);
        assert_eq!(by_id("default").timeout_minutes, None);
        assert_eq!(by_id("default").stall_timeout_minutes(), 30);
    }

    // -----------------------------------------------------------------
    // Query / LLM step kinds (delib-20260426-1bcd #5)
    // -----------------------------------------------------------------

    #[test]
    fn test_query_step_parsed_with_state_source() {
        let toml = r#"
formula = "extract-id"
version = 1
description = "."

[[steps]]
id = "extract"
title = "Extract id"
description = "Extract the molecule id."

[steps.query]
expr = ".id"
source = "state"
output_var = "this_id"
"#;
        let f = Formula::parse(toml).unwrap();
        let q = f.steps[0].query.as_ref().unwrap();
        assert_eq!(q.expr, ".id");
        assert_eq!(q.output_var, "this_id");
        assert_eq!(q.source, QuerySource::CurrentMoleculeState);
        assert!(f.steps[0].is_query());
        assert!(f.steps[0].is_automated());
    }

    #[test]
    fn test_query_step_source_molecule_id_parsed() {
        let toml = r#"
formula = "extract-from-other"
version = 1
description = "."

[[steps]]
id = "extract"
title = "Extract"
description = "."

[steps.query]
expr = ".variables.versions"
source = "molecule:idea-20260101-abcd"
output_var = "versions"
"#;
        let f = Formula::parse(toml).unwrap();
        let q = f.steps[0].query.as_ref().unwrap();
        match &q.source {
            QuerySource::MoleculeState(id) => {
                assert_eq!(id.as_str(), "idea-20260101-abcd");
            }
            other => panic!("expected MoleculeState, got {other:?}"),
        }
    }

    #[test]
    fn test_query_step_unknown_source_rejected() {
        let toml = r#"
formula = "bad"
version = 1
description = "."

[[steps]]
id = "extract"
title = "Extract"
description = "."

[steps.query]
expr = ".id"
source = "github:org/repo"
output_var = "x"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::QueryUnknownSource { ref step, .. } if step == "extract"
        ));
    }

    #[test]
    fn test_query_step_missing_field_rejected() {
        let toml = r#"
formula = "missing"
version = 1
description = "."

[[steps]]
id = "extract"
title = "Extract"
description = "."

[steps.query]
expr = ""
source = "state"
output_var = "x"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::QueryMissingField { field: "expr", .. }
        ));
    }

    #[test]
    fn test_query_step_mutually_exclusive_with_command() {
        let toml = r#"
formula = "clash"
version = 1
description = "."

[[steps]]
id = "extract"
title = "Extract"
description = "."
command = "echo hi"

[steps.query]
expr = ".id"
source = "state"
output_var = "x"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::MultipleStepKinds { ref step, ref kinds } if step == "extract" && kinds.contains("query")
        ));
    }

    #[test]
    fn test_llm_step_parsed_with_defaults() {
        let toml = r#"
formula = "synth"
version = 1
description = "."

[[steps]]
id = "synthesize"
title = "Synthesize"
description = "Synthesize a panel response."

[steps.llm]
provider = "mock"
model = "claude-opus-4-7"
prompt = "Hello, panel."
output_path = "synthesis.md"
"#;
        let f = Formula::parse(toml).unwrap();
        let l = f.steps[0].llm.as_ref().unwrap();
        assert_eq!(l.provider, "mock");
        assert_eq!(l.model, "claude-opus-4-7");
        assert_eq!(l.prompt.as_deref(), Some("Hello, panel."));
        assert_eq!(l.output_path, std::path::PathBuf::from("synthesis.md"));
        // Defaults from RawLlmSpec.
        assert_eq!(l.checkpoint_every_secs, 30);
        assert_eq!(l.timeout_per_checkpoint_secs, 120);
        assert_eq!(l.max_total_minutes, 30);
        assert_eq!(l.max_retries, 3);
        assert!(f.steps[0].is_llm());
        assert!(f.steps[0].is_automated());
    }

    #[test]
    fn test_llm_step_overrides_defaults() {
        let toml = r#"
formula = "fast-synth"
version = 1
description = "."

[[steps]]
id = "synthesize"
title = "Synthesize"
description = "."

[steps.llm]
provider = "mock"
model = "tiny"
prompt = "go"
output_path = "out.md"
checkpoint_every = 5
timeout_per_checkpoint = 15
max_total_minutes = 2
max_retries = 1
"#;
        let f = Formula::parse(toml).unwrap();
        let l = f.steps[0].llm.as_ref().unwrap();
        assert_eq!(l.checkpoint_every_secs, 5);
        assert_eq!(l.timeout_per_checkpoint_secs, 15);
        assert_eq!(l.max_total_minutes, 2);
        assert_eq!(l.max_retries, 1);
    }

    #[test]
    fn test_llm_step_prompt_and_prompt_file_clash_rejected() {
        let toml = r#"
formula = "clash"
version = 1
description = "."

[[steps]]
id = "s"
title = "."
description = "."

[steps.llm]
provider = "mock"
model = "x"
prompt = "inline"
prompt_file = "p.md"
output_path = "o.md"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(err, FormulaError::LlmPromptAmbiguous(ref s) if s == "s"));
    }

    #[test]
    fn test_llm_step_missing_prompt_rejected() {
        let toml = r#"
formula = "missing"
version = 1
description = "."

[[steps]]
id = "s"
title = "."
description = "."

[steps.llm]
provider = "mock"
model = "x"
output_path = "o.md"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::LlmMissingField {
                field: "prompt",
                ..
            }
        ));
    }

    #[test]
    fn test_llm_and_query_mutually_exclusive() {
        let toml = r#"
formula = "clash2"
version = 1
description = "."

[[steps]]
id = "s"
title = "."
description = "."

[steps.query]
expr = ".id"
source = "state"
output_var = "x"

[steps.llm]
provider = "mock"
model = "x"
prompt = "y"
output_path = "o.md"
"#;
        let err = Formula::parse(toml).unwrap_err();
        assert!(matches!(
            err,
            FormulaError::MultipleStepKinds { ref kinds, .. } if kinds.contains("query") && kinds.contains("llm")
        ));
    }
}
