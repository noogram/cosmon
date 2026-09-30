// SPDX-License-Identifier: AGPL-3.0-only

//! Authority reads must fail closed and invalidate preliminary gates.

use std::collections::{BTreeSet, HashMap};

use cosmon_core::id::{FleetId, FormulaId, MoleculeId, ProjectId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_harvest::authorization_facts::{
    strict_mission_root, AuthorizationFacts, AuthorizationSources,
};
use cosmon_minisign_testkit::Operator;
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

fn id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("valid fixture id")
}

fn molecule(raw: &str) -> MoleculeData {
    MoleculeData {
        id: id(raw),
        fleet_id: FleetId::new("default").expect("fleet"),
        formula_id: FormulaId::new("task-work").expect("formula"),
        status: MoleculeStatus::Completed,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        total_steps: 2,
        current_step: 2,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: Default::default(),
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
        base_branch: None,
        protected_paths: Vec::new(),
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
        harvest_reason: None,
    }
}

struct World {
    root: TempDir,
    store: FileStore,
    molecule: MoleculeId,
}

impl World {
    fn new() -> Self {
        let root = TempDir::new().expect("temp root");
        let state = root.path().join(".cosmon/state");
        std::fs::create_dir_all(&state).expect("state directory");
        std::fs::write(
            root.path().join(".cosmon/config.toml"),
            "[project]\nproject_id = \"cosmon-test\"\n",
        )
        .expect("config");
        let store = FileStore::new(&state);
        let molecule = id("task-20260929-39cd");
        store
            .save_molecule(&molecule, &self::molecule(molecule.as_str()))
            .expect("molecule");
        Self {
            root,
            store,
            molecule,
        }
    }

    fn snapshot(&self) -> Result<AuthorizationFacts, cosmon_core::error::CosmonError> {
        AuthorizationFacts::load(&AuthorizationSources {
            store: &self.store,
            config_path: &self.root.path().join(".cosmon/config.toml"),
            galaxy_root: self.root.path(),
            repo_root: self.root.path(),
            molecule: &self.molecule,
        })
    }

    fn save(&self, edit: impl FnOnce(&mut MoleculeData)) {
        let mut mol = self.store.load_molecule(&self.molecule).expect("load");
        edit(&mut mol);
        self.store
            .save_molecule(&self.molecule, &mol)
            .expect("save");
    }
}

#[test]
fn each_precheck_change_refuses_the_effect_snapshot() {
    let cases: &[(&str, fn(&World))] = &[
        ("required", |w| {
            std::fs::write(
                w.root.path().join(".cosmon/config.toml"),
                "[project]\nproject_id = \"cosmon-test\"\n[harvest_authority]\nrequired = true\n",
            )
            .expect("config")
        }),
        ("project identity", |w| {
            std::fs::write(
                w.root.path().join(".cosmon/config.toml"),
                "[project]\nproject_id = \"other-project\"\n",
            )
            .expect("config")
        }),
        ("persisted base", |w| {
            w.save(|m| m.base_branch = Some("release".to_owned()))
        }),
        ("status", |w| {
            w.save(|m| m.status = MoleculeStatus::Collapsed)
        }),
        ("protected paths", |w| {
            w.save(|m| m.protected_paths.push("reference".to_owned()))
        }),
        ("scope perimeter", |w| {
            w.save(|m| {
                m.variables
                    .insert("scope_allow".to_owned(), "src/**".to_owned());
            })
        }),
        ("reservation tag", |w| {
            w.save(|m| {
                m.tags.insert(Tag::new("security:high").expect("tag"));
            })
        }),
        ("mission parent", |w| {
            w.save(|m| {
                m.typed_links.push(MoleculeLink::BlockedBy {
                    source: id("task-20260929-parent"),
                })
            })
        }),
        ("epoch", |w| {
            std::fs::write(w.root.path().join(".cosmon/harvest.epoch"), "2\n").expect("epoch")
        }),
        ("policy bytes", |w| {
            std::fs::write(
                w.root.path().join(".cosmon/harvest-policy.toml"),
                "limit = 1\n",
            )
            .expect("policy")
        }),
        ("trust root", |w| {
            std::fs::write(
                w.root.path().join(".cosmon/harvest.pub"),
                Operator::from_seed(7).public_key_file(),
            )
            .expect("root")
        }),
    ];
    for (name, edit) in cases {
        let world = World::new();
        let before = world.snapshot().expect("preliminary facts");
        edit(&world);
        let current = world.snapshot().expect("locked facts");
        let error = before.require_unchanged(&current).expect_err(name);
        assert!(
            error.to_string().contains("harvest_facts_changed"),
            "{name}: {error}"
        );
    }
}

#[test]
fn preliminary_molecule_must_match_both_change_perimeters() {
    for edit in [
        (|m: &mut MoleculeData| m.protected_paths.push("reference".to_owned()))
            as fn(&mut MoleculeData),
        |m: &mut MoleculeData| {
            m.variables
                .insert("scope_allow".to_owned(), "src/**".to_owned());
        },
    ] {
        let world = World::new();
        let preliminary = world.store.load_molecule(&world.molecule).expect("load");
        world.save(edit);
        let facts = world.snapshot().expect("current facts");
        assert!(facts.require_same_molecule(&preliminary).is_err());
    }
}

#[test]
fn successful_preload_never_masks_a_later_read_failure() {
    for name in ["config", "state", "epoch", "policy", "key"] {
        let world = World::new();
        let operator = Operator::from_seed(7);
        std::fs::write(
            world.root.path().join(".cosmon/harvest.pub"),
            operator.public_key_file(),
        )
        .expect("key");
        world.snapshot().expect("preliminary load");
        match name {
            "config" => std::fs::remove_file(world.root.path().join(".cosmon/config.toml"))
                .expect("remove config"),
            "state" => {
                std::fs::remove_file(world.store.molecule_dir(&world.molecule).join("state.json"))
                    .expect("remove state")
            }
            "epoch" => std::fs::write(world.root.path().join(".cosmon/harvest.epoch"), "invalid\n")
                .expect("bad epoch"),
            "policy" => std::fs::create_dir(world.root.path().join(".cosmon/harvest-policy.toml"))
                .expect("unreadable policy"),
            "key" => {
                let path = world.root.path().join(".cosmon/harvest.pub");
                std::fs::remove_file(&path).expect("remove key");
                std::fs::create_dir(&path).expect("unreadable key path");
            }
            _ => unreachable!(),
        }
        assert!(
            world.snapshot().is_err(),
            "{name} must fault after precheck"
        );
    }
}

#[test]
fn absent_optional_policy_is_distinct_from_empty_and_unreadable() {
    let world = World::new();
    let absent = world.snapshot().expect("absent optional policy");
    assert!(absent.policy_bytes.is_none());
    let path = world.root.path().join(".cosmon/harvest-policy.toml");
    std::fs::write(&path, []).expect("empty policy");
    let empty = world.snapshot().expect("present empty policy");
    assert_eq!(empty.policy_bytes, Some(Vec::new()));
    assert!(absent.require_unchanged(&empty).is_err());
    std::fs::remove_file(&path).expect("remove empty policy");
    std::fs::create_dir(&path).expect("unreadable policy path");
    assert!(world.snapshot().is_err());
}

#[test]
fn stamped_molecule_cannot_be_harvested_under_a_different_project() {
    let world = World::new();
    world.save(|m| m.project_id = Some(ProjectId::new("other-0000").expect("project")));
    assert!(world.snapshot().is_err());
}

#[test]
fn mission_scope_requires_one_readable_acyclic_root() {
    let world = World::new();
    assert_eq!(
        strict_mission_root(&world.store, &world.molecule).expect("root"),
        world.molecule
    );

    let missing = id("task-20260929-miss");
    world.save(|m| {
        m.typed_links.push(MoleculeLink::BlockedBy {
            source: missing.clone(),
        })
    });
    assert!(
        strict_mission_root(&world.store, &world.molecule).is_err(),
        "missing parent"
    );

    world
        .store
        .save_molecule(&missing, &molecule(missing.as_str()))
        .expect("parent");
    assert_eq!(
        strict_mission_root(&world.store, &world.molecule).expect("one root"),
        missing
    );

    let second = id("task-20260929-other");
    world
        .store
        .save_molecule(&second, &molecule(second.as_str()))
        .expect("second");
    world.save(|m| {
        m.typed_links.push(MoleculeLink::BlockedBy {
            source: second.clone(),
        })
    });
    assert!(
        strict_mission_root(&world.store, &world.molecule).is_err(),
        "ambiguous roots"
    );

    world.save(|m| {
        m.typed_links.retain(
            |link| !matches!(link, MoleculeLink::BlockedBy { source } if source == &second),
        );
    });
    let mut parent = world.store.load_molecule(&missing).expect("parent");
    parent.typed_links.push(MoleculeLink::BlockedBy {
        source: world.molecule.clone(),
    });
    world.store.save_molecule(&missing, &parent).expect("cycle");
    assert!(
        strict_mission_root(&world.store, &world.molecule).is_err(),
        "cycle"
    );
}
