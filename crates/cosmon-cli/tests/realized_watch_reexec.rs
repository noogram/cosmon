// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test — the **real re-exec** of the first-turn realized-model
//! watcher (round-4 / COND-1 reserve, delib-20260718-c70e / D4).
//!
//! The unit test in `cmd::realized_watch` drives `watch_realized` in-process:
//! it proves the loop, not the dispatch. In production nothing calls that
//! function directly — `cs tackle` re-execs the binary as a detached
//! `cs realized-watch …` child, and everything between "tackle decided to arm
//! the watcher" and "the loop runs" (argv shape, global `--config` routing,
//! clap wiring of the hidden subcommand, the child's own `$HOME` session-log
//! resolution) was untested. That gap is what this file closes: the `cs`
//! binary is invoked as a subprocess, with the argv `cs tackle` itself builds
//! ([`cosmon_cli::realized_watcher::watcher_argv`]), against a fixture
//! molecule.
//!
//! Four properties, one dispatch each:
//!
//! 1. **First turn** — the child is armed *before* any turn exists; the turn
//!    is written afterwards and `ModelObserved` lands on `events.jsonl` while
//!    the molecule is still Running (no `cs wait`, no `cs complete`).
//! 2. **Atomic dedup** — many ticks over an unchanged trajectory leave
//!    exactly one observation.
//! 3. **Life bound** — the molecule leaving the live set winds the child down
//!    on its own; it exits 0 without being signalled.
//! 4. **Hard timeout** — a molecule that never leaves Running still bounds
//!    the child: `--timeout-secs` fires and it exits (ADR-016: bounded,
//!    never a daemon).
//!
//! Deterministic and self-contained: no tmux, no network, no real agent, and
//! `$HOME` is redirected **per child process** rather than mutated globally,
//! so the test is parallel-safe and cheap enough (a few seconds) to live in
//! the ordinary `Test` job of `ci.yml`.

#![cfg(unix)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use chrono::Utc;
use cosmon_core::event_v2::{AdapterSelectionSource, EventV2};
use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};

/// Upper bound on any single wait in this test. Generous enough to absorb a
/// loaded CI runner, short enough that a genuine hang is reported as a
/// failure instead of stalling the job.
const PATIENCE: Duration = Duration::from_secs(30);

/// One dispatch's fixture: the state dir the watcher writes to, the worktree
/// it resolves the session log from, and the `$HOME` its lookup is rooted at.
struct Dispatch {
    _root: tempfile::TempDir,
    _home: tempfile::TempDir,
    home: PathBuf,
    state_dir: PathBuf,
    worktree: PathBuf,
    mol: MoleculeId,
    store: FileStore,
}

impl Dispatch {
    /// Seed a Running molecule with its dispatch journal (`AdapterSelected` +
    /// `WorkerSpawned`) — the two records the capture core needs to resolve
    /// the adapter family and to scope the observation to a worker (F-02).
    fn seed(id: &str) -> Self {
        let root = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let mol = MoleculeId::new(id).unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let worktree = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        let store = FileStore::new(&state_dir);
        store.save_molecule(&mol, &running_molecule(&mol)).unwrap();

        let log = cosmon_state::event_log::resolve_events_log_path(&state_dir);
        cosmon_state::event_log::emit_one(
            &log,
            EventV2::AdapterSelected {
                mol_id: mol.clone(),
                adapter_name: "claude".to_owned(),
                selected_at: Utc::now(),
                selection_source: AdapterSelectionSource::Cli {
                    flag: "claude".to_owned(),
                },
                role_hint: None,
                loop_ownership: Default::default(),
            },
            None,
        )
        .unwrap();
        cosmon_state::event_log::emit_one(
            &log,
            EventV2::WorkerSpawned {
                worker_id: WorkerId::new("worker-1").unwrap(),
                molecule: Some(mol.clone()),
                session_name: "sess".to_owned(),
                role: "polecat".to_owned(),
                adapter_name: "claude".to_owned(),
                loop_ownership: Default::default(),
            },
            None,
        )
        .unwrap();

        Self {
            home: home.path().to_path_buf(),
            _root: root,
            _home: home,
            state_dir,
            worktree,
            mol,
            store,
        }
    }

    /// Arm the watcher the way `cs tackle` does: the real binary, the real
    /// argv, detached stdio — only the cadence is compressed so the test runs
    /// in seconds instead of hours.
    fn arm_watcher(&self, interval_ms: u64, timeout_secs: u64) -> Child {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.args(cosmon_cli::realized_watcher::watcher_argv(
            self.mol.as_str(),
            &self.worktree,
            &self.state_dir,
        ))
        .arg("--interval-ms")
        .arg(interval_ms.to_string())
        .arg("--timeout-secs")
        .arg(timeout_secs.to_string())
        // Redirect the child's session-log lookup into the fixture instead
        // of mutating this process's HOME — that is what keeps the test
        // parallel-safe where the in-process unit test needs a mutex.
        .env("HOME", &self.home)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        cmd.spawn().expect("spawn cs realized-watch")
    }

    /// Write the worker's first model-bearing assistant turn, exactly where a
    /// real `claude` run in `worktree` would put it.
    fn write_first_turn(&self, model: &str) {
        let encoded: String = self
            .worktree
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let proj = self.home.join(".claude").join("projects").join(encoded);
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            format!("{{\"type\":\"assistant\",\"message\":{{\"model\":\"{model}\"}}}}\n"),
        )
        .unwrap();
    }

    /// Move the molecule out of the live set — what harvest/collapse does in
    /// production, and the watcher's normal wind-down signal.
    fn collapse(&self) {
        let mut data = self.store.load_molecule(&self.mol).unwrap();
        data.status = MoleculeStatus::Collapsed;
        self.store.save_molecule(&self.mol, &data).unwrap();
    }

    /// Every `ModelObserved` currently on the journal.
    fn observations(&self) -> Vec<EventV2> {
        let log = cosmon_state::event_log::resolve_events_log_path(&self.state_dir);
        cosmon_state::event_log::read_all(&log)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.event)
            .filter(|e| matches!(e, EventV2::ModelObserved { .. }))
            .collect()
    }

    /// Fold the molecule's journal into its adapter attribution — the shape
    /// the operator surfaces (`cs observe`, `compact_cell`) actually read.
    fn attribution(&self) -> cosmon_core::adapter_attribution::AdapterAttribution {
        let log = cosmon_state::event_log::resolve_events_log_path(&self.state_dir);
        let events: Vec<EventV2> = cosmon_state::event_log::read_all(&log)
            .unwrap()
            .into_iter()
            .filter(|e| e.event.molecule_id() == Some(&self.mol))
            .map(|e| e.event)
            .collect();
        cosmon_core::adapter_attribution::AdapterAttribution::fold(&events)
    }
}

/// A minimal Running molecule — enough for the watcher's liveness predicate.
fn running_molecule(mol: &MoleculeId) -> MoleculeData {
    MoleculeData {
        id: mol.clone(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Running,
        variables: HashMap::new(),
        assigned_worker: Some(WorkerId::new("worker-1").unwrap()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: vec![],
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: Vec::new(),
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        pending_step: None,
        merged_at: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    }
}

/// Poll `cond` until it holds or [`PATIENCE`] elapses. Returns whether it
/// held — a timeout is a finding (the watcher never emitted), never a retry.
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Wait for `child` to exit on its own within [`PATIENCE`]; kill and fail
/// otherwise. A watcher that must be killed is a watcher that leaked.
fn wait_for_exit(child: &mut Child, what: &str) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "{what}: watcher exited {status}");
                return;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("{what}: watcher never exited on its own — it became a daemon");
}

/// The whole COND-1 seam through the production entrypoint: arm the detached
/// `cs realized-watch` child at dispatch, let the worker write its first
/// model-bearing turn afterwards, and require the observation to land while
/// the molecule is still Running — then wind the child down by collapsing the
/// molecule and check the dedup left exactly one line.
#[test]
fn reexeced_watcher_observes_first_turn_then_winds_down() {
    let d = Dispatch::seed("task-20260719-1a01");
    let mut child = d.arm_watcher(50, 600);

    // Armed before any turn exists — the ordering is the point.
    assert!(d.observations().is_empty(), "nothing to observe yet");
    d.write_first_turn("claude-opus-4-8");

    assert!(
        wait_until(|| !d.observations().is_empty()),
        "the re-execed watcher must emit ModelObserved on the first turn, \
         with no cs wait / cs run / cs complete in the picture"
    );
    assert_eq!(
        d.store.load_molecule(&d.mol).unwrap().status,
        MoleculeStatus::Running,
        "the observation must land during the run, not at teardown"
    );

    // Many more ticks over an unchanged trajectory: the dedup is atomic.
    std::thread::sleep(Duration::from_millis(500));
    d.collapse();
    wait_for_exit(&mut child, "first-turn watcher");

    assert_eq!(
        d.observations().len(),
        1,
        "many ticks, one observation — the dedup holds across the re-exec"
    );
    assert_eq!(
        d.attribution().realized,
        cosmon_core::adapter_attribution::Realized::Observed(vec!["claude-opus-4-8".to_string()]),
        "the folded attribution names the model the worker actually ran"
    );
}

/// ADR-016: bounded, never a daemon. A crashed worker's molecule can sit in
/// Running forever (nobody harvests it), so the watcher must not depend on
/// the wind-down signal to terminate — `--timeout-secs` bounds it alone.
#[test]
fn reexeced_watcher_respects_its_hard_life_bound() {
    let d = Dispatch::seed("task-20260719-1a02");
    // The molecule stays Running for the whole test: only the timeout can
    // end this child.
    let mut child = d.arm_watcher(50, 1);

    wait_for_exit(&mut child, "life-bounded watcher");
    assert_eq!(
        d.store.load_molecule(&d.mol).unwrap().status,
        MoleculeStatus::Running,
        "the molecule never left the live set — the bound is what fired"
    );
}

/// The post-mortem property, through the re-exec: a turn written after the
/// worker's death is still on disk, and the watcher's final sweep picks it
/// up. Nothing about the emission is teardown-borne — there is no
/// `MoleculeCompleted` anywhere on the journal.
#[test]
fn reexeced_watcher_final_sweep_catches_a_dead_workers_turn() {
    let d = Dispatch::seed("task-20260719-1a03");
    let mut child = d.arm_watcher(50, 600);

    // The worker dies and the molecule is collapsed *in the same breath* as
    // its last turn hitting the disk — the race the final sweep exists for.
    let log = cosmon_state::event_log::resolve_events_log_path(&d.state_dir);
    cosmon_state::event_log::emit_one(
        &log,
        EventV2::WorkerExited {
            molecule_id: d.mol.clone(),
            exit_code: Some(137),
            reason: "pane_died".to_owned(),
        },
        None,
    )
    .unwrap();
    d.write_first_turn("claude-sonnet-5");
    d.collapse();

    wait_for_exit(&mut child, "post-mortem watcher");
    assert_eq!(
        d.observations().len(),
        1,
        "the final sweep must recover the dead worker's durable turn"
    );
    assert!(
        !read_events(&d.state_dir)
            .iter()
            .any(|e| matches!(e, EventV2::MoleculeCompleted { .. })),
        "no completion ever happened — the emission cannot be teardown-borne"
    );
}

/// Every event on a state dir's journal, in order.
fn read_events(state_dir: &Path) -> Vec<EventV2> {
    let log = cosmon_state::event_log::resolve_events_log_path(state_dir);
    cosmon_state::event_log::read_all(&log)
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.event)
        .collect()
}
