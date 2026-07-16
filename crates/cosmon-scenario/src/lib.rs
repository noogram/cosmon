// SPDX-License-Identifier: AGPL-3.0-only

//! Cosmon scenario harness — declarative DAG lifecycle tests.
//!
//! A scenario is a TOML document describing:
//!   - `[given]` — the initial molecule graph (molecules + typed links)
//!   - `[[actions]]` — operations applied to drive the graph
//!   - `[[assert]]` — observable postconditions
//!
//! The engine runs entirely in-memory using a lightweight simulation of the
//! cosmon lifecycle: no tmux, no Claude, no subprocess. Native steps resolve
//! to in-process Rust functions registered in a test-only `NativeRegistry`.
//!
//! See `docs/spec-suite.md` for the authoring guide.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

pub mod native;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// The top-level scenario document.
#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub scenario: ScenarioMeta,
    #[serde(default)]
    pub binds: Binds,
    #[serde(default)]
    pub given: Given,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(rename = "assert", default)]
    pub asserts: Vec<Assert>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Binds {
    #[serde(default)]
    pub constitution_clause: Option<String>,
    #[serde(default)]
    pub foundry_proposition: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Given {
    #[serde(default)]
    pub molecules: Vec<GivenMolecule>,
    #[serde(default)]
    pub links: Vec<GivenLink>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GivenMolecule {
    pub id: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub steps: Vec<GivenStep>,
}

fn default_kind() -> String {
    "task".into()
}
fn default_status() -> String {
    "pending".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GivenStep {
    pub name: String,
    pub native: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GivenLink {
    pub from: String,
    pub to: String,
    /// One of: `Blocks`, `DecayProduct`, `Entangled`, `Refines`.
    pub kind: String,
}

/// An action applied to the scenario state.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    /// Drive the root molecule (and all its transitively ready predecessors)
    /// to a terminal state by draining native steps.
    RunRoot { target: String },
    /// Execute one scheduler tick: pick one ready molecule, drain one step.
    Tick {},
    /// Collapse a molecule with a reason. Cascades to molecules it blocks.
    Collapse {
        target: String,
        #[serde(default)]
        reason: String,
    },
    /// Freeze a Running molecule.
    Freeze { target: String },
    /// Thaw a Frozen molecule. Idempotent on Running.
    Thaw { target: String },
    /// Advance a Pending → Running without executing any step (tackle).
    Activate { target: String },
    /// Record a snapshot of `ready_frontier` into the trace.
    SnapshotFrontier {},
}

/// An assertion evaluated after all actions have run.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Assert {
    /// Per-molecule assertion.
    Molecule {
        molecule: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        step: Option<String>,
        #[serde(default)]
        collapse_reason: Option<String>,
    },
    /// Property of the execution trace.
    Property {
        property: String,
        #[serde(default)]
        target: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Per-molecule mutable state inside the engine.
#[derive(Debug, Clone)]
pub struct MolState {
    pub id: String,
    pub kind: String,
    pub steps: Vec<GivenStep>,
    pub current_step: usize,
    pub status: Status,
    pub collapse_reason: Option<String>,
    /// Branch base pointer — simulated by recording the id of the molecule
    /// whose merge this molecule's branch was forked from. `None` means
    /// "branched from main".
    pub branch_base: Option<String>,
    /// Whether this molecule's output has been merged to main.
    pub merged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Frozen,
    Completed,
    Collapsed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Frozen => "frozen",
            Self::Completed => "completed",
            Self::Collapsed => "collapsed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "frozen" => Self::Frozen,
            "completed" => Self::Completed,
            "collapsed" => Self::Collapsed,
            _ => return None,
        })
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Collapsed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Blocks,
    DecayProduct,
    Entangled,
    Refines,
}

impl LinkKind {
    fn parse(s: &str) -> Result<Self, ScenarioError> {
        Ok(match s {
            "Blocks" | "blocks" => Self::Blocks,
            "DecayProduct" | "decay_product" => Self::DecayProduct,
            "Entangled" | "entangled" => Self::Entangled,
            "Refines" | "refines" => Self::Refines,
            other => return Err(ScenarioError::UnknownLinkKind(other.into())),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
}

/// Execution trace — observable history for property assertions.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    pub frontier_snapshots: Vec<BTreeSet<String>>,
    /// For each molecule, the list of (predecessor_id, predecessor_merged_at_dispatch).
    /// Populated when a molecule transitions Pending→Running: we check every
    /// `Blocks`-predecessor was merged=true at that moment.
    pub dispatch_events: Vec<DispatchEvent>,
    pub record_log: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct DispatchEvent {
    pub molecule: String,
    pub predecessors_merged: BTreeMap<String, bool>,
}

/// The scenario engine.
pub struct Engine {
    pub mols: BTreeMap<String, MolState>,
    pub links: Vec<Link>,
    pub registry: native::NativeRegistry,
    pub trace: Trace,
    /// When `true`, a `Collapsed` parent releases the `DecayProduct` gate
    /// on its lateral children (option B — see
    /// `DIAGNOSIS-mission-collapse.md`). Default `false` reproduces the
    /// orphaned-children pathology.
    pub decay_collapse_releases: bool,
}

impl Engine {
    pub fn new(scenario: &Scenario) -> Result<Self, ScenarioError> {
        let mut mols = BTreeMap::new();
        for m in &scenario.given.molecules {
            let status = Status::parse(&m.status)
                .ok_or_else(|| ScenarioError::UnknownStatus(m.status.clone()))?;
            mols.insert(
                m.id.clone(),
                MolState {
                    id: m.id.clone(),
                    kind: m.kind.clone(),
                    steps: m.steps.clone(),
                    current_step: 0,
                    status,
                    collapse_reason: None,
                    branch_base: None,
                    merged: false,
                },
            );
        }
        let mut links = Vec::new();
        for l in &scenario.given.links {
            links.push(Link {
                from: l.from.clone(),
                to: l.to.clone(),
                kind: LinkKind::parse(&l.kind)?,
            });
        }
        Ok(Self {
            mols,
            links,
            registry: native::NativeRegistry::with_test_builtins(),
            trace: Trace::default(),
            decay_collapse_releases: true,
        })
    }

    /// Toggle the option-B fix (collapsed parent releases `DecayProduct`
    /// gate). Used by the regression test in `scenarios.rs` to confirm
    /// the pre-fix behavior is actually red.
    pub fn set_decay_collapse_releases(&mut self, v: bool) {
        self.decay_collapse_releases = v;
    }

    /// Molecules directly blocking `id` via `Blocks` links.
    pub fn blockers(&self, id: &str) -> Vec<String> {
        self.links
            .iter()
            .filter(|l| l.kind == LinkKind::Blocks && l.to == id)
            .map(|l| l.from.clone())
            .collect()
    }

    /// Molecules that emitted `id` as a `DecayProduct` (lateral parents).
    pub fn decay_parents(&self, id: &str) -> Vec<String> {
        self.links
            .iter()
            .filter(|l| l.kind == LinkKind::DecayProduct && l.to == id)
            .map(|l| l.from.clone())
            .collect()
    }

    /// Molecules this one blocks.
    pub fn blocks(&self, id: &str) -> Vec<String> {
        self.links
            .iter()
            .filter(|l| l.kind == LinkKind::Blocks && l.from == id)
            .map(|l| l.to.clone())
            .collect()
    }

    /// The current ready frontier.
    ///
    /// A molecule is ready iff:
    ///   - every `Blocks`-predecessor has reached a **terminal** state —
    ///     `Completed`, `Collapsed`, or `Frozen`. `blocked-by` releases on
    ///     *done*, not on *verdict* (task-20260706-4d1e): a `reproduce` that
    ///     collapsed "refuted" still unblocks the `fix`. This mirrors
    ///     `cosmon_state::frontier::compute_from_molecules` and the runtime's
    ///     `DagPolicy`, AND
    ///   - every `DecayProduct`-parent has reached a terminal state where
    ///     the lateral decomposition gate is released. A `Completed` parent
    ///     always releases the gate; a `Collapsed` parent does **not** by
    ///     default (this models the pathology diagnosed in
    ///     `DIAGNOSIS-mission-collapse.md`). The option-B fix flips this
    ///     via `Engine::set_decay_collapse_releases(true)`.
    pub fn ready_frontier(&self) -> BTreeSet<String> {
        let mut f = BTreeSet::new();
        for (id, m) in &self.mols {
            if m.status.is_terminal() || m.status == Status::Frozen {
                continue;
            }
            let blocks_cleared = self.blockers(id).iter().all(|b| {
                self.mols.get(b).is_some_and(|p| {
                    matches!(
                        p.status,
                        Status::Completed | Status::Collapsed | Status::Frozen
                    )
                })
            });
            if !blocks_cleared {
                continue;
            }
            let decay_cleared = self.decay_parents(id).iter().all(|b| {
                self.mols.get(b).is_some_and(|p| match p.status {
                    Status::Completed => true,
                    Status::Collapsed => self.decay_collapse_releases,
                    _ => false,
                })
            });
            if !decay_cleared {
                continue;
            }
            f.insert(id.clone());
        }
        f
    }

    /// Transition a pending molecule to Running (the simulated tackle).
    /// Records a dispatch event capturing the merge-state of every blocker
    /// at dispatch time.
    fn activate(&mut self, id: &str) -> Result<(), ScenarioError> {
        let blockers = self.blockers(id);
        let predecessors_merged: BTreeMap<String, bool> = blockers
            .iter()
            .map(|b| (b.clone(), self.mols.get(b).is_some_and(|m| m.merged)))
            .collect();
        let base = blockers.last().cloned();
        let m = self
            .mols
            .get_mut(id)
            .ok_or_else(|| ScenarioError::UnknownMolecule(id.into()))?;
        match m.status {
            Status::Pending | Status::Frozen => {
                m.status = Status::Running;
                m.branch_base = base;
            }
            Status::Running => {}
            other => {
                return Err(ScenarioError::InvalidTransition(
                    id.into(),
                    other.as_str().into(),
                    "running".into(),
                ))
            }
        }
        self.trace.dispatch_events.push(DispatchEvent {
            molecule: id.into(),
            predecessors_merged,
        });
        Ok(())
    }

    /// Execute native steps one-by-one on a Running molecule until it
    /// completes or a step fails. Returns the number of steps drained.
    fn drain_steps(&mut self, id: &str) -> Result<usize, ScenarioError> {
        let mut drained = 0usize;
        loop {
            let (finished, step_key, step_idx) = {
                let m = self
                    .mols
                    .get(id)
                    .ok_or_else(|| ScenarioError::UnknownMolecule(id.into()))?;
                if m.status != Status::Running {
                    break;
                }
                if m.current_step >= m.steps.len() {
                    (true, String::new(), 0)
                } else {
                    (
                        false,
                        m.steps[m.current_step].native.clone(),
                        m.current_step,
                    )
                }
            };
            if finished {
                let m = self.mols.get_mut(id).unwrap();
                m.status = Status::Completed;
                m.merged = true; // merge-on-complete (simulated)
                break;
            }
            let ctx = native::NativeCtx {
                molecule: id.into(),
                step_index: step_idx,
            };
            let outcome = self.registry.call(&step_key, &ctx)?;
            match outcome {
                native::Outcome::Ok => {
                    let m = self.mols.get_mut(id).unwrap();
                    m.current_step += 1;
                    drained += 1;
                }
                native::Outcome::Fail(reason) => {
                    let m = self.mols.get_mut(id).unwrap();
                    m.status = Status::Collapsed;
                    m.collapse_reason = Some(reason);
                    break;
                }
                native::Outcome::Record { tag, value } => {
                    self.trace.record_log.push((tag, value));
                    let m = self.mols.get_mut(id).unwrap();
                    m.current_step += 1;
                    drained += 1;
                }
            }
        }
        Ok(drained)
    }

    pub fn apply(&mut self, action: &Action) -> Result<(), ScenarioError> {
        match action {
            Action::SnapshotFrontier {} => {
                self.trace.frontier_snapshots.push(self.ready_frontier());
            }
            Action::Activate { target } => {
                self.activate(target)?;
            }
            Action::Tick {} => {
                self.trace.frontier_snapshots.push(self.ready_frontier());
                let ready = self.ready_frontier();
                if let Some(id) = ready.into_iter().next() {
                    let status = self.mols.get(&id).map(|m| m.status);
                    if status == Some(Status::Pending) {
                        self.activate(&id)?;
                    }
                    // drain exactly one step
                    let m = self.mols.get(&id).unwrap();
                    if m.status == Status::Running && m.current_step < m.steps.len() {
                        let key = m.steps[m.current_step].native.clone();
                        let idx = m.current_step;
                        let ctx = native::NativeCtx {
                            molecule: id.clone(),
                            step_index: idx,
                        };
                        match self.registry.call(&key, &ctx)? {
                            native::Outcome::Ok => {
                                let m = self.mols.get_mut(&id).unwrap();
                                m.current_step += 1;
                                if m.current_step >= m.steps.len() {
                                    m.status = Status::Completed;
                                    m.merged = true;
                                }
                            }
                            native::Outcome::Fail(r) => {
                                let m = self.mols.get_mut(&id).unwrap();
                                m.status = Status::Collapsed;
                                m.collapse_reason = Some(r);
                            }
                            native::Outcome::Record { tag, value } => {
                                self.trace.record_log.push((tag, value));
                                let m = self.mols.get_mut(&id).unwrap();
                                m.current_step += 1;
                                if m.current_step >= m.steps.len() {
                                    m.status = Status::Completed;
                                    m.merged = true;
                                }
                            }
                        }
                    } else if m.status == Status::Running && m.steps.is_empty() {
                        let m = self.mols.get_mut(&id).unwrap();
                        m.status = Status::Completed;
                        m.merged = true;
                    }
                }
                self.trace.frontier_snapshots.push(self.ready_frontier());
            }
            Action::RunRoot { target } => {
                // Drive until either the ready frontier is empty or the guard
                // trips. We do **not** stop as soon as `target` reaches a
                // terminal state: the DAG scope also covers any lateral
                // `DecayProduct` descendants visited along the way, and
                // they must drain before `cs run` may declare "no work
                // left" (see `DIAGNOSIS-mission-collapse.md`, option B).
                //
                // A blocker that collapses does **not** cascade-collapse its
                // forward `Blocks` dependents — `blocked-by` releases on
                // *done*, not on *verdict* (task-20260706-4d1e). The
                // dependent becomes ready (see `ready_frontier`) and runs;
                // it is free to read the collapse verdict from disk and
                // decide its own fate. The exit condition is "no progress
                // AND no ready molecules," which covers both axes.
                let _ = target; // reserved for future scope-narrowing
                let mut guard = 0usize;
                loop {
                    guard += 1;
                    if guard > 10_000 {
                        return Err(ScenarioError::RunawayRunRoot);
                    }
                    self.trace.frontier_snapshots.push(self.ready_frontier());
                    let ready = self.ready_frontier();
                    if ready.is_empty() {
                        break;
                    }
                    let mut progressed = false;
                    for id in ready {
                        let st = self.mols.get(&id).map(|m| m.status);
                        if st == Some(Status::Pending) {
                            self.activate(&id)?;
                            progressed = true;
                        }
                        let drained = self.drain_steps(&id)?;
                        if drained > 0 {
                            progressed = true;
                        }
                        // A mid-drain collapse does NOT cascade to forward
                        // `Blocks` dependents (task-20260706-4d1e). They are
                        // released by `ready_frontier` once their blocker is
                        // terminal, and run on a later loop iteration.
                    }
                    if !progressed {
                        break;
                    }
                }
            }
            Action::Collapse { target, reason } => {
                let m = self
                    .mols
                    .get_mut(target)
                    .ok_or_else(|| ScenarioError::UnknownMolecule(target.clone()))?;
                m.status = Status::Collapsed;
                m.collapse_reason = Some(if reason.is_empty() {
                    "manual".into()
                } else {
                    reason.clone()
                });
                // No cascade: collapsing a blocker RELEASES its forward
                // `Blocks` dependents rather than collapsing them
                // (task-20260706-4d1e — blocked-by releases on done, not on
                // verdict). The successor becomes ready and runs.
            }
            Action::Freeze { target } => {
                let m = self
                    .mols
                    .get_mut(target)
                    .ok_or_else(|| ScenarioError::UnknownMolecule(target.clone()))?;
                if m.status == Status::Running || m.status == Status::Pending {
                    m.status = Status::Frozen;
                } else if m.status == Status::Frozen {
                    // idempotent
                } else {
                    return Err(ScenarioError::InvalidTransition(
                        target.clone(),
                        m.status.as_str().into(),
                        "frozen".into(),
                    ));
                }
            }
            Action::Thaw { target } => {
                let m = self
                    .mols
                    .get_mut(target)
                    .ok_or_else(|| ScenarioError::UnknownMolecule(target.clone()))?;
                if m.status == Status::Frozen {
                    m.status = Status::Running;
                }
                // idempotent otherwise
            }
        }
        Ok(())
    }

    pub fn run(&mut self, actions: &[Action]) -> Result<(), ScenarioError> {
        for a in actions {
            self.apply(a)?;
        }
        Ok(())
    }

    /// Evaluate an assertion. Returns Ok(()) on pass, Err with detail on fail.
    pub fn check(&self, a: &Assert) -> Result<(), String> {
        match a {
            Assert::Molecule {
                molecule,
                status,
                step,
                collapse_reason,
            } => {
                let m = self
                    .mols
                    .get(molecule)
                    .ok_or_else(|| format!("unknown molecule '{molecule}'"))?;
                if let Some(want) = status {
                    if m.status.as_str() != want {
                        return Err(format!(
                            "molecule {molecule}: expected status {want}, got {}",
                            m.status.as_str()
                        ));
                    }
                }
                if let Some(want) = step {
                    let got = format!("{}/{}", m.current_step, m.steps.len());
                    if got != *want {
                        return Err(format!(
                            "molecule {molecule}: expected step {want}, got {got}"
                        ));
                    }
                }
                if let Some(want) = collapse_reason {
                    match &m.collapse_reason {
                        Some(got) if got.contains(want) => {}
                        Some(got) => {
                            return Err(format!(
                                "molecule {molecule}: collapse_reason {got:?} does not contain {want:?}"
                            ))
                        }
                        None => {
                            return Err(format!(
                                "molecule {molecule}: expected collapse_reason, found none"
                            ))
                        }
                    }
                }
                Ok(())
            }
            Assert::Property { property, target } => match property.as_str() {
                "ready_frontier_monotone" => {
                    // For every consecutive pair (S_t, S_{t+1}), every molecule
                    // in S_t must be in S_{t+1} OR have become terminal.
                    let snaps = &self.trace.frontier_snapshots;
                    for w in snaps.windows(2) {
                        for id in &w[0] {
                            if w[1].contains(id) {
                                continue;
                            }
                            let terminated =
                                self.mols.get(id).is_some_and(|m| m.status.is_terminal());
                            if !terminated {
                                return Err(format!(
                                    "ready_frontier lost {id} without termination"
                                ));
                            }
                        }
                    }
                    Ok(())
                }
                "merge_before_dispatch" => {
                    // At every dispatch event, every predecessor must be merged.
                    for ev in &self.trace.dispatch_events {
                        for (pred, merged) in &ev.predecessors_merged {
                            if !merged {
                                return Err(format!(
                                    "molecule {} dispatched while predecessor {} not merged",
                                    ev.molecule, pred
                                ));
                            }
                        }
                    }
                    Ok(())
                }
                "target_branch_base_is_blocker" => {
                    let t = target
                        .as_deref()
                        .ok_or_else(|| "property requires 'target'".to_string())?;
                    let m = self.mols.get(t).ok_or_else(|| format!("no molecule {t}"))?;
                    let blockers = self.blockers(t);
                    if blockers.is_empty() {
                        return Ok(());
                    }
                    match &m.branch_base {
                        Some(base) if blockers.contains(base) => Ok(()),
                        other => Err(format!(
                            "molecule {t}: branch_base {other:?} not among blockers {blockers:?}"
                        )),
                    }
                }
                "record_count_ge_1" => {
                    if self.trace.record_log.is_empty() {
                        Err("expected at least 1 record".into())
                    } else {
                        Ok(())
                    }
                }
                other => Err(format!("unknown property: {other}")),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Public run API
// ---------------------------------------------------------------------------

/// Outcome of running one scenario.
#[derive(Debug, Clone)]
pub struct ScenarioResult {
    pub name: String,
    pub passed: bool,
    pub duration_ms: u128,
    pub failures: Vec<String>,
}

/// Load a scenario from a TOML file.
pub fn load_scenario(path: &Path) -> Result<Scenario, ScenarioError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ScenarioError::Io(path.display().to_string(), e.to_string()))?;
    toml::from_str(&text)
        .map_err(|e| ScenarioError::Parse(path.display().to_string(), e.to_string()))
}

/// Execute a single scenario and return its result.
pub fn run_scenario(path: &Path) -> ScenarioResult {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scenario")
        .to_string();
    let start = std::time::Instant::now();
    let failures = match load_scenario(path) {
        Ok(s) => match Engine::new(&s) {
            Ok(mut eng) => match eng.run(&s.actions) {
                Ok(()) => {
                    let mut fails = Vec::new();
                    for a in &s.asserts {
                        if let Err(e) = eng.check(a) {
                            fails.push(e);
                        }
                    }
                    fails
                }
                Err(e) => vec![format!("run error: {e}")],
            },
            Err(e) => vec![format!("engine init: {e}")],
        },
        Err(e) => vec![format!("load: {e}")],
    };
    let duration_ms = start.elapsed().as_millis();
    ScenarioResult {
        name,
        passed: failures.is_empty(),
        duration_ms,
        failures,
    }
}

/// Resolve a glob pattern (repo-root-relative) to a sorted list of TOML files.
pub fn discover(pattern: &str) -> Result<Vec<std::path::PathBuf>, ScenarioError> {
    let mut v: Vec<_> = glob::glob(pattern)
        .map_err(|e| ScenarioError::Parse(pattern.into(), e.to_string()))?
        .filter_map(Result::ok)
        .collect();
    v.sort();
    Ok(v)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("io error on {0}: {1}")]
    Io(String, String),
    #[error("parse error on {0}: {1}")]
    Parse(String, String),
    #[error("unknown molecule: {0}")]
    UnknownMolecule(String),
    #[error("unknown status: {0}")]
    UnknownStatus(String),
    #[error("unknown link kind: {0}")]
    UnknownLinkKind(String),
    #[error("invalid transition on {0}: {1} -> {2}")]
    InvalidTransition(String, String, String),
    #[error("unknown native: {0}")]
    UnknownNative(String),
    #[error("native error: {0}")]
    Native(String),
    #[error("run_root made no progress after 10000 iterations")]
    RunawayRunRoot,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl MolState {
    /// Used by the `cs test` renderer.
    #[must_use]
    pub fn progress(&self) -> String {
        format!("{}/{}", self.current_step, self.steps.len())
    }
}

#[allow(dead_code)]
fn _typecheck_hashmap<K: std::hash::Hash + Eq, V>() -> HashMap<K, V> {
    HashMap::new()
}
