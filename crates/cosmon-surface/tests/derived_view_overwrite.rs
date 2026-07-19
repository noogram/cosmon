// SPDX-License-Identifier: AGPL-3.0-only

//! Surfaces are derived views: regeneration is always a clean atomic
//! overwrite, never a merge.
//!
//! These tests pin the 2026-05-09 fix for the conflict-marker-stacking
//! bug. The retired escalation path merged `cs done`'s
//! out-of-band edits to STATUS.md / ISSUES.md by writing git-style
//! `<<<<<<<` blocks into the auto-generated file, which then re-wrapped on
//! every subsequent run (4 observed levels). The fix: `project_surfaces`
//! truncate-rewrites every surface from authoritative state, so a polluted
//! file on disk is replaced wholesale — no marker can survive a projection.

use std::collections::{BTreeSet, HashMap};

use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{Fleet, MoleculeData};
use cosmon_surface::{
    project_surfaces, Branding, DeclarationMap, FormulaMap, Surface, SurfaceConfig, SurfaceKind,
};

fn test_mol(id: &str, status: MoleculeStatus) -> MoleculeData {
    MoleculeData {
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        total_steps: 2,
        current_step: 0,
        completed_steps: vec![],
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: vec![],
        kind: Some(MoleculeKind::Task),
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: vec![],
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
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    }
}

fn status_surface() -> Surface {
    Surface {
        referent: "project.status".to_owned(),
        kind: SurfaceKind::Markdown,
        path: "STATUS.md".to_owned(),
        template: None,
        repo: None,
        labels: vec![],
        molecule_kinds: vec![],
        branding: Branding::HostNative,
    }
}

/// A STATUS.md already polluted with 4 levels of stacked git conflict
/// markers — exactly the artefact observed 2026-05-08/09 — is regenerated
/// to clean content with no markers. Projection is a full overwrite, so the
/// pollution cannot persist or grow.
#[test]
fn polluted_status_is_regenerated_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let config = SurfaceConfig {
        surface: vec![status_surface()],
    };
    let fleet = Fleet::default();
    let fm = FormulaMap::new();
    let dm = DeclarationMap::new();
    let mol = test_mol("task-20260509-aaaa", MoleculeStatus::Running);

    // Seed the on-disk surface with 4-level stacked conflict markers.
    let polluted = "\
<<<<<<< human (surface edit)
<<<<<<< human (surface edit)
<<<<<<< human (surface edit)
<<<<<<< human (surface edit)
<!-- auto-generated from .cosmon/ -->
# Status
some stale body
=======
# Status
a second copy
>>>>>>> source (cs state)
";
    std::fs::write(root.join("STATUS.md"), polluted).unwrap();

    let written =
        project_surfaces(&config, root, &fleet, std::slice::from_ref(&mol), &fm, &dm).unwrap();
    assert_eq!(written, vec!["STATUS.md".to_owned()]);

    let regenerated = std::fs::read_to_string(root.join("STATUS.md")).unwrap();
    for marker in ["<<<<<<<", "=======", ">>>>>>>"] {
        assert!(
            !regenerated.contains(marker),
            "regenerated STATUS.md must not contain conflict marker {marker:?}; got:\n{regenerated}"
        );
    }
    // The fresh projection is the canonical status render — it carries the
    // molecule id, proving real content replaced the pollution.
    assert!(regenerated.contains("task-20260509-aaaa"));
}

/// Re-projecting twice over a polluted file converges (idempotent): the
/// second projection produces byte-identical output, never an extra wrapped
/// layer. This is the direct anti-regression for the "markers stack on every
/// run" pathology.
#[test]
fn reprojection_is_idempotent_and_never_stacks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let config = SurfaceConfig {
        surface: vec![status_surface()],
    };
    let fleet = Fleet::default();
    let fm = FormulaMap::new();
    let dm = DeclarationMap::new();
    let mol = test_mol("task-20260509-bbbb", MoleculeStatus::Running);

    std::fs::write(
        root.join("STATUS.md"),
        "<<<<<<< human (surface edit)\ngarbage\n>>>>>>> source (cs state)\n",
    )
    .unwrap();

    project_surfaces(&config, root, &fleet, std::slice::from_ref(&mol), &fm, &dm).unwrap();
    let first = std::fs::read_to_string(root.join("STATUS.md")).unwrap();

    project_surfaces(&config, root, &fleet, std::slice::from_ref(&mol), &fm, &dm).unwrap();
    let second = std::fs::read_to_string(root.join("STATUS.md")).unwrap();

    assert_eq!(first, second, "projection must be idempotent");
    assert!(!second.contains("<<<<<<<"));
}

/// The atomic tempfile + rename leaves no `.tmp` sibling behind on success.
#[test]
fn atomic_write_leaves_no_tmp_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let config = SurfaceConfig {
        surface: vec![status_surface()],
    };
    let fleet = Fleet::default();
    let fm = FormulaMap::new();
    let dm = DeclarationMap::new();
    let mol = test_mol("task-20260509-cccc", MoleculeStatus::Running);

    project_surfaces(&config, root, &fleet, std::slice::from_ref(&mol), &fm, &dm).unwrap();

    assert!(root.join("STATUS.md").exists());
    assert!(
        !root.join("STATUS.md.tmp").exists(),
        "the atomic write must rename the .tmp sibling away"
    );
}
