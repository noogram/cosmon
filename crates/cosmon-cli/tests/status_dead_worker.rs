// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #172 — `cs status` must not call a dead worker alive or the galaxy
//! clean.
//!
//! A `Running` molecule whose recorded tmux session no longer exists is a
//! dead-pane ghost: `cs ensemble` already reports it (`EFFECTIVE dead`,
//! `⚠ DEAD`). `cs status` counted it among the alive molecules and printed
//! `clean`, so the one command read to decide "is anything wrong?" said no.
//! These tests drive the real binary against a fixture whose session cannot
//! exist and assert the property the issue asks for.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::worker::WorkerStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};

const MOL: &str = "task-20260101-dead";
const WORKER: &str = "worker-whose-host-vanished";

fn running_molecule(id: &str, worker: &WorkerId) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status: MoleculeStatus::Running,
        variables: HashMap::new(),
        assigned_worker: Some(worker.clone()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 2,
        current_step: 1,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: Vec::new(),
        protected_paths: Vec::new(),
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
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

fn seed_dead_worker(state_dir: &Path) {
    let worker_id = WorkerId::new(WORKER).expect("valid worker id");
    let molecule = running_molecule(MOL, &worker_id);
    let mut fleet = Fleet::default();
    fleet.workers.insert(
        worker_id.clone(),
        WorkerData::new(
            worker_id,
            AgentId::new("tackle").expect("valid agent id"),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        )
        .with_molecule(molecule.id.clone()),
    );
    let store = FileStore::new(state_dir);
    store.save_fleet(&fleet).expect("save fleet");
    store
        .save_molecule(&molecule.id, &molecule)
        .expect("save molecule");
}

fn run_status(tmp: &Path, state_dir: &Path, json: bool) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(tmp)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    if json {
        cmd.arg("--json");
    }
    cmd.args([
        "--config",
        state_dir.to_str().expect("utf-8 state dir"),
        "status",
    ]);
    let out = cmd.output().expect("run cs status");
    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The property of the issue: a running molecule without a live session is
/// counted as dead, is not counted as healthy alive work, and makes the
/// galaxy not clean.
#[test]
fn running_molecule_without_a_session_is_dead_not_alive_and_not_clean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed_dead_worker(&state_dir);

    let value: serde_json::Value =
        serde_json::from_str(&run_status(tmp.path(), &state_dir, true)).expect("status JSON");

    assert_eq!(value["hygiene"]["clean"], false, "a dead worker is residue");
    assert_eq!(value["hygiene"]["dead_workers"], 1);
    assert_eq!(value["molecules"]["dead_workers"], 1);
    assert_eq!(value["molecules"]["dead"][0]["molecule"], MOL);
    // `alive` keeps its historical meaning (every non-terminal molecule);
    // the healthy count is the additive key.
    assert_eq!(value["molecules"]["alive"], 1);
    assert_eq!(value["molecules"]["alive_healthy"], 0);
}

/// The headline names the dead worker and its reclaim gesture, and does not
/// print `clean`.
#[test]
fn compact_headline_names_the_dead_worker_and_its_reclaim_gesture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed_dead_worker(&state_dir);

    let text = run_status(tmp.path(), &state_dir, false);

    let headline = text.lines().next().unwrap_or_default();
    assert!(
        headline.contains("1\u{1F480} dead"),
        "the first line must count the dead worker: {text}"
    );
    assert!(text.contains(WORKER), "names the worker: {text}");
    assert!(
        text.contains(&format!("cs purge {WORKER}")),
        "names the reclaim gesture: {text}"
    );
    assert!(!text.contains("clean"), "must not print clean: {text}");
    assert!(!text.contains("0 dead"), "{text}");
}

/// Issue #175 — a pane that is alive but whose pasted brief was never
/// submitted is not a dead worker. `cs purge` on it would destroy a live,
/// recoverable session.
mod unsubmitted_brief {
    use super::*;

    struct Tmux {
        socket: String,
    }

    impl Tmux {
        fn run(&self, args: &[&str]) -> std::process::Output {
            Command::new("tmux")
                .args(["-L", &self.socket])
                .args(args)
                .output()
                .expect("run tmux")
        }
    }

    impl Drop for Tmux {
        fn drop(&mut self) {
            let _ = self.run(&["kill-server"]);
        }
    }

    /// Start a live session named after the worker and paste a brief into
    /// its composer without the submitting Enter.
    fn live_pane_with_unsubmitted_brief(state_dir: &Path) -> Tmux {
        let socket = format!("cosmon-175-{}", std::process::id());
        let fleets = state_dir.join("fleets");
        std::fs::create_dir_all(&fleets).expect("fleets dir");
        std::fs::write(
            fleets.join("default.json"),
            serde_json::json!({ "name": socket }).to_string(),
        )
        .expect("fleet spec");
        let tmux = Tmux { socket };
        let started = tmux.run(&["new-session", "-d", "-s", WORKER, "cat"]);
        assert!(started.status.success(), "tmux new-session failed");
        // Literal text, no Enter: the brief sits in the composer.
        tmux.run(&["send-keys", "-t", WORKER, "-l", "# Autonomous work mode"]);
        let has = tmux.run(&["has-session", "-t", &format!("={WORKER}")]);
        assert!(has.status.success(), "fixture: the session must be alive");
        tmux
    }

    #[test]
    fn live_session_with_unsubmitted_brief_is_not_reported_dead() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        seed_dead_worker(&state_dir);
        let _tmux = live_pane_with_unsubmitted_brief(&state_dir);

        let value: serde_json::Value =
            serde_json::from_str(&run_status(tmp.path(), &state_dir, true)).expect("status JSON");
        assert_eq!(value["molecules"]["dead_workers"], 0, "{value}");

        let text = run_status(tmp.path(), &state_dir, false);
        assert!(
            !text.contains(&format!("cs purge {WORKER}")),
            "never advise purging a live worker: {text}"
        );
    }
}

/// Issue #206 — `cs ensemble` called a live spore worker DEAD and advised
/// `cs purge`. A worker is dead only when every probe answered "no pane" and
/// the molecule is past its post-tackle grace; a live pane on the galaxy's
/// own tmux socket is never dead, and neither is a worker whose probe could
/// not answer.
mod issue_204 {
    use super::*;

    struct Tmux {
        socket: String,
    }

    impl Tmux {
        fn run(&self, args: &[&str]) -> std::process::Output {
            Command::new("tmux")
                .args(["-L", &self.socket])
                .args(args)
                .output()
                .expect("run tmux")
        }
    }

    impl Drop for Tmux {
        fn drop(&mut self) {
            let _ = self.run(&["kill-server"]);
        }
    }

    /// A galaxy whose `config.toml` names its own tmux socket, with a
    /// running molecule tackled `tackled_ago` seconds ago. No fleet spec is
    /// written: the galaxy socket is the only one the probe may rely on.
    fn seed_galaxy(state_dir: &Path, socket: &str, tackled_ago: i64) {
        std::fs::create_dir_all(state_dir).expect("state dir");
        std::fs::write(
            state_dir.join("config.toml"),
            format!("[project]\nproject_id = \"{socket}\"\n"),
        )
        .expect("config");
        let worker_id = WorkerId::new(WORKER).expect("valid worker id");
        let mut molecule = running_molecule(MOL, &worker_id);
        molecule.tackled_at = Some(Utc::now() - chrono::Duration::seconds(tackled_ago));
        molecule.project_id = Some(cosmon_core::id::ProjectId::new(socket).expect("project id"));
        let mut fleet = Fleet::default();
        fleet.workers.insert(
            worker_id.clone(),
            WorkerData::new(
                worker_id,
                AgentId::new("tackle").expect("valid agent id"),
                AgentRole::Implementation,
                Clearance::Write,
                WorkerStatus::Active,
            )
            .with_molecule(molecule.id.clone()),
        );
        let store = FileStore::new(state_dir);
        store.save_fleet(&fleet).expect("save fleet");
        store
            .save_molecule(&molecule.id, &molecule)
            .expect("save molecule");
    }

    /// One socket per test: the tests run in parallel and a shared name
    /// would let one test's live pane answer another's probe.
    fn socket_name(test: &str) -> String {
        format!("g204{test}{}-ab12", std::process::id())
    }

    fn live_pane(socket: &str) -> Tmux {
        let tmux = Tmux {
            socket: socket.to_owned(),
        };
        let started = tmux.run(&["new-session", "-d", "-s", WORKER, "cat"]);
        assert!(started.status.success(), "tmux new-session failed");
        tmux
    }

    fn run_cs(tmp: &Path, state_dir: &Path, path: Option<&str>, args: &[&str]) -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(tmp)
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR");
        if let Some(path) = path {
            cmd.env("PATH", path);
        }
        cmd.args(["--config", state_dir.to_str().expect("utf-8 state dir")])
            .args(args);
        let out = cmd.output().expect("run cs");
        assert!(
            out.status.success(),
            "cs {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Assert the ensemble and the status verdicts both say "not dead".
    fn assert_not_dead(tmp: &Path, state_dir: &Path, path: Option<&str>) {
        let ensemble: serde_json::Value =
            serde_json::from_str(&run_cs(tmp, state_dir, path, &["--json", "ensemble"]))
                .expect("ensemble JSON");
        assert_eq!(
            ensemble["stall_alert"]["dead_workers"],
            serde_json::json!([]),
            "{ensemble}"
        );
        assert_ne!(ensemble["workers"][0]["effective"], "dead", "{ensemble}");
        let text = run_cs(tmp, state_dir, path, &["ensemble"]);
        assert!(!text.contains("DEAD"), "{text}");
        assert!(!text.contains("cs purge"), "never advise purging: {text}");
        let status: serde_json::Value =
            serde_json::from_str(&run_cs(tmp, state_dir, path, &["--json", "status"]))
                .expect("status JSON");
        assert_eq!(status["molecules"]["dead_workers"], 0, "{status}");
    }

    #[test]
    fn live_pane_on_the_galaxy_socket_is_never_dead() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let socket = socket_name("live");
        seed_galaxy(&state_dir, &socket, 3600);
        let _tmux = live_pane(&socket);
        assert_not_dead(tmp.path(), &state_dir, None);
    }

    /// A probe that cannot answer is not a "no pane" answer.
    #[test]
    fn a_failing_probe_is_not_a_dead_verdict() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        seed_galaxy(&state_dir, &socket_name("probe"), 3600);
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let fake = bin.join("tmux");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'protocol version mismatch' >&2\nexit 1\n",
        )
        .expect("fake tmux");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        assert_not_dead(tmp.path(), &state_dir, Some(&path));
    }

    /// `cs tackle` binds the worker before the pane exists (issue #117).
    #[test]
    fn a_just_tackled_worker_without_a_pane_yet_is_not_dead() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        seed_galaxy(&state_dir, &socket_name("fresh"), 5);
        assert_not_dead(tmp.path(), &state_dir, None);
    }

    /// Control: past the grace, with every probe answering "no pane", the
    /// verdict and the reclaim gesture are still emitted.
    #[test]
    fn an_old_worker_without_a_pane_is_still_dead() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        seed_galaxy(&state_dir, &socket_name("old"), 3600);
        let text = run_cs(tmp.path(), &state_dir, None, &["ensemble"]);
        assert!(text.contains("DEAD"), "{text}");
        assert!(text.contains("cs purge"), "{text}");
    }
}
