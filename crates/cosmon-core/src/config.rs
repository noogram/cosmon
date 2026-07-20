// SPDX-License-Identifier: AGPL-3.0-only

//! Project-level configuration — parsed from `.cosmon/config.toml`.
//!
//! This module defines the pure domain types for the project configuration
//! file. The actual file I/O lives in `cosmon-filestore`; this module is
//! zero-I/O and only knows how to deserialize TOML into typed structs.
//!
//! ```toml
//! [worker]
//! on_complete = "commit"  # "commit" | "commit+push" | "commit+push+pr"
//!
//! [surfaces]
//! auto_reconcile = false
//!
//! [documentation]
//! enabled = true
//!
//! [hooks]
//! post_merge = "just install"  # optional — runs after cs done merges
//!
//! [gates]
//! build_command = "cargo check --workspace"
//! test_command = "cargo test --workspace"
//! lint_command = "cargo clippy --workspace -- -D warnings"
//! format_command = "cargo fmt --all -- --check"
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::id::ProjectId;

/// The full project configuration, loaded from `.cosmon/config.toml`.
///
/// The `[project]` section (specifically `project_id`) is **required** — all
/// commands error if it is missing. Other sections are optional and default to
/// sensible values when absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Project identity — required after `cs init`.
    #[serde(default)]
    pub project: ProjectSection,

    /// Worker behavior configuration.
    #[serde(default)]
    pub worker: WorkerConfig,

    /// Surface auto-reconciliation settings.
    #[serde(default)]
    pub surfaces: SurfacesAutoConfig,

    /// Documentation generation settings.
    #[serde(default)]
    pub documentation: DocumentationConfig,

    /// Lifecycle hooks — commands run at specific points in the molecule
    /// lifecycle (e.g. after a successful merge in `cs done`).
    #[serde(default)]
    pub hooks: HooksConfig,

    /// Project verification gates — the commands a worker should run to
    /// prove its change is green. Language-agnostic: each field is an
    /// opaque shell command, so cosmon does not assume Rust, Python, Node,
    /// or any particular toolchain.
    #[serde(default)]
    pub gates: GatesConfig,

    /// Archive subsystem — durable capture of terminated molecules under
    /// `.cosmon/state/archive/` (plumbing only in M1; actual archive writes
    /// land in later milestones).
    #[serde(default)]
    pub archive: ArchiveConfig,

    /// Whisper subsystem — `cs whisper` perturbation port settings.
    #[serde(default)]
    pub whisper: WhisperConfig,

    /// Git remote blocklist — structural anti-leak guard.
    ///
    /// Names a set of remote URL substrings that no cosmon worktree may
    /// carry. `cs done` refuses to merge a worker's branch when any
    /// configured remote URL contains one of these substrings, closing
    /// the leak channel structurally rather than by convention. See
    /// [`GitRemoteBlocklistConfig`]. The guard exists because an internal
    /// dependency was copied in from a public upstream by snapshot: cosmon
    /// worktrees must never re-acquire that upstream as a remote and leak
    /// commits back to it.
    #[serde(default)]
    pub git_remote_blocklist: GitRemoteBlocklistConfig,

    /// Publish-identity gate — structural anti-leak guard over the git
    /// author/committer identity stamped into the commits a `cs done` merge
    /// would introduce (ADR-128 §V1, the D7 publish closure).
    ///
    /// Closes the confirmed residual leak past V0's file-content gate: the
    /// operator email (`operator@example.org`) is stamped into *every* commit
    /// and is invisible to any file-content grep. This block widens the
    /// detection surface to the git-identity channel and supports the
    /// closed-codebook inversion (`allowed_emails`) shannon called for. See
    /// [`PublishIdentityConfig`].
    #[serde(default)]
    pub publish_identity: PublishIdentityConfig,

    /// Confidential-content publish gate — substring patterns that no
    /// publish-bound artifact may contain in its content. Sibling of
    /// [`git_remote_blocklist`](Self::git_remote_blocklist): the remote
    /// guard inspects worktree remotes, this one inspects file content.
    /// `cs done` aborts the merge if any file matched by `publish_globs`
    /// contains a forbidden substring. See [`ConfidentialBlocklistConfig`]
    /// and ADR-128. This is the V0 floor that V1's [`publish_identity`](Self::publish_identity)
    /// gate widens beyond file content to the git-identity channel.
    #[serde(default)]
    pub confidential_blocklist: ConfidentialBlocklistConfig,

    /// Scope-guard policy — the warn-vs-abort knob for the `cs done`
    /// change-perimeter gate (P3 of `task-20260712-3819`). A molecule
    /// declares its allowed perimeter with `--var scope_allow=<globs>`;
    /// this block decides whether an out-of-scope merge is advisory (the
    /// default) or a hard abort. See [`ScopeGuardConfig`].
    #[serde(default)]
    pub scope_guard: ScopeGuardConfig,

    /// Energy circuit-breaker defaults — per-molecule step budget that
    /// arms `cs evolve`'s runaway-loop protection (THESIS Part XI).
    #[serde(default)]
    pub energy: EnergyConfig,

    /// Worker-Spawn Port Adapters — `cs tackle --adapter <name>` looks
    /// each entry up here (ADR-097 / C6). Absent when the project uses
    /// only the built-in `"claude"` adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapters: Option<AdaptersConfig>,

    /// Fail-closed per-galaxy model-dispatch ceiling (delib-20260704-b476 /
    /// C4). Bounds the worst-case credit burn to `K·k` (constant, independent
    /// of the number of dispatches) rather than the unbounded `N·k` of the
    /// sticky-`/model` leak. Opt-in: absent (the default) means no ceiling —
    /// byte-identical to today. See [`ModelBudgetConfig`].
    #[serde(default)]
    pub model_budget: ModelBudgetConfig,

    /// Gödel self-reference guards — depth limiter and staleness
    /// circuit-breaker for recursive spawn chains.
    #[serde(default)]
    pub self_reference_guard: SelfReferenceGuardConfig,

    /// Provider-family diversity baseline for cross-provider reading
    /// committees (ADR-147 tier a / C3).
    ///
    /// The **exogenous, add-only** requirement-set the audited worker cannot
    /// lower: readers/falsifiers that must sit on the committee and a floor on
    /// the number of distinct *resolved* provider endpoints it must span. The
    /// effective set is the monotone union `baseline ∪ ⋃ profiles`
    /// ([`ProviderBiasConfig::effective`]); a downgrade is *inexpressible in the
    /// type*, not merely forbidden. `cs reconcile --check` reddens when the
    /// committee's resolved endpoints collapse below its own floor (the
    /// tier-(a) proxy-costume guard). Absent (the default) means no committee is
    /// declared — byte-identical to a galaxy that predates the knob. See
    /// [`ProviderBiasConfig`] and [`crate::provider_diversity`].
    #[serde(default)]
    pub provider_bias: ProviderBiasConfig,

    /// Public-attribution source of truth — the maker name, URL, and
    /// contactable address that every shipped/public artifact must carry.
    ///
    /// Inherited by every galaxy that runs cosmon so the public name lives
    /// in *one place to change*, never re-typed per project. `cs tackle`
    /// folds a one-line directive built from this block into the worker
    /// bootstrap prompt, filling the attribution vacuum at the source (the
    /// model has the right name in hand before it reaches a "built by"
    /// slot). See [`AttributionConfig`] and ADR-128.
    #[serde(default)]
    pub attribution: AttributionConfig,
}

/// Per-project defaults for the molecule-level `StepBudget` circuit breaker
/// (THESIS Part XI).
///
/// `cs nucleate` stamps the new molecule with a budget equal to
/// `default_step_budget` unless the operator passes `--energy-budget <N>`.
/// `cs evolve` decrements the budget once per step; at zero the next attempt
/// transitions the molecule to `Frozen` with reason `"energy-exhausted"`.
///
/// ```toml
/// [energy]
/// default_step_budget = 100
/// ```
///
/// 100 is the empirically-tunable starting cap — it leaves comfortable
/// headroom for the formulas in tree (most are 2–6 steps) while still firing
/// long before a
/// silent loop wastes meaningful operator budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnergyConfig {
    /// Default per-molecule step budget. `0` disables the circuit breaker
    /// (no budget stamped at nucleate, `cs evolve` ignores the field).
    /// Default: `100`.
    #[serde(default = "default_step_budget")]
    pub default_step_budget: u32,
}

impl Default for EnergyConfig {
    fn default() -> Self {
        Self {
            default_step_budget: default_step_budget(),
        }
    }
}

const fn default_step_budget() -> u32 {
    100
}

/// Fail-closed per-galaxy model-dispatch ceiling (delib-20260704-b476 / C4,
/// carnot's safety property).
///
/// Bounds the worst-case credit burn by capping the number of **strong**
/// dispatches (`cs tackle` on a model in its adapter's
/// [`strong`](AdapterEntry::strong) set) inside a rolling window. On the
/// (K+1)th strong pin, `cs tackle` fails closed — [downgrades or aborts per
/// `on_overflow`](crate::model_budget::OverflowPolicy) — never a soft warning
/// the burst can ignore. The running total is re-derived as a fold over
/// `events.jsonl` ([`crate::model_budget::count_strong_in_window`]), the `cs
/// reconcile` idiom — never a mutable counter file.
///
/// **Opt-in per galaxy.** Absent (`strong_dispatch_cap = None`, the default)
/// means no ceiling: strong dispatches are unbounded, byte-identical to a
/// galaxy that never configured a budget. Cosmon proposes the mechanism; it
/// does not impose it.
///
/// ```toml
/// [model_budget]
/// strong_dispatch_cap = 3      # at most 3 strong dispatches per window
/// window_hours = 24            # rolling 24h window (the default)
/// on_overflow = "downgrade"    # or "abort" (the default is downgrade)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBudgetConfig {
    /// Maximum number of strong dispatches allowed inside the rolling window.
    /// `None` (the default) disables the ceiling entirely. `Some(0)` is a
    /// legitimate "no strong dispatches at all" policy — the first strong pin
    /// is already at the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strong_dispatch_cap: Option<u32>,

    /// Width of the rolling window, in hours, the cap is measured over.
    /// Default: 24.
    #[serde(default = "default_budget_window_hours")]
    pub window_hours: u32,

    /// What to do when a strong dispatch would exceed the cap — downgrade to
    /// the safe floor (keep spawning economical) or abort the spawn. Default:
    /// [`OverflowPolicy::Downgrade`](crate::model_budget::OverflowPolicy::Downgrade).
    #[serde(default)]
    pub on_overflow: crate::model_budget::OverflowPolicy,
}

impl Default for ModelBudgetConfig {
    fn default() -> Self {
        Self {
            strong_dispatch_cap: None,
            window_hours: default_budget_window_hours(),
            on_overflow: crate::model_budget::OverflowPolicy::default(),
        }
    }
}

const fn default_budget_window_hours() -> u32 {
    24
}

/// A provider-diversity **requirement-set** — the declarative object the
/// monotone union operates on (ADR-147 / C3).
///
/// Before this type, cosmon had *no* first-class "requirement-set" object
/// (`delib-20260711-f62a` §26): "add-only" for a reading committee was
/// **English prose**, not a computable relation. This struct makes the
/// requirement-set a value, so *"the effective committee is the monotone union
/// of the baseline and every profile"* becomes an operation
/// ([`ProviderRequirementSet::join`]) instead of a sentence someone can argue
/// with.
///
/// # Why this is NOT `model_budget` (feynman's correction)
///
/// The survey claimed the add-only golden rule *"maps exactly onto
/// `model_budget`"*. It does not. [`crate::model_budget::config_default_is_strong`]
/// is a **value predicate over one field** (is `default_model` in `strong`?) and
/// it is **fail-open** (an unknown id is treated as safe). The add-only
/// guarantee is a different shape entirely: a **subset / monotonicity relation
/// between two requirement-sets** (does the effective set still contain
/// everything the baseline required, and never require *less*?). One is
/// "predicate on a scalar"; the other is "order relation on a lattice." They
/// share a *ceiling* (§8b visibility) but not a *mechanism*.
///
/// # The cap-négatif-absent trick
///
/// There is **no** `remove_readers`, `remove_falsifiers`, `max_distinct`, or
/// any subtract/override field — by construction. Exactly as
/// [`ModelBudgetConfig`] has a `strong_dispatch_cap` but no *negative* cap (you
/// cannot configure "burn extra credits"), this type can only **grow** a
/// requirement. A downgrade is not "forbidden by a rule"; it is **inexpressible
/// in the type**. That is the whole design: an add-only guarantee enforced by a
/// schema is structural; the same guarantee enforced by prose over an
/// override-capable schema is elastic and gameable — so the override schema is
/// deliberately never built.
///
/// ```toml
/// [provider_bias]
/// additional_readers = ["openai", "deepseek"]
/// additional_falsifiers = ["xai"]
/// min_distinct_provider_endpoints = 2
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRequirementSet {
    /// Adapter-name seats that MUST sit on the committee as **readers**
    /// (auditors). A monotone-union member: it can only be added to, never
    /// removed. Resolved to endpoint tuples for the tier-(a) diversity check —
    /// the *name* is a handle, the *resolved endpoint* is what independence is
    /// measured on (ADR-147).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_readers: Vec<String>,

    /// Adapter-name seats that MUST sit on the committee as **falsifiers**
    /// (adversarial refuters). Same add-only semantics as
    /// [`additional_readers`](Self::additional_readers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_falsifiers: Vec<String>,

    /// Floor on the number of **distinct resolved provider endpoints** the
    /// committee must span (`(provider, base_url, model-family)` tuples). The
    /// error-independence axis: a committee whose seats collapse below this
    /// many distinct endpoints is an echo chamber.
    ///
    /// Joined by **max**, never overwrite: a profile may raise the floor, never
    /// lower it (the numeric face of the add-only guarantee). `None` (the
    /// default) means "no floor declared" — opt-in, byte-identical to a galaxy
    /// that never configured a committee.
    ///
    /// **Values are low-confidence hypotheses, measured A/B on *our* workload**
    /// (real-time audio-debug load), **never lifted from a public leaderboard**
    /// — a leaderboard measures a different distribution than the one this
    /// committee audits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_distinct_provider_endpoints: Option<u32>,
}

impl ProviderRequirementSet {
    /// Monotone **join** of two requirement-sets on the add-only lattice:
    /// set-union of the reader/falsifier ids, **max** of the endpoint floor.
    ///
    /// There is no inverse operation — nothing here can lower a value. The
    /// result is normalized (ids sorted + deduplicated) so `effective()` is
    /// deterministic regardless of TOML key order.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        let readers = union_sorted(&self.additional_readers, &other.additional_readers);
        let falsifiers = union_sorted(&self.additional_falsifiers, &other.additional_falsifiers);
        Self {
            additional_readers: readers,
            additional_falsifiers: falsifiers,
            min_distinct_provider_endpoints: join_floor(
                self.min_distinct_provider_endpoints,
                other.min_distinct_provider_endpoints,
            ),
        }
    }
}

/// Join two endpoint floors by **max** — the monotone (add-only) operation on
/// the `Option<u32>` lattice, where `None` is the bottom (no floor).
fn join_floor(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Set-union of two id lists, sorted and deduplicated for determinism.
fn union_sorted(a: &[String], b: &[String]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = a.iter().cloned().collect();
    set.extend(b.iter().cloned());
    set.into_iter().collect()
}

/// Provider-bias committee baseline + named profiles (ADR-147 tier a, C3).
///
/// The **effective** requirement-set a committee must satisfy is the monotone
/// union `baseline ∪ ⋃ profiles` ([`Self::effective`]). The baseline is the
/// *exogenous* floor — it lives in the add-only project config the audited
/// worker cannot lower (buterin's S-1: *the constraint must be exogenous to the
/// party it constrains*; otherwise the fox appoints the inspector). A
/// per-molecule / per-context profile may **add** readers, falsifiers, or raise
/// the endpoint floor; it can never subtract.
///
/// This is Q1 and Q4 of `delib-20260711-f62a` being the **same mechanism**: a
/// diversity invariant is only collusion-resistant if it lives where the
/// audited worker cannot edit it, and "cannot lower it" is guaranteed by the
/// monotone-union schema, not by a rule.
///
/// ```toml
/// [provider_bias]
/// # Baseline — the floor no profile can go under.
/// additional_readers = ["openai"]
/// min_distinct_provider_endpoints = 2
///
/// # A stricter profile for security-stake work — adds a falsifier, raises the
/// # floor. It cannot remove `openai` or drop the floor below 2.
/// [provider_bias.profiles.security]
/// additional_falsifiers = ["deepseek"]
/// min_distinct_provider_endpoints = 3
/// ```
///
/// **Residual (S-3, named not closed):** the monotone union closes *removal*,
/// but elasticity is conserved — it migrates to **stake self-classification** (a
/// worker never removes a requirement; it declares its own molecule low-stake so
/// the committee requirement never triggers). No schema closes that; only the
/// empirical calibration probe (C5) polices it. This type does not pretend to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderBiasConfig {
    /// The exogenous baseline requirement-set (fields flattened directly under
    /// `[provider_bias]`). The floor every profile builds on and none can lower.
    #[serde(flatten)]
    pub baseline: ProviderRequirementSet,

    /// Named profiles, each contributing **additively** to the effective set.
    /// Keyed by profile name; the name is a label for humans, never a
    /// distinctness axis (ADR-147: the lint compares resolved endpoints, never
    /// section names).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub profiles: std::collections::BTreeMap<String, ProviderRequirementSet>,
}

impl ProviderBiasConfig {
    /// The **effective** requirement-set: `baseline ∪ ⋃ profiles`, add-only by
    /// construction. Deterministic (sorted ids) regardless of profile order.
    #[must_use]
    pub fn effective(&self) -> ProviderRequirementSet {
        let mut eff = ProviderRequirementSet::default().join(&self.baseline);
        for profile in self.profiles.values() {
            eff = eff.join(profile);
        }
        eff
    }

    /// `true` when nothing is declared — the opt-in default. A galaxy that
    /// never writes `[provider_bias]` is byte-identical to one before the knob
    /// existed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.baseline == ProviderRequirementSet::default() && self.profiles.is_empty()
    }
}

/// Gödel self-reference guards — structural depth limiter and staleness
/// circuit-breaker that prevent recursive spawn chains.
///
/// When `cs tackle` spawns a worker, the worker inherits `CB_DEPTH` (spawn
/// depth) and `CB_SESSION_ROLE` (broker vs worker). A broker session
/// orchestrates other molecules but must never be spawned recursively.
/// The depth counter caps spawn chains at `max_depth` (default 2).
///
/// The staleness invariant ensures a broker never acts on a gauge reading
/// older than `max_staleness_secs`: if the last observation exceeds the
/// window, the molecule enters `Frozen` rather than dispatching on stale
/// data.
///
/// `debounce_secs` sets the minimum interval between consecutive spawn
/// attempts from the same parent — guards against rapid-fire retry loops.
///
/// ```toml
/// [self_reference_guard]
/// max_depth = 2
/// debounce_secs = 5
/// max_staleness_secs = 300
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfReferenceGuardConfig {
    /// Maximum spawn depth (0 = root operator session). Workers at depth
    /// `>= max_depth` are refused by `cs tackle`. Default: 2.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,

    /// Minimum seconds between consecutive spawn attempts from the same
    /// parent molecule. Default: 5.
    #[serde(default = "default_debounce_secs")]
    pub debounce_secs: u64,

    /// Maximum age in seconds for a broker's gauge reading before the
    /// molecule is frozen with reason `"stale-gauge"`. Default: 300.
    #[serde(default = "default_max_staleness_secs")]
    pub max_staleness_secs: u64,
}

impl Default for SelfReferenceGuardConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            debounce_secs: default_debounce_secs(),
            max_staleness_secs: default_max_staleness_secs(),
        }
    }
}

const fn default_max_depth() -> u32 {
    2
}

const fn default_debounce_secs() -> u64 {
    5
}

const fn default_max_staleness_secs() -> u64 {
    300
}

/// Git remote blocklist — substring patterns that no cosmon worktree may
/// carry as a remote URL.
///
/// The motivating threat: a tired agent at
/// 3am adds a public mirror as a remote and pushes private code to it.
/// Discipline ("never add this remote") fails categorically against
/// fatigue, hooks, and helpfulness. The structural answer is to make the
/// merge gate refuse: if any worktree remote URL contains a forbidden
/// substring, `cs done` aborts before the merge happens.
///
/// Pattern semantics: a *substring match* against each line of
/// `git remote -v` output. Substring (not regex) on purpose — the
/// patterns are short, literal repository identifiers and the matching
/// rule must be obvious to a tired-3am-agent reading the error message.
///
/// ```toml
/// [git_remote_blocklist]
/// forbidden_substrings = [
///   "github.com:noogram/almanac",
///   "github.com/noogram/almanac",
/// ]
/// ```
///
/// Empty (the default) means no blocklist is enforced, which matches the
/// behavior of every cosmon project that predates this config knob.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRemoteBlocklistConfig {
    /// Substrings that must not appear in any worktree remote URL.
    ///
    /// Each entry is matched as a literal substring against every line of
    /// `git remote -v`. A non-empty list with no matches passes; a single
    /// match aborts `cs done` with an error message naming the pattern
    /// and the offending remote line so the operator can act.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_substrings: Vec<String>,
}

impl GitRemoteBlocklistConfig {
    /// `true` when no patterns are configured — fast-path for the common
    /// case of projects that do not need the structural guard.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forbidden_substrings.is_empty()
    }
}

/// Scope-guard policy — the warn-vs-abort knob for the `cs done`
/// change-perimeter gate (P3 of `task-20260712-3819`).
///
/// The *perimeter itself* is declared per-molecule (the `scope_allow`
/// variable, comma/newline-separated globs), not here — this block only
/// carries the **policy**: what `cs done` does when a merge touches files
/// outside the declared perimeter.
///
/// The forcing incident: a worker briefed on `docs/book/src/**` + `README.md`
/// rewrote 40 crate-source files under `crates/cosmon-cli/src/cmd/`, which
/// would have broken the golden man-page test and silently changed
/// `cs --help` output. The escape was caught only by a hand `git status`.
///
/// # Policy — advisory by default (invariants §8b)
///
/// The default is **advisory** (`strict = false`): an out-of-scope merge
/// prints a structured warning and the merge proceeds. This follows the
/// cosmon discipline *propose mechanisms of verification, do not impose
/// them* — the seal is a trace, not a lock. Unlike the anti-leak gates
/// (`[git_remote_blocklist]`, `[confidential_blocklist]`), an out-of-scope
/// change is a *quality/integrity* signal, not a confidentiality breach, so
/// the honest default warns rather than aborting a legitimate adjacent-file
/// refactor. Setting `strict = true` escalates the warning to a hard
/// `cs done` abort for galaxies that want the perimeter enforced
/// structurally.
///
/// ```toml
/// [scope_guard]
/// strict = true
/// ```
///
/// A molecule that declares no `scope_allow` perimeter is unaffected by
/// either policy — the guard is inert with no perimeter, so this block is a
/// zero-cost default for every project and every molecule that predates it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeGuardConfig {
    /// When `true`, an out-of-scope merge aborts `cs done` instead of merely
    /// warning. Default `false` (advisory) — the §8b-aligned honest default.
    #[serde(default)]
    pub strict: bool,
}

/// Publish-identity gate — the D7 publish-closure widening (ADR-128 §V1).
///
/// V0 (`[confidential_blocklist]`) scans *file content* of the merged tree.
/// But the highest-probability provable leak past a file grep is the **git
/// author/committer identity** stamped into every commit: in the
/// flow-models incident the operator email `operator@example.org`
/// rode every commit of a shipped public repo, invisible to any content
/// scan. This block widens the detection surface to that channel and is read
/// by `cs done` over the commits a merge would introduce (`<base>..<branch>`),
/// so the operator's legitimate identity on pre-existing history is never
/// flagged — only the *new* commits about to be published.
///
/// Two complementary layers, both optional and both empty by default
/// (backward-compatible for every project that predates the knob — notably
/// cosmon itself, which is internal and where the operator identity is
/// legitimate):
///
/// - **`allowed_emails` — the closed-codebook whitelist (primary, recall →
///   1 on this slot).** When non-empty, ANY author or committer email on a
///   to-be-published commit that is not in this set is a violation *by
///   construction*. This is shannon's inversion: blacklist scanning is
///   open-vocabulary (paraphrase, alias — recall structurally < 1);
///   whitelisting the one canonical publish identity makes any other string
///   out-of-codebook. The cost shifts from recall (unfixable) to slot
///   enumeration (finite, auditable). Pair it with a pinned publish git
///   identity (`git config user.email <canonical>`) so commits are stamped
///   right at the source.
/// - **`forbidden_substrings` — the blacklist (secondary, defense-in-depth).**
///   Scanned against author/committer *names* and commit messages, where no
///   codebook exists (a stray sentence, a display name). A literal-substring
///   match (not regex), same rule as [`GitRemoteBlocklistConfig`] so the
///   error is obvious to a tired agent at 3am.
///
/// The gate is **syntactic**, like its siblings: it cannot detect
/// paraphrase, implication, encoded, or composed disclosure (Rice-theorem
/// -adjacent undecidability — turing). It widens *decidable* coverage and
/// raises recall on the enumerated git-identity slot; it does not close the
/// semantic class. The `cs done` error message says so.
///
/// ```toml
/// # External-facing galaxy: published commits must carry only the
/// # canonical Noogram publish identity, never the operator's.
/// [publish_identity]
/// allowed_emails = ["bot@noogram.org"]
/// forbidden_substrings = ["operator@example.org", "example.org", "Tenant-Demo"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishIdentityConfig {
    /// Closed-codebook whitelist of author/committer emails permitted on
    /// published commits. Empty (the default) disables the whitelist layer;
    /// non-empty makes any other email a by-construction violation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_emails: Vec<String>,

    /// Literal substrings that must not appear in any author/committer name
    /// or commit message of a to-be-published commit. Empty (the default)
    /// disables the blacklist layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_substrings: Vec<String>,
}

impl PublishIdentityConfig {
    /// `true` when neither layer is configured — fast-path for projects
    /// (like cosmon itself) that do not need the publish-identity guard.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed_emails.is_empty() && self.forbidden_substrings.is_empty()
    }
}

/// Confidential-content publish gate — substring patterns that must not
/// appear in the *content* of external-facing artifacts at merge time.
///
/// V0 floor of the publish gate (ADR-128), the
/// sibling that [`PublishIdentityConfig`] (V1) widens from file content to
/// the git-identity channel. Where [`GitRemoteBlocklistConfig`] inspects
/// the worktree's *remotes*, this one inspects the *content* of a narrow
/// set of publish-bound files. The motivating threat: the fleet stamps the
/// operator's confidential fund name into external boilerplate (a README,
/// a footer, an index page) because the attribution slot is a *vacuum* —
/// with no authorized public name supplied, Type-1 retrieval emits the
/// highest-activation associate. A negative per-molecule guard ("don't
/// say X") fails by construction; the deterministic merge gate is the
/// structural floor that does not.
///
/// `cs done` reads every file matched by `publish_globs` in the merged
/// tree, case-folds it, and aborts the merge if any `forbidden_substrings`
/// entry appears. Both lists are matched as literal (case-folded)
/// substrings — same obvious-to-a-tired-agent rule as the remote guard.
///
/// **Scope is intentionally NARROW** — only files matched by
/// `publish_globs`, never a `grep -r` of the whole tree. Internal docs
/// that legitimately name the confidential entity must not false-positive.
///
/// ```toml
/// [confidential_blocklist]
/// forbidden_substrings = ["Tenant-Demo Research", "Tenant-Demo", "example.org"]
/// publish_globs = ["README*", "**/footer.html", "**/index.html", "site/**"]
/// ```
///
/// Empty `forbidden_substrings` (the default) means the gate is a no-op.
/// When substrings are present but `publish_globs` is omitted, the gate
/// scans [`ConfidentialBlocklistConfig::DEFAULT_PUBLISH_GLOBS`] — a
/// substrings-only config is no longer silently inert.
///
/// **Federation source.** Because the operator's
/// confidential fund name is the *same* secret across every galaxy, it must
/// not be re-typed (or committed) per galaxy. `cs done` merges the operator's
/// machine-wide `~/.config/cosmon/config.toml::[confidential_blocklist]` into
/// the per-galaxy config via [`ConfidentialBlocklistConfig::merged_with`], so
/// the name lives in ONE private file yet protects all galaxies.
///
/// **Residual risk (undecidable, do not over-promise):** this gate blocks
/// the *literal* confidential name, its registered aliases, the operator
/// domain, and the operator email. It does NOT detect paraphrase,
/// implication, encoded, or composed disclosure — those are
/// Rice-theorem-adjacent and remain the responsibility of human review.
/// The gate reduces the *realized* failure class to zero; it does not
/// reduce the *semantic* failure class.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfidentialBlocklistConfig {
    /// Substrings that must not appear in the content of any publish-bound
    /// file. Matched case-insensitively as literal substrings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_substrings: Vec<String>,

    /// Glob patterns selecting the narrow set of publish-bound files to
    /// scan. Matched against paths relative to the repository root (e.g.
    /// `README*`, `**/footer.html`, `site/**`). An empty list means the
    /// gate scans nothing — a no-op even if `forbidden_substrings` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish_globs: Vec<String>,

    /// When `true`, this galaxy is the HOME of the federation confidential
    /// name(s): it owns them and names itself freely. The machine-wide
    /// federation blocklist is therefore NOT merged into this galaxy's gate
    /// (see [`super`]'s `effective_confidential_blocklist`), so the owner is
    /// never blocked from writing its own name in its own internal files.
    ///
    /// Generic by design — no galaxy name is ever baked into cosmon. The
    /// owning galaxy *declares itself* with this flag. It leaks nothing (a
    /// boolean, never the secret), so it is safe in a committed per-galaxy
    /// `.cosmon/config.toml`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub owns_federation_secret: bool,
}

impl ConfidentialBlocklistConfig {
    /// Built-in publish-bound glob surfaces scanned when a project declares
    /// `forbidden_substrings` but no `publish_globs` of its own.
    ///
    /// Covers every artifact family where a maker / author / copyright /
    /// colophon block is emitted and would otherwise be filled from the
    /// operator's private context: rendered sites and landing pages
    /// (`README*`, `index.html`, `footer.html`, `site/**`) **and**
    /// academic-paper deliverables (`*.tex`, colophon / author / citation
    /// files, `paper.md`). The leak that re-opened this issue was a *paper*
    /// author block + colophon — surfaces the original README/site-only globs
    /// never scanned. Globs are not secret, so shipping them as a built-in default
    /// (unlike the substrings) leaks nothing.
    pub const DEFAULT_PUBLISH_GLOBS: &'static [&'static str] = &[
        "README*",
        "**/README*",
        "**/footer.html",
        "**/index.html",
        "site/**",
        "**/*.tex",
        "**/colophon*",
        "**/COLOPHON*",
        "**/AUTHORS*",
        "**/CITATION*",
        "**/paper.md",
        "**/paper/**/*.md",
    ];

    /// `true` when the gate cannot fire — no forbidden substrings to match.
    ///
    /// Emptiness keys **only** on `forbidden_substrings`.
    /// A project that sets substrings but no globs is
    /// no longer a silent no-op — it scans [`DEFAULT_PUBLISH_GLOBS`]. The old
    /// "either-empty short-circuits" rule was a foot-gun: it let a configured
    /// blocklist sit inert because the operator forgot the `publish_globs`
    /// line, which is exactly how the fund name kept reaching shipped
    /// artifacts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forbidden_substrings.is_empty()
    }

    /// The publish globs actually scanned: the project's own when non-empty,
    /// otherwise the built-in [`DEFAULT_PUBLISH_GLOBS`].
    ///
    /// `cs done`'s gate must consult this — never `publish_globs` directly —
    /// so a substrings-only config (e.g. a galaxy that inherits only the
    /// federation blocklist) still scans a sane publish surface.
    #[must_use]
    pub fn effective_publish_globs(&self) -> Vec<String> {
        if self.publish_globs.is_empty() {
            Self::DEFAULT_PUBLISH_GLOBS
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        } else {
            self.publish_globs.clone()
        }
    }

    /// Union-merge another blocklist into this one — order-preserving dedup
    /// union of both `forbidden_substrings` and `publish_globs`.
    ///
    /// The federation primitive: the operator's
    /// PRIVATE fund name is a federation-wide secret that must be blocked in
    /// *every* galaxy, yet must live in exactly ONE place that is never
    /// committed to a public repo. `cs done` loads the operator's
    /// machine-wide `~/.config/cosmon/config.toml::[confidential_blocklist]`
    /// and merges it into the per-galaxy config with this method, so the name
    /// is supplied once and inherited everywhere — closing the gap that left
    /// every galaxy with an empty, inert gate.
    #[must_use]
    pub fn merged_with(&self, other: &Self) -> Self {
        Self {
            forbidden_substrings: union_dedup(
                &self.forbidden_substrings,
                &other.forbidden_substrings,
            ),
            publish_globs: union_dedup(&self.publish_globs, &other.publish_globs),
            // Ownership is sticky: if either side owns the federation secret,
            // the merged config does too. (Not load-bearing on the owner path —
            // `effective_confidential_blocklist` short-circuits before merging
            // for an owner — but keeps `merged_with` total and surprise-free.)
            owns_federation_secret: self.owns_federation_secret || other.owns_federation_secret,
        }
    }
}

/// `skip_serializing_if` helper for the `owns_federation_secret` bool — keeps
/// the default (`false`) out of serialized configs, mirroring the
/// `Vec::is_empty` skips above so a default blocklist round-trips empty.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Order-preserving dedup union of two string slices — first-seen wins.
///
/// Shared by [`ConfidentialBlocklistConfig::merged_with`] to fold the
/// operator's machine-wide blocklist into a per-galaxy one without
/// duplicating entries the galaxy already declared.
fn union_dedup(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(a.len() + b.len());
    for s in a.iter().chain(b.iter()) {
        if !out.iter().any(|x| x == s) {
            out.push(s.clone());
        }
    }
    out
}

/// Public-attribution source of truth — the canonical, machine-readable
/// maker name, URL, and contactable address used wherever a shipped or
/// public artifact must name its author.
///
/// # Why a config block, not a hard-coded string
///
/// The recurring "attribution vacuum" (ADR-128):
/// when a worker reaches a "built by" / author / copyright slot and finds
/// it empty, the model "helpfully" fills it from context — and the nearest
/// context is the operator's *private* fund affiliation. You cannot win by
/// telling the model *don't say X* (negation is not Type-1-executable;
/// suppression keeps the token warm). You win by giving it *the right
/// thing to say* so X never surfaces — "put the right thing nearest." This
/// block is that positive supply: one canonical name, inherited by every
/// galaxy that runs cosmon, so the public maker name is changed in one
/// place and never re-typed per project.
///
/// # Closed-codebook angle
///
/// A single canonical attribution string also turns open-vocabulary leak
/// detection (hard, recall < 1) into closed-codebook validation (easy,
/// recall → 1): any string in an attribution slot that is not the
/// canonical codeword is a violation by construction. This block IS that
/// codebook — downstream detection consumes it as its whitelist.
///
/// ```toml
/// [attribution]
/// public_name      = "Noogram"
/// public_url       = "noogram.org"
/// contact          = "hello@noogram.org"
/// footer           = "© 2026 Noogram · noogram.org"
/// readme_byline    = "Built by Noogram — open agent infrastructure and AI tooling."
/// repo_description = "Maintained by Noogram (noogram.org). Open tooling for AI agent fleets."
/// authors_line     = "Noogram <hello@noogram.org>"
/// coauthor_name    = "Noogram"
/// coauthor_email   = "noreply@noogram.org"
/// ```
///
/// The byline fields resolve to a single canonical domain (`noogram.org`) —
/// the same domain the [`coauthor_email`](Self::coauthor_email) trailer commits
/// (delib-20260717-194b, F7). Aligning them removes the split where
/// [`directive`](Self::directive) taught the worker one domain while the trailer
/// stamped another. `coauthor_email` is unchanged (it was already `.org`).
///
/// # The `Co-Authored-By` trailer facet
///
/// [`coauthor_name`](Self::coauthor_name) + [`coauthor_email`](Self::coauthor_email)
/// drive [`coauthor_trailers`](Self::coauthor_trailers), which `cs done` stamps
/// onto the worker-produced commit so it credits the maker identity while
/// recording the *real* adapter that ran the molecule in the display name.
///
/// **Empty is a no-op.** When the section is absent (or `public_name` is
/// blank), [`AttributionConfig::is_empty`] returns `true`, no directive is
/// injected, and the worker bootstrap prompt is byte-identical to a
/// pre-attribution cosmon. This matches every project that predates the
/// config knob — the passive-helper discipline of the `CLAUDE_CONFIG_DIR`
/// propagation (CLAUDE.md §Multi-account).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttributionConfig {
    /// The public maker name (e.g. `"Noogram"`). This is the load-bearing
    /// field: when it is empty the whole block is treated as absent and no
    /// directive is injected.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub public_name: String,

    /// Public URL for the maker (e.g. `"noogram.org"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub public_url: String,

    /// Contactable, non-fund address (e.g. `"hello@noogram.org"`). A
    /// maker-name *with* a contactable address is the doctrine: a bare
    /// anonymous footer is its own vacuum that the next worker
    /// "helpfully" enriches from context.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contact: String,

    /// Copyright / footer line for shipped artifacts
    /// (e.g. `"© 2026 Noogram · noogram.org"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub footer: String,

    /// README byline — a true, forwardable claim
    /// (e.g. `"Built by Noogram — open agent infrastructure and AI tooling."`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub readme_byline: String,

    /// Repository description (e.g. for the GitHub "About" field).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo_description: String,

    /// `Authors:` / `Cargo.toml authors` line
    /// (e.g. `"Noogram <hello@noogram.org>"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub authors_line: String,

    /// Display name for the automatic `Co-Authored-By` git trailer stamped on
    /// commits the worker path produces (`cs done`'s artifact/state commits).
    ///
    /// When empty, [`public_name`](Self::public_name) is used instead — the
    /// canonical maker name is the natural co-author, so a galaxy that sets
    /// `public_name = "Noogram"` gets a Noogram co-author trailer with no
    /// extra typing. The *email* ([`coauthor_email`](Self::coauthor_email)) is
    /// the load-bearing field: without it no trailer is emitted (a
    /// `Co-Authored-By` line without a valid address is not honoured by git
    /// hosts), so this whole facet is opt-in and byte-identical to a
    /// pre-attribution cosmon when the email is blank.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub coauthor_name: String,

    /// Verified email for the automatic `Co-Authored-By` git trailer
    /// (e.g. `"noreply@noogram.org"`). This is the load-bearing field of the
    /// trailer facet: the primary trailer is emitted **only** when it is set.
    ///
    /// The same stable address is used whether or not an adapter witness is
    /// available. When one is available (`claude` / `gemini` / `mistral` / …,
    /// folded from the durable event log), it is recorded in parentheses in
    /// the maker display name, never synthesized into a model-specific email.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub coauthor_email: String,
}

impl AttributionConfig {
    /// `true` when no public maker name is configured — the fast-path for
    /// projects that have not adopted the attribution primitive. An empty
    /// block injects nothing and leaves the worker prompt byte-identical.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.public_name.is_empty()
    }

    /// Render the one-line attribution directive folded into the worker
    /// bootstrap prompt by `cs tackle`.
    ///
    /// Returns `None` when the block [`is_empty`](Self::is_empty) so the
    /// caller can keep the prompt byte-identical to the legacy shape. When
    /// present, the directive names the public maker so the model has the
    /// right token in hand *before* it reaches an attribution slot, and
    /// states the operator's fund affiliation is private — positive supply,
    /// not negated suppression.
    ///
    /// The `public_url` is rendered in parentheses only when set, so a
    /// name-only block still produces a clean sentence.
    #[must_use]
    pub fn directive(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let name = &self.public_name;
        // `` `Name` (url)`` when a URL is set, else just `` `Name` `` — a
        // name-only block still reads as a clean sentence.
        let named = if self.public_url.is_empty() {
            format!("`{name}`")
        } else {
            format!("`{name}` ({})", self.public_url)
        };
        Some(format!(
            "External attribution for this fleet is {named}. \
             Anywhere a maker, author, copyright holder, or \"built by\" name \
             is required in a shipped/public artifact, use `{name}`. The \
             operator's fund affiliation is PRIVATE and never appears in any \
             artifact.",
        ))
    }

    /// The single `Co-Authored-By` git trailer to stamp on a commit the worker
    /// path produces.
    ///
    /// The maker name lives in exactly one config block and is never re-typed
    /// per commit. When `adapter` names the agent that actually ran the
    /// molecule (folded from the durable event log, so it is the honest
    /// dispatch, never a guess), the format is
    /// `Co-Authored-By: {coauthor_name || public_name} ({adapter}) <{coauthor_email}>`.
    /// Without an adapter witness the parentheses are omitted. The address is
    /// always the configured stable maker address; model-specific synthetic
    /// addresses create phantom identities and are never emitted.
    ///
    /// # The email is load-bearing (fail-closed to empty)
    ///
    /// A `Co-Authored-By` line without a valid address is inert on every git
    /// host, so [`coauthor_email`](Self::coauthor_email) gates the whole
    /// facet: when it is empty the returned vec is empty and the commit message
    /// is byte-identical to a pre-attribution cosmon.
    ///
    /// The trailer key is spelled `Co-Authored-By` to match the hand-written
    /// bootstrap commits; git hosts recognize the key case-insensitively.
    #[must_use]
    pub fn coauthor_trailers(&self, adapter: Option<&str>) -> Vec<String> {
        // The address gates everything — no email, no honoured trailer.
        if self.coauthor_email.is_empty() {
            return Vec::new();
        }
        // Maker identity. Falls back to the canonical `public_name` so a
        //    galaxy that only set `public_name` still gets a named co-author.
        let name = if self.coauthor_name.is_empty() {
            self.public_name.as_str()
        } else {
            self.coauthor_name.as_str()
        };
        if name.is_empty() {
            return Vec::new();
        }

        let display = adapter
            .map(str::trim)
            .filter(|adapter| !adapter.is_empty())
            .map_or_else(|| name.to_owned(), |adapter| format!("{name} ({adapter})"));
        vec![format!(
            "Co-Authored-By: {display} <{}>",
            self.coauthor_email
        )]
    }
}

/// Whisper subsystem configuration (`cs whisper`).
///
/// Governs the perturbation port used to inject semantic text into a live
/// worker tmux pane. Every knob here is a safety dial: the command refuses
/// to paste unless the target pane is running one of the allowed foreground
/// commands, so a worker that crashed into its shell cannot be co-opted.
///
/// ```toml
/// [whisper]
/// allowed_commands = ["claude", "node"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WhisperConfig {
    /// Foreground process names (as reported by tmux `pane_current_command`)
    /// that `cs whisper` will target. Default: `["claude"]`.
    #[serde(default = "default_whisper_allowed_commands")]
    pub allowed_commands: Vec<String>,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            allowed_commands: default_whisper_allowed_commands(),
        }
    }
}

fn default_whisper_allowed_commands() -> Vec<String> {
    vec!["claude".to_owned()]
}

/// Archive subsystem configuration.
///
/// When enabled, terminated molecules (completed / collapsed / stuck) have
/// their durable artifacts copied under `.cosmon/state/archive/` so the
/// chain of reasoning survives worktree teardown and branch deletion.
///
/// Default: disabled. `#[non_exhaustive]` — future fields (compression,
/// filters) can be added without breaking callers.
///
/// ```toml
/// [archive]
/// enabled = true
///
/// [archive.retention]
/// keep_all = false
/// max_age_days = 180
/// max_total_mb = 512
/// keep_kinds = ["decision", "deliberation"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ArchiveConfig {
    /// Whether the archive subsystem is enabled. Default: `false`.
    #[serde(default)]
    pub enabled: bool,

    /// Retention policy — which archive entries `cs archive prune` may
    /// delete. Defaults to the safe "keep everything" policy.
    #[serde(default)]
    pub retention: RetentionConfig,
}

/// Archive retention policy — drives `cs archive prune`.
///
/// The policy is **purely permissive**: absent any rule, every archive
/// entry is kept. An entry is a *candidate for deletion* only when:
///
/// * `keep_all` is **false**, and
/// * it is older than `max_age_days` (when `max_age_days > 0`), or
///   the archive's total size exceeds `max_total_mb` (when
///   `max_total_mb > 0`, evicting oldest-first until under budget), and
/// * its [`MoleculeKind`] (snake-case string) is not in `keep_kinds`.
///
/// Hash-chain integrity is enforced *on top* of the policy: an entry
/// that is referenced as a parent (via `DecayedFrom`, `BlockedBy`, or
/// `MergedFrom`) by a kept entry is itself promoted to kept, even if
/// it would otherwise be a deletion candidate. See
/// `cosmon_state::archive::retention` for the execution model.
///
/// [`MoleculeKind`]: crate::kind::MoleculeKind
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetentionConfig {
    /// Safety switch: when `true`, `cs archive prune` never deletes
    /// anything. Default: `true` — surviving upgrades that land the
    /// retention subsystem without arming it.
    #[serde(default = "default_retention_keep_all")]
    pub keep_all: bool,

    /// Maximum age in days before an archive entry becomes a deletion
    /// candidate. `0` disables the age rule. Default: `0`.
    #[serde(default)]
    pub max_age_days: u32,

    /// Soft cap on total archive size in mebibytes. When the scan
    /// measures a larger total, the oldest non-kept entries are
    /// considered for deletion until the total drops under the cap.
    /// `0` disables the size rule. Default: `0`.
    #[serde(default)]
    pub max_total_mb: u64,

    /// Molecule kinds that must never be deleted, regardless of age or
    /// size pressure. Values are the lower-case `MoleculeKind` names
    /// (`"idea"`, `"task"`, `"decision"`, `"issue"`, `"signal"`,
    /// `"deliberation"`). Default: `["decision", "deliberation"]` —
    /// the two kinds that function as institutional memory.
    #[serde(default = "default_retention_keep_kinds")]
    pub keep_kinds: Vec<String>,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            keep_all: default_retention_keep_all(),
            max_age_days: 0,
            max_total_mb: 0,
            keep_kinds: default_retention_keep_kinds(),
        }
    }
}

const fn default_retention_keep_all() -> bool {
    true
}

fn default_retention_keep_kinds() -> Vec<String> {
    vec!["decision".to_owned(), "deliberation".to_owned()]
}

/// The `[project]` section of `config.toml`.
///
/// Contains the project identity. Generated by `cs init` and never changes
/// for the lifetime of the project.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSection {
    /// The unique project identifier (e.g. `cosmon-a1b2`).
    ///
    /// Generated by `cs init` as `{dirname}-{sha256_4hex}` of the canonical
    /// project root path. `None` only for legacy projects that have not been
    /// re-initialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,

    /// Tenant (noyau) this galaxy belongs to. ADR-063 layer-3 label and
    /// ADR-080 §8.1 multi-tenant routing axis.
    ///
    /// Set by `cs init --tenant <noyau>` at provisioning. Single-operator
    /// galaxies (no remote pilot exposure) leave this `None`. The field is
    /// purely advisory in the transactional core; the remote-pilot adapter
    /// (ADR-080) reads it to verify a galaxy actually corresponds to its
    /// declared tenant before routing requests in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noyau: Option<String>,
}

impl ProjectConfig {
    /// Parse a `config.toml` string into a [`ProjectConfig`].
    ///
    /// # Errors
    ///
    /// Returns an error if the TOML is invalid or contains unknown fields
    /// that cannot be deserialized.
    pub fn parse(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }

    /// Return the project ID or an error if it is not set.
    ///
    /// All commands should call this instead of accessing `project.project_id`
    /// directly — there is no silent fallback for a missing project ID.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error message if `project_id` is `None`.
    pub fn require_project_id(&self) -> Result<&ProjectId, String> {
        self.project.project_id.as_ref().ok_or_else(|| {
            "project_id not found in .cosmon/config.toml. \
             Run `cs init --upgrade` to establish project identity."
                .to_string()
        })
    }
}

/// Worker behavior configuration.
///
/// Controls what a worker does after completing its molecule work.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerConfig {
    /// What to do after a worker completes its task.
    ///
    /// - `commit` (default): commit changes locally only
    /// - `commit+push`: commit and push to remote
    /// - `commit+push+pr`: commit, push, and create a pull request
    #[serde(default)]
    pub on_complete: OnComplete,
}

/// The action a worker takes after completing its molecule.
///
/// Each variant is a superset of the previous: `CommitPush` implies
/// `Commit`, and `CommitPushPr` implies `CommitPush`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum OnComplete {
    /// Commit changes locally. This is the default — safe, reversible,
    /// and compatible with the transactional core model.
    #[default]
    Commit,
    /// Commit and push to the remote branch.
    CommitPush,
    /// Commit, push, and open a pull request.
    CommitPushPr,
}

impl fmt::Display for OnComplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit => write!(f, "commit"),
            Self::CommitPush => write!(f, "commit+push"),
            Self::CommitPushPr => write!(f, "commit+push+pr"),
        }
    }
}

impl FromStr for OnComplete {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "commit" => Ok(Self::Commit),
            "commit+push" => Ok(Self::CommitPush),
            "commit+push+pr" => Ok(Self::CommitPushPr),
            other => Err(format!(
                "invalid on_complete value: \"{other}\". \
                 Expected one of: \"commit\", \"commit+push\", \"commit+push+pr\""
            )),
        }
    }
}

impl Serialize for OnComplete {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for OnComplete {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// Surface auto-reconciliation settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfacesAutoConfig {
    /// Whether to automatically run `cs reconcile` after state-mutating
    /// operations. Default: `false` (explicit reconcile required).
    #[serde(default)]
    pub auto_reconcile: bool,
}

/// Documentation generation settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentationConfig {
    /// Whether documentation generation is enabled. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Helper for serde default that returns `true`.
fn default_true() -> bool {
    true
}

impl Default for DocumentationConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Lifecycle hooks — shell commands triggered at specific points in the
/// molecule lifecycle.
///
/// All hooks are optional. When absent, the corresponding lifecycle event
/// proceeds silently.
///
/// # Two hook classes — blocking vs advisory
///
/// The hooks here differ in *when* they run relative to the irreversible
/// merge, and therefore in whether they may abort:
///
/// - **`pre_done` — blocking.** Runs *before* the merge, while nothing has
///   landed yet. A non-zero exit **aborts the whole teardown** (no merge,
///   no worktree removal, no branch delete, no tmux kill) with the hook's
///   stderr as the reason. This is the primitive that lets a galaxy make a
///   falsifiable Definition-of-Done enforceable from *inside* the molecule
///   cycle — "a rigorous workflow must be able to refuse a DONE it cannot
///   prove" (showroom delib-20260701-bfdf, torvalds D1). Cosmon runs the
///   script and forwards its verdict; the *policy* lives in the galaxy's
///   script, not in cosmon.
/// - **`post_merge` — advisory.** Runs *after* the merge, when the merge has
///   already landed and is irreversible. A non-zero exit only *warns*; it
///   never aborts, because aborting would leave the merge half-honoured.
///
/// The asymmetry is structural, not stylistic: a hook may only abort while
/// the operation it guards is still reversible. `pre_done` is; `post_merge`
/// is not.
///
/// ```toml
/// [hooks]
/// pre_done   = "tools/ci/verify-functional-evidence.sh"
/// post_merge = "just install"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HooksConfig {
    /// Shell command to run *before* `cs done` merges a worker branch —
    /// a **blocking** gate on teardown.
    ///
    /// Runs from the repository root with the molecule ID appended as a
    /// trailing argument (so `tools/ci/verify.sh` is invoked as
    /// `sh -c 'tools/ci/verify.sh' -- <molecule-id>`, reachable as `$1`).
    /// If the command exits non-zero, `cs done` **aborts before the merge**
    /// and returns a hard error carrying the hook's stderr — the merge,
    /// worktree, branch, and tmux session are all left untouched, so the
    /// operator (or the worker) can fix the gap and rerun. If it exits zero,
    /// teardown proceeds.
    ///
    /// Idempotent: the hook is a pure read of repository/molecule state and
    /// mutates nothing, so a failed-then-retried `cs done` re-runs it with
    /// identical effect. Not set by default (backward compatible). The
    /// per-invocation kill-switch is `cs done --skip-pre-done-hook` (or the
    /// `COSMON_SKIP_PRE_DONE_HOOK` environment variable), reserved for the
    /// human operator overriding a known-good deliverable the script cannot
    /// see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_done: Option<String>,

    /// Shell command to run after `cs done` successfully merges a worker
    /// branch into the base branch.
    ///
    /// Runs from the repository root. If the command exits non-zero, a
    /// warning is emitted but teardown continues (the merge already landed).
    /// Not set by default (backward compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_merge: Option<String>,
}

/// Project verification gates — language-agnostic shell commands.
///
/// Each field is an opaque command string; cosmon does not inspect or parse
/// it, only forwards it to the worker. All fields are optional so projects
/// pick only the gates that apply to their stack.
///
/// ```toml
/// [gates]
/// build_command     = "cargo check --workspace"
/// test_command      = "cargo test --workspace"
/// lint_command      = "cargo clippy --workspace -- -D warnings"
/// format_command    = "cargo fmt --all -- --check"
/// typecheck_command = "mypy ."
/// setup_command     = "uv sync"
/// doc_command       = "RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct GatesConfig {
    /// Command that builds / compiles / type-checks the project (fast).
    /// Examples: `cargo check --workspace`, `uv sync`, `npm install`,
    /// `go build ./...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Command that runs the project's test suite.
    /// Examples: `cargo test --workspace`, `pytest`, `npm test`,
    /// `go test ./...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,

    /// Command that runs the project's linter with errors-as-failures.
    /// Examples: `cargo clippy --workspace -- -D warnings`, `ruff check .`,
    /// `eslint .`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint_command: Option<String>,

    /// Command that verifies code formatting without rewriting files.
    /// Examples: `cargo fmt --all -- --check`, `ruff format --check .`,
    /// `prettier --check .`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_command: Option<String>,

    /// Command that runs a standalone type-checker when the build step
    /// does not already do so. Examples: `mypy .`, `tsc --noEmit`.
    /// For Rust this is usually `None` — `cargo check` subsumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typecheck_command: Option<String>,

    /// Command that prepares the worktree before any other gate runs
    /// (install dependencies, sync virtualenv, etc.). Examples:
    /// `uv sync`, `npm ci`, `bundle install`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,

    /// Command that builds the project's API documentation with warnings
    /// promoted to errors. Examples:
    /// `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps`,
    /// `sphinx-build -W docs docs/_build`, `typedoc --treatAsError`.
    ///
    /// Exists as a slot of its own because a broken doc build is invisible to
    /// every other gate: `build_command` type-checks code but never resolves a
    /// doc link, `lint_command` runs the *code* linter (clippy is not rustdoc),
    /// and doc warnings are therefore only ever discovered by CI on the trunk —
    /// after the merge, when the worker that authored the link is long gone.
    /// Declaring it here moves that discovery back into the worker's own verify
    /// step, where the author is still present to fix it.
    ///
    /// The gate is comparatively slow (a full workspace doc build), which is why
    /// it is a worker-side gate rather than a rung of the `cs done` post-merge
    /// cascade: it is paid once per molecule, not once per merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_command: Option<String>,

    /// The top rung of the post-merge integrity cascade `cs done` runs on the
    /// **combined** tree, once the branch has merged but before `merged_at`
    /// makes the landing observable. When set, cosmon runs this command
    /// verbatim from the repo root and reads its exit code as the single
    /// verdict bit — exit 0 = verified, non-zero = the merge is rolled back to
    /// its pre-merge revision. This is how a galaxy declares *what* "the merged
    /// tree is sound" means; cosmon owns only *when* to ask (ADR-158,
    /// Transport ≠ Cognition — the integrity gate is the DAG edge in disguise:
    /// cosmon carries the bit, the declared command authors it).
    ///
    /// It is the *fast* combined-compile/type-check tier, hard-bounded by the
    /// post-merge gate timeout — NOT a test run (heavy assurance is the
    /// operator's explicit `cs validate` gesture). Examples:
    /// `cargo check --workspace --all-targets`,
    /// `python -m compileall . && mypy .`, `go build ./... && go vet ./...`.
    ///
    /// Falls back to [`build_command`](Self::build_command) only after the
    /// zero-config cargo-metadata auto-detect declines (the repo is not a Cargo
    /// workspace cargo resolves from the root). Fallback is composition, not a
    /// rename: `build_command` is the worker's cheap pre-completion self-check,
    /// this is the authoritative merge-time gate; a galaxy may want both to
    /// differ. Cosmon's own config leaves this unset so it rides the
    /// cargo-metadata rung, unchanged.
    ///
    /// Security: this is repo-supplied shell, so `cs done` refuses to exec it
    /// in an untrusted clone (B5, RCE-by-clone) — `cs trust` is the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity_command: Option<String>,

    /// Promote a post-merge `Unverified` outcome from LOUD-fail-OPEN (the merge
    /// lands, witnessed durably as `ok:unverified` in `events.jsonl`, exit 0) to
    /// **fail-CLOSED** (roll the merge back to its pre-merge revision and exit
    /// non-zero). Defaults to `false`: on ship day a galaxy that has declared no
    /// verification — no [`integrity_command`](Self::integrity_command), no
    /// Cargo workspace cargo resolves, no [`build_command`](Self::build_command)
    /// — still merges, but the gap is a loud, countable witness, never a silent
    /// clean `ok`. An operator who wants "no declared verification ⇒ no merge"
    /// sets this to `true`.
    ///
    /// This is the per-galaxy override for delib-20260714-7605's D1 tension. The
    /// panel *recommended* an expected-gate discriminator (fail-closed when a
    /// gate is *expected* but a code diff went unchecked, fail-open-loud when
    /// nothing was declared) but flagged the default as an operator decision, not
    /// one to resolve silently. cosmon therefore ships the conservative
    /// fail-open-loud default (backward-compatible with inc-1's binding
    /// acceptance test) and exposes this bool as the lever; the `expected`
    /// discriminator is *recorded* in the advisory for legibility. See ADR-158.
    #[serde(default)]
    pub fail_closed_on_unverified: bool,
}

impl GatesConfig {
    /// Return `true` when no gate command is configured.
    ///
    /// Callers (notably `cs tackle`) use this to decide whether to render
    /// a concrete gate list in the worker prompt or fall back to a neutral
    /// instruction.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.build_command.is_none()
            && self.test_command.is_none()
            && self.lint_command.is_none()
            && self.format_command.is_none()
            && self.typecheck_command.is_none()
            && self.setup_command.is_none()
            && self.doc_command.is_none()
    }
}

/// The built-in Worker-Spawn Port Adapter *floor* — the adapter `cs
/// tackle` resolves to when no `--adapter` flag, no formula-step pin, no
/// `$COSMON_DEFAULT_ADAPTER`, and no `[adapters.default]` (per-galaxy or
/// global) names one.
///
/// **Single source of truth.** This is the *only* place the floor name is
/// spelled. Every resolution site (the dispatch registry, the resolver's
/// terminal arm, the local-fallback record) references this constant
/// instead of restating the literal, so the value cannot drift across
/// sites — "no config = local autonomy" is config-undeletable *and*
/// copy-undeletable by construction. Doc comments and prose that need to
/// name the floor point here rather than restate `"local"`.
pub const BUILTIN_FLOOR_ADAPTER: &str = "local";

/// `[adapters]` — Worker-Spawn Port Adapter inventory (ADR-097 / C6).
///
/// Replaces the in-code dispatch table from C5 with a TOML-driven
/// lookup. The schema is intentionally additive: existing
/// `.cosmon/config.toml` files without an `[adapters]` section keep
/// working — `cs tackle` falls back to the built-in `"claude"`
/// Adapter (recorded as
/// [`AdapterSelectionSource::Default`](crate::event_v2::AdapterSelectionSource::Default)
/// on the `AdapterSelected` event).
///
/// ```toml
/// [adapters]
/// default = "claude"
///
/// [adapters.claude]
/// pane_signatures = ["claude"]
/// briefing_format = "markdown"
///
/// [adapters.aider]
/// pane_signatures = ["aider", "python", "python3.11"]
/// briefing_format = "markdown"
/// extra_args = ["--no-auto-commits", "--yes-always"]
/// ```
///
/// **No new state file (forgemaster §4.5 / ADR-095 RR-2).** The
/// inventory is config; the dynamic selection is the
/// [`AdapterSelected`](crate::event_v2::EventV2::AdapterSelected)
/// event. C6 does not introduce a third surface.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdaptersConfig {
    /// The Adapter name `cs tackle` uses when no `--adapter` flag is passed
    /// and the formula step pins no adapter (the per-galaxy *policy* locus).
    /// `None` means "fall through to the built-in
    /// floor [`BUILTIN_FLOOR_ADAPTER`]" — the config-undeletable invariant
    /// that no config = local autonomy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Per-Adapter entries keyed by Adapter name (`claude`, `aider`, …).
    ///
    /// Serde flattens the rest of the `[adapters]` table into this map
    /// using `#[serde(flatten)]`, so `[adapters.aider]` deserialises
    /// into `entries["aider"]`.
    #[serde(flatten, default)]
    pub entries: std::collections::BTreeMap<String, AdapterEntry>,
}

/// One row in the `[adapters]` table — an Adapter's static inventory
/// signature (ADR-097 / C6, ADR-079 §6).
///
/// Every field is optional so a sparse `.cosmon/config.toml` is valid
/// (`[adapters.foo]` with no body declares an Adapter name with all
/// defaults). Adapter constructors read the entries they recognise and
/// fall back to compile-time defaults for the rest — forgemaster §3.1
/// constructor-injection discipline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterEntry {
    /// `pane_current_command` signatures the propulsion / whisper
    /// gates accept for this Adapter (ADR-079 §6). Empty means
    /// "fall back to the Adapter's compile-time default".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane_signatures: Vec<String>,

    /// Format the Adapter expects for the worker's bootstrap briefing.
    /// Today only `"markdown"` is meaningful; the field exists so a
    /// future API-driven Adapter can declare `"json"` or similar
    /// without a schema change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub briefing_format: Option<String>,

    /// Additional shell arguments the Adapter prepends to every worker
    /// invocation (e.g. Aider's `--no-auto-commits --yes-always`).
    /// Empty means "no extra args".
    ///
    /// For the `codex` adapter in interactive mode this row, when
    /// non-empty, **replaces** the built-in autonomy / inline-scrollback
    /// defaults (`cosmon_transport::codex::DEFAULT_INTERACTIVE_ARGS`)
    /// verbatim — the escape hatch for an installation that needs a
    /// different sandbox posture or model flag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,

    /// External-CLI launch mode. Today only the `codex` adapter reads this
    /// row.
    ///
    /// `"interactive"` (the absence-default) spawns codex's steerable TUI —
    /// **parity with the `claude` adapter**: the pane stays open after the
    /// task, the worker is driveable by `cs whisper`, and completion is the
    /// worker calling `cs evolve`/`cs complete` rather than the pane dying.
    /// `"exec"` selects the legacy non-interactive `codex exec '<prompt>'`
    /// fire-and-forget batch mode. Any unrecognised value fails *open* to
    /// interactive (see `cosmon_transport::codex::CodexMode::from_config_str`).
    /// Absent means "the adapter's own default" (interactive for codex);
    /// other adapters ignore this row.
    ///
    /// ```toml
    /// [adapters.codex]
    /// mode = "exec"   # opt back into the batch fire-and-forget path
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// Name of the environment variable that holds the Adapter's API
    /// credential. Direct-API adapters (`openai`, `anthropic`) read this
    /// **before** falling back to the historical vendor defaults
    /// (`OPENAI_API_KEY`, `XAI_API_KEY`, `MOONSHOT_API_KEY`,
    /// `ANTHROPIC_API_KEY`).
    ///
    /// The structural reason this exists: a free-rider build (one
    /// `openai`-named Adapter aimed at xAI / Moonshot / `DeepSeek` via
    /// `base_url` override) used to depend on the operator manually
    /// `env -u OPENAI_API_KEY XAI_API_KEY=… cs tackle`. Without an
    /// explicit `api_key_env`, the first non-empty vendor key in the
    /// shell silently won — a request meant for `api.x.ai` could leak
    /// to `api.openai.com` with a Grok model identifier (a 404, not
    /// data exfiltration, but the silent-failure class is the issue).
    /// Declaring `api_key_env = "XAI_API_KEY"` in
    /// `[adapters.openai]` makes the choice authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// Base URL the Adapter POSTs against. Overrides the in-code
    /// `DEFAULT_BASE_URL` for the matching provider. Use cases: xAI
    /// (`https://api.x.ai`), Moonshot (`https://api.moonshot.ai`),
    /// a local proxy, or a test double. Empty / absent means
    /// "use the provider's compile-time default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Default model identifier the Adapter passes to the provider's
    /// chat-completions endpoint when no explicit `--model` flag is in
    /// scope. Overrides the per-provider compile-time default
    /// (`gpt-4o-mini` for `OpenAI`, `claude-opus-4-7` for Anthropic) and
    /// the legacy env-var path (`OPENAI_MODEL` / `ANTHROPIC_MODEL`).
    /// Absent means "use the provider's vendor default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,

    /// Operator-declared **strong cost-class** — the set of model ids for this
    /// adapter that drain credits fast enough to warrant the fail-closed
    /// ceiling and the `⚡strong` surface glyph (delib-20260704-b476 / C4).
    ///
    /// **This is a cost-class annotation, not a validity table.** It is
    /// *fail-open*: an id absent from this set is treated as non-strong
    /// (cheap/safe) by default, and the id itself is still carried opaquely
    /// for legality (the backend judges legality — von-neumann's verdict C).
    /// The set is consulted only on the cost/safety axis:
    /// [`crate::model_budget::is_strong_model`] classifies a resolved pin, the
    /// per-galaxy [`ModelBudgetConfig`] ceiling counts strong dispatches over
    /// it, and `cs reconcile --check` rejects a `default_model` that lands in
    /// it (Ghost A — a config may only *downgrade*, never default to strong).
    ///
    /// Empty (the absence-default) means "no strong models declared" — nothing
    /// trips the ceiling and no dispatch carries the glyph, which is the right
    /// behaviour for a galaxy that has not opted into cost governance.
    ///
    /// ```toml
    /// [adapters.claude]
    /// strong = ["claude-fable-5", "claude-opus-4-8"]
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strong: Vec<String>,

    /// Upper bound on the number of tokens the Adapter asks the provider
    /// to generate per completion.
    ///
    /// Today only the in-process `llama-cpp` Adapter reads this row: its
    /// spawn path used to hard-code a 256-token cap, which silently
    /// truncated reasoning ("thinking") models — Qwen3 emits 200+ tokens
    /// inside a `<think>…</think>` block *before* the user-visible answer,
    /// so 256 tokens were exhausted mid-thought and the final reply never
    /// arrived. Declaring
    /// `max_tokens` here lifts the cap to whatever the model's context
    /// window allows; the resolver clamps the value against the
    /// provider's advertised `max_context`
    /// ([`Capabilities::can_fit`](../../cosmon_provider/struct.Capabilities.html))
    /// so an over-eager config can never trip the context budget.
    /// Absent means "use the Adapter's compile-time default" (2048 for
    /// `llama-cpp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Per-request HTTP timeout, in **seconds**, for a Direct-API adapter
    /// that forwards completions over HTTP — the `local` / `ollama` floor
    /// today (task-20260707-7d27, academy banc Mode C, hole #3).
    ///
    /// Background: `cosmon_provider::OpenAIProvider` hard-codes a 60 s
    /// per-request timeout in every constructor, and the `local` floor
    /// spawn path never overrode it. On a single-GPU oracle
    /// (`ollama-g5`: 48 GB ≈ one 120 B model), a **cold** load of a
    /// 120 B model takes minutes, and a reasoning-model generation or a
    /// queued request can exceed 60 s even when warm — so the floor died
    /// with a transport-timeout (SF-1) at *exactly* 60 s, systematically,
    /// long before the model could answer. That is the failure that
    /// blocked Mode C of the academy bench.
    ///
    /// Resolution order for the floor: `[adapters.<name>].timeout_secs`
    /// (this field) → `COSMON_LOCAL_TIMEOUT` (seconds) → the compile-time
    /// floor default (600 s / 10 min — generous enough to absorb a cold
    /// 120 B load without masking a genuinely hung daemon forever).
    /// Absent means "use that env / compile-time default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Context window (in tokens) the in-process `llama-cpp` Adapter
    /// allocates for the `llama_context` KV-cache, overriding the
    /// model-derived default.
    ///
    /// Absent (the common case) means "let cosmon size the window from
    /// the GGUF header": `min(n_ctx_train, 16384)` — the model's trained
    /// context length, capped for memory. The previous behaviour
    /// hard-coded 4096, which on a Qwen3-class model (trained at 32 768)
    /// throttled the effective prompt budget to `4096 − max_tokens` and
    /// tripped a spurious context overflow on a normal cosmon briefing.
    ///
    /// Declaring `n_ctx` here pins the window explicitly: raise it toward
    /// the model's full trained length (e.g. `32768` for Qwen3-8B) when a
    /// workload needs a longer prompt budget, or lower it to bound
    /// KV-cache memory on constrained hardware. The value also becomes the
    /// provider's advertised `max_context`, so `max_tokens` is clamped
    /// against it. Only the `llama-cpp` Adapter reads this row; a `0` is
    /// ignored (it would mean a zero-width window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx: Option<u32>,

    /// Who runs the agent loop for this Adapter — the per-Adapter
    /// [`LoopOwnership`](crate::spawn_seam::LoopOwnership) axis
    /// (ADR-103).
    ///
    /// `"external"` (the absence-default) means cosmon spawns an
    /// external binary that owns its own loop (`claude` / `aider` /
    /// `codex`). `"cosmon"` means the loop runs in-process inside
    /// `cosmon-agent-harness` (`openai` / `anthropic` Direct-API
    /// adapters). Built-in names override the default from the
    /// [`BUILT_IN_AXES`](crate::spawn_seam) table — this row is the
    /// escape hatch for TOML-only adapters declared at the
    /// installation perimeter.
    ///
    /// Absence is the legacy contract: hand-authored
    /// `[adapters.<name>]` rows that pre-date ADR-103 keep their
    /// observable behaviour (`External`) without touching the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,

    /// Who runs the model server an Adapter forwards completions to —
    /// the per-Adapter
    /// [`RuntimeOwnership`](crate::spawn_seam::RuntimeOwnership) axis
    /// (ADR-104, successor to ADR-103).
    ///
    /// `"operated"` means cosmon (or the operator) installs,
    /// version-pins, restarts, and reads logs from the model server
    /// — a vllm-mlx sidecar on `localhost:8000` (Path B) or a `pub(crate)`
    /// Rust library in cosmon's address space (Path A v0).
    /// `"vendor"` means a third-party endpoint cosmon merely
    /// consumes (`api.openai.com`, `api.anthropic.com`).
    ///
    /// This row is the per-installation override path: the same
    /// adapter name (`openai`) resolves to different runtime cells
    /// depending on the operator's `.cosmon/config.toml`. Built-in
    /// names default to `Vendor` (see
    /// [`runtime_for_built_in`](crate::spawn_seam::runtime_for_built_in));
    /// declaring `runtime = "operated"` opts into the
    /// operator-supervised path, typically alongside `base_url =
    /// "http://localhost:8000"`.
    ///
    /// Absence is the honest default: hand-authored
    /// `[adapters.<name>]` rows that pre-date ADR-104 keep their
    /// observable behaviour (`Vendor`) without touching the file —
    /// because operators running a sidecar are the ones who opt in,
    /// not the other way around.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// Override the Adapter's built-in system prompt with operator-supplied
    /// text. Used by the in-process `llama-cpp` Adapter to tune behaviour
    /// per GGUF model.
    ///
    /// Background: the
    /// `LlamaProvider` ships a single hard-coded `SYSTEM_PROMPT` that was
    /// dialled in against Qwen3-Instruct. Other model families on the same
    /// Path A drifted:
    ///
    /// - **Llama-3.3-70B-Instruct** captured the autonomous-work-mode
    ///   template from the briefing context and hallucinated a fictional
    ///   `cs complete` invocation instead of writing the requested line.
    /// - **Qwen3-Coder-30B-A3B-Instruct** emitted an `exec_command`
    ///   tool-call malformed against the expected fence shape.
    ///
    /// Declaring `system_prompt_override = "…"` in
    /// `[adapters."llama-coder"]` (or any side-config) replaces the default
    /// system message entirely. The dynamic `<tools>…</tools>` tool-schema
    /// block is still appended (the model would have no way to discover
    /// the registry otherwise), so the override only re-writes the role
    /// text and trust fences — not the tool advertisement.
    ///
    /// Absent (the common case for Qwen3-Instruct families) means "use
    /// the Adapter's compile-time default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_override: Option<String>,

    /// Per-model chat-template kwargs (e.g. Qwen3's `enable_thinking`,
    /// model-specific tool-fence selectors).
    ///
    /// Plumbed end-to-end from `.cosmon/config.toml` down to
    /// `cosmon_llama::Model::apply_chat_template_with_kwargs`. The
    /// llama.cpp `llama_chat_apply_template` C API is **not** a Jinja
    /// interpreter (`include/llama.h`: "does not use a jinja parser"),
    /// so the FFI cannot consume these kwargs natively — but the
    /// Adapter inspects them on the Rust side and can wire structural
    /// equivalents where one exists (the `enable_thinking=false` seed is
    /// the canonical case; see `THINK_DISABLE_SEED` in
    /// `cosmon_provider::llama`).
    ///
    /// `BTreeMap` (not `HashMap`) so iteration order in error/diagnostic
    /// strings is deterministic across runs — same discipline as
    /// `AdaptersConfig::entries`. Empty (the absence-default) means "no
    /// kwargs", which is the right behaviour for every Qwen3-Instruct
    /// GGUF currently in cosmon's fleet.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub chat_template_kwargs: std::collections::BTreeMap<String, String>,
}

impl AdaptersConfig {
    /// Resolve the Adapter name `cs tackle` should use when the
    /// operator did not pass `--adapter`. Returns `None` when no
    /// `default` is configured — the caller falls back to the
    /// built-in `"claude"`.
    #[must_use]
    pub fn default_adapter(&self) -> Option<&str> {
        self.default.as_deref()
    }

    /// Look up a single Adapter entry by name.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&AdapterEntry> {
        self.entries.get(name)
    }

    /// All declared Adapter names, sorted lexicographically.
    ///
    /// Used by the `AdapterNotFound` error in `cosmon-transport` to
    /// build a useful diagnostic (`available: ["aider", "claude"]`).
    #[must_use]
    pub fn available_names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ProjectConfig::default();
        assert_eq!(config.worker.on_complete, OnComplete::Commit);
        assert!(!config.surfaces.auto_reconcile);
        assert!(config.documentation.enabled);
        assert_eq!(config.energy.default_step_budget, 100);
    }

    #[test]
    fn test_parse_energy_config() {
        let config = ProjectConfig::parse(
            r"
            [energy]
            default_step_budget = 25
            ",
        )
        .unwrap();
        assert_eq!(config.energy.default_step_budget, 25);
    }

    #[test]
    fn test_energy_config_default_when_section_absent() {
        let config = ProjectConfig::parse(
            r#"
            [worker]
            on_complete = "commit"
            "#,
        )
        .unwrap();
        assert_eq!(config.energy.default_step_budget, 100);
    }

    #[test]
    fn test_energy_config_zero_disables_breaker() {
        let config = ProjectConfig::parse(
            r"
            [energy]
            default_step_budget = 0
            ",
        )
        .unwrap();
        assert_eq!(config.energy.default_step_budget, 0);
    }

    #[test]
    fn test_parse_empty_toml() {
        let config = ProjectConfig::parse("").unwrap();
        assert_eq!(config, ProjectConfig::default());
    }

    #[test]
    fn test_provider_bias_absent_is_empty_default() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(config.provider_bias.is_empty());
        assert!(config
            .provider_bias
            .effective()
            .additional_readers
            .is_empty());
    }

    #[test]
    fn test_provider_bias_baseline_and_profiles_parse() {
        let config = ProjectConfig::parse(
            r#"
            [provider_bias]
            additional_readers = ["openai"]
            min_distinct_provider_endpoints = 2

            [provider_bias.profiles.security]
            additional_falsifiers = ["deepseek"]
            min_distinct_provider_endpoints = 3
            "#,
        )
        .unwrap();
        assert_eq!(
            config.provider_bias.baseline.additional_readers,
            vec!["openai"]
        );
        assert_eq!(
            config
                .provider_bias
                .baseline
                .min_distinct_provider_endpoints,
            Some(2)
        );
        assert!(config.provider_bias.profiles.contains_key("security"));
    }

    #[test]
    fn test_provider_bias_effective_is_monotone_union() {
        let config = ProjectConfig::parse(
            r#"
            [provider_bias]
            additional_readers = ["openai"]
            min_distinct_provider_endpoints = 2

            [provider_bias.profiles.security]
            additional_readers = ["deepseek"]
            additional_falsifiers = ["xai"]
            min_distinct_provider_endpoints = 3
            "#,
        )
        .unwrap();
        let eff = config.provider_bias.effective();
        // Readers are the UNION (sorted), never the profile alone.
        assert_eq!(eff.additional_readers, vec!["deepseek", "openai"]);
        assert_eq!(eff.additional_falsifiers, vec!["xai"]);
        // Floor is the MAX (add-only), never overwritten by the profile.
        assert_eq!(eff.min_distinct_provider_endpoints, Some(3));
    }

    #[test]
    fn test_provider_bias_profile_cannot_lower_floor() {
        // A profile declaring a SMALLER floor than the baseline cannot lower
        // the effective floor — the join is `max`, so the downgrade is
        // inexpressible even when a profile "asks" for it.
        let config = ProjectConfig::parse(
            r"
            [provider_bias]
            min_distinct_provider_endpoints = 3

            [provider_bias.profiles.lax]
            min_distinct_provider_endpoints = 1
            ",
        )
        .unwrap();
        assert_eq!(
            config
                .provider_bias
                .effective()
                .min_distinct_provider_endpoints,
            Some(3)
        );
    }

    #[test]
    fn test_parse_full_config() {
        let config = ProjectConfig::parse(
            r#"
            [project]
            project_id = "cosmon-a1b2"

            [worker]
            on_complete = "commit+push+pr"

            [surfaces]
            auto_reconcile = true

            [documentation]
            enabled = false
            "#,
        )
        .unwrap();
        assert_eq!(
            config.project.project_id,
            Some(crate::id::ProjectId::new("cosmon-a1b2").unwrap())
        );
        assert_eq!(config.worker.on_complete, OnComplete::CommitPushPr);
        assert!(config.surfaces.auto_reconcile);
        assert!(!config.documentation.enabled);
    }

    #[test]
    fn test_parse_partial_config() {
        let config = ProjectConfig::parse(
            r#"
            [worker]
            on_complete = "commit+push"
            "#,
        )
        .unwrap();
        assert_eq!(config.worker.on_complete, OnComplete::CommitPush);
        assert!(!config.surfaces.auto_reconcile);
        assert!(config.documentation.enabled);
    }

    #[test]
    fn test_on_complete_display() {
        assert_eq!(OnComplete::Commit.to_string(), "commit");
        assert_eq!(OnComplete::CommitPush.to_string(), "commit+push");
        assert_eq!(OnComplete::CommitPushPr.to_string(), "commit+push+pr");
    }

    #[test]
    fn test_on_complete_from_str() {
        assert_eq!("commit".parse::<OnComplete>().unwrap(), OnComplete::Commit);
        assert_eq!(
            "commit+push".parse::<OnComplete>().unwrap(),
            OnComplete::CommitPush
        );
        assert_eq!(
            "commit+push+pr".parse::<OnComplete>().unwrap(),
            OnComplete::CommitPushPr
        );
        assert!("invalid".parse::<OnComplete>().is_err());
    }

    #[test]
    fn test_on_complete_serde_roundtrip() {
        // Test roundtrip through a wrapper struct (TOML requires a table at top level).
        for variant in [
            OnComplete::Commit,
            OnComplete::CommitPush,
            OnComplete::CommitPushPr,
        ] {
            let config = WorkerConfig {
                on_complete: variant,
            };
            let serialized = toml::to_string(&config).unwrap();
            let deserialized: WorkerConfig = toml::from_str(&serialized).unwrap();
            assert_eq!(variant, deserialized.on_complete);
        }
    }

    #[test]
    fn test_require_project_id_present() {
        let config = ProjectConfig::parse(
            r#"
            [project]
            project_id = "myproj-f00d"
            "#,
        )
        .unwrap();
        let pid = config.require_project_id().unwrap();
        assert_eq!(pid.as_str(), "myproj-f00d");
    }

    #[test]
    fn test_require_project_id_missing() {
        let config = ProjectConfig::default();
        assert!(config.require_project_id().is_err());
    }

    #[test]
    fn test_project_section_absent_defaults_to_none() {
        let config = ProjectConfig::parse("").unwrap();
        assert_eq!(config.project.project_id, None);
    }

    #[test]
    fn test_project_id_serde_roundtrip_in_config() {
        let mut config = ProjectConfig::default();
        config.project.project_id = Some(crate::id::ProjectId::new("cosmon-abcd").unwrap());
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config.project.project_id, deserialized.project.project_id);
    }

    #[test]
    fn test_parse_hooks_config() {
        let config = ProjectConfig::parse(
            r#"
            [hooks]
            post_merge = "just install"
            "#,
        )
        .unwrap();
        assert_eq!(config.hooks.post_merge, Some("just install".to_owned()));
    }

    #[test]
    fn test_parse_pre_done_hook() {
        let config = ProjectConfig::parse(
            r#"
            [hooks]
            pre_done   = "tools/ci/verify-functional-evidence.sh"
            post_merge = "just install"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.hooks.pre_done,
            Some("tools/ci/verify-functional-evidence.sh".to_owned())
        );
        assert_eq!(config.hooks.post_merge, Some("just install".to_owned()));
    }

    #[test]
    fn test_pre_done_hook_serde_roundtrip() {
        let mut config = ProjectConfig::default();
        config.hooks.pre_done = Some("verify.sh".to_owned());
        let serialized = toml::to_string(&config).unwrap();
        let deserialized = ProjectConfig::parse(&serialized).unwrap();
        assert_eq!(deserialized.hooks.pre_done, Some("verify.sh".to_owned()));
    }

    #[test]
    fn test_hooks_default_is_none() {
        let config = ProjectConfig::default();
        assert_eq!(config.hooks.post_merge, None);
        assert_eq!(config.hooks.pre_done, None);
    }

    #[test]
    fn test_hooks_absent_section_defaults() {
        let config = ProjectConfig::parse(
            r#"
            [worker]
            on_complete = "commit"
            "#,
        )
        .unwrap();
        assert_eq!(config.hooks.post_merge, None);
        assert_eq!(config.hooks.pre_done, None);
    }

    #[test]
    fn test_gates_default_is_empty() {
        let config = ProjectConfig::default();
        assert!(config.gates.is_empty());
        assert_eq!(config.gates.build_command, None);
        assert_eq!(config.gates.test_command, None);
    }

    #[test]
    fn test_parse_gates_config() {
        let config = ProjectConfig::parse(
            r#"
            [gates]
            build_command = "cargo check --workspace"
            test_command = "cargo test --workspace"
            lint_command = "cargo clippy --workspace -- -D warnings"
            format_command = "cargo fmt --all -- --check"
            typecheck_command = "mypy ."
            setup_command = "uv sync"
            "#,
        )
        .unwrap();
        assert!(!config.gates.is_empty());
        assert_eq!(
            config.gates.build_command.as_deref(),
            Some("cargo check --workspace")
        );
        assert_eq!(
            config.gates.test_command.as_deref(),
            Some("cargo test --workspace")
        );
        assert_eq!(
            config.gates.lint_command.as_deref(),
            Some("cargo clippy --workspace -- -D warnings")
        );
        assert_eq!(
            config.gates.format_command.as_deref(),
            Some("cargo fmt --all -- --check")
        );
        assert_eq!(config.gates.typecheck_command.as_deref(), Some("mypy ."));
        assert_eq!(config.gates.setup_command.as_deref(), Some("uv sync"));
    }

    #[test]
    fn test_gates_partial() {
        let config = ProjectConfig::parse(
            r#"
            [gates]
            test_command = "pytest"
            "#,
        )
        .unwrap();
        assert!(!config.gates.is_empty());
        assert_eq!(config.gates.test_command.as_deref(), Some("pytest"));
        assert_eq!(config.gates.build_command, None);
    }

    #[test]
    fn test_gates_serde_roundtrip() {
        let original = GatesConfig {
            build_command: Some("cargo check".to_owned()),
            test_command: Some("cargo test".to_owned()),
            lint_command: None,
            format_command: None,
            typecheck_command: None,
            setup_command: None,
            doc_command: Some("cargo doc --workspace --no-deps".to_owned()),
            integrity_command: Some("cargo check --workspace --all-targets".to_owned()),
            fail_closed_on_unverified: true,
        };
        let serialized = toml::to_string(&original).unwrap();
        let deserialized: GatesConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    /// The two ADR-158 fields default cleanly for a legacy config that predates
    /// them: `integrity_command` is `None` (so `cs done` rides the cargo rung)
    /// and `fail_closed_on_unverified` is `false` (fail-open-loud default). This
    /// is the backward-compatibility proof — an old `[gates]` block still parses.
    #[test]
    fn test_gates_new_fields_default_for_legacy_config() {
        let legacy = "build_command = \"cargo check\"\n";
        let gates: GatesConfig = toml::from_str(legacy).unwrap();
        assert_eq!(gates.integrity_command, None);
        assert!(!gates.fail_closed_on_unverified);
    }

    #[test]
    fn test_archive_default_disabled() {
        let config = ProjectConfig::default();
        assert!(!config.archive.enabled);
    }

    #[test]
    fn test_parse_archive_config() {
        let config = ProjectConfig::parse(
            "
            [archive]
            enabled = true
            ",
        )
        .unwrap();
        assert!(config.archive.enabled);
    }

    #[test]
    fn test_archive_absent_section_defaults() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(!config.archive.enabled);
    }

    #[test]
    fn test_archive_serde_roundtrip() {
        let original = ArchiveConfig {
            enabled: true,
            retention: RetentionConfig::default(),
        };
        let serialized = toml::to_string(&original).unwrap();
        let deserialized: ArchiveConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_retention_defaults_are_safe() {
        // Default retention must keep everything — landing the subsystem
        // without a config change must never delete an operator's work.
        let config = ProjectConfig::default();
        assert!(config.archive.retention.keep_all);
        assert_eq!(config.archive.retention.max_age_days, 0);
        assert_eq!(config.archive.retention.max_total_mb, 0);
        assert_eq!(
            config.archive.retention.keep_kinds,
            vec!["decision".to_owned(), "deliberation".to_owned()],
        );
    }

    #[test]
    fn test_parse_archive_retention_section() {
        let config = ProjectConfig::parse(
            r#"
            [archive]
            enabled = true

            [archive.retention]
            keep_all = false
            max_age_days = 90
            max_total_mb = 256
            keep_kinds = ["decision"]
            "#,
        )
        .unwrap();
        assert!(config.archive.enabled);
        assert!(!config.archive.retention.keep_all);
        assert_eq!(config.archive.retention.max_age_days, 90);
        assert_eq!(config.archive.retention.max_total_mb, 256);
        assert_eq!(config.archive.retention.keep_kinds, vec!["decision"]);
    }

    #[test]
    fn test_archive_absent_retention_defaults() {
        // [archive] present without [archive.retention] still gets the
        // safe default retention.
        let config = ProjectConfig::parse(
            r"
            [archive]
            enabled = true
            ",
        )
        .unwrap();
        assert!(config.archive.retention.keep_all);
    }

    #[test]
    fn test_retention_partial_override() {
        // Missing fields should fall back to defaults — operators can
        // set just `max_age_days` without re-specifying every knob.
        let config = ProjectConfig::parse(
            r"
            [archive.retention]
            max_age_days = 30
            ",
        )
        .unwrap();
        // keep_all is still the default (true) — the operator must
        // explicitly disarm it for prune to delete anything.
        assert!(config.archive.retention.keep_all);
        assert_eq!(config.archive.retention.max_age_days, 30);
        assert_eq!(config.archive.retention.max_total_mb, 0);
    }

    #[test]
    fn test_invalid_on_complete_value() {
        let result = ProjectConfig::parse(
            r#"
            [worker]
            on_complete = "yolo"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_git_remote_blocklist_default_empty() {
        let config = ProjectConfig::default();
        assert!(config.git_remote_blocklist.is_empty());
        assert!(config.git_remote_blocklist.forbidden_substrings.is_empty());
    }

    #[test]
    fn test_parse_git_remote_blocklist() {
        let config = ProjectConfig::parse(
            r#"
            [git_remote_blocklist]
            forbidden_substrings = [
              "github.com:noogram/almanac",
              "github.com/noogram/almanac",
            ]
            "#,
        )
        .unwrap();
        assert!(!config.git_remote_blocklist.is_empty());
        assert_eq!(config.git_remote_blocklist.forbidden_substrings.len(), 2);
        assert!(config
            .git_remote_blocklist
            .forbidden_substrings
            .iter()
            .any(|s| s == "github.com:noogram/almanac"));
    }

    #[test]
    fn test_git_remote_blocklist_absent_section_defaults_empty() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(config.git_remote_blocklist.is_empty());
    }

    #[test]
    fn test_git_remote_blocklist_serde_roundtrip() {
        let original = GitRemoteBlocklistConfig {
            forbidden_substrings: vec!["github.com/noogram/almanac".to_owned()],
        };
        let serialized = toml::to_string(&original).unwrap();
        let deserialized: GitRemoteBlocklistConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    // ── publish_identity (ADR-128 §V1, task-20260617-4bce) ───────────

    #[test]
    fn test_publish_identity_default_empty() {
        let config = ProjectConfig::default();
        assert!(config.publish_identity.is_empty());
        assert!(config.publish_identity.allowed_emails.is_empty());
        assert!(config.publish_identity.forbidden_substrings.is_empty());
    }

    #[test]
    fn test_publish_identity_absent_section_defaults_empty() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(config.publish_identity.is_empty());
    }

    #[test]
    fn test_parse_publish_identity_both_layers() {
        let config = ProjectConfig::parse(
            r#"
            [publish_identity]
            allowed_emails = ["bot@noogram.org"]
            forbidden_substrings = ["operator@example.org", "example.org"]
            "#,
        )
        .unwrap();
        assert!(!config.publish_identity.is_empty());
        assert_eq!(
            config.publish_identity.allowed_emails,
            vec!["bot@noogram.org"]
        );
        assert_eq!(config.publish_identity.forbidden_substrings.len(), 2);
    }

    #[test]
    fn test_publish_identity_whitelist_only_is_not_empty() {
        // The whitelist layer alone arms the gate — a project may pin the
        // canonical identity without also enumerating a blacklist.
        let config = ProjectConfig::parse(
            r#"
            [publish_identity]
            allowed_emails = ["bot@noogram.org"]
            "#,
        )
        .unwrap();
        assert!(!config.publish_identity.is_empty());
        assert!(config.publish_identity.forbidden_substrings.is_empty());
    }

    #[test]
    fn test_publish_identity_serde_roundtrip() {
        let original = PublishIdentityConfig {
            allowed_emails: vec!["bot@noogram.org".to_owned()],
            forbidden_substrings: vec!["operator@example.org".to_owned()],
        };
        let serialized = toml::to_string(&original).unwrap();
        let deserialized: PublishIdentityConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_attribution_default_empty_and_no_directive() {
        let config = ProjectConfig::default();
        assert!(config.attribution.is_empty());
        assert!(config.attribution.directive().is_none());
    }

    #[test]
    fn test_attribution_absent_section_defaults_empty() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(config.attribution.is_empty());
        assert!(config.attribution.directive().is_none());
    }

    #[test]
    fn test_parse_attribution_block() {
        let config = ProjectConfig::parse(
            r#"
            [attribution]
            public_name      = "Noogram"
            public_url       = "noogram.org"
            contact          = "hello@noogram.org"
            footer           = "© 2026 Noogram · noogram.org"
            readme_byline    = "Built by Noogram — open agent infrastructure and AI tooling."
            repo_description = "Maintained by Noogram (noogram.org). Open tooling for AI agent fleets."
            authors_line     = "Noogram <hello@noogram.org>"
            "#,
        )
        .unwrap();
        assert!(!config.attribution.is_empty());
        assert_eq!(config.attribution.public_name, "Noogram");
        assert_eq!(config.attribution.public_url, "noogram.org");
        assert_eq!(config.attribution.contact, "hello@noogram.org");
        assert_eq!(
            config.attribution.authors_line,
            "Noogram <hello@noogram.org>"
        );
    }

    #[test]
    fn test_attribution_directive_is_verbatim() {
        let config = ProjectConfig::parse(
            r#"
            [attribution]
            public_name = "Noogram"
            public_url  = "noogram.org"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.attribution.directive().unwrap(),
            "External attribution for this fleet is `Noogram` (noogram.org). \
             Anywhere a maker, author, copyright holder, or \"built by\" name \
             is required in a shipped/public artifact, use `Noogram`. The \
             operator's fund affiliation is PRIVATE and never appears in any \
             artifact."
        );
    }

    #[test]
    fn test_attribution_directive_name_only_drops_parenthetical() {
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            ..AttributionConfig::default()
        };
        // No URL → no ` (url)` parenthetical, still a clean sentence.
        assert_eq!(
            cfg.directive().unwrap(),
            "External attribution for this fleet is `Noogram`. \
             Anywhere a maker, author, copyright holder, or \"built by\" name \
             is required in a shipped/public artifact, use `Noogram`. The \
             operator's fund affiliation is PRIVATE and never appears in any \
             artifact."
        );
    }

    #[test]
    fn test_attribution_empty_name_is_no_op_even_with_url() {
        // A URL without a public_name is still treated as empty — the maker
        // name is the load-bearing field.
        let cfg = AttributionConfig {
            public_url: "noogram.org".to_owned(),
            ..AttributionConfig::default()
        };
        assert!(cfg.is_empty());
        assert!(cfg.directive().is_none());
    }

    #[test]
    fn test_attribution_serde_roundtrip() {
        let original = AttributionConfig {
            public_name: "Noogram".to_owned(),
            public_url: "noogram.org".to_owned(),
            contact: "hello@noogram.org".to_owned(),
            footer: "© 2026 Noogram · noogram.org".to_owned(),
            readme_byline: "Built by Noogram — open agent infrastructure and AI tooling."
                .to_owned(),
            repo_description: "Maintained by Noogram (noogram.org).".to_owned(),
            authors_line: "Noogram <hello@noogram.org>".to_owned(),
            coauthor_name: "Noogram".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
        };
        let serialized = toml::to_string(&original).unwrap();
        let deserialized: AttributionConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_coauthor_trailers_no_email_is_empty() {
        // The email gates the whole facet — a name without an address yields
        // no trailer, byte-identical to a pre-attribution cosmon.
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            ..AttributionConfig::default()
        };
        assert!(cfg.coauthor_trailers(Some("claude")).is_empty());
        assert!(cfg.coauthor_trailers(None).is_empty());
    }

    #[test]
    fn test_coauthor_trailers_maker_only_when_adapter_unknown() {
        // Email set, no adapter known → exactly the maker trailer, named from
        // `public_name` (no explicit `coauthor_name`).
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
            ..AttributionConfig::default()
        };
        assert_eq!(
            cfg.coauthor_trailers(None),
            vec!["Co-Authored-By: Noogram <noreply@noogram.org>".to_owned()]
        );
    }

    #[test]
    fn test_coauthor_trailers_records_real_adapter_in_single_display_name() {
        // The real adapter is recorded in the maker display name while the
        // configured stable address remains unchanged.
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
            ..AttributionConfig::default()
        };
        assert_eq!(
            cfg.coauthor_trailers(Some("claude")),
            vec!["Co-Authored-By: Noogram (claude) <noreply@noogram.org>".to_owned()]
        );
        assert_eq!(
            cfg.coauthor_trailers(Some("gemini")),
            vec!["Co-Authored-By: Noogram (gemini) <noreply@noogram.org>".to_owned()]
        );
    }

    #[test]
    fn test_coauthor_trailers_explicit_name_overrides_public_name() {
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_name: "Noogram Agents".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
            ..AttributionConfig::default()
        };
        assert_eq!(
            cfg.coauthor_trailers(None),
            vec!["Co-Authored-By: Noogram Agents <noreply@noogram.org>".to_owned()]
        );
    }

    #[test]
    fn test_coauthor_trailers_blank_adapter_is_ignored() {
        // A whitespace/empty adapter string never produces a phantom second
        // co-author — only a real, non-blank adapter is credited.
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
            ..AttributionConfig::default()
        };
        assert_eq!(cfg.coauthor_trailers(Some("   ")).len(), 1);
        assert_eq!(cfg.coauthor_trailers(Some("")).len(), 1);
    }

    /// A configured address is reproduced verbatim; adapter attribution never
    /// attempts to derive or repair an address from its domain.
    #[test]
    fn test_coauthor_trailers_preserves_email_without_at() {
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_email: "noreply".to_owned(), // no '@domain'
            ..AttributionConfig::default()
        };
        assert_eq!(
            cfg.coauthor_trailers(Some("claude")),
            vec!["Co-Authored-By: Noogram (claude) <noreply>".to_owned()]
        );
        assert_eq!(cfg.coauthor_trailers(Some("claude")).len(), 1);
    }

    /// A trailing `@` is likewise preserved without synthesizing a replacement.
    #[test]
    fn test_coauthor_trailers_preserves_email_with_empty_domain() {
        let cfg = AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_email: "noreply@".to_owned(), // '@' present, domain empty
            ..AttributionConfig::default()
        };
        assert_eq!(
            cfg.coauthor_trailers(Some("claude")),
            vec!["Co-Authored-By: Noogram (claude) <noreply@>".to_owned()]
        );
    }

    #[test]
    fn test_confidential_blocklist_default_empty() {
        let config = ProjectConfig::default();
        // Both lists empty → the gate cannot fire.
        assert!(config.confidential_blocklist.is_empty());
        assert!(config
            .confidential_blocklist
            .forbidden_substrings
            .is_empty());
        assert!(config.confidential_blocklist.publish_globs.is_empty());
    }

    #[test]
    fn test_confidential_blocklist_absent_section_defaults_empty() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(config.confidential_blocklist.is_empty());
    }

    #[test]
    fn test_parse_confidential_blocklist() {
        let config = ProjectConfig::parse(
            r#"
            [confidential_blocklist]
            forbidden_substrings = ["Tenant-Demo Research", "Tenant-Demo", "example.org"]
            publish_globs = ["README*", "**/footer.html", "site/**"]
            "#,
        )
        .unwrap();
        assert!(!config.confidential_blocklist.is_empty());
        assert_eq!(config.confidential_blocklist.forbidden_substrings.len(), 3);
        assert_eq!(config.confidential_blocklist.publish_globs.len(), 3);
        assert!(config
            .confidential_blocklist
            .forbidden_substrings
            .iter()
            .any(|s| s == "Tenant-Demo"));
    }

    #[test]
    fn test_confidential_blocklist_substrings_only_is_active_with_default_globs() {
        // task-20260622-7207: substrings set but no globs is NO LONGER a
        // silent no-op — the gate stays active and scans DEFAULT_PUBLISH_GLOBS.
        // This is the foot-gun the old "either-empty" rule created: a
        // configured blocklist sat inert because publish_globs was omitted.
        let config = ProjectConfig::parse(
            r#"
            [confidential_blocklist]
            forbidden_substrings = ["Tenant-Demo"]
            "#,
        )
        .unwrap();
        assert!(
            !config.confidential_blocklist.is_empty(),
            "substrings present → gate active even without explicit globs"
        );
        let globs = config.confidential_blocklist.effective_publish_globs();
        assert_eq!(
            globs,
            ConfidentialBlocklistConfig::DEFAULT_PUBLISH_GLOBS
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
            "no explicit globs falls back to the built-in default set"
        );
    }

    #[test]
    fn test_confidential_blocklist_default_globs_cover_paper_deliverables() {
        // The qfa leak (spark-20260621-f55e) was a PAPER author block +
        // colophon — surfaces the README/site-only globs never scanned.
        let globs = ConfidentialBlocklistConfig::DEFAULT_PUBLISH_GLOBS;
        for needle in ["**/*.tex", "**/colophon*", "**/AUTHORS*", "**/paper.md"] {
            assert!(
                globs.contains(&needle),
                "default globs must cover paper deliverables, missing {needle}"
            );
        }
    }

    #[test]
    fn test_confidential_blocklist_explicit_globs_win_over_default() {
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned()],
            publish_globs: vec!["docs/*.md".to_owned()],
            ..Default::default()
        };
        assert_eq!(cfg.effective_publish_globs(), vec!["docs/*.md".to_owned()]);
    }

    #[test]
    fn test_confidential_blocklist_merge_unions_substrings_and_globs() {
        // The federation primitive: the operator's machine-wide blocklist
        // folds into a per-galaxy one — union, dedup, first-seen order.
        let per_galaxy = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["GalaxyName".to_owned()],
            publish_globs: vec!["docs/*.md".to_owned()],
            ..Default::default()
        };
        let global = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo Research".to_owned(), "GalaxyName".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        let merged = per_galaxy.merged_with(&global);
        assert_eq!(
            merged.forbidden_substrings,
            vec!["GalaxyName".to_owned(), "Tenant-Demo Research".to_owned()],
            "union dedups GalaxyName, keeps first-seen order"
        );
        assert_eq!(
            merged.publish_globs,
            vec!["docs/*.md".to_owned(), "README*".to_owned()]
        );
    }

    #[test]
    fn test_confidential_blocklist_merge_with_empty_per_galaxy_inherits_global() {
        // The qfa case: a galaxy with NO blocklist inherits the operator's
        // federation-wide secret entirely from the global config.
        let per_galaxy = ConfidentialBlocklistConfig::default();
        let global = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo Research".to_owned()],
            publish_globs: vec![],
            ..Default::default()
        };
        let merged = per_galaxy.merged_with(&global);
        assert!(!merged.is_empty(), "inherited gate must be active");
        assert_eq!(
            merged.forbidden_substrings,
            vec!["Tenant-Demo Research".to_owned()]
        );
        // No globs on either side → the scan falls back to the paper-aware
        // default set.
        assert!(merged
            .effective_publish_globs()
            .contains(&"**/*.tex".to_owned()));
    }

    #[test]
    fn test_confidential_blocklist_serde_roundtrip() {
        let original = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        let serialized = toml::to_string(&original).unwrap();
        let deserialized: ConfidentialBlocklistConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn adapters_absent_section_falls_back_to_none() {
        let config = ProjectConfig::parse("").unwrap();
        assert!(config.adapters.is_none());
    }

    #[test]
    fn adapters_parses_default_and_per_adapter_entries() {
        let config = ProjectConfig::parse(
            r#"
            [adapters]
            default = "aider"

            [adapters.claude]
            pane_signatures = ["claude"]
            briefing_format = "markdown"

            [adapters.aider]
            pane_signatures = ["aider", "python", "python3.11"]
            briefing_format = "markdown"
            extra_args = ["--no-auto-commits", "--yes-always"]
            "#,
        )
        .unwrap();
        let adapters = config.adapters.expect("adapters section present");
        assert_eq!(adapters.default_adapter(), Some("aider"));
        let aider = adapters.entry("aider").expect("aider entry");
        assert_eq!(aider.pane_signatures, vec!["aider", "python", "python3.11"]);
        assert_eq!(aider.briefing_format.as_deref(), Some("markdown"));
        assert_eq!(aider.extra_args, vec!["--no-auto-commits", "--yes-always"]);
        let claude = adapters.entry("claude").expect("claude entry");
        assert!(claude.extra_args.is_empty());
        // Sorted lexicographically — useful for AdapterNotFound's diagnostic.
        assert_eq!(adapters.available_names(), vec!["aider", "claude"]);
    }

    #[test]
    fn adapters_default_only_no_entries() {
        let config = ProjectConfig::parse(
            r#"
            [adapters]
            default = "claude"
            "#,
        )
        .unwrap();
        let adapters = config.adapters.expect("adapters section present");
        assert_eq!(adapters.default_adapter(), Some("claude"));
        assert!(adapters.entry("claude").is_none());
        assert!(adapters.available_names().is_empty());
    }

    /// `[adapters.openai]` parses the Direct-API knobs added for the
    /// xAI / Moonshot / `DeepSeek` free-rider build (academy smoke
    /// chronicle 2026-05-18, GAP #4). Each field is optional so a
    /// sparse table stays valid.
    #[test]
    fn adapters_parses_direct_api_overrides() {
        let config = ProjectConfig::parse(
            r#"
            [adapters.openai]
            api_key_env = "XAI_API_KEY"
            base_url = "https://api.x.ai"
            default_model = "grok-3"
            "#,
        )
        .unwrap();
        let adapters = config.adapters.expect("adapters section present");
        let openai = adapters.entry("openai").expect("openai entry");
        assert_eq!(openai.api_key_env.as_deref(), Some("XAI_API_KEY"));
        assert_eq!(openai.base_url.as_deref(), Some("https://api.x.ai"));
        assert_eq!(openai.default_model.as_deref(), Some("grok-3"));
    }

    /// Sparse `[adapters.openai]` with only one override is valid —
    /// the other two fields stay `None`, leaving the provider to fall
    /// back to env-var / compile-time defaults.
    #[test]
    fn adapters_direct_api_fields_are_independently_optional() {
        let config = ProjectConfig::parse(
            r#"
            [adapters.openai]
            default_model = "gpt-4o"
            "#,
        )
        .unwrap();
        let adapters = config.adapters.expect("adapters section present");
        let openai = adapters.entry("openai").expect("openai entry");
        assert_eq!(openai.api_key_env, None);
        assert_eq!(openai.base_url, None);
        assert_eq!(openai.default_model.as_deref(), Some("gpt-4o"));
    }

    /// `[adapters."llama-cpp"]` parses the per-model prompt-tuning knobs
    /// added for the multi-GGUF Path A.
    /// `system_prompt_override` replaces the default
    /// `SYSTEM_PROMPT` entirely; `chat_template_kwargs` ride along as a
    /// deterministic key/value map for adapter-side interpretation.
    #[test]
    fn adapters_parses_llama_prompt_tuning_knobs() {
        let config = ProjectConfig::parse(
            r#"
            [adapters."llama-cpp"]
            default_model = "/cache/Llama-3.3-70B-Instruct.gguf"
            system_prompt_override = "You are Llama-3.3-70B. Do NOT fabricate task completions or invent cs commands."

            [adapters."llama-cpp".chat_template_kwargs]
            enable_thinking = "false"
            tool_fence = "qwen3"
            "#,
        )
        .unwrap();
        let adapters = config.adapters.expect("adapters section present");
        let llama = adapters
            .entry("llama-cpp")
            .expect("llama-cpp entry present");
        assert_eq!(
            llama.system_prompt_override.as_deref(),
            Some("You are Llama-3.3-70B. Do NOT fabricate task completions or invent cs commands.")
        );
        assert_eq!(
            llama
                .chat_template_kwargs
                .get("enable_thinking")
                .map(String::as_str),
            Some("false")
        );
        assert_eq!(
            llama
                .chat_template_kwargs
                .get("tool_fence")
                .map(String::as_str),
            Some("qwen3")
        );
        // BTreeMap → deterministic ordering for diagnostics.
        let keys: Vec<&str> = llama
            .chat_template_kwargs
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["enable_thinking", "tool_fence"]);
    }

    /// Absent `system_prompt_override` + empty `chat_template_kwargs` is
    /// the absence-default contract: Qwen3-Instruct families never need
    /// either knob, and a bare `[adapters."llama-cpp"]` row keeps its
    /// today-behaviour unchanged.
    #[test]
    fn adapters_llama_prompt_tuning_knobs_default_to_absent_and_empty() {
        let config = ProjectConfig::parse(
            r#"
            [adapters."llama-cpp"]
            default_model = "/cache/Qwen3-8B-Instruct.gguf"
            "#,
        )
        .unwrap();
        let adapters = config.adapters.expect("adapters section present");
        let llama = adapters
            .entry("llama-cpp")
            .expect("llama-cpp entry present");
        assert_eq!(llama.system_prompt_override, None);
        assert!(llama.chat_template_kwargs.is_empty());
    }

    #[test]
    fn self_reference_guard_defaults() {
        let config = ProjectConfig::default();
        assert_eq!(config.self_reference_guard.max_depth, 2);
        assert_eq!(config.self_reference_guard.debounce_secs, 5);
        assert_eq!(config.self_reference_guard.max_staleness_secs, 300);
    }

    #[test]
    fn self_reference_guard_parses_from_toml() {
        let config = ProjectConfig::parse(
            r"
            [self_reference_guard]
            max_depth = 4
            debounce_secs = 10
            max_staleness_secs = 120
            ",
        )
        .unwrap();
        assert_eq!(config.self_reference_guard.max_depth, 4);
        assert_eq!(config.self_reference_guard.debounce_secs, 10);
        assert_eq!(config.self_reference_guard.max_staleness_secs, 120);
    }

    #[test]
    fn self_reference_guard_absent_section_uses_defaults() {
        let config = ProjectConfig::parse(
            r#"
            [worker]
            on_complete = "commit"
            "#,
        )
        .unwrap();
        assert_eq!(config.self_reference_guard.max_depth, 2);
        assert_eq!(config.self_reference_guard.debounce_secs, 5);
        assert_eq!(config.self_reference_guard.max_staleness_secs, 300);
    }

    #[test]
    fn self_reference_guard_partial_section_fills_defaults() {
        let config = ProjectConfig::parse(
            r"
            [self_reference_guard]
            max_depth = 5
            ",
        )
        .unwrap();
        assert_eq!(config.self_reference_guard.max_depth, 5);
        assert_eq!(config.self_reference_guard.debounce_secs, 5);
        assert_eq!(config.self_reference_guard.max_staleness_secs, 300);
    }
}
