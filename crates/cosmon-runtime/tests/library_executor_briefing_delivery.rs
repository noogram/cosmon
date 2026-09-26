// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #81 point 1 — the library executor hands its briefing to the
//! installed [`BriefingDelivery`] port and acts on what the port saw.
//!
//! The reported defect: an API-dispatched Claude worker kept its briefing
//! in the composer, unsubmitted, because the executor wrote it once at spawn
//! time and never looked again. The port is where the wait and the re-submit
//! live; what is asserted here is the executor's half — the port is the one
//! that delivers, its report is recorded, and a briefing the composer never
//! released fails the dispatch and tears the worker down instead of leaving
//! a `running` molecule behind.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cosmon_core::id::MoleculeId;
use cosmon_core::injection::BriefingDeliveryOutcome;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::transport::{TransportBackend, TransportError};
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    BriefingDelivery, BriefingDeliveryContext, BriefingDeliveryReport, DispatchPin, Executor,
    LibraryExecutor,
};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::{MockBackend, MockCall};

/// The same shadow environment the sibling falsifier installs: a `PATH` with
/// no `cs`, and none of the cosmon env tiers that would let a dispatch read
/// adapter defaults from outside the fixture. Without the last part the
/// operator's own `COSMON_DEFAULT_ADAPTER` would decide which arm of the
/// launch builder this test exercises.
fn shadow_env() {
    static SHADOW: OnceLock<()> = OnceLock::new();
    SHADOW.get_or_init(|| {
        std::env::set_var("PATH", "/usr/bin:/bin");
        for var in [
            "COSMON_STATE_DIR",
            "COSMON_CONFIG",
            "COSMON_FORMULAS_DIR",
            "COSMON_DEFAULT_ADAPTER",
            "COSMON_DEFAULT_MODEL",
            "ANTHROPIC_MODEL",
        ] {
            std::env::remove_var(var);
        }
        let scratch = std::env::temp_dir().join(format!("cosmon-75-cfg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        std::env::set_var("COSMON_CONFIG_HOME", &scratch);
    });
}

/// A pending molecule in the canonical fleet shape.
fn pending_molecule(id: &str) -> MoleculeData {
    let now = chrono::Utc::now();
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("id"),
        fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet"),
        formula_id: cosmon_core::id::FormulaId::new("task-work").expect("formula"),
        status: MoleculeStatus::Pending,
        variables: std::collections::HashMap::new(),
        assigned_worker: None,
        created_at: now,
        updated_at: now,
        total_steps: 2,
        current_step: 0,
        completed_steps: vec![],
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: vec![],
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: vec![],
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: vec![],
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        base_branch: None,
        pending_step: None,
        merged_at: None,
        non_integration: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
        adapter: None,
    }
}

/// A throwaway galaxy: a git repository whose root carries `.cosmon/state/`
/// with one pending molecule pinned to the `claude` adapter.
fn fixture(id: &str) -> (tempfile::TempDir, PathBuf, FileStore, MoleculeData) {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let init = std::process::Command::new("git")
        .args(["-C", &project.to_string_lossy(), "init", "-q"])
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let state_dir = project.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(project.join(".cosmon").join("config.toml"), "").expect("config marker");
    let store = FileStore::new(&state_dir);
    let mol = pending_molecule(id);
    store.save_molecule(&mol.id, &mol).expect("seed molecule");
    (dir, project, store, mol)
}

/// Pin the dispatch to the `claude` adapter, so the test exercises the arm of
/// the launch builder it is about rather than whatever adapter the fixture's
/// (empty) config would default to.
fn claude_pin() -> DispatchPin {
    DispatchPin {
        adapter: Some("claude".to_owned()),
        model: None,
        base_branch: None,
    }
}

/// A port that reports a fixed outcome and remembers what it was handed.
#[derive(Debug)]
struct ScriptedDelivery {
    outcome: BriefingDeliveryOutcome,
    seen: Mutex<Vec<(String, String)>>,
}

impl ScriptedDelivery {
    fn new(outcome: BriefingDeliveryOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            seen: Mutex::new(Vec::new()),
        })
    }
}

impl BriefingDelivery for ScriptedDelivery {
    fn deliver(
        &self,
        _backend: &dyn TransportBackend,
        ctx: &BriefingDeliveryContext<'_>,
    ) -> Result<BriefingDeliveryReport, TransportError> {
        self.seen
            .lock()
            .expect("lock")
            .push((ctx.adapter.to_owned(), ctx.worker.name().to_owned()));
        Ok(BriefingDeliveryReport {
            outcome: self.outcome,
            resubmits: 3,
            elapsed: Duration::from_secs(2),
        })
    }
}

/// The `BriefingDelivery` rows recorded in the molecule's directory.
fn delivery_rows(store: &FileStore, id: &MoleculeId) -> Vec<String> {
    let log =
        std::fs::read_to_string(store.molecule_dir(id).join("events.jsonl")).unwrap_or_default();
    log.lines()
        .filter(|l| l.contains("briefing_delivery") || l.contains("BriefingDelivery"))
        .map(str::to_owned)
        .collect()
}

fn run(
    id: &str,
    outcome: BriefingDeliveryOutcome,
) -> (
    tempfile::TempDir,
    FileStore,
    MoleculeId,
    MockBackend,
    Arc<ScriptedDelivery>,
    Result<(), cosmon_runtime::RuntimeError>,
) {
    shadow_env();
    let (dir, project, store, mol) = fixture(id);
    let backend = MockBackend::new();
    let port = ScriptedDelivery::new(outcome);
    let executor =
        LibraryExecutor::new(&project, backend.clone()).with_briefing_delivery(port.clone());
    let result = executor.dispatch_with_pin(&mol.id, &claude_pin());
    (dir, store, mol.id, backend, port, result)
}

/// The port, not a bare write, delivers the briefing; its report lands in
/// the molecule's event log.
#[test]
fn a_delivered_briefing_goes_through_the_port_and_is_recorded() {
    let (_dir, store, id, backend, port, result) =
        run("task-20260925-d001", BriefingDeliveryOutcome::Delivered);

    result.expect("a delivered briefing is a successful dispatch");
    let seen = port.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1, "the port must be called exactly once");
    assert_eq!(seen[0].0, "claude");
    assert!(
        !backend
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::SendInput { .. })),
        "with a port installed the executor must not write the briefing itself"
    );
    let rows = delivery_rows(&store, &id);
    assert_eq!(rows.len(), 1, "one delivery row expected: {rows:?}");
    assert!(rows[0].contains("\"delivered\""), "{}", rows[0]);
}

/// The issue #81 stall, made loud: a briefing still in the composer at the
/// end of the port's budget fails the dispatch, terminates the worker, and
/// leaves the molecule dispatchable rather than `running`.
#[test]
fn an_undelivered_briefing_fails_the_dispatch_and_tears_the_worker_down() {
    let (_dir, store, id, backend, _port, result) =
        run("task-20260925-d002", BriefingDeliveryOutcome::Undelivered);

    let err = result.expect_err("an undelivered briefing must fail the dispatch");
    assert!(
        err.to_string().contains("briefing not delivered"),
        "the error must name the cause: {err}"
    );
    assert!(
        backend
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::Terminate { .. })),
        "the stranded worker must be terminated"
    );
    let mol = store.load_molecule(&id).expect("molecule");
    assert_ne!(
        mol.status,
        MoleculeStatus::Running,
        "a worker that never started must not leave the molecule running"
    );
    let rows = delivery_rows(&store, &id);
    assert_eq!(
        rows.len(),
        1,
        "the undelivered outcome must be recorded: {rows:?}"
    );
    assert!(rows[0].contains("\"undelivered\""), "{}", rows[0]);
}

/// A briefing the port could not observe proceeds, as in `cs tackle`'s
/// claude arm: absence of evidence is not evidence of a stranded worker.
#[test]
fn an_unobservable_delivery_does_not_fail_the_dispatch() {
    let (_dir, _store, _id, backend, _port, result) =
        run("task-20260925-d003", BriefingDeliveryOutcome::Unobservable);

    result.expect("an unobservable delivery proceeds");
    assert!(
        !backend
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::Terminate { .. })),
        "a worker that may be working must not be terminated"
    );
}
