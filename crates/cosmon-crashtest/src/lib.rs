// SPDX-License-Identifier: AGPL-3.0-only

//! # cosmon-crashtest
//!
//! Property-based crash-resilience harness for cosmon.
//!
//! ## Why this crate exists
//!
//! The bisimulation property states
//! that a crashed + resumed run must produce the same observable lifecycle
//! trace as an uninterrupted run, modulo wall-clock timestamps and opaque LLM
//! content. Manual smoke tests cover one crash point; proptest exercises
//! thousands. This crate is the harness.
//!
//! ## Scope and non-scope
//!
//! Scope: in-process simulation of the cosmon lifecycle state machine —
//! DAG traversal, step evolution, per-step artifact emission, SIGKILL-
//! equivalent abort at a random step, reload from disk, resume to completion,
//! and canonical trace comparison.
//!
//! Non-scope: fsync-ordering fault injection (needs `dm-flakey` + a Linux CI
//! lane), Byzantine concurrent-writer fuzz, real subprocess spawning with
//! actual `SIGKILL`. When the sibling tasks `cs resume` + idempotence guards
//! land and a Linux CI lane is wired up, the simulator here can be swapped
//! for a real-subprocess driver; the canonicalization + property statement
//! stay identical. That is the upgrade path from bisimulation (content-free)
//! to full structural equality on real traces.

#![forbid(unsafe_code)]

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Identifier of a single molecule within a generated DAG.
pub type MoleculeId = String;

/// A synthetic DAG: molecules with ordered steps and `blocked_by` edges.
///
/// The shape here matches the abstract structure of a cosmon task DAG: each
/// molecule has a linear sequence of steps, and inter-molecule ordering is
/// enforced by a `blocked_by` list. This is the minimal structure needed to
/// exercise the crash-and-resume invariant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dag {
    /// Molecules keyed by id. `BTreeMap` gives a deterministic iteration order,
    /// which matters for trace canonicalization.
    pub molecules: BTreeMap<MoleculeId, MoleculeSpec>,
}

/// A single molecule's spec: how many steps it runs and which molecules must
/// finish first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoleculeSpec {
    /// Number of steps in this molecule's lifecycle.
    pub steps: u8,
    /// Ids of molecules that must reach `Completed` before this one can start.
    pub blocked_by: Vec<MoleculeId>,
}

/// Lifecycle state of a molecule on the state lattice.
///
/// Terminal states are `Completed` and `Collapsed`. The bisimulation property
/// asserts terminal-state equality across runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lifecycle {
    Pending,
    Active,
    Completed,
    Collapsed,
}

/// Events emitted during a run. These are the observable trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// A molecule left `Pending` and entered `Active`.
    Nucleated { molecule: MoleculeId },
    /// A step advanced within an active molecule.
    Evolved {
        molecule: MoleculeId,
        step: u8,
        /// Classified artifact digest: `"present"` / `"absent"` / `"empty"`.
        artifact_class: String,
    },
    /// A molecule reached a terminal state.
    Completed { molecule: MoleculeId },
}

/// Full snapshot of a run's persisted state. This is what survives a crash
/// and is reloaded on resume.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunState {
    pub status: BTreeMap<MoleculeId, Lifecycle>,
    pub step_cursor: BTreeMap<MoleculeId, u8>,
    pub events: Vec<Event>,
}

/// Deterministic stand-in for an LLM worker. Given a seed, produces the same
/// canned artifact for a given `(molecule, step)` across the baseline and
/// resumed runs. This is the lever that turns bisimulation into structural
/// equality on content presence.
#[derive(Debug, Clone)]
pub struct DeterministicLlmStub {
    seed: u64,
}

impl DeterministicLlmStub {
    /// Build a new stub. The seed governs all synthetic artifact generation
    /// and is the only source of run-to-run variance when crashes are absent.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Emit a canned artifact for a step. Written verbatim into the state dir
    /// by the driver.
    #[must_use]
    pub fn artifact_for(&self, molecule: &str, step: u8) -> Vec<u8> {
        // A seeded RNG mixing seed + molecule + step — same inputs, same bytes.
        let mut rng = StdRng::seed_from_u64(
            self.seed
                ^ fnv1a(molecule.as_bytes())
                ^ u64::from(step).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let len = rng.gen_range(8..64);
        (0..len).map(|_| rng.gen::<u8>()).collect()
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A temporary fleet directory backed by the OS tmp (usually tmpfs on Linux).
///
/// Each run uses a fresh one to eliminate cross-run coupling.
#[derive(Debug)]
pub struct TempFleet {
    dir: TempDir,
}

impl TempFleet {
    /// Allocate a new fleet dir. Panics on OS failure — acceptable in a test
    /// harness.
    #[must_use]
    pub fn new_tmpfs() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tmpfs allocation failed"),
        }
    }

    /// Path to the fleet root.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    fn state_path(&self) -> PathBuf {
        self.dir.path().join("state.json")
    }

    /// Load persisted state. Returns default if the file is absent.
    ///
    /// # Errors
    /// Returns an error if the state file exists but cannot be read or parsed.
    pub fn load(&self) -> Result<RunState, String> {
        match fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RunState::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Persist state. Written atomically via rename so a crash cannot produce
    /// a torn state file — this is the property we want to exercise.
    ///
    /// # Errors
    /// Returns an error if the state file cannot be written.
    pub fn save(&self, state: &RunState) -> Result<(), String> {
        let tmp = self.state_path().with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
        fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.state_path()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Outcome of a run: terminal lifecycle states + emitted event trace.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub terminal: BTreeMap<MoleculeId, Lifecycle>,
    pub events: Vec<Event>,
}

/// Drive a DAG to completion inside `fleet`, writing artifacts from `llm`.
///
/// If `abort_after` is `Some(k)`, the driver returns after emitting exactly
/// `k` steps without completing — simulating a `SIGKILL` on a worker. State
/// on disk is consistent at every return point (atomic rename semantics).
///
/// # Errors
/// Propagates any error from state persistence.
pub fn run(
    fleet: &TempFleet,
    dag: &Dag,
    llm: &DeterministicLlmStub,
    abort_after: Option<usize>,
) -> Result<RunOutcome, String> {
    let mut state = fleet.load()?;
    let mut steps_executed: usize = 0;

    while let Some(id) = pick_ready(dag, &state) {
        let status = state.status.entry(id.clone()).or_insert(Lifecycle::Pending);
        if *status == Lifecycle::Pending {
            *status = Lifecycle::Active;
            state.events.push(Event::Nucleated {
                molecule: id.clone(),
            });
            fleet.save(&state)?;
        }

        let spec = &dag.molecules[&id];
        let cursor = state.step_cursor.entry(id.clone()).or_insert(0);
        if *cursor < spec.steps {
            let step = *cursor;
            let artifact = llm.artifact_for(&id, step);
            let artifact_path = fleet.path().join(format!("{id}.{step}.artifact"));
            fs::write(&artifact_path, &artifact).map_err(|e| e.to_string())?;
            let class = if artifact.is_empty() {
                "empty"
            } else {
                "present"
            };
            state.events.push(Event::Evolved {
                molecule: id.clone(),
                step,
                artifact_class: class.to_string(),
            });
            *cursor += 1;
            fleet.save(&state)?;
            steps_executed += 1;
            if Some(steps_executed) == abort_after {
                // Simulated SIGKILL — return without completing.
                return Ok(RunOutcome {
                    terminal: state.status.clone(),
                    events: state.events.clone(),
                });
            }
            continue;
        }

        // Molecule finished all its steps.
        state.status.insert(id.clone(), Lifecycle::Completed);
        state.events.push(Event::Completed {
            molecule: id.clone(),
        });
        fleet.save(&state)?;
    }

    Ok(RunOutcome {
        terminal: state.status.clone(),
        events: state.events.clone(),
    })
}

fn pick_ready(dag: &Dag, state: &RunState) -> Option<MoleculeId> {
    for (id, spec) in &dag.molecules {
        let status = state.status.get(id).copied().unwrap_or(Lifecycle::Pending);
        if status == Lifecycle::Completed || status == Lifecycle::Collapsed {
            continue;
        }
        let blockers_done = spec.blocked_by.iter().all(|b| {
            matches!(
                state.status.get(b).copied().unwrap_or(Lifecycle::Pending),
                Lifecycle::Completed
            )
        });
        if blockers_done {
            return Some(id.clone());
        }
    }
    None
}

/// Canonicalize an event trace for bisimulation comparison.
///
/// Erases wall-clock timestamps (there are none in the model — that is
/// intentional; real-subprocess mode must strip them), keeps the sequence of
/// `(molecule, step, artifact_class)` tuples, and normalizes concurrent
/// transitions by sorting on `(molecule_id, logical_step)` — the iteration
/// order of `BTreeMap` already gives us the `molecule_id` dimension.
#[must_use]
pub fn canonicalize(events: &[Event]) -> Vec<Event> {
    // In this model the driver is single-threaded and deterministic, so the
    // only canonicalization step is to drop any hypothetical nondeterminism
    // hook. Kept as a function so the property statement reads the same as
    // the real-subprocess harness will.
    events.to_vec()
}

/// Convenience: generate a small random DAG. Used by the proptest strategy
/// and by doc-tests. Node count in `1..=max_size`.
///
/// # Panics
/// Panics if `max_size` is 0.
#[must_use]
pub fn gen_dag(seed: u64, max_size: u8) -> Dag {
    assert!(max_size >= 1, "max_size must be >= 1");
    let mut rng = StdRng::seed_from_u64(seed);
    let n = rng.gen_range(1..=max_size);
    let ids: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
    let mut molecules = BTreeMap::new();
    for (i, id) in ids.iter().enumerate() {
        let steps = rng.gen_range(1u8..=3);
        let mut blocked_by = Vec::new();
        for prev in &ids[..i] {
            if rng.gen_bool(0.3) {
                blocked_by.push(prev.clone());
            }
        }
        molecules.insert(id.clone(), MoleculeSpec { steps, blocked_by });
    }
    Dag { molecules }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninterrupted_run_reaches_all_completed() {
        let fleet = TempFleet::new_tmpfs();
        let dag = gen_dag(42, 4);
        let llm = DeterministicLlmStub::new(7);
        let out = run(&fleet, &dag, &llm, None).unwrap();
        for id in dag.molecules.keys() {
            assert_eq!(out.terminal.get(id), Some(&Lifecycle::Completed));
        }
    }

    #[test]
    fn crash_and_resume_reaches_same_terminal_states() {
        let dag = gen_dag(123, 3);
        let llm = DeterministicLlmStub::new(11);

        let baseline_fleet = TempFleet::new_tmpfs();
        let baseline = run(&baseline_fleet, &dag, &llm, None).unwrap();

        let crashed_fleet = TempFleet::new_tmpfs();
        let _ = run(&crashed_fleet, &dag, &llm, Some(2)).unwrap();
        let resumed = run(&crashed_fleet, &dag, &llm, None).unwrap();

        assert_eq!(
            canonicalize(&baseline.events),
            canonicalize(&resumed.events)
        );
        assert_eq!(baseline.terminal, resumed.terminal);
    }

    #[test]
    fn deterministic_stub_is_deterministic() {
        let a = DeterministicLlmStub::new(5).artifact_for("m0", 1);
        let b = DeterministicLlmStub::new(5).artifact_for("m0", 1);
        assert_eq!(a, b);
    }
}
