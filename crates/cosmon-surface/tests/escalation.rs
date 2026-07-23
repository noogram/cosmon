// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for [`cosmon_surface::escalation`].
//!
//! These tests exercise the full mechanical-first escalation pipeline at the
//! library level: `classify_surface` → `SurfaceDecision` → `project_surfaces` (for
//! writable decisions) → snapshot round-trip. The CLI plumbing (nucleate,
//! tackle) lives in `cosmon-cli::cmd::reconcile` and is tested via the CLI
//! crate's own unit tests.

use std::collections::{BTreeSet, HashMap};

use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{Fleet, MoleculeData};
use cosmon_surface::escalation::{
    classify_surface, format_conflict_block, ConflictRecord, SurfaceDecision,
};
use cosmon_surface::snapshot::{self, record_projection};
use cosmon_surface::{
    project_surfaces, render_status_content, Branding, DeclarationMap, FormulaMap, Surface,
    SurfaceConfig, SurfaceKind,
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
        adapter: None,
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
        public: false,
    }
}

/// End-to-end: first projection writes the surface with `NeverProjected`, next
/// classify after the write is `UpToDate` (snapshot round-trip).
#[test]
fn escalation_first_projection_then_up_to_date() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let config = SurfaceConfig {
        surface: vec![status_surface()],
    };
    let fleet = Fleet::default();
    let fm = FormulaMap::new();
    let dm = DeclarationMap::new();
    let mol = test_mol("task-20260411-aaaa", MoleculeStatus::Running);

    // First run: file does not exist → NeverProjected.
    let decision = classify_surface(
        None,
        "",
        &render_status_content(
            &fleet,
            std::slice::from_ref(&mol),
            &fm,
            Branding::HostNative,
        ),
    );
    assert_eq!(decision, SurfaceDecision::Write);

    project_surfaces(&config, root, &fleet, std::slice::from_ref(&mol), &fm, &dm).unwrap();

    // Record snapshot (as the CLI would).
    let mut snap = snapshot::ProjectionSnapshot::default();
    let content = std::fs::read_to_string(root.join("STATUS.md")).unwrap();
    record_projection(&mut snap, "STATUS.md", &content);
    let state_dir = root.join(".cosmon/state");
    std::fs::create_dir_all(&state_dir).unwrap();
    snapshot::save_snapshot(&state_dir, &snap).unwrap();

    // Second run with unchanged state: classify returns UpToDate → Write
    // (idempotent no-op; project_surfaces will rewrite identical bytes).
    let snap2 = snapshot::load_snapshot(&state_dir);
    let new_content = render_status_content(&fleet, &[mol], &fm, Branding::HostNative);
    let snapshot_hash = snap2
        .surfaces
        .get("STATUS.md")
        .map(|s| s.content_hash.as_str());
    let decision = classify_surface(snapshot_hash, &content, &new_content);
    assert_eq!(decision, SurfaceDecision::Write);
}

/// Human edits the file while the source stays put → Preserve. The
/// classification must not overwrite the human edit.
#[test]
fn escalation_preserves_human_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fleet = Fleet::default();
    let fm = FormulaMap::new();
    let mol = test_mol("task-20260411-bbbb", MoleculeStatus::Running);
    let baseline = render_status_content(
        &fleet,
        std::slice::from_ref(&mol),
        &fm,
        Branding::HostNative,
    );

    // Snapshot the baseline as if we had just projected it.
    let mut snap = snapshot::ProjectionSnapshot::default();
    record_projection(&mut snap, "STATUS.md", &baseline);
    let state_dir = root.join(".cosmon/state");
    std::fs::create_dir_all(&state_dir).unwrap();
    snapshot::save_snapshot(&state_dir, &snap).unwrap();

    // Human inserts a line — file hash != snapshot hash, source unchanged.
    let edited = format!("{baseline}\n## Added by human");
    let snap2 = snapshot::load_snapshot(&state_dir);
    let snapshot_hash = snap2
        .surfaces
        .get("STATUS.md")
        .map(|s| s.content_hash.as_str());
    let decision = classify_surface(snapshot_hash, &edited, &baseline);
    assert_eq!(decision, SurfaceDecision::Preserve);
}

/// True 3-way conflict: human edited the file AND a new molecule appeared
/// in the source state. `classify_surface` must return Escalate carrying both
/// contents so the caller can nucleate a resolver.
#[test]
fn escalation_reports_both_sides_of_true_conflict() {
    let fleet = Fleet::default();
    let fm = FormulaMap::new();

    let mol_before = test_mol("task-20260411-cccc", MoleculeStatus::Running);
    let baseline = render_status_content(
        &fleet,
        std::slice::from_ref(&mol_before),
        &fm,
        Branding::HostNative,
    );

    // Record baseline as snapshot.
    let mut snap = snapshot::ProjectionSnapshot::default();
    record_projection(&mut snap, "STATUS.md", &baseline);
    let snapshot_hash = snap
        .surfaces
        .get("STATUS.md")
        .map(|s| s.content_hash.as_str());

    // Human edited the file.
    let human_edit = format!("{baseline}\n## Human annotation");

    // Source advanced (new molecule appeared).
    let mol_new = test_mol("task-20260411-dddd", MoleculeStatus::Pending);
    let fresh = render_status_content(&fleet, &[mol_before, mol_new], &fm, Branding::HostNative);
    assert_ne!(fresh, baseline, "source must have changed for this test");
    assert_ne!(fresh, human_edit, "human and source must both differ");

    match classify_surface(snapshot_hash, &human_edit, &fresh) {
        SurfaceDecision::Escalate {
            human_content,
            source_content,
        } => {
            assert_eq!(human_content, human_edit);
            assert_eq!(source_content, fresh);
        }
        other => panic!("expected Escalate, got {other:?}"),
    }
}

/// `format_conflict_block` produces standard git conflict markers that a
/// merge-aware tool (editor, pre-commit hook) will flag automatically.
#[test]
fn escalation_conflict_block_uses_git_marker_syntax() {
    let block = format_conflict_block("alpha\nbeta", "gamma\ndelta");
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(lines[0], "<<<<<<< human (surface edit)");
    assert!(lines.contains(&"======="));
    assert_eq!(*lines.last().unwrap(), ">>>>>>> source (cs state)");
    assert!(lines.contains(&"alpha"));
    assert!(lines.contains(&"gamma"));
}

/// `ConflictRecord` is the payload handed to the resolver worker. This test
/// guards its basic shape so future CLI refactors can rely on it.
#[test]
fn escalation_conflict_record_carries_path_and_both_sides() {
    let record = ConflictRecord {
        path: "STATUS.md".to_owned(),
        human_content: "H".to_owned(),
        source_content: "S".to_owned(),
    };
    assert_eq!(record.path, "STATUS.md");
    assert_eq!(record.human_content, "H");
    assert_eq!(record.source_content, "S");
}
