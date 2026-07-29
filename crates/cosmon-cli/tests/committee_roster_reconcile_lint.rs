// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI proof that a **witness-failing committee roster is refused
//! by the tool**, through `cs reconcile --check`.
//!
//! # Why the assertion is on the command and not on the predicate
//!
//! `cosmon_core::committee` decides who may sit on a cross-provider jury, and
//! every one of its predicates passed its own unit tests while enforcing
//! nothing. Verified by grep on 2026-07-28: `plan_committee`,
//! `committee_requirement`, `fold_committee`, `jury_integrity`,
//! `sor_may_not_resurrect` and `RosterPlan::floor_bearing_seats` had **zero
//! production callers** anywhere in the workspace. The only committee
//! references outside the module were the posture-injection plumbing in
//! `cs evolve` — not the decision kernel. So witness (1), witness (2) and the
//! diversity floor were held up by a worker reading prose, and a roster that
//! failed one of them was contradicted by nothing.
//!
//! Adding more tests to the predicates would have reproduced that exactly:
//! they already pass. Every case here therefore runs the binary and asserts on
//! its exit status, which is the only thing that can actually refuse a roster.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_isolated(state_dir: &Path) -> Command {
    let config_path = state_dir
        .parent()
        .expect("state_dir must live under .cosmon/")
        .join("config.toml");
    let mut cmd = cosmon_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        .current_dir(state_dir);
    cmd
}

/// Two adapters resolving to genuinely distinct families, so nothing here is
/// decided by the *config* lint that already existed — every verdict below is
/// the roster lint's.
const CONFIG: &str = "\
[project]
project_id = \"test-roster-witness\"

[adapters.gen_seat]
default_model = \"claude-opus-4-8\"

[adapters.ref_seat]
default_model = \"gpt-4o\"

[adapters.ref_seat_two]
default_model = \"grok-4\"

# An echo seat: a DIFFERENT config section resolving to the generator's own
# family. This is what a real same-family collapse looks like, and it is the
# only honest way to express one now that the lint re-derives every tuple. The
# collision case used to declare family \"anthropic\" on `ref_seat`, whose
# section resolves to openai — so the fixture could only produce a collision by
# LYING about its endpoint, which the resolution check catches one step earlier
# than the collision. Two sections, one family, is the shape ADR-147 is about.
[adapters.ref_seat_echo]
default_model = \"claude-opus-4-8\"
";

/// A `.cosmon/` with one molecule directory, ready for a `roster.json`.
fn setup_project(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let mol_dir = state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join("committee-20260728-test");
    fs::create_dir_all(&mol_dir).expect("molecule dir");
    fs::write(cosmon_dir.join("config.toml"), CONFIG).expect("config");
    fs::write(
        state_dir.join("fleet.json"),
        "{\"workers\":{},\"repos\":{}}\n",
    )
    .expect("fleet.json");
    (state_dir, mol_dir)
}

/// A seat as the roster declares it. `endpoint` is the RESOLVED tuple — the
/// axis distinctness is measured on — and the persona block carries the
/// witness-(2) facts.
///
/// `adapter` is set to the seat id, which is also its `[adapters.<name>]`
/// section in `CONFIG`, so a legal fixture's declaration survives re-derivation.
/// It is not decoration: the lint re-resolves every tuple from that section's
/// `base_url` + `model` and refuses a declaration the derivation contradicts.
/// A seat that named no adapter was self-attesting — it could claim any family
/// and pass — and this fixture predates the field, which is why the control
/// case below started failing rather than any code being wrong.
fn seat(
    seat_id: &str,
    role: &str,
    family: &str,
    role_id: &str,
    injected: bool,
    artifact: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "seat_id": seat_id,
        "role": role,
        "adapter": seat_id,
        "family": {
            "endpoint": { "provider": family, "base_url": "", "family": family },
            "model": serde_json::Value::Null,
        },
        "persona": {
            "role_id": role_id,
            "briefing": {
                "version": cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
                "contract_hash": "blake3:test",
                "injected": injected,
            },
            "falsification_artifact": artifact,
        },
    })
}

/// Put a seat's molecule directory on disk in the state `cs tackle` leaves
/// behind: the durable `committee-posture.md` plus a `briefing.md` carrying the
/// pointer at it.
///
/// Every fixture whose roster claims `injected: true` must call this, because a
/// claim of delivery for a seat with **no directory at all** is now refused —
/// delivery is a fact about two files, and where there is no directory there
/// are no files to have it. Before that refusal existed, `None` from the
/// observation port meant "leave the claim alone", so an entirely absent seat
/// kept whatever the roster typed. That is what made the legal-roster control
/// below pass while its seats were never nucleated.
fn deliver(mol_dir: &Path, seat_id: &str) {
    let seat_dir = mol_dir.parent().expect("molecules dir").join(seat_id);
    fs::create_dir_all(&seat_dir).expect("seat dir");
    fs::write(
        seat_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        // The contract `seat()` declares — same version, same `contract_hash`.
        // This used to be `"# posture\n"`, and the witness accepted it, because
        // the witness only counted the file. It reads it now, so a fixture that
        // means "delivered" has to deliver something.
        cosmon_core::committee::render_committee_posture(
            cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
            "blake3:test",
            "Audit the artefacts. The generator's confidence is not evidence.",
        ),
    )
    .expect("posture");
    fs::write(
        seat_dir.join("briefing.md"),
        cosmon_core::committee::committee_posture_reference(),
    )
    .expect("briefing");
}

/// Like [`seat`], but the seat sits on an adapter whose name is NOT its seat
/// id — which is the only way to express a seat on a **registry-only** adapter
/// such as `codex`, since the fixture's `CONFIG` deliberately declares no
/// `[adapters.codex]` section.
fn seat_on_adapter(
    seat_id: &str,
    adapter: &str,
    role: &str,
    family: &str,
    role_id: &str,
    injected: bool,
    artifact: Option<&str>,
) -> serde_json::Value {
    let mut v = seat(seat_id, role, family, role_id, injected, artifact);
    v.as_object_mut()
        .expect("seat is an object")
        .insert("adapter".to_owned(), serde_json::json!(adapter));
    v
}

fn write_roster(mol_dir: &Path, refuters: Vec<serde_json::Value>) {
    // The generator always claims `injected: true`, so it always owes the two
    // files that claim names.
    deliver(mol_dir, "gen_seat");
    let roster = serde_json::json!({
        "stake": "root",
        "cross_provider": true,
        "generator": seat(
            "gen_seat",
            "generator",
            "anthropic",
            "generator",
            true,
            Some("falsifier.md"),
        ),
        "refuters": refuters,
    });
    fs::write(
        mol_dir.join(cosmon_core::committee::COMMITTEE_ROSTER_FILE),
        serde_json::to_string_pretty(&roster).expect("serialize roster"),
    )
    .expect("write roster");
}

fn reconcile_check(state_dir: &Path) -> std::process::Output {
    cosmon_bin_isolated(state_dir)
        .args(["reconcile", "--check"])
        .output()
        .expect("spawn cs reconcile --check")
}

/// The control. A roster whose single refuter passes BOTH witnesses and meets
/// the floor must be accepted — without this, every refusal below could be
/// satisfied by a gate that refuses everything.
#[test]
fn a_legal_roster_is_accepted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "a roster passing both witnesses must not be refused; got:\n{stderr}"
    );
}

/// Witness (1). A refuter resolving to the generator's own family is the
/// same-family collapse — two seats on one endpoint are an echo, not two
/// witnesses.
#[test]
fn reconcile_check_refuses_a_family_collision() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat_echo");
    write_roster(
        &mol_dir,
        vec![seat(
            // A distinct config section that RESOLVES to the generator's own
            // family — the proxy-costume collapse, declared honestly.
            "ref_seat_echo",
            "refuter",
            "anthropic",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a roster whose refuter shares the generator's endpoint must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("family-collision"),
        "the refusal must name the witness axis that rejected; got:\n{stderr}"
    );
    assert!(
        stderr.contains("ref_seat_echo") && stderr.contains("gen_seat"),
        "the refusal must name both colliding seats so a convener can fix it; \
         got:\n{stderr}"
    );
}

/// Witness (2), first clause. Two providers both told to play the same role
/// are one posture wearing two hats — channel independence without posture
/// independence is a costume.
#[test]
fn reconcile_check_refuses_a_persona_collision() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    deliver(&mol_dir, "ref_seat_two");
    write_roster(
        &mol_dir,
        vec![
            seat(
                "ref_seat",
                "refuter",
                "openai",
                "adversary",
                true,
                Some("falsification-attempt.md"),
            ),
            seat(
                "ref_seat_two",
                "refuter",
                "xai",
                // Same role_id as its peer.
                "adversary",
                true,
                Some("falsification-attempt.md"),
            ),
        ],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "two refuters sharing a role_id must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("persona-collision"),
        "the refusal must name the persona axis; got:\n{stderr}"
    );
}

/// Witness (2), second clause — the one the whole `committee-posture.md`
/// mechanism exists for. A contract that is declared but not delivered is a
/// posture the refuter never received.
#[test]
fn reconcile_check_refuses_a_briefing_that_was_never_injected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            // Declared, never delivered.
            false,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a paper adversarial contract must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("briefing-not-injected"),
        "the refusal must name the undelivered contract; got:\n{stderr}"
    );
}

/// Witness (2), third clause. A refuter with no falsification-attempt artefact
/// shipped no evidence it tried to break the fix — it is a reader.
#[test]
fn reconcile_check_refuses_a_refuter_that_shipped_no_falsification_artefact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            None,
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a refuter with no falsification artefact must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("falsification-artifact-missing"),
        "the refusal must name the missing artefact; got:\n{stderr}"
    );
}

/// The floor itself. A generator with no admissible refuter at all spans one
/// family against a root-stake floor of two — the committee cannot be
/// convened, and that is a refusal, not a note.
#[test]
fn reconcile_check_refuses_a_roster_below_its_family_floor() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(&mol_dir, vec![]);

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a roster below its diversity floor must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("below the required floor"),
        "the refusal must name the floor it fell below; got:\n{stderr}"
    );
}

/// An unparseable roster is an UNCHECKED roster. Skipping it would rebuild the
/// defect this lint closes one level up: a gate that reports green on the one
/// file it could not read.
#[test]
fn reconcile_check_refuses_a_roster_it_cannot_parse() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    fs::write(
        mol_dir.join(cosmon_core::committee::COMMITTEE_ROSTER_FILE),
        "{ this is not a roster",
    )
    .expect("write bad roster");

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a roster the gate cannot parse must be REFUSED, never skipped"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("UNCHECKED"),
        "the refusal must say why an unparseable roster is refused; got:\n{stderr}"
    );
}

/// A molecule that declares no roster is not a committee. The lint must be
/// invisible to the entire rest of the fleet.
#[test]
fn a_molecule_with_no_roster_is_not_a_committee() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, _mol_dir) = setup_project(tmp.path());

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "an ordinary molecule must not be linted as a committee; got:\n{stderr}"
    );
}

/// **A1 at the CLI boundary.** The lint plans the RESOLVED tuple, not the
/// declared one.
///
/// Before this, `FamilyWitness::resolve` had zero callers anywhere in the
/// workspace and the lint deserialized the convener's own tuples and planned
/// them — so a roster could declare two families it did not have and pass.
/// Here `ref_seat` sits on `[adapters.ref_seat]` (gpt-4o -> openai) while
/// declaring `anthropic`. The declaration is refused by name, and the message
/// gives the convener both tuples so the fix is obvious.
#[test]
fn reconcile_check_refuses_a_declaration_that_does_not_resolve() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            // A lie: this section resolves to openai.
            "anthropic",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a roster whose declared tuple does not survive resolution must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("RESOLVES to") && stderr.contains("ref_seat"),
        "the refusal must name the seat and both tuples; got:\n{stderr}"
    );
}

/// **A1, the other end.** A seat naming no `adapter` is refused as a
/// self-attestation rather than skipped — otherwise the check above is
/// opt-out by deleting one field, which is the shape the whole gate exists to
/// refuse.
#[test]
fn reconcile_check_refuses_a_seat_that_names_no_adapter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    let mut refuter = seat(
        "ref_seat",
        "refuter",
        "openai",
        "adversary",
        true,
        Some("falsification-attempt.md"),
    );
    refuter
        .as_object_mut()
        .expect("seat is an object")
        .remove("adapter");
    write_roster(&mol_dir, vec![refuter]);

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "an unresolvable claim must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("SELF-ATTESTATION") && stderr.contains("ref_seat"),
        "the refusal must name the seat and say why the claim is unresolvable; \
         got:\n{stderr}"
    );
}

/// **A2 at the CLI boundary.** A convene-shaped roster owes a contract, not a
/// wider floor.
///
/// At convene nothing has been dispatched, so no refuter carries an injected
/// briefing, so none is admitted, so a floor counted over ADMITTED seats reads
/// 1 and refuses every correctly shaped committee that ever convenes — a bar no
/// convene step can clear, which is an outage rather than a control. The floor
/// is counted over the families the roster REACHES, so the honest finding (the
/// contract is not delivered yet) is the only one reported.
#[test]
fn a_convene_shaped_roster_is_told_about_its_contract_not_its_floor() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            // Convene: nothing dispatched, so nothing delivered.
            false,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("briefing-not-injected"),
        "the undelivered contract is the true finding; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("cannot be convened as written"),
        "the floor is REACHABLE — the second family is on the roster and merely \
         undispatched. Refusing it here is a bar no convene step can clear; \
         got:\n{stderr}"
    );
}

/// **A3 at the CLI boundary.** `injected: true` is a claim about two files, and
/// the lint reads the files.
///
/// The recipe told the convener to "flip that seat's
/// `persona.briefing.injected` to true once the durable file exists" — making
/// the load-bearing field of witness (2) a self-declaration, exactly like the
/// declared family before A1. Here the seat has a real molecule directory whose
/// `committee-posture.md` was never written, so the claim is contradicted.
#[test]
fn reconcile_check_refuses_an_injection_the_files_contradict() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    // The seat's own directory exists (it was nucleated) but carries no durable
    // posture file, so there is something on disk to contradict the claim.
    let seat_dir = mol_dir.parent().expect("molecules dir").join("ref_seat");
    fs::create_dir_all(&seat_dir).expect("seat dir");
    fs::write(seat_dir.join("briefing.md"), "# a briefing\n").expect("briefing");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            // The paper contract.
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(!out.status.success(), "a paper contract must be REFUSED");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("claims `injected: true`") && stderr.contains("is MISSING"),
        "the refusal must name the claim and which fact fails; got:\n{stderr}"
    );
}

/// **A3, the other direction — and the proof that the gate can be PASSED.**
///
/// The seat's directory carries the durable `committee-posture.md` AND a
/// `briefing.md` pointing at it: the exact state `cs tackle` leaves behind.
/// The roster is accepted. Without this case, the refusal above could be
/// satisfied by a gate that refuses every seat with a directory — a gate that
/// always fails, which is the outage A2 was about, arriving on the other axis.
///
/// This is also the empirical answer to "where does the fully-green bar
/// belong?". The live committee's remaining line is exactly the state above
/// MINUS the briefing pointer, because that seat was never tackled — and
/// `cs tackle` is what writes it. So the bar is satisfiable by the pilot after
/// tackle and by nobody before it, which is where the recipe now puts it.
#[test]
fn a_delivered_contract_on_disk_is_accepted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    // The rendered contract the roster declares, plus what `cs tackle` /
    // `cs evolve` append — the stable pointer, not an inline copy, which
    // regeneration would clobber.
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "a seat whose two delivery facts BOTH hold on disk must be accepted, or \
         the gate cannot be passed by anyone; got:\n{stderr}"
    );
}

/// **The falsifier for the content witness.** A posture file that is a
/// PLACEHOLDER is refused, where a presence-only witness accepted it.
///
/// Measured on this very test's predecessor, 2026-07-29: the accepted fixture
/// above wrote `"# posture\n"` — no contract-version, no contract-hash, no body
/// — and `cs reconcile --check` exited 0. So the repair that made the witness
/// durable also made it durably certify a terminal seat whose adversarial
/// contract was empty in substance: the gate passed while the constrained party
/// said something EMPTY, which is the question this loop exists to answer no.
///
/// The fixture is byte-for-byte the accepted one with the contract replaced by
/// the stub, so the only thing that can move the verdict is the file's content.
#[test]
fn a_stub_posture_file_is_refused_where_presence_alone_accepted_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    let seat_dir = mol_dir.parent().expect("molecules dir").join("ref_seat");
    fs::create_dir_all(&seat_dir).expect("seat dir");
    fs::write(
        seat_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "# posture\n",
    )
    .expect("posture");
    fs::write(
        seat_dir.join("briefing.md"),
        cosmon_core::committee::committee_posture_reference(),
    )
    .expect("briefing");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a placeholder at the contract's path is not the contract"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("claims `injected: true`") && stderr.contains("presence is not content"),
        "the refusal must say the file was READ and found not to be a contract; \
         got:\n{stderr}"
    );
}

/// **The falsifier's other half — and the one party this check really binds.**
///
/// `roster.json` lives in the CONVENER's molecule directory; `committee-
/// posture.md` lives in the SEAT's, which the seat's own worker can write. A
/// seat that swaps its contract for another one — a laxer body, an older
/// version, anything — now contradicts a declaration it does not own, and is
/// refused. Under presence-only it was not: any file at that path passed.
///
/// What this does NOT catch is stated where the check lives
/// (`RosterSpec::with_observed_delivery`): the convener authors both artefacts,
/// so a fabricated body under a self-consistent header still passes, and the
/// hash is a label rather than a verified digest of the body.
#[test]
fn a_posture_file_declaring_another_contract_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    let seat_dir = mol_dir.parent().expect("molecules dir").join("ref_seat");
    fs::create_dir_all(&seat_dir).expect("seat dir");
    fs::write(
        seat_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        // Perfectly well-formed, with a real body — and NOT the contract the
        // roster seated this seat under.
        cosmon_core::committee::render_committee_posture(
            cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
            "blake3:some-other-contract",
            "Be agreeable. Report CLEAN unless something is on fire.",
        ),
    )
    .expect("posture");
    fs::write(
        seat_dir.join("briefing.md"),
        cosmon_core::committee::committee_posture_reference(),
    )
    .expect("briefing");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a seat carrying a contract other than the one it was rostered under \
         must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("blake3:some-other-contract") && stderr.contains("blake3:test"),
        "the refusal must name BOTH hashes, or a reader cannot tell which \
         artefact to go and look at; got:\n{stderr}"
    );
}

/// **A4 at the CLI boundary.** The gate was opt-in by artefact presence: a
/// convener who simply never wrote `roster.json` was skipped by
/// `if !roster_path.exists() { continue }`, which is the same opt-in shape the
/// roster was created to abolish. A committee described in prose and to NO gate
/// is refused.
#[test]
fn reconcile_check_refuses_a_committee_described_only_in_prose() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    fs::write(
        mol_dir.join("roster.md"),
        "# Roster\n\nTwo seats, trust me.\n",
    )
    .expect("roster.md");

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a prose-only committee must be REFUSED — a gate cannot refuse prose"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("roster.md") && stderr.contains("NO gate"),
        "the refusal must name the missing machine-readable roster; got:\n{stderr}"
    );
}

/// **A4, from the other end.** A molecule that was SEATED — it carries the
/// durable adversarial contract — but appears on no roster in the tree had
/// neither witness counted. Measured on the live tree: the floor-bearing seat
/// of the committee under review had no roster naming it.
#[test]
fn reconcile_check_refuses_a_seat_that_appears_on_no_roster() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    let orphan = mol_dir
        .parent()
        .expect("molecules dir")
        .join("cmbverify-20260728-orphan");
    fs::create_dir_all(&orphan).expect("orphan seat dir");
    fs::write(
        orphan.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "# posture\n",
    )
    .expect("posture");

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a seat nobody rostered is not an exempt seat, it is an unexamined one"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cmbverify-20260728-orphan") && stderr.contains("NO `roster.json`"),
        "the refusal must name the unrostered seat; got:\n{stderr}"
    );
}

/// The counterweight to both A4 cases: a TERMINAL molecule is not refused for
/// the ABSENCE of a roster.
///
/// 32 of the 34 cases the presence checks found on the live tree predate
/// `roster.json` existing at all and cannot grow one retroactively, so refusing
/// them on every future run makes the gate permanently red over history nobody
/// can change — an outage wearing a control's clothes.
///
/// This once carried the line *"terminality excuses the absence of a file,
/// never its contents"*, and that second clause was itself an outage: one
/// finished committee held `cs reconcile --check` red permanently, with no
/// action on any current work able to clear it. Terminal CONTENTS are now
/// reported as HISTORICAL and do not fail the gate either — see
/// `a_terminal_committees_violating_roster_is_reported_but_does_not_fail_the_gate`
/// and its live-work counterweight.
#[test]
fn a_terminal_molecule_is_not_refused_for_a_roster_it_can_no_longer_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    fs::write(mol_dir.join("roster.md"), "# Roster\n\nRan last week.\n").expect("roster.md");
    fs::write(mol_dir.join("state.json"), "{\"status\":\"done\"}\n").expect("state.json");

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "a finished committee cannot grow a roster retroactively, and a refusal \
         nobody can act on is an outage; got:\n{stderr}"
    );
}

/// **R3-1 at the CLI boundary — the ghostseat probe.**
///
/// A seat naming an `[adapters.<name>]` section that DOES NOT EXIST resolved
/// from the bare name: `resolve_endpoint_tuple` found no entry, so
/// `provider_family` fell through to `family_from_name(adapter_name)` and
/// returned the seat's own label. Declared then equalled resolved by
/// construction and the A1 mismatch check could not fire.
///
/// The sharp part is not that the hole survived A1 — it is that after A1 the
/// gate DEPENDS on the forbidden axis. A1 makes name-as-family illegal, and a
/// dangling adapter name is precisely a seat whose family IS its name. So
/// `ghostseat`, with no section anywhere, declared family `ghostseat` and passed.
#[test]
fn reconcile_check_refuses_a_seat_naming_an_adapter_section_that_does_not_exist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ghostseat");
    write_roster(
        &mol_dir,
        vec![seat(
            // No `[adapters.ghostseat]` in CONFIG — the whole point.
            "ghostseat",
            "refuter",
            "ghostseat",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a seat whose `[adapters.…]` section does not exist self-attests its \
         family — the resolution has nothing to resolve against — and must be \
         REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ghostseat") && stderr.contains("no `[adapters.ghostseat]`"),
        "the refusal must name the seat and the section that is missing, so the \
         convener knows what to write; got:\n{stderr}"
    );
    // Pin WHY it is refused, so widening the gate to admit registry-only
    // adapters (`codex`, …) cannot quietly reopen this hole: a ghost is
    // refused because nothing can dispatch it, not merely because it has no
    // TOML section — `codex` has no section either and is legal.
    assert!(
        stderr.contains("cannot dispatch"),
        "a ghost is a name NOTHING answers to; the refusal must say so rather \
         than blame the missing section alone; got:\n{stderr}"
    );
}

/// **R3-1, the passability control.** A seat naming a section that DOES exist
/// is accepted. Without this, the refusal above is satisfied by a gate that
/// refuses every seat, which is an outage rather than a control.
#[test]
fn a_seat_naming_a_section_that_exists_is_accepted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "a seat sitting on a real `[adapters.…]` section must be accepted; \
         got:\n{stderr}"
    );
}

/// **F1 — the round-1 finding.** A seat on a **registry-only** adapter is
/// rosterable, and refusing it was the defect.
///
/// The gate used to ask *does this adapter have an `[adapters.<name>]`
/// section?*, which is the property NEXT TO the one that matters. `codex` (and
/// `claude`, `aider`, `opencode`) dispatch with no section at all — the section
/// is optional and only tunes launch mode — so the gate refused seats cosmon
/// really spawns. In a galaxy whose only non-generator family is reached
/// through `codex`, that meant the provider-diversity gate refused the sole
/// provider that would have supplied the diversity, and no jury could be seated
/// at all (measured 2026-07-28, converge-20260728-7161 round 1).
///
/// The remedy the old message prescribed was worse than the defect: `codex` has
/// no `base_url` and no `api_key_env`, so any section written to satisfy the
/// gate is a fiction nothing verifies against the real dispatch path — a fix
/// that abolished self-attestation resting on a self-attestation.
#[test]
fn a_seat_on_a_registry_only_adapter_with_no_section_is_rosterable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat_on_adapter(
            "ref_seat",
            // No `[adapters.codex]` in CONFIG — the whole point.
            "codex",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "`codex` is a built-in adapter cosmon dispatches and whose family it \
         resolves; refusing it refuses the only diversity many galaxies can \
         reach; got:\n{stderr}"
    );
}

/// **F1's acceptance test: can the gate still pass when the constrained party
/// lies?**
///
/// Admitting a registry-only adapter is only a fix if the family is derived
/// from what the registry knows about the binary it spawns, NEVER from the
/// seat's own label. Derived from the label, `declared == resolved` returns by
/// construction and the defect has moved rather than been fixed.
///
/// So a seat sitting on `codex` and calling itself `anthropic` must be
/// contradicted: `codex` names the OpenAI CLI, and that is a fact about the
/// binary, not a restatement of anything the roster wrote.
#[test]
fn a_registry_only_adapter_still_contradicts_a_seat_that_lies_about_its_family() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat_on_adapter(
            "ref_seat",
            "codex",
            "refuter",
            // The lie: `codex` resolves to openai, whatever the roster says.
            "anthropic",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a seat may not declare a family its adapter contradicts — otherwise \
         admitting registry-only adapters just moves the self-attestation"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("RESOLVES to") && stderr.contains("ref_seat"),
        "the refusal must name the seat and both tuples; got:\n{stderr}"
    );
}

/// **F1, the other edge: dispatchable is not the same as resolvable.**
///
/// `aider` IS in the built-in registry, so the ghost refusal does not apply to
/// it — and cosmon still knows nothing about which weights answer it, because
/// aider is provider-agnostic and will serve whatever it is pointed at. Its
/// tuple therefore falls through to the seat's own NAME, which is the
/// self-attestation this gate exists to refuse. Widening the gate to admit
/// registry-only adapters must not widen it to admit these.
#[test]
fn a_dispatchable_but_unresolvable_adapter_is_still_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    write_roster(
        &mol_dir,
        vec![seat_on_adapter(
            "ref_seat",
            // Built-in, so dispatchable — and no `[adapters.aider]` section,
            // so nothing on the record says which endpoint it reaches.
            "aider",
            "refuter",
            "aider",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a provider-agnostic adapter with no declared endpoint self-attests its \
         family and must be REFUSED"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("SELF-ATTESTATION") && stderr.contains("cannot RESOLVE"),
        "the refusal must distinguish `cosmon cannot resolve this` from `this \
         name does not exist`, since the fixes differ; got:\n{stderr}"
    );
}

/// **R3-2 at the CLI boundary.** Delivery claimed where it CANNOT have
/// occurred.
///
/// `with_observed_delivery`'s port returned `None` for a seat with no molecule
/// directory, and the seat was returned unchanged — so `injected: true`
/// survived untouched exactly where the two facts it names cannot exist. The
/// A3 fix read the files whenever there were files; the state with no files at
/// all was the one it left self-attested.
///
/// The adapter here is `ref_seat`, a section that really exists, so the R3-1
/// refusal cannot be what produces this one.
#[test]
fn reconcile_check_refuses_delivery_claimed_for_a_seat_with_no_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    // Deliberately NOT calling `deliver` — the seat has no directory at all.
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            // Claimed for a molecule that does not exist.
            true,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "`injected: true` for a seat with NO molecule directory must be REFUSED: \
         the two facts it asserts are about files that cannot exist"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ref_seat") && stderr.contains("has no molecule directory"),
        "the refusal must say the directory is absent, not merely that a file is \
         missing — they are different facts and only one is actionable by \
         nucleating; got:\n{stderr}"
    );
}

/// **R3-2, the other direction.** A seat with no directory that claims NOTHING
/// is a planned seat at convene, and must not be refused for the absence — the
/// ordinary `briefing-not-injected` line is the honest finding there.
#[test]
fn a_planned_seat_claiming_no_delivery_is_not_refused_for_its_absence() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            // Honest: nothing dispatched, nothing claimed.
            false,
            Some("falsification-attempt.md"),
        )],
    );

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("has no molecule directory"),
        "a seat claiming no delivery has made no claim to contradict; refusing it \
         for the absence would refuse every honest convene; got:\n{stderr}"
    );
    assert!(
        stderr.contains("briefing-not-injected"),
        "…and the undelivered contract is still the true finding; got:\n{stderr}"
    );
}

/// §8b, same contract as its two sibling lints: a plain `cs reconcile` reports
/// and never aborts. A lint must not wedge a surface projection.
#[test]
fn plain_reconcile_reports_but_does_not_abort() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(&mol_dir, vec![]);

    let out = cosmon_bin_isolated(&state_dir)
        .arg("reconcile")
        .output()
        .expect("spawn cs reconcile");
    assert!(
        out.status.success(),
        "plain cs reconcile must not abort on a roster violation: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("committee roster violation"),
        "…but it must still report it"
    );
}

/// **R3-6.** A project config the gate could not PARSE is a config it did not
/// CHECK.
///
/// `load_project_config(...).ok()` swallowed the parse error, and the lint then
/// ran on `[provider_bias]` defaults against an EMPTY adapter inventory — it
/// measured a floor nobody configured, against sections it could not see, and
/// said nothing. An unparseable `roster.json` had earned an explicit violation
/// since the lint was written; its own inputs were held to a quieter rule.
///
/// An ABSENT config is deliberately not this case: `load_project_config`
/// returns the default for a path that does not exist, so the only way into
/// this arm is a file that exists and is malformed.
#[test]
fn reconcile_check_refuses_a_project_config_it_cannot_parse() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            true,
            Some("falsification-attempt.md"),
        )],
    );
    fs::write(
        state_dir.parent().expect("cosmon dir").join("config.toml"),
        "[adapters.ref_seat\nthis is not toml",
    )
    .expect("broken config");

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a config the roster gate cannot parse must be REFUSED, never defaulted \
         away"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not be read as a project config"),
        "the refusal must name the config as the cause, not report the \
         consequences of having read nothing; got:\n{stderr}"
    );
}

/// The counterweight: a project with NO `config.toml` at all is not refused.
/// An absent config is the documented default, not a parse failure, and
/// refusing it would redden every project that never wrote one.
#[test]
fn a_project_with_no_config_is_not_refused_for_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    deliver(&mol_dir, "ref_seat");
    fs::remove_file(state_dir.parent().expect("cosmon dir").join("config.toml"))
        .expect("remove config");

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("could not be read as a project config"),
        "an absent config is the default, not a parse failure; got:\n{stderr}"
    );
}

/// **CARRIED (a).** The gate must not be opt-in by artefact presence.
///
/// Three artefacts could make a molecule "a committee": `roster.json`, the
/// prose `roster.md`, a seat's `committee-posture.md`. All three are things a
/// convener CHOOSES to write, so a convener who writes none was never
/// inspected, and the lint said so in its own doc comment — "the gate cannot
/// refuse what leaves no trace anywhere".
///
/// It does leave a trace. `cs nucleate` records `formula_id` in the molecule's
/// own `state.json` before any worker runs. Committee-hood is now RESOLVED from
/// that, not declared by an artefact: a live `cross-provider-committee` with an
/// empty directory is refused.
#[test]
fn reconcile_check_refuses_a_convener_that_wrote_no_artefact_at_all() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    // The molecule IS a committee convener and has written nothing: no
    // roster.json, no roster.md, no posture file.
    fs::write(
        mol_dir.join("state.json"),
        "{\"status\":\"running\",\"formula_id\":\"cross-provider-committee\"}\n",
    )
    .expect("state.json");

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a live committee convener that wrote no artefact must be REFUSED — \
         otherwise the gate is opt-in by the party it constrains"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("its formula is a committee convener"),
        "the refusal must say committee-hood was read from the FORMULA, so a \
         convener knows the artefact was never the trigger; got:\n{stderr}"
    );
}

/// CARRIED (a), the counterweight. An ordinary molecule's `formula_id` is not a
/// convener's, and it must stay invisible to this lint. Without this, the
/// refusal above is satisfied by a gate that refuses the whole fleet.
#[test]
fn a_non_committee_formula_is_not_inspected_as_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    fs::write(
        mol_dir.join("state.json"),
        "{\"status\":\"running\",\"formula_id\":\"task-work\"}\n",
    )
    .expect("state.json");

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("committee roster violation"),
        "an ordinary task must not be linted as a committee; got:\n{stderr}"
    );
}

/// **CARRIED (b).** A refusal nobody can act on is an outage, not a control.
///
/// "Terminality excuses the ABSENCE of a roster, never its CONTENTS" turned one
/// finished committee into a permanent red. Measured on the live tree
/// 2026-07-28: `committee-20260728-f744` is `completed`, its seat was never
/// tackled, and no action on any current work could return `cs reconcile
/// --check` to green — the seat cannot be tackled because the committee is
/// over.
///
/// Terminal contents are now reported as HISTORICAL and do not fail the gate.
#[test]
fn a_terminal_committees_violating_roster_is_reported_but_does_not_fail_the_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    // A roster that really does fail a witness: the refuter's contract was
    // never delivered.
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            false,
            Some("falsification-attempt.md"),
        )],
    );
    fs::write(mol_dir.join("state.json"), "{\"status\":\"completed\"}\n").expect("state.json");
    // The committee's SEATS are over too — a finished committee has no live
    // seat, and `deliver` above gave each of them the durable adversarial
    // contract, which is one of the doors that makes a seat owe a verdict. A
    // seat carrying that contract and emitting nothing is a finding in its own
    // right (the sibling polarity lint), and on a terminal seat it is reported
    // rather than refused, exactly as this roster is.
    for seat_id in ["gen_seat", "ref_seat"] {
        let seat_dir = mol_dir.parent().expect("molecules dir").join(seat_id);
        if seat_dir.is_dir() {
            fs::write(seat_dir.join("state.json"), "{\"status\":\"completed\"}\n")
                .expect("seat state.json");
        }
    }

    let out = reconcile_check(&state_dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a committee that already finished cannot be fixed by any current work, \
         so its roster must not hold the gate red forever; got:\n{stderr}"
    );
    assert!(
        stderr.contains("HISTORICAL") && stderr.contains("briefing-not-injected"),
        "…and it must still be REPORTED in full — silence would be the other \
         failure; got:\n{stderr}"
    );
}

/// CARRIED (b), the counterweight. The same roster on a LIVE molecule still
/// fails. Without this, the historical route is an exemption anyone reaches by
/// having a roster at all.
#[test]
fn the_same_violating_roster_on_live_work_still_fails_the_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state_dir, mol_dir) = setup_project(tmp.path());
    write_roster(
        &mol_dir,
        vec![seat(
            "ref_seat",
            "refuter",
            "openai",
            "adversary",
            false,
            Some("falsification-attempt.md"),
        )],
    );
    fs::write(mol_dir.join("state.json"), "{\"status\":\"running\"}\n").expect("state.json");

    let out = reconcile_check(&state_dir);
    assert!(
        !out.status.success(),
        "a live committee's failing roster must still REFUSE — terminality is \
         the only thing that moves a finding to historical"
    );
}
