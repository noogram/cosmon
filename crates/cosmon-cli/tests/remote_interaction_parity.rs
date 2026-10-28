// SPDX-License-Identifier: AGPL-3.0-only

//! The `cs` half of the remote-verb parity for `decay`, `merge` and
//! `transform` (pilot plan 0001, lot E2).
//!
//! `crates/cosmon-rpp-adapter/tests/v1_decay_merge_transform.rs` drives the
//! HTTP routes over a file store and asserts a projection of what they leave
//! behind. This file runs the real `cs` binary on the same fixture and asserts
//! the same projection against the same literal, so "the route leaves the
//! state `cs` leaves" is checked on both sides rather than inferred from the
//! two calling one function.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::MoleculeLink;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};
use serde_json::{json, Value};

/// The formula the adapter suite installs for its tenants.
const TASK_WORK: &str = r#"
formula = "task-work"
version = 1
description = "minimal task-work formula for tests"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Implement"
description = "Do the work."
"#;

struct Project {
    tmp: tempfile::TempDir,
    state_dir: PathBuf,
    formulas_dir: PathBuf,
}

impl Project {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join(".cosmon").join("formulas");
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::write(formulas_dir.join("task-work.formula.toml"), TASK_WORK).unwrap();
        Self {
            tmp,
            state_dir,
            formulas_dir,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cs"))
            .args(args)
            .current_dir(self.tmp.path())
            .env(
                "COSMON_CONFIG_HOME",
                self.tmp.path().join("isolated-config"),
            )
            .env("COSMON_STATE_DIR", &self.state_dir)
            .env("COSMON_FORMULAS_DIR", &self.formulas_dir)
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env_remove("COSMON_API_REQUEST")
            .output()
            .expect("spawn cs")
    }

    fn store(&self) -> FileStore {
        FileStore::new(&self.state_dir)
    }

    fn load(&self, id: &MoleculeId) -> MoleculeData {
        self.store().load_molecule(id).unwrap()
    }

    /// Nucleate a molecule of `kind` on `task-work`.
    fn molecule(&self, kind: &str) -> MoleculeId {
        let out = self.run(&[
            "--json",
            "nucleate",
            "task-work",
            "--kind",
            kind,
            "--store-dir",
            self.state_dir.to_str().unwrap(),
            "--formulas-dir",
            self.formulas_dir.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "{}", stderr(&out));
        let json: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        MoleculeId::new(json["id"].as_str().unwrap()).unwrap()
    }

    /// Every file under the state dir with its bytes.
    fn snapshot(&self) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn walk(
            dir: &std::path::Path,
            root: &std::path::Path,
            out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(&self.state_dir, &self.state_dir, &mut out);
        out
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// What a molecule's typed links say, with ids replaced by their role. The
/// twin of `links` in the adapter suite.
fn links(data: &MoleculeData, names: &[(&MoleculeId, &str)]) -> Vec<String> {
    let name = |id: &MoleculeId| {
        names
            .iter()
            .find(|(raw, _)| *raw == id)
            .map_or_else(|| id.to_string(), |(_, role)| (*role).to_owned())
    };
    data.typed_links
        .iter()
        .map(|link| match link {
            MoleculeLink::DecayedFrom { id } => format!("decayed_from:{}", name(id)),
            MoleculeLink::DecayProduct { id } => format!("decay_product:{}", name(id)),
            MoleculeLink::MergedFrom { ids } => format!(
                "merged_from:{}",
                ids.iter().map(&name).collect::<Vec<_>>().join(",")
            ),
            MoleculeLink::MergedInto { id } => format!("merged_into:{}", name(id)),
            MoleculeLink::TransformedFrom { kind } => format!("transformed_from:{kind}"),
            other => format!("{other:?}"),
        })
        .collect()
}

/// One molecule as the parity projection sees it. The twin of `project` in
/// the adapter suite.
fn projection(data: &MoleculeData, names: &[(&MoleculeId, &str)]) -> Value {
    json!({
        "kind": data.kind.map(|k| k.to_string()),
        "status": data.status.to_string(),
        "formula": data.formula_id.as_str(),
        "links": links(data, names),
    })
}

// The three literals below are the ones in the adapter suite, verbatim.

fn expected_decay() -> Value {
    json!({
        "source": {
            "kind": "idea", "status": "completed", "formula": "task-work",
            "links": ["decay_product:<product>"],
        },
        "product": {
            "kind": "task", "status": "pending", "formula": "task-work",
            "links": ["decayed_from:<source>"],
        },
    })
}

fn expected_merge() -> Value {
    json!({
        "source_1": {
            "kind": "task", "status": "completed", "formula": "task-work",
            "links": ["merged_into:<product>"],
        },
        "source_2": {
            "kind": "task", "status": "completed", "formula": "task-work",
            "links": ["merged_into:<product>"],
        },
        "product": {
            "kind": "decision", "status": "pending", "formula": "task-work",
            "links": ["merged_from:<source_1>,<source_2>"],
        },
    })
}

fn expected_transform() -> Value {
    json!({
        "molecule": {
            "kind": "task", "status": "pending", "formula": "task-work",
            "links": ["transformed_from:idea"],
        },
    })
}

fn interactions(project: &Project) -> Vec<Value> {
    fs::read_to_string(project.state_dir.join("interactions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// `cs decay` leaves the projection the decay route asserts.
#[test]
fn cs_decay_leaves_the_state_the_decay_route_asserts() {
    let project = Project::new();
    let idea = project.molecule("idea");

    let out = project.run(&[
        "--json",
        "decay",
        idea.as_str(),
        "--formula",
        "task-work",
        "--reason",
        "split the idea",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let wire: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(wire["interaction"], "decay");
    assert_eq!(wire["product_kind"], "task");
    let products = wire["products"].as_array().unwrap();
    assert_eq!(products.len(), 1);
    let product = MoleculeId::new(products[0].as_str().unwrap()).unwrap();

    let names = [(&idea, "<source>"), (&product, "<product>")];
    assert_eq!(
        json!({
            "source": project_of(&project, &idea, &names),
            "product": project_of(&project, &product, &names),
        }),
        expected_decay()
    );
    let log = interactions(&project);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["type"], "decay");
    let events = fs::read_to_string(project.state_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("molecule_decayed"), "{events}");
}

fn project_of(project: &Project, id: &MoleculeId, names: &[(&MoleculeId, &str)]) -> Value {
    projection(&project.load(id), names)
}

/// `cs decay --count 2` is still refused by the f4e1 guard, with its own exit
/// code, and writes nothing.
#[test]
fn cs_decay_still_refuses_identical_children() {
    let project = Project::new();
    let idea = project.molecule("idea");
    let before = project.snapshot();
    let out = project.run(&[
        "decay",
        idea.as_str(),
        "--formula",
        "task-work",
        "--reason",
        "r",
        "--count",
        "2",
    ]);
    assert_eq!(out.status.code(), Some(11), "{}", stderr(&out));
    assert_eq!(project.snapshot(), before);
}

/// `cs merge` leaves the projection the merge route asserts.
#[test]
fn cs_merge_leaves_the_state_the_merge_route_asserts() {
    let project = Project::new();
    let (one, two) = (project.molecule("task"), project.molecule("task"));

    let out = project.run(&[
        "--json",
        "merge",
        one.as_str(),
        two.as_str(),
        "--formula",
        "task-work",
        "--reason",
        "synthesise",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let wire: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(wire["interaction"], "merge");
    assert_eq!(wire["product_kind"], "decision");
    let product = MoleculeId::new(wire["product"].as_str().unwrap()).unwrap();

    let names = [
        (&one, "<source_1>"),
        (&two, "<source_2>"),
        (&product, "<product>"),
    ];
    assert_eq!(
        json!({
            "source_1": project_of(&project, &one, &names),
            "source_2": project_of(&project, &two, &names),
            "product": project_of(&project, &product, &names),
        }),
        expected_merge()
    );
    let log = interactions(&project);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["type"], "merge");
    let events = fs::read_to_string(project.state_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("molecule_merged"), "{events}");
}

/// `cs merge` with one source the store does not hold refuses the whole call:
/// the sources that are there are not completed, no synthesis is written. The
/// all-or-nothing is the shared operation's, so it holds for `cs` as it does
/// for the route.
#[test]
fn cs_merge_with_a_missing_source_changes_nothing() {
    let project = Project::new();
    let (one, two) = (project.molecule("task"), project.molecule("task"));
    let before = project.snapshot();
    for sources in [
        [one.as_str(), two.as_str(), "task-20261028-ffff"],
        [one.as_str(), "task-20261028-ffff", two.as_str()],
        ["task-20261028-ffff", one.as_str(), two.as_str()],
    ] {
        let out = project.run(&[
            "merge",
            sources[0],
            sources[1],
            sources[2],
            "--formula",
            "task-work",
            "--reason",
            "r",
        ]);
        assert!(!out.status.success(), "{sources:?} merged");
        assert_eq!(project.snapshot(), before, "{sources:?} left a trace");
    }
}

/// `cs transform` leaves the projection the transform route asserts.
#[test]
fn cs_transform_leaves_the_state_the_transform_route_asserts() {
    let project = Project::new();
    let idea = project.molecule("idea");

    let out = project.run(&[
        "--json",
        "transform",
        idea.as_str(),
        "--to",
        "task",
        "--reason",
        "ready to build",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let wire: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(wire["from"], "idea");
    assert_eq!(wire["to"], "task");

    assert_eq!(
        json!({"molecule": project_of(&project, &idea, &[])}),
        expected_transform()
    );
    let log = interactions(&project);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["type"], "transform");
    let events = fs::read_to_string(project.state_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("molecule_transformed"), "{events}");
}
