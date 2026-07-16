// SPDX-License-Identifier: AGPL-3.0-only

//! # cosmon-fuzz
//!
//! Property-based, in-process fuzz harness for the `cs` CLI surface.
//!
//! ## Why this crate exists
//!
//! A companion effort tightens the `cs` CLI type system so
//! ill-formed invocations are rejected by the compiler. That closes one
//! filter. This crate closes a second, orthogonal filter: it exercises
//! random inputs that are **grammatically valid** (would pass `clap`'s
//! parser) but **semantically ill-formed** (missing `--blocked-by` when
//! one is required, contradictory `--tag` flags, malformed molecule ids,
//! cyclic link graphs, etc.). Two orthogonal filters = additive bits of
//! safety: a defence-in-depth argument where each independent filter
//! adds capacity to reject bad input.
//!
//! ## Scope
//!
//! The harness runs **entirely in-process** against the pure domain APIs
//! that the CLI layer wraps:
//!
//! | Surface                        | Oracle                                                 |
//! |--------------------------------|--------------------------------------------------------|
//! | [`cosmon_core::id::MoleculeId`]  | parse never panics; round-trip equality                |
//! | [`cosmon_core::tag::Tag`]        | parse never panics; key/value invariants               |
//! | [`cosmon_core::formula::Formula`] | parse never panics on arbitrary TOML text              |
//! | [`cosmon_core::nucleate::nucleate`] | required-var detection; pass-through invariant        |
//! | Lifecycle state machine        | `Ensemble` simulator: tackle/complete/done idempotence |
//! | Blocker DAG                    | no command admits a cycle; topological soundness       |
//!
//! This mirrors the choice made by `cosmon-crashtest` for the same
//! reason: an in-process simulator is deterministic, fast enough for
//! tens of thousands of cases per second, and does not require a
//! separate `cargo fuzz` toolchain. The harness is deliberately runnable
//! as a property-test suite under
//! `cargo test --workspace --features fuzz` rather than only as
//! `cargo fuzz run <target>`.
//!
//! ## Non-scope
//!
//! - Subprocess spawning (`cs tackle` → tmux → real worker).
//! - fsync-ordering or crash injection — covered by `cosmon-crashtest`.
//! - LLM-driven input generation (explicitly listed out-of-scope in the
//!   task briefing).
//! - Replacing the scenario harness (L2) — orthogonal.
//!
//! ## Running
//!
//! ```bash
//! cargo test -p cosmon-fuzz
//! # longer soak:
//! PROPTEST_CASES=10000 cargo test -p cosmon-fuzz --release
//! ```
//!
//! ## Extending
//!
//! 1. Add a new generator (`gen_*`) for the input class.
//! 2. Add an oracle function that asserts the invariant.
//! 3. Add a `proptest!` block in `tests/properties.rs` wiring the two.
//!
//! ## Historical corpus
//!
//! The regression corpus lives in [`corpus`]. Each entry is a synthetic
//! reproduction of a known incident (e.g. `convoy_cascade`, `b22c`,
//! `f4e1`). New incidents get a new entry with a link to the chronicle.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use cosmon_core::formula::Formula;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::nucleate::{nucleate, NucleateError, NucleateRequest, NucleateResult};
use cosmon_core::tag::Tag;
use rand::SeedableRng;

// ---------------------------------------------------------------------------
// Parse oracles — pure-function "does not panic" assertions
// ---------------------------------------------------------------------------

/// Oracle for [`MoleculeId::new`].
///
/// Asserts the parser is total: every input string maps to either
/// `Ok(MoleculeId)` or `Err(IdError)` without panicking. Also asserts the
/// "success = round-trip" invariant: a parsed id re-printed and re-parsed
/// yields an equal value.
///
/// Returns `true` if parse succeeded, `false` if it rejected the input.
pub fn oracle_parse_molecule_id(raw: &str) -> bool {
    match MoleculeId::new(raw.to_string()) {
        Ok(id) => {
            let round = MoleculeId::new(id.as_str().to_string())
                .expect("round-trip of a valid MoleculeId must succeed");
            assert_eq!(
                round.as_str(),
                id.as_str(),
                "MoleculeId round-trip changed the value"
            );
            assert_eq!(round.prefix(), id.prefix());
            assert_eq!(round.date(), id.date());
            assert_eq!(round.suffix(), id.suffix());
            true
        }
        Err(_) => false,
    }
}

/// Oracle for [`Tag::new`].
///
/// Asserts totality and key/value invariants on success: the key matches
/// the declared grammar and the raw form round-trips.
pub fn oracle_parse_tag(raw: &str) -> bool {
    match Tag::new(raw.to_string()) {
        Ok(t) => {
            let key = t.key();
            assert!(!key.is_empty(), "accepted tag has empty key: {raw:?}");
            let first = key.chars().next().expect("non-empty key");
            assert!(
                first.is_ascii_lowercase(),
                "accepted tag key starts with non-[a-z]: {raw:?}"
            );
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "accepted tag key violates kebab grammar: {raw:?}"
            );
            if let Some(value) = t.value() {
                assert!(!value.is_empty(), "accepted tag value is empty: {raw:?}");
                assert!(
                    value.chars().all(|c| !c.is_whitespace() && c != ':'),
                    "accepted tag value contains forbidden char: {raw:?}"
                );
            }
            let reparsed = Tag::new(t.to_string()).expect("Tag display must round-trip");
            assert_eq!(reparsed.to_string(), t.to_string());
            true
        }
        Err(_) => false,
    }
}

/// Oracle for [`WorkerId::new`].
///
/// Asserts totality; on success the `ep-` prefix bit round-trips.
pub fn oracle_parse_worker_id(raw: &str) -> bool {
    match WorkerId::new(raw.to_string()) {
        Ok(id) => {
            let reparsed =
                WorkerId::new(id.as_str().to_string()).expect("WorkerId round-trip must succeed");
            assert_eq!(reparsed.has_ensemble_prefix(), id.has_ensemble_prefix());
            assert_eq!(reparsed.name(), id.name());
            true
        }
        Err(_) => false,
    }
}

/// Oracle for [`Formula::parse`].
///
/// Asserts the TOML parser never panics on arbitrary input and that on
/// success every step has a non-empty id and `order` is within bounds.
pub fn oracle_parse_formula(toml_text: &str) -> bool {
    match Formula::parse(toml_text) {
        Ok(formula) => {
            assert!(!formula.steps.is_empty(), "accepted formula has no steps");
            let n = formula.steps.len();
            for step in &formula.steps {
                assert!(!step.id.is_empty(), "accepted formula has empty step id");
                assert!(
                    step.order < n,
                    "step order {} >= step count {}",
                    step.order,
                    n
                );
            }
            // Step ids must be unique within the formula.
            let mut seen = BTreeSet::new();
            for step in &formula.steps {
                assert!(
                    seen.insert(step.id.clone()),
                    "accepted formula has duplicate step id {:?}",
                    step.id
                );
            }
            true
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Nucleate oracle — property-level invariants over the domain function
// ---------------------------------------------------------------------------

/// A synthetic formula built from a seed, used as input to the nucleate
/// property tests. Avoids TOML round-tripping so we can generate thousands
/// of variants per second.
pub fn synthetic_formula(
    prefix: &str,
    required_vars: &[&str],
    optional_vars: &[(&str, &str)],
) -> Formula {
    let mut toml = String::new();
    toml.push_str(&format!(
        "formula = \"synth-{prefix}\"\nversion = 1\nid_prefix = \"{prefix}\"\n"
    ));
    for (k, v) in optional_vars {
        toml.push_str(&format!(
            "[vars.{k}]\nrequired = false\ndefault = \"{v}\"\n"
        ));
    }
    for k in required_vars {
        toml.push_str(&format!("[vars.{k}]\nrequired = true\n"));
    }
    toml.push_str("[[steps]]\nid = \"step-1\"\ntitle = \"Only step\"\ndescription = \"do it\"\n");
    Formula::parse(&toml).expect("synthetic formula must parse")
}

/// Outcome class of a nucleate attempt, used for property assertions.
#[derive(Debug, PartialEq, Eq)]
pub enum NucleateClass {
    /// Nucleation succeeded — a `NucleateResult` was produced.
    Ok,
    /// A required variable was missing.
    MissingVariable,
    /// A required variable was supplied blank (briefless-molecule guard,
    /// task-20260711-919a).
    EmptyVariable,
    /// The id_prefix failed validation.
    IdGeneration,
}

/// Classify a nucleate result for property assertions.
pub fn classify_nucleate(r: &Result<NucleateResult, NucleateError>) -> NucleateClass {
    match r {
        Ok(_) => NucleateClass::Ok,
        Err(NucleateError::MissingVariable(_)) => NucleateClass::MissingVariable,
        Err(NucleateError::EmptyVariable(_)) => NucleateClass::EmptyVariable,
        Err(NucleateError::IdGeneration(_)) => NucleateClass::IdGeneration,
    }
}

/// Oracle for [`nucleate`]: exercises the "required variable" contract.
///
/// - If every required var in `formula` is satisfied in `vars`, nucleation
///   must succeed and the produced `variables` map must contain every
///   required key.
/// - If at least one required var is missing, nucleation must fail with
///   [`NucleateError::MissingVariable`].
/// - The generated [`MoleculeId`] must carry the formula's `id_prefix`.
/// - User-supplied vars not declared by the formula are preserved
///   (pass-through invariant).
pub fn oracle_nucleate(formula: &Formula, vars: BTreeMap<String, String>) {
    // A required variable is *satisfied* only when it is present AND
    // non-blank — the briefless-molecule guard (task-20260711-919a) rejects
    // a required var supplied empty/whitespace exactly as it rejects a
    // missing one. Required vars with a default are always satisfiable.
    let all_required_satisfied = formula
        .variables
        .iter()
        .filter(|(_, v)| v.required && v.default.is_none())
        .all(|(k, _)| vars.get(k).is_some_and(|val| !val.trim().is_empty()));

    let req = NucleateRequest {
        formula,
        variables: vars.clone().into_iter().collect(),
        assign: None,
    };
    // Deterministic RNG keeps the property-test oracle reproducible without
    // touching ambient entropy.
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);
    let result = nucleate(req, &mut rng);
    let class = classify_nucleate(&result);

    if all_required_satisfied {
        assert_eq!(
            class,
            NucleateClass::Ok,
            "all required vars supplied but nucleation failed: {result:?}"
        );
        let ok = result.expect("checked above");
        // id_prefix preserved.
        assert_eq!(
            ok.id.prefix(),
            formula.id_prefix,
            "generated MoleculeId has wrong prefix"
        );
        // Every required var ended up in the resolved set.
        for (k, v) in &formula.variables {
            if v.required {
                assert!(
                    ok.variables.contains_key(k),
                    "resolved variables missing required key {k:?}"
                );
            }
        }
        // Pass-through: every user-supplied key is preserved.
        for (k, v) in &vars {
            assert_eq!(
                ok.variables.get(k),
                Some(v),
                "pass-through var {k:?} mutated or dropped"
            );
        }
        // total_steps matches.
        assert_eq!(ok.total_steps, formula.steps.len());
    } else {
        // At least one required var is unsatisfied — absent or blank. The
        // iteration order over the formula's variable map is non-deterministic
        // (HashMap), so when both an absent and a blank required var exist we
        // cannot predict which refusal fires first. Either typed refusal is a
        // correct outcome; both are the briefless-molecule contract.
        assert!(
            matches!(
                class,
                NucleateClass::MissingVariable | NucleateClass::EmptyVariable
            ),
            "unsatisfied required var but nucleation succeeded or wrong error: {result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Lifecycle state-machine simulator
// ---------------------------------------------------------------------------

/// A minimal model of the lifecycle state that `cs nucleate / tackle /
/// complete / done` manipulate. Used to exercise the merge-before-dispatch
/// invariant (`CLAUDE.md`, "Command perimeters") and the "tackle cannot
/// run while blockers are pending" contract.
///
/// This model is **deliberately smaller** than `cosmon-state::MoleculeData`:
/// only the fields that participate in lifecycle transitions are modeled.
/// The goal is to catch *control-flow* bugs (wrong predicate, missing
/// `--blocked-by` guard), not field-level serde drift.
#[derive(Debug, Default, Clone)]
pub struct Ensemble {
    /// Molecules keyed by id, in insertion order for determinism.
    pub molecules: BTreeMap<String, SimMolecule>,
}

/// State of a single simulated molecule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimMolecule {
    /// Current lifecycle status.
    pub status: SimStatus,
    /// Molecules that must be `Done` before this one can be `Tackled`.
    pub blocked_by: BTreeSet<String>,
    /// Tags currently attached to the molecule.
    pub tags: BTreeSet<String>,
}

/// Lifecycle status projection used by the simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimStatus {
    /// Created, not yet tackled.
    Pending,
    /// `cs tackle` has run — a worker is nominally active.
    Active,
    /// Worker reported completion, not yet merged.
    Completed,
    /// `cs done` has run — terminal, archived.
    Done,
    /// `cs collapse` has run — terminal, failure.
    Collapsed,
}

/// One CLI-shaped command that the simulator can execute. The naming
/// mirrors the `cs` CLI so the invariants read the same way the operator
/// protocol does.
#[derive(Debug, Clone)]
pub enum SimCommand {
    /// `cs nucleate <formula> [--blocked-by ...]`
    Nucleate {
        /// New molecule id.
        id: String,
        /// Predecessors — may reference non-existent ids (fuzzer job).
        blocked_by: Vec<String>,
    },
    /// `cs tag <id> --add <tag>`
    Tag {
        /// Target molecule.
        id: String,
        /// Tag text (may be ill-formed).
        tag: String,
    },
    /// `cs tackle <id>`
    Tackle {
        /// Target molecule.
        id: String,
    },
    /// Worker reports completion (`cs complete`).
    Complete {
        /// Target molecule.
        id: String,
    },
    /// `cs done <id>` — terminal, merges + tears down.
    Done {
        /// Target molecule.
        id: String,
    },
    /// `cs collapse <id> --reason <r>`
    Collapse {
        /// Target molecule.
        id: String,
    },
}

/// Outcome of applying a [`SimCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimOutcome {
    /// Applied successfully.
    Applied,
    /// Refused because the command is not legal in the current state.
    Refused(SimError),
}

/// Reasons a [`SimCommand`] may be refused by the simulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimError {
    /// Target molecule does not exist.
    UnknownMolecule,
    /// Molecule already exists (nucleate collision).
    DuplicateMolecule,
    /// `--blocked-by` references an unknown molecule.
    UnknownBlocker,
    /// `--blocked-by` introduces a cycle.
    CyclicBlocker,
    /// `tackle` attempted while a blocker is not yet `Done`.
    BlockerNotDone,
    /// Lifecycle transition is not allowed from the current status.
    IllegalTransition,
    /// Tag text failed [`Tag::new`] validation.
    InvalidTag,
    /// Self-referential `--blocked-by`.
    SelfBlocker,
}

impl Ensemble {
    /// Create an empty ensemble.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a [`SimCommand`] to the ensemble. All state transitions pass
    /// through this function; the invariants in [`Ensemble::check`] are
    /// preserved by construction when this function returns
    /// [`SimOutcome::Applied`].
    pub fn apply(&mut self, cmd: &SimCommand) -> SimOutcome {
        match cmd {
            SimCommand::Nucleate { id, blocked_by } => self.do_nucleate(id, blocked_by),
            SimCommand::Tag { id, tag } => self.do_tag(id, tag),
            SimCommand::Tackle { id } => self.do_tackle(id),
            SimCommand::Complete { id } => self.do_complete(id),
            SimCommand::Done { id } => self.do_done(id),
            SimCommand::Collapse { id } => self.do_collapse(id),
        }
    }

    fn do_nucleate(&mut self, id: &str, blocked_by: &[String]) -> SimOutcome {
        if self.molecules.contains_key(id) {
            return SimOutcome::Refused(SimError::DuplicateMolecule);
        }
        // Self-reference is illegal even if the id does not yet exist.
        if blocked_by.iter().any(|b| b == id) {
            return SimOutcome::Refused(SimError::SelfBlocker);
        }
        for b in blocked_by {
            if !self.molecules.contains_key(b) {
                return SimOutcome::Refused(SimError::UnknownBlocker);
            }
        }
        // Cycles: since blockers must pre-exist and the new id is fresh,
        // a cycle can only come from the self-reference we already rejected.
        // But we still double-check by walking the blocker chain — a bug
        // that lets a cycle through would surface here.
        let mut to_visit: Vec<String> = blocked_by.to_vec();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(b) = to_visit.pop() {
            if b == id {
                return SimOutcome::Refused(SimError::CyclicBlocker);
            }
            if !seen.insert(b.clone()) {
                continue;
            }
            if let Some(m) = self.molecules.get(&b) {
                for b2 in &m.blocked_by {
                    to_visit.push(b2.clone());
                }
            }
        }
        self.molecules.insert(
            id.to_string(),
            SimMolecule {
                status: SimStatus::Pending,
                blocked_by: blocked_by.iter().cloned().collect(),
                tags: BTreeSet::new(),
            },
        );
        SimOutcome::Applied
    }

    fn do_tag(&mut self, id: &str, tag: &str) -> SimOutcome {
        let Some(m) = self.molecules.get_mut(id) else {
            return SimOutcome::Refused(SimError::UnknownMolecule);
        };
        if Tag::new(tag.to_string()).is_err() {
            return SimOutcome::Refused(SimError::InvalidTag);
        }
        m.tags.insert(tag.to_string());
        SimOutcome::Applied
    }

    fn do_tackle(&mut self, id: &str) -> SimOutcome {
        // Read blockers first (borrow scope).
        let (current, blockers) = match self.molecules.get(id) {
            None => return SimOutcome::Refused(SimError::UnknownMolecule),
            Some(m) => (m.status, m.blocked_by.clone()),
        };
        if current != SimStatus::Pending {
            return SimOutcome::Refused(SimError::IllegalTransition);
        }
        // merge-before-dispatch: every blocker must be Done, not merely
        // Completed. This is the invariant documented in CLAUDE.md under
        // "Merge-before-dispatch".
        for b in &blockers {
            match self.molecules.get(b).map(|m| m.status) {
                Some(SimStatus::Done) => {}
                _ => return SimOutcome::Refused(SimError::BlockerNotDone),
            }
        }
        self.molecules.get_mut(id).expect("checked").status = SimStatus::Active;
        SimOutcome::Applied
    }

    fn do_complete(&mut self, id: &str) -> SimOutcome {
        let Some(m) = self.molecules.get_mut(id) else {
            return SimOutcome::Refused(SimError::UnknownMolecule);
        };
        match m.status {
            SimStatus::Active => {
                m.status = SimStatus::Completed;
                SimOutcome::Applied
            }
            SimStatus::Completed => SimOutcome::Applied, // idempotent
            _ => SimOutcome::Refused(SimError::IllegalTransition),
        }
    }

    fn do_done(&mut self, id: &str) -> SimOutcome {
        let Some(m) = self.molecules.get_mut(id) else {
            return SimOutcome::Refused(SimError::UnknownMolecule);
        };
        match m.status {
            SimStatus::Completed => {
                m.status = SimStatus::Done;
                SimOutcome::Applied
            }
            SimStatus::Done => SimOutcome::Applied, // idempotent
            _ => SimOutcome::Refused(SimError::IllegalTransition),
        }
    }

    fn do_collapse(&mut self, id: &str) -> SimOutcome {
        let Some(m) = self.molecules.get_mut(id) else {
            return SimOutcome::Refused(SimError::UnknownMolecule);
        };
        match m.status {
            SimStatus::Pending | SimStatus::Active => {
                m.status = SimStatus::Collapsed;
                SimOutcome::Applied
            }
            SimStatus::Collapsed => SimOutcome::Applied, // idempotent
            _ => SimOutcome::Refused(SimError::IllegalTransition),
        }
    }

    /// Global invariants that must hold after any sequence of applied
    /// commands. Violations indicate a bug in `apply` or in the caller's
    /// reasoning about state transitions — exactly the bug class the
    /// CLI type-tightening task aims to prevent by construction.
    ///
    /// # Panics
    /// Panics on invariant violation. Intended for use inside a
    /// proptest oracle.
    pub fn check(&self) {
        // Invariant 1: every blocker reference resolves to an existing molecule.
        for (id, m) in &self.molecules {
            for b in &m.blocked_by {
                assert!(
                    self.molecules.contains_key(b),
                    "molecule {id:?} has dangling blocker {b:?}"
                );
                assert_ne!(b, id, "molecule {id:?} blocks itself");
            }
        }
        // Invariant 2: no cycles in the blocker DAG (Kahn's algorithm).
        let mut in_degree: BTreeMap<String, usize> =
            self.molecules.keys().map(|k| (k.clone(), 0)).collect();
        for m in self.molecules.values() {
            for b in &m.blocked_by {
                *in_degree.entry(b.clone()).or_insert(0) += 0; // sink
            }
        }
        // Build reverse: for each (id -> blockers), we want blockers → id.
        let mut rev: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, m) in &self.molecules {
            for b in &m.blocked_by {
                rev.entry(b.clone()).or_default().push(id.clone());
                *in_degree.get_mut(id).expect("init") += 1;
            }
        }
        let mut queue: Vec<String> = in_degree
            .iter()
            .filter_map(|(k, &d)| if d == 0 { Some(k.clone()) } else { None })
            .collect();
        let mut visited = 0usize;
        while let Some(node) = queue.pop() {
            visited += 1;
            if let Some(successors) = rev.get(&node) {
                for s in successors {
                    let d = in_degree.get_mut(s).expect("init");
                    *d -= 1;
                    if *d == 0 {
                        queue.push(s.clone());
                    }
                }
            }
        }
        assert_eq!(
            visited,
            self.molecules.len(),
            "blocker DAG contains a cycle"
        );

        // Invariant 3: an Active or Completed molecule cannot have a blocker
        // whose status is anything other than Done. This is the
        // merge-before-dispatch invariant restated as a global check.
        for (id, m) in &self.molecules {
            if matches!(m.status, SimStatus::Active | SimStatus::Completed) {
                for b in &m.blocked_by {
                    let bstate = self
                        .molecules
                        .get(b)
                        .map(|m| m.status)
                        .expect("invariant 1");
                    assert_eq!(
                        bstate,
                        SimStatus::Done,
                        "molecule {id:?} is {:?} but blocker {b:?} is {:?}",
                        m.status,
                        bstate
                    );
                }
            }
        }

        // Invariant 4: every tag on every molecule parses as a Tag.
        for (id, m) in &self.molecules {
            for t in &m.tags {
                assert!(
                    Tag::new(t.clone()).is_ok(),
                    "molecule {id:?} has unparseable tag {t:?}"
                );
            }
        }
    }

    /// Reconcile projection — for the simulator, a deterministic summary
    /// of the ensemble that any observer can compute without modifying
    /// state. Used to assert `reconcile` idempotence:
    /// `reconcile(state) == reconcile(reconcile(state))`.
    #[must_use]
    pub fn reconcile(&self) -> ReconcileSurface {
        let mut pending = 0;
        let mut active = 0;
        let mut completed = 0;
        let mut done = 0;
        let mut collapsed = 0;
        let mut tags: BTreeMap<String, usize> = BTreeMap::new();
        for m in self.molecules.values() {
            match m.status {
                SimStatus::Pending => pending += 1,
                SimStatus::Active => active += 1,
                SimStatus::Completed => completed += 1,
                SimStatus::Done => done += 1,
                SimStatus::Collapsed => collapsed += 1,
            }
            for t in &m.tags {
                *tags.entry(t.clone()).or_insert(0) += 1;
            }
        }
        ReconcileSurface {
            pending,
            active,
            completed,
            done,
            collapsed,
            tags,
        }
    }
}

/// A deterministic projection of [`Ensemble`] computed by
/// [`Ensemble::reconcile`]. The reconcile function must be idempotent
/// and a pure function of the ensemble state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileSurface {
    /// Count of molecules in [`SimStatus::Pending`].
    pub pending: usize,
    /// Count of molecules in [`SimStatus::Active`].
    pub active: usize,
    /// Count of molecules in [`SimStatus::Completed`].
    pub completed: usize,
    /// Count of molecules in [`SimStatus::Done`].
    pub done: usize,
    /// Count of molecules in [`SimStatus::Collapsed`].
    pub collapsed: usize,
    /// Tag histogram across all molecules.
    pub tags: BTreeMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Corpus — regression seeds for historical incidents
// ---------------------------------------------------------------------------

/// Synthetic reproductions of historical incidents used as seed corpus.
///
/// Each function builds the minimal [`SimCommand`] sequence that a
/// correct simulator must refuse (or, conversely, must accept without
/// leaving the ensemble in a bad state). Running these cases on every
/// `cargo test` catches *regressions* of fixed bugs — the fuzz
/// generators explore forward from the corpus.
pub mod corpus {
    use super::{SimCommand, SimOutcome};

    /// A seed sequence: the label + commands to apply in order + the
    /// expected outcome of the **last** command.
    #[derive(Debug, Clone)]
    pub struct Seed {
        /// Human-readable incident name / chronicle slug.
        pub name: &'static str,
        /// Commands to apply, in order.
        pub commands: Vec<SimCommand>,
        /// Expected outcome of the final command.
        pub expected_final: SimOutcome,
    }

    /// `convoy-cascade` — a `cs run` / `cs patrol --respawn` resurrected
    /// stale `pending` molecules because they lacked `temp:*` tags.
    ///
    /// Synthetic shape: three `pending` molecules form a chain; the
    /// middle one has *no* blocker edge back to the first. Tackle of
    /// the tail should still refuse until the head is `done`.
    #[must_use]
    pub fn convoy_cascade() -> Seed {
        let cmds = vec![
            SimCommand::Nucleate {
                id: "head".into(),
                blocked_by: vec![],
            },
            SimCommand::Nucleate {
                id: "middle".into(),
                blocked_by: vec!["head".into()],
            },
            SimCommand::Nucleate {
                id: "tail".into(),
                blocked_by: vec!["middle".into()],
            },
            // Operator forgets to close `head` and `middle`; tries
            // tackling `tail` directly. Must refuse.
            SimCommand::Tackle { id: "tail".into() },
        ];
        Seed {
            name: "convoy-cascade",
            commands: cmds,
            expected_final: SimOutcome::Refused(super::SimError::BlockerNotDone),
        }
    }

    /// `b22c` placeholder — reserved slot for the "wrong-predicate on
    /// blocker check" incident referenced by the task briefing. The
    /// actual reproduction is attached here when the chronicle lands.
    ///
    /// Current shape: a dangling `--blocked-by` reference. A correct
    /// simulator must refuse with `UnknownBlocker`.
    #[must_use]
    pub fn b22c() -> Seed {
        let cmds = vec![SimCommand::Nucleate {
            id: "orphan".into(),
            blocked_by: vec!["does-not-exist".into()],
        }];
        Seed {
            name: "b22c-placeholder",
            commands: cmds,
            expected_final: SimOutcome::Refused(super::SimError::UnknownBlocker),
        }
    }

    /// `f4e1` placeholder — reserved slot for the "self-referencing
    /// `--blocked-by`" incident. A correct simulator must refuse with
    /// `SelfBlocker`.
    #[must_use]
    pub fn f4e1() -> Seed {
        let cmds = vec![SimCommand::Nucleate {
            id: "me".into(),
            blocked_by: vec!["me".into()],
        }];
        Seed {
            name: "f4e1-placeholder",
            commands: cmds,
            expected_final: SimOutcome::Refused(super::SimError::SelfBlocker),
        }
    }

    /// All seeds. New incidents append here.
    #[must_use]
    pub fn all() -> Vec<Seed> {
        vec![convoy_cascade(), b22c(), f4e1()]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_convoy_cascade_reproduces() {
        let seed = corpus::convoy_cascade();
        let mut ens = Ensemble::new();
        let mut last = None;
        for cmd in &seed.commands {
            last = Some(ens.apply(cmd));
            ens.check();
        }
        assert_eq!(last.as_ref(), Some(&seed.expected_final));
    }

    #[test]
    fn corpus_b22c_reproduces() {
        let seed = corpus::b22c();
        let mut ens = Ensemble::new();
        let mut last = None;
        for cmd in &seed.commands {
            last = Some(ens.apply(cmd));
            ens.check();
        }
        assert_eq!(last.as_ref(), Some(&seed.expected_final));
    }

    #[test]
    fn corpus_f4e1_reproduces() {
        let seed = corpus::f4e1();
        let mut ens = Ensemble::new();
        let mut last = None;
        for cmd in &seed.commands {
            last = Some(ens.apply(cmd));
            ens.check();
        }
        assert_eq!(last.as_ref(), Some(&seed.expected_final));
    }

    #[test]
    fn happy_path_runs_to_done() {
        let mut ens = Ensemble::new();
        assert_eq!(
            ens.apply(&SimCommand::Nucleate {
                id: "a".into(),
                blocked_by: vec![]
            }),
            SimOutcome::Applied
        );
        assert_eq!(
            ens.apply(&SimCommand::Tackle { id: "a".into() }),
            SimOutcome::Applied
        );
        assert_eq!(
            ens.apply(&SimCommand::Complete { id: "a".into() }),
            SimOutcome::Applied
        );
        assert_eq!(
            ens.apply(&SimCommand::Done { id: "a".into() }),
            SimOutcome::Applied
        );
        // Idempotent.
        assert_eq!(
            ens.apply(&SimCommand::Done { id: "a".into() }),
            SimOutcome::Applied
        );
        ens.check();
    }

    #[test]
    fn oracle_nucleate_missing_required_var() {
        let f = synthetic_formula("fz", &["topic"], &[]);
        oracle_nucleate(&f, BTreeMap::new());
    }

    #[test]
    fn oracle_nucleate_satisfied_required_var() {
        let f = synthetic_formula("fz", &["topic"], &[]);
        let vars: BTreeMap<String, String> = [("topic".to_string(), "fuzz".to_string())]
            .into_iter()
            .collect();
        oracle_nucleate(&f, vars);
    }

    #[test]
    fn parse_oracles_are_total_on_empty_input() {
        // These must never panic — just return false on rejection.
        assert!(!oracle_parse_molecule_id(""));
        assert!(!oracle_parse_tag(""));
        assert!(!oracle_parse_worker_id(""));
        assert!(!oracle_parse_formula(""));
    }
}
