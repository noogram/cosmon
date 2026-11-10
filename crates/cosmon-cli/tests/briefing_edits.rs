// SPDX-License-Identifier: AGPL-3.0-only

//! Step changes must preserve edits to the generated briefing, through `cs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Galaxy {
    root: tempfile::TempDir,
    formula: PathBuf,
    molecule: PathBuf,
    id: String,
}

impl Galaxy {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cosmon = root.path().join(".cosmon");
        let formulas = cosmon.join("formulas");
        fs::create_dir_all(&formulas).unwrap();
        fs::write(
            cosmon.join("config.toml"),
            "[project]\nproject_id = \"briefing-edits\"\n",
        )
        .unwrap();
        let formula = formulas.join("edit-test.formula.toml");
        fs::write(
            &formula,
            r#"formula = "edit-test"
version = 1
description = "Preserve a pilot's briefing edits"
id_prefix = "edit"

[[steps]]
id = "first"
title = "First"
description = "Do the first step."
acceptance = "Done"

[[steps]]
id = "second"
title = "Second"
description = "Do the second step."
acceptance = "Done"

[[steps]]
id = "third"
title = "Third"
description = "Do the third step."
acceptance = "Done"
"#,
        )
        .unwrap();
        let output = command(root.path())
            .args([
                "--json",
                "nucleate",
                "edit-test",
                "--var",
                "topic=Keep intent",
            ])
            .arg("--formulas-dir")
            .arg(&formulas)
            .output()
            .unwrap();
        success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let id = value["id"].as_str().unwrap().to_owned();
        let molecule = cosmon.join("state/fleets/default/molecules").join(&id);
        // Isolate the step-change seam without dispatching a paid worker.
        let state_path = molecule.join("state.json");
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        state["status"] = "running".into();
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        Self {
            root,
            formula,
            molecule,
            id,
        }
    }

    fn briefing(&self) -> PathBuf {
        self.molecule.join("briefing.md")
    }

    fn advance(&self) -> Output {
        command(self.root.path())
            .args(["evolve", &self.id, "--evidence", "step done", "--formula"])
            .arg(&self.formula)
            .output()
            .unwrap()
    }

    fn backups(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.molecule)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("briefing.md.edited-")
            })
            .collect()
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cs"));
    command
        .current_dir(root)
        .env("COSMON_CONFIG_HOME", root.join("config-home"));
    // The enclosing cargo run uses no-pilot-env.sh; keep direct IDE runs safe too.
    for variable in cosmon_core::pilot_env::PilotVar::ALL {
        command.env_remove(variable.name());
    }
    command
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hand_edit_is_kept_and_warning_names_recovery() {
    let galaxy = Galaxy::new();
    let edited = b"Pilot's replacement briefing.\nKeep this exact text.\xff\n";
    fs::write(galaxy.briefing(), edited).unwrap();
    let output = galaxy.advance();
    success(&output);
    let backup = galaxy.molecule.join("briefing.md.edited-1");
    assert_eq!(fs::read(&backup).unwrap(), edited);
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(warning.contains("warning:"), "{warning}");
    assert!(warning.contains(backup.to_str().unwrap()), "{warning}");
    assert!(warning.contains("--var-file"), "{warning}");
    let rendered = fs::read_to_string(galaxy.briefing()).unwrap();
    assert!(rendered.contains("Do the second step."), "{rendered}");
    assert!(rendered.contains("Keep intent"), "{rendered}");
    assert_eq!(galaxy.backups().len(), 1);
}

#[test]
fn unchanged_briefings_advance_without_backup_or_warning() {
    let galaxy = Galaxy::new();
    for next in ["Do the second step.", "Do the third step.", "COMPLETED"] {
        let output = galaxy.advance();
        success(&output);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("warning:"));
        assert!(galaxy.backups().is_empty());
        assert!(fs::read_to_string(galaxy.briefing())
            .unwrap()
            .contains(next));
    }
}

#[test]
fn edit_after_a_step_change_uses_that_steps_render() {
    let galaxy = Galaxy::new();
    success(&galaxy.advance());
    // Even editor-only byte changes differ from cosmon's last render.
    let edited = fs::read_to_string(galaxy.briefing())
        .unwrap()
        .replace('\n', "\r\n");
    fs::write(galaxy.briefing(), &edited).unwrap();
    success(&galaxy.advance());
    assert_eq!(
        fs::read(galaxy.molecule.join("briefing.md.edited-2")).unwrap(),
        edited.as_bytes()
    );
    assert_eq!(galaxy.backups().len(), 1);
}

#[test]
fn rerun_reuses_backup_without_overwriting_it() {
    let galaxy = Galaxy::new();
    let state_path = galaxy.molecule.join("state.json");
    let before = fs::read(&state_path).unwrap();
    let edited = b"Hand addition retained across retry.\n";
    fs::write(galaxy.briefing(), edited).unwrap();
    success(&galaxy.advance());
    let backup = galaxy.molecule.join("briefing.md.edited-1");
    let modified = fs::metadata(&backup).unwrap().modified().unwrap();
    // Replay an interrupted advance from its saved state and edited input.
    fs::write(&state_path, before).unwrap();
    fs::write(galaxy.briefing(), edited).unwrap();
    success(&galaxy.advance());
    assert_eq!(fs::read(&backup).unwrap(), edited);
    assert_eq!(fs::metadata(&backup).unwrap().modified().unwrap(), modified);
    assert_eq!(galaxy.backups().len(), 1);
    let output = galaxy.advance();
    success(&output);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("warning:"));
    assert_eq!(galaxy.backups().len(), 1);
}

#[test]
fn conflicting_backup_refuses_before_losing_either_edit() {
    let galaxy = Galaxy::new();
    let previous = b"Earlier preserved edit.\n";
    let edited = b"A different pilot edit.\n";
    let backup = galaxy.molecule.join("briefing.md.edited-1");
    fs::write(&backup, previous).unwrap();
    fs::write(galaxy.briefing(), edited).unwrap();
    let state = fs::read(galaxy.molecule.join("state.json")).unwrap();
    let output = galaxy.advance();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("briefing.md.edited-1"));
    assert_eq!(fs::read(&backup).unwrap(), previous);
    assert_eq!(fs::read(galaxy.briefing()).unwrap(), edited);
    assert_eq!(fs::read(galaxy.molecule.join("state.json")).unwrap(), state);
}

#[test]
fn legacy_briefing_without_render_record_is_kept_conservatively() {
    let galaxy = Galaxy::new();
    let before = fs::read(galaxy.briefing()).unwrap();
    fs::remove_file(galaxy.molecule.join("briefing.md.rendered-blake3")).unwrap();
    let output = galaxy.advance();
    success(&output);
    assert_eq!(
        fs::read(galaxy.molecule.join("briefing.md.edited-1")).unwrap(),
        before
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("--var-file"));
    success(&galaxy.advance());
    assert_eq!(galaxy.backups().len(), 1);
}

#[test]
fn missing_briefing_is_regenerated_without_edit_warning() {
    let galaxy = Galaxy::new();
    fs::remove_file(galaxy.briefing()).unwrap();
    let output = galaxy.advance();
    success(&output);
    assert!(fs::read_to_string(galaxy.briefing())
        .unwrap()
        .contains("Do the second step."));
    assert!(galaxy.backups().is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("warning:"));
}

#[cfg(unix)]
#[test]
fn backup_symlink_cannot_substitute_for_a_saved_copy() {
    let galaxy = Galaxy::new();
    let edited = b"Do not lose this through a backup pointing at the live file.\n";
    fs::write(galaxy.briefing(), edited).unwrap();
    std::os::unix::fs::symlink(
        galaxy.briefing(),
        galaxy.molecule.join("briefing.md.edited-1"),
    )
    .unwrap();
    let output = galaxy.advance();
    assert!(!output.status.success());
    assert_eq!(fs::read(galaxy.briefing()).unwrap(), edited);
}

fn deliver_posture(galaxy: &Galaxy) {
    let body = "Refute the implementation from the artifacts.";
    fs::write(
        galaxy
            .molecule
            .join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        cosmon_core::committee::render_committee_posture(
            cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
            &cosmon_core::committee::committee_contract_hash(body),
            body,
        ),
    )
    .unwrap();
    let output = command(galaxy.root.path())
        .args([
            "tackle",
            &galaxy.id,
            "--dry-run",
            "--no-worktree",
            "--adapter",
            "codex",
            "--model",
            "gpt-6.1-sol",
        ])
        .output()
        .unwrap();
    success(&output);
    assert!(fs::read_to_string(galaxy.briefing())
        .unwrap()
        .contains("committee-posture.md"));
}

#[test]
fn generated_tackle_additions_are_silent_on_the_next_advance() {
    let galaxy = Galaxy::new();
    deliver_posture(&galaxy);
    let output = galaxy.advance();
    success(&output);
    assert!(galaxy.backups().is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("warning:"));
}

#[test]
fn generated_additions_do_not_hide_a_preexisting_hand_edit() {
    let galaxy = Galaxy::new();
    let mut edited = fs::read_to_string(galaxy.briefing()).unwrap();
    edited.push_str("\nPilot context that must survive tackle.\n");
    fs::write(galaxy.briefing(), edited).unwrap();
    deliver_posture(&galaxy);
    let handed_to_worker = fs::read(galaxy.briefing()).unwrap();
    success(&galaxy.advance());
    assert_eq!(
        fs::read(galaxy.molecule.join("briefing.md.edited-1")).unwrap(),
        handed_to_worker
    );
}

#[test]
fn edited_final_step_is_preserved_before_the_completed_render() {
    let galaxy = Galaxy::new();
    success(&galaxy.advance());
    success(&galaxy.advance());
    let edited = b"Last step's hand edit.\n";
    fs::write(galaxy.briefing(), edited).unwrap();
    success(&galaxy.advance());
    assert_eq!(
        fs::read(galaxy.molecule.join("briefing.md.edited-3")).unwrap(),
        edited
    );
    assert!(fs::read_to_string(galaxy.briefing())
        .unwrap()
        .contains("COMPLETED"));
}

#[test]
fn unreadable_briefing_or_digest_refuses_the_advance() {
    for filename in ["briefing.md", "briefing.md.rendered-blake3"] {
        let galaxy = Galaxy::new();
        let state = fs::read(galaxy.molecule.join("state.json")).unwrap();
        let path = galaxy.molecule.join(filename);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let output = galaxy.advance();
        assert!(!output.status.success(), "{filename}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("could not preserve briefing.md"));
        assert_eq!(fs::read(galaxy.molecule.join("state.json")).unwrap(), state);
    }
}

#[test]
fn invalid_utf8_briefing_is_not_treated_as_missing_during_tackle() {
    let galaxy = Galaxy::new();
    let edited = b"Pilot's non-UTF-8 edit: \xff\n";
    fs::write(galaxy.briefing(), edited).unwrap();
    fs::write(
        galaxy.molecule.join("committee-posture.md"),
        "Reviewer contract",
    )
    .unwrap();
    let output = command(galaxy.root.path())
        .args([
            "tackle",
            &galaxy.id,
            "--dry-run",
            "--no-worktree",
            "--adapter",
            "codex",
            "--model",
            "gpt-6.1-sol",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(galaxy.briefing()).unwrap(), edited);
}

#[cfg(unix)]
#[test]
fn readonly_digest_does_not_leave_a_committed_step_reported_as_failed() {
    use std::os::unix::fs::PermissionsExt;
    let galaxy = Galaxy::new();
    let digest = galaxy.molecule.join("briefing.md.rendered-blake3");
    fs::set_permissions(&digest, fs::Permissions::from_mode(0o444)).unwrap();
    let output = galaxy.advance();
    success(&output);
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(galaxy.molecule.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["current_step"], 1);
    assert!(state["pending_step"].is_null());
    success(&galaxy.advance());
    assert!(galaxy.backups().is_empty());
}
