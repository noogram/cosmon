// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI proof that a seat whose verdict **cannot be read** is refused
//! by the tool, through `cs reconcile --check`.
//!
//! # Why this exists
//!
//! `converge-clean-room.formula.toml` requires that a `verdict.json` with no
//! `mechanism_polarity`, or an inconsistent `(polarity, verdict, VERDICT:)`
//! triple, is NOT-CLEAN — and mitigates the residual with the sentence *"a seat
//! convened by this loop is ALWAYS `polarity: fix`"*. That sentence was a
//! **declaration about seats that nothing resolved**: no code anywhere read the
//! field, so a seat could omit it, or state it falsely, and every gate stayed
//! green. It is the same shape the roster lint was written to close one layer
//! up — a witness with zero production callers.
//!
//! The question that governs this lineage is *can the gate still pass when the
//! constrained party lies, or is simply absent?* Asserting on
//! [`cosmon_core::committee::read_seat_emission`] alone would not answer it: a
//! predicate that passes its own unit tests while nothing calls it is exactly
//! the failure being fixed. So every case here runs the binary and asserts on
//! its exit status.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

const CONFIG: &str = "\
[project]
project_id = \"test-seat-verdict-polarity\"
";

/// A `.cosmon/` with one molecule directory, ready for a seat's artefacts.
///
/// No `roster.json`, no `roster.md`, and no `formula_id` in the molecule's
/// state — so the sibling roster lint has nothing to say and every exit status
/// below belongs to the polarity lint.
fn setup(tmp: &Path, molecule: &str) -> (PathBuf, PathBuf) {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let mol_dir = state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(molecule);
    fs::create_dir_all(&mol_dir).expect("molecule dir");
    fs::write(cosmon_dir.join("config.toml"), CONFIG).expect("config");
    fs::write(
        state_dir.join("fleet.json"),
        "{\"workers\":{},\"repos\":{}}\n",
    )
    .expect("fleet.json");
    (state_dir, mol_dir)
}

/// Write a seat's two artefacts plus the molecule status the live/historical
/// split reads.
fn emit(mol_dir: &Path, status: &str, verdict_json: &str, report_first_line: &str) {
    fs::write(
        mol_dir.join("state.json"),
        format!("{{\"status\":\"{status}\"}}\n"),
    )
    .expect("state.json");
    fs::write(mol_dir.join("verdict.json"), verdict_json).expect("verdict.json");
    fs::write(
        mol_dir.join("referee-report.md"),
        format!("{report_first_line}\n\nbody\n"),
    )
    .expect("referee-report.md");
}

/// Run `cs reconcile --check` against an isolated state dir and return whether
/// it succeeded.
fn reconcile_check_passes(state_dir: &Path) -> (bool, String) {
    let config_path = state_dir
        .parent()
        .expect("state_dir under .cosmon/")
        .join("config.toml");
    let out = cosmon_bin()
        .env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", &config_path)
        .current_dir(state_dir)
        .args(["reconcile", "--check"])
        .output()
        .expect("run cs reconcile --check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// **The control.** A coherent seat — `fix` + `confirmed` + `VERDICT: CLEAN` —
/// passes. Without this the refusals below would prove only that the command
/// exits 1, not that the lint discriminates.
#[test]
fn a_coherent_seat_passes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-ok");
    emit(
        &mol_dir,
        "running",
        r#"{"verdict":"confirmed","mechanism_polarity":"fix","count":0}"#,
        "VERDICT: CLEAN",
    );
    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        passed,
        "a seat stating `fix` + `confirmed` + `VERDICT: CLEAN` is one row of the \
         table and must pass. Output:\n{out}"
    );
}

/// **The absent constrained party.** The field is simply not there. The formula
/// says the loop's seats are always `polarity: fix`, which would map
/// `confirmed` to CLEAN — and the gate must refuse to make that assumption.
#[test]
fn a_missing_polarity_is_refused_even_though_the_formula_says_it_is_always_fix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-bare");
    emit(
        &mol_dir,
        "running",
        r#"{"verdict":"confirmed","count":0}"#,
        "VERDICT: CLEAN",
    );
    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        !passed,
        "a `verdict.json` speaking the RELATIVE cmb-verify door with no \
         `mechanism_polarity` must fail the gate. Assuming the polarity that \
         makes the round pass is how the inversion was introduced. Output:\n{out}"
    );
    assert!(
        out.contains("mechanism_polarity"),
        "the refusal must name the missing field so a human can act on it. \
         Output:\n{out}"
    );
}

/// **The lying constrained party.** Both files are affirmative and they agree
/// in form — the shape the both-files rule passes — while the stated polarity
/// makes them say opposite things: "the defect reproduces" and "nothing found"
/// in one breath.
#[test]
fn the_agreeing_but_wrong_pair_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-lying");
    emit(
        &mol_dir,
        "running",
        r#"{"verdict":"confirmed","mechanism_polarity":"defect","count":0}"#,
        "VERDICT: CLEAN",
    );
    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        !passed,
        "`defect` + `confirmed` maps to FINDINGS — the defect REPRODUCES — so a \
         seat writing `VERDICT: CLEAN` beside it is off the table and must fail \
         the gate. Two files agreeing is not two files being right. Output:\n{out}"
    );
}

/// The absolute vocabulary needs no polarity, and demanding one there would be
/// noise a reader learns to ignore. Scope is decided by the verdict's own
/// vocabulary, never by the molecule's kind.
#[test]
fn an_absolute_verdict_needs_no_polarity() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "task-20260728-gate");
    emit(
        &mol_dir,
        "running",
        r#"{"verdict":"FINDINGS","count":3}"#,
        "VERDICT: FINDINGS (3)",
    );
    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        passed,
        "`FINDINGS` is the ABSOLUTE door — its meaning does not depend on what \
         the stated mechanism claimed — so no polarity is required. Output:\n{out}"
    );
}

/// A terminal molecule's verdict cannot be corrected by any current work, so it
/// is reported and never fails the gate — the same trade the roster lint makes,
/// and for the same reason: a refusal nobody can act on is an outage.
#[test]
fn a_terminal_molecule_is_historical_not_a_refusal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-past");
    emit(
        &mol_dir,
        "completed",
        r#"{"verdict":"refuted","count":2}"#,
        "VERDICT: FINDINGS (2)",
    );
    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        passed,
        "a completed molecule's unreadable verdict is a historical advisory, \
         never a gate failure. Output:\n{out}"
    );
    assert!(
        out.contains("HISTORICAL"),
        "…but it must still be PRINTED. Silently dropping it would hide the \
         history the advisory exists to keep. Output:\n{out}"
    );
}

// ── F3 — a seat that says nothing was silent; a seat that lies was recorded ──

/// Put a molecule in the scope where an ABSENT verdict is judged rather than
/// skipped, through the door no seat can decline: the `formula_id` `cs
/// nucleate` recorded before any worker ran.
///
/// Deliberately NOT the durable `committee-posture.md` here. That is a second,
/// equally valid door — exercised by `the_posture_door_also_puts_a_seat_in_scope`
/// below — but a posture with no roster in the tree is refused by the SIBLING
/// roster lint, which exits first and would decide these exit statuses for a
/// reason that has nothing to do with the verdict.
fn seat_it(mol_dir: &Path, status: &str) {
    fs::write(
        mol_dir.join("state.json"),
        format!("{{\"status\":\"{status}\",\"formula_id\":\"cmb-verify\"}}\n"),
    )
    .expect("state.json");
}

/// **The falsifier.** A seated seat with NO `verdict.json` at all passed the
/// lint, while the same seat with an unparseable one was refused. The contract
/// says a missing verdict is NOT-CLEAN and never a pass; `SeatReadingRefusal`
/// declared a `NoVerdict` variant for exactly this case and **no caller ever
/// constructed it** — the rule had no code enforcement while the type
/// advertised that it had.
#[test]
fn a_seated_seat_with_no_verdict_file_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-mute");
    seat_it(&mol_dir, "running");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        !passed,
        "a seat that emitted NOTHING must fail closed exactly as one that \
         emitted garbage does — the asymmetry between them IS the bug. \
         Output:\n{out}"
    );
    assert!(
        out.contains("cmbverify-20260728-mute") && out.contains("NOT-CLEAN"),
        "the refusal must name the silent seat and what its silence means. \
         Output:\n{out}"
    );
}

/// The same absence arriving one field along: the file exists and carries no
/// verdict in EITHER vocabulary. This is the `continue` that sat between the
/// two the fix closed.
#[test]
fn a_verdict_file_carrying_no_verdict_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-empty");
    seat_it(&mol_dir, "running");
    fs::write(mol_dir.join("verdict.json"), r#"{"count":0,"findings":[]}"#).expect("verdict.json");
    fs::write(mol_dir.join("referee-report.md"), "VERDICT: CLEAN\n").expect("report");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        !passed,
        "a `verdict.json` with no `verdict` field carries no verdict; a clean \
         report beside it is one file, not both. Output:\n{out}"
    );
}

/// And the third: a seat that spoke in `verdict.json` and wrote no report. The
/// contract asks for an affirmative verdict in BOTH files, and one file is one
/// file — `NoReport` was the enum's other unconstructed variant.
#[test]
fn a_seat_with_no_referee_report_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-halfspoken");
    seat_it(&mol_dir, "running");
    fs::write(
        mol_dir.join("verdict.json"),
        r#"{"verdict":"confirmed","mechanism_polarity":"fix"}"#,
    )
    .expect("verdict.json");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(!passed, "a verdict.json alone is one file. Output:\n{out}");
    assert!(
        out.contains("referee-report.md"),
        "the refusal must name the file that is missing. Output:\n{out}"
    );
}

/// **The counterweight, and the one that keeps this from being an outage.** A
/// seated seat that emitted BOTH files coherently still passes. A gate that
/// cannot pass is not a control.
#[test]
fn a_seated_seat_that_emitted_both_files_still_passes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-good");
    seat_it(&mol_dir, "running");
    fs::write(
        mol_dir.join("verdict.json"),
        r#"{"verdict":"confirmed","mechanism_polarity":"fix","count":0}"#,
    )
    .expect("verdict.json");
    fs::write(
        mol_dir.join("referee-report.md"),
        "VERDICT: CLEAN\n\nbody\n",
    )
    .expect("report");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        passed,
        "a seat that delivered both files coherently must pass. Output:\n{out}"
    );
}

/// **The other counterweight: scope.** A molecule that is NOT a seat owes no
/// verdict, and refusing every molecule without a `verdict.json` would redden
/// every tree cosmon has ever written. Absence is judged only where something
/// other than the molecule's own choice put it in scope.
#[test]
fn a_molecule_that_is_not_a_seat_owes_no_verdict() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "task-20260728-ordinary");
    fs::write(mol_dir.join("state.json"), "{\"status\":\"running\"}\n").expect("state.json");
    fs::write(mol_dir.join("briefing.md"), "# Briefing\n").expect("briefing");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        passed,
        "an ordinary task molecule is not a seat and owes nothing. Output:\n{out}"
    );
}

/// A seat that wrote only the human report — no `verdict.json` — is in scope
/// through the second door, because it spoke as a seat. Otherwise a seat could
/// leave the machine-readable half off and be out of scope for having done so,
/// which is opt-out by omission.
#[test]
fn a_report_without_its_machine_readable_half_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-prose");
    fs::write(mol_dir.join("state.json"), "{\"status\":\"running\"}\n").expect("state.json");
    fs::write(mol_dir.join("referee-report.md"), "VERDICT: CLEAN\n").expect("report");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        !passed,
        "a prose CLEAN with no machine-readable verdict is a verdict no gate \
         can read. Output:\n{out}"
    );
}

/// The second scope door, on a molecule that has already finished so the
/// sibling roster lint's unrostered-seat pass (which reads only LIVE molecules)
/// stays out of the way: a seat carrying the durable adversarial contract is in
/// scope on that fact alone, whatever its formula says.
#[test]
fn the_posture_door_also_puts_a_seat_in_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup(tmp.path(), "cmbverify-20260728-postured");
    fs::write(mol_dir.join("state.json"), "{\"status\":\"completed\"}\n").expect("state.json");
    fs::write(
        mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "# posture\n",
    )
    .expect("posture");

    let (passed, out) = reconcile_check_passes(&state_dir);
    assert!(
        out.contains("cmbverify-20260728-postured") && out.contains("NOT-CLEAN"),
        "a seat that carries the durable contract and emitted no verdict must \
         be NAMED. Output:\n{out}"
    );
    assert!(
        passed,
        "…and, being terminal, reported rather than refused — the same trade \
         the sibling lint makes. Output:\n{out}"
    );
}
