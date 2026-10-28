// SPDX-License-Identifier: AGPL-3.0-only

//! The `cs` half of the remote-verb parity for `evolve`, `deps`, `prime`
//! and `resurrect` (pilot plan 0001, lot E1).
//!
//! `crates/cosmon-rpp-adapter/tests/v1_evolve_resurrect_deps_prime.rs`
//! drives the HTTP routes over a file store and asserts what they produce.
//! This file runs the real `cs` binary on the same fixtures and asserts the
//! same properties, so "the route does what `cs` does" is checked on both
//! sides rather than inferred from the two calling one function.
//!
//! `cs resurrect` needs a live tmux to spawn, so only its `--dry-run` — the
//! part that runs the shared preconditions and composes the shared prompt —
//! is exercised here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::ops;
use cosmon_state::StateStore;

/// The same two-step formula the adapter suite plants.
const TWO_STEPS: &str = r#"
formula = "two-steps"
version = 1
description = "two steps, for evolve"
id_prefix = "task"

[[steps]]
id = "design"
title = "Design"
description = "Decide the approach."
acceptance = "The approach is written down."

[[steps]]
id = "build"
title = "Build"
description = "Do the work."
needs = ["design"]
"#;

struct Project {
    tmp: tempfile::TempDir,
    state_dir: PathBuf,
    formula_path: PathBuf,
}

impl Project {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas = tmp.path().join(".cosmon").join("formulas");
        fs::create_dir_all(&formulas).unwrap();
        let formula_path = formulas.join("two-steps.formula.toml");
        fs::write(&formula_path, TWO_STEPS).unwrap();
        Self {
            tmp,
            state_dir,
            formula_path,
        }
    }

    fn cs(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(self.tmp.path())
            .env(
                "COSMON_CONFIG_HOME",
                self.tmp.path().join("isolated-config"),
            )
            .env("COSMON_STATE_DIR", &self.state_dir)
            .env("COSMON_FORMULAS_DIR", self.formula_path.parent().unwrap())
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env_remove("COSMON_API_REQUEST");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cs().args(args).output().expect("spawn cs")
    }

    fn store(&self) -> FileStore {
        FileStore::new(&self.state_dir)
    }

    /// Nucleate a molecule on the two-step formula and mark it running, which
    /// is the state a worker's `cs evolve` finds it in.
    fn running_molecule(&self) -> MoleculeId {
        let out = self.run(&[
            "--json",
            "nucleate",
            "two-steps",
            "--store-dir",
            self.state_dir.to_str().unwrap(),
            "--formulas-dir",
            self.formula_path.parent().unwrap().to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        let id = MoleculeId::new(json["id"].as_str().unwrap()).unwrap();
        let mut mol = self.store().load_molecule(&id).unwrap();
        mol.status = MoleculeStatus::Running;
        self.store().save_molecule(&id, &mol).unwrap();
        id
    }

    fn molecule_dir(&self, id: &MoleculeId) -> PathBuf {
        self.store().molecule_dir(id)
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `cs evolve` leaves the molecule in the state the route test asserts: next
/// step current, the step recorded, the intent record cleared, the log and the
/// briefing rewritten, the step-completed and evolved events emitted, and the
/// briefing sealed.
#[test]
fn cs_evolve_leaves_the_state_the_evolve_route_asserts() {
    let project = Project::new();
    let id = project.running_molecule();

    let out = project.run(&[
        "--json",
        "evolve",
        id.as_str(),
        "--evidence",
        "approach written down in design.md",
        "--formula",
        project.formula_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wire: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(wire["completed_step"], "design");
    assert_eq!(wire["new_step"], "build");
    assert_eq!(wire["new_status"], "running");

    let mol = project.store().load_molecule(&id).unwrap();
    assert_eq!(mol.current_step, 1);
    assert_eq!(mol.completed_steps.len(), 1);
    assert_eq!(mol.completed_steps[0].as_str(), "design");
    assert!(mol.pending_step.is_none());
    assert!(!mol.briefing_seals.is_empty());

    let dir = project.molecule_dir(&id);
    let log = fs::read_to_string(dir.join("log.md")).unwrap();
    assert!(log.contains("approach written down"), "{log}");
    let briefing = fs::read_to_string(dir.join("briefing.md")).unwrap();
    assert!(briefing.contains("Build"), "{briefing}");
    let events = fs::read_to_string(project.state_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("molecule_step_completed"), "{events}");
    assert!(events.contains("molecule_evolved"), "{events}");
}

/// A chain A → B → C, written as the store holds it.
fn plant_chain(project: &Project) -> (MoleculeId, MoleculeId, MoleculeId) {
    let a = project.running_molecule();
    let b = project.running_molecule();
    let c = project.running_molecule();
    let store = project.store();
    let link = |mol: &MoleculeId, links: serde_json::Value| {
        let mut data = serde_json::to_value(store.load_molecule(mol).unwrap()).unwrap();
        data["typed_links"] = links;
        store
            .save_molecule(mol, &serde_json::from_value(data).unwrap())
            .unwrap();
    };
    let rel = |kind: &str, field: &str, other: &MoleculeId| serde_json::json!({ "rel": kind, field: other.as_str() });
    link(&a, serde_json::json!([rel("blocks", "target", &b)]));
    link(
        &b,
        serde_json::json!([rel("blocked_by", "source", &a), rel("blocks", "target", &c)]),
    );
    link(&c, serde_json::json!([rel("blocked_by", "source", &b)]));
    (a, b, c)
}

/// `cs --json deps` prints the shared walk, the body the route returns.
#[test]
fn cs_deps_prints_what_the_deps_route_returns() {
    let project = Project::new();
    let (a, _b, _c) = plant_chain(&project);

    for transitive in [false, true] {
        let mut args = vec!["--json", "deps", a.as_str()];
        if transitive {
            args.push("--transitive");
        }
        let out = project.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let printed: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();

        let view = ops::deps::deps(
            &project.store(),
            a.as_str(),
            &ops::DepsRequest { transitive },
        )
        .unwrap();
        let shared = serde_json::to_value(ops::DepsJson::from_view(&view)).unwrap();
        assert_eq!(printed, shared, "transitive={transitive}");
    }
}

/// `cs prime` prints the reminder the route returns, and nothing for a
/// terminal molecule.
#[test]
fn cs_prime_prints_the_reminder_the_prime_route_returns() {
    let project = Project::new();
    let id = project.running_molecule();

    let out = project.run(&["prime", id.as_str()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let formulas = project.formula_path.parent().unwrap();
    let expected = match ops::prime::prime(&project.store(), formulas, &id).unwrap() {
        ops::PrimeView::Context(ctx) => ctx.render(),
        other => panic!("expected a context, got {other:?}"),
    };
    assert_eq!(stdout(&out), expected);
    assert!(stdout(&out).contains("**Step 1: Design** ◀ EXECUTE NOW"));

    let mut mol = project.store().load_molecule(&id).unwrap();
    mol.status = MoleculeStatus::Completed;
    project.store().save_molecule(&id, &mol).unwrap();
    let out = project.run(&["prime", id.as_str()]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "", "a terminal molecule has nothing to prime");
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git on PATH");
    assert!(out.status.success(), "git {args:?}");
}

/// `cs resurrect --dry-run` runs the shared preconditions and composes the
/// shared prompt: the same one the route's `dry_run` returns.
#[test]
fn cs_resurrect_dry_run_composes_the_prompt_the_route_composes() {
    let project = Project::new();
    let root = project.tmp.path();
    // `cs resurrect` refuses a project with no identity.
    fs::write(
        root.join(".cosmon").join("config.toml"),
        "[project]\nproject_id = \"parity-test-a1a1\"\n",
    )
    .unwrap();
    git(root, &["init", "--initial-branch=main"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    git(root, &["config", "user.name", "test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("README.md"), "x\n").unwrap();
    git(root, &["add", "README.md", ".cosmon"]);
    git(root, &["commit", "--no-verify", "-m", "seed"]);

    let id = project.running_molecule();
    let mut mol = project.store().load_molecule(&id).unwrap();
    mol.status = MoleculeStatus::Frozen;
    project.store().save_molecule(&id, &mol).unwrap();
    let dir = project.molecule_dir(&id);
    fs::write(dir.join("prompt.md"), "Original operator intent.").unwrap();
    fs::write(dir.join("briefing.md"), "The plan.").unwrap();

    let out = project.run(&["--json", "resurrect", id.as_str(), "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(printed["dry_run"], true);
    assert_eq!(printed["prior_count"], 0);
    let prompt = printed["prompt"].as_str().unwrap();
    assert!(prompt.contains("Original operator intent."));
    assert!(prompt.contains("The plan."));

    // The shared half, called directly, composes the same prompt.
    let revival = cosmon_runtime::resurrect::prepare(
        &project.store(),
        &project.state_dir,
        &id,
        root,
        &|_| false,
    )
    .unwrap();
    assert_eq!(revival.prior_count, 0);
    assert_eq!(
        revival.composed_prompt_bytes,
        printed["composed_prompt_bytes"].as_u64().unwrap()
    );
    assert!(revival.prompt.contains("Original operator intent."));

    // A dry run changes nothing.
    assert_eq!(
        project.store().load_molecule(&id).unwrap().status,
        MoleculeStatus::Frozen
    );
    assert!(!dir.join("wrecks").exists());
}
