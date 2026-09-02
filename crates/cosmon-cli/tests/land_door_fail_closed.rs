// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest door refuses by default, and stays on the closed list's
//! right side — against the real `cs` binary (ADR-176, issue #51).
//!
//! # What is being falsified
//!
//! ADR-176 D1 makes the harvest authority an **operator-sealed capability**:
//! the JWT authenticates the requester, the seal authorises the effect. If
//! `cs land` did anything at all in a galaxy that never armed
//! `[harvest_authority] required`, the door would be a bearer token spending
//! an authority nobody issued — the exact shape D1 rejects.
//!
//! A unit test cannot see this. The property is about the *shipped binary's*
//! behaviour on a galaxy whose config says nothing, which is the state every
//! galaxy is in until an operator acts.
//!
//! The second claim here is about the closed list. `cs done` stays forbidden
//! under the §8p envelope, and the door must not have become a way around
//! that: `land` and `done` are different gestures with different argument
//! sets, and the audit trail has to keep them apart.

use std::path::Path;
use std::process::Command;

use cosmon_core::api_envelope::{REQUEST_ENV, REQUEST_ID_ENV};
use cosmon_core::harvest_door::DoorRefusal;

fn cs_in(state_dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove(REQUEST_ENV)
        .env_remove(REQUEST_ID_ENV)
        .env("COSMON_STATE_DIR", state_dir)
        .current_dir(state_dir);
    cmd
}

/// A galaxy that has armed nothing — which is every galaxy on day one.
fn unarmed_galaxy(tmp: &Path) -> std::path::PathBuf {
    let dir = tmp.join(".cosmon/state");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The default answer is no.
///
/// Not "no because the molecule is missing" — the refusal must fire on the
/// missing *authority*, before the door has looked at any state, or the
/// property would hold only for molecules that happen not to exist.
#[test]
fn an_unarmed_galaxy_refuses_every_harvest_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = unarmed_galaxy(tmp.path());

    let out = cs_in(&dir)
        .args(["land", "task-20260101-aaaa"])
        .output()
        .expect("cs land");

    assert_eq!(
        out.status.code(),
        Some(DoorRefusal::NotAuthorized.exit_code()),
        "an unarmed galaxy must refuse `not_authorized`, stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not_authorized"),
        "the refusal must name itself: {stderr}",
    );
    assert!(
        stderr.contains("harvest_authority"),
        "the refusal must name the gesture that lifts it: {stderr}",
    );
}

/// The door exposes no parameter — asserted on the shipped clap tree, which
/// is the only place a flag can actually appear.
///
/// `no_option_crosses_the_wire` pins the adapter's argv; this pins the other
/// end. Both are needed: a flag added to `cs land` would be invisible to the
/// adapter test, and an argv change would be invisible to clap.
#[test]
fn the_door_accepts_no_flag_at_all() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = unarmed_galaxy(tmp.path());

    for flag in [
        "--force",
        "--strategy",
        "--skip-pre-done-hook",
        "--no-branch-delete",
        "--propel-message",
        "--no-merge",
    ] {
        let out = cs_in(&dir)
            .args(["land", "task-20260101-aaaa", flag])
            .output()
            .expect("cs land");
        assert_eq!(
            out.status.code(),
            Some(2),
            "`{flag}` must be a clap usage error, not a parameter (ADR-176 D4)",
        );
    }
}

/// `cs land` is not a rename of `cs done`, and the help text says so.
///
/// The risk this guards is a reviewer — or a future ADR — reading the door
/// as "done, exposed". It is not: the door varies nothing and refuses
/// without a seal, and `cs done` keeps every flag and stays off the wire.
#[test]
fn the_help_distinguishes_the_door_from_cs_done() {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args(["land", "--help"])
        .output()
        .expect("cs land --help");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("harvest_authority"),
        "the help must name the arming gesture: {text}",
    );
    assert!(
        !text.contains("--strategy") && !text.contains("--force"),
        "the door's help must advertise no derogation: {text}",
    );
}

// ---------------------------------------------------------------------------
// An armed galaxy — the three refusals a request can reach before git
// ---------------------------------------------------------------------------

/// Build a galaxy that HAS armed the mechanism, with a backlog ceiling of
/// one so the bounded queue is reachable in a test rather than only in a
/// galaxy that has already accumulated eight stranded branches.
fn armed_galaxy(tmp: &Path) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(&formulas_dir).unwrap();
    std::fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"land-door-test\"\n\n\
         [harvest_authority]\nrequired = true\nmax_unintegrated = 1\n",
    )
    .unwrap();
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    std::fs::copy(&src, formulas_dir.join("task-work.formula.toml")).unwrap();
    std::fs::write(state_dir.join("fleet.json"), "{}\n").unwrap();
    state_dir
}

fn cs_armed(state_dir: &Path) -> Command {
    let mut cmd = cs_in(state_dir);
    cmd.env(
        "COSMON_CONFIG",
        state_dir
            .parent()
            .expect("state under .cosmon")
            .join("config.toml"),
    );
    cmd
}

/// Nucleate one molecule and return its id.
fn nucleate(state_dir: &Path, topic: &str) -> String {
    let out = cs_armed(state_dir)
        .args(["--json", "nucleate", "task-work", "--var"])
        .arg(format!("topic={topic}"))
        .arg("--no-parent")
        .output()
        .expect("cs nucleate");
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["id"].as_str().expect("id").to_owned()
}

fn land(state_dir: &Path, id: &str) -> std::process::Output {
    cs_armed(state_dir)
        .args(["land", id])
        .output()
        .expect("cs land")
}

/// Work still in flight is not landable, and the door says which fact
/// stopped it.
///
/// The value of the assertion is the *specific* code: before this verb, a
/// tenant asking for a harvest of a pending molecule got `cs done
/// --if-completed`'s silent success, which is the shape of issue #51.
#[test]
fn a_molecule_still_in_flight_refuses_not_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = armed_galaxy(tmp.path());
    let id = nucleate(&dir, "still in flight");

    let out = land(&dir, &id);
    assert_eq!(
        out.status.code(),
        Some(DoorRefusal::NotCompleted.exit_code()),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("status is pending"));
}

/// A reservation the door cannot lift stops it, and the refusal names the
/// tag rather than merely saying "reserved".
///
/// `security` is a *protected runtime reservation* — `cs tag --remove`
/// refuses to lift it before terminal teardown — so this is not a label the
/// requester can take off before asking again.
#[test]
fn a_reserved_molecule_refuses_and_names_its_reservation() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = armed_galaxy(tmp.path());
    let id = nucleate(&dir, "reserved");

    assert!(cs_armed(&dir)
        .args(["tag", &id, "--add", "security"])
        .output()
        .expect("cs tag")
        .status
        .success());
    assert!(cs_armed(&dir)
        .args(["complete", &id, "--reason", "work done"])
        .output()
        .expect("cs complete")
        .status
        .success());

    let out = land(&dir, &id);
    assert_eq!(
        out.status.code(),
        Some(DoorRefusal::ReservationRequiresSeal.exit_code()),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("reserved by `security`"),
        "the refusal must name the tag: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The queue is bounded, and reaching the bound is a named refusal.
///
/// ADR-110 I4 forbids the silent block; ADR-176 D7 answers it with "a
/// bounded, refusing queue is a delayed refusal, not a stall". This test is
/// what makes that sentence true rather than aspirational: one stranded
/// molecule against a ceiling of one, and the next request is told so.
#[test]
fn a_full_backlog_refuses_the_next_request_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = armed_galaxy(tmp.path());

    // One molecule closed and stranded — a `pre_done` verdict that never
    // came. Written directly because the only in-repo path that produces
    // this record is a real merge against a real trunk.
    let stranded = nucleate(&dir, "stranded");
    assert!(cs_armed(&dir)
        .args(["complete", &stranded, "--reason", "work done"])
        .output()
        .expect("cs complete")
        .status
        .success());
    let path = dir
        .join("fleets/default/molecules")
        .join(&stranded)
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["non_integration"] = serde_json::json!({
        "reason": "pre-done-refused",
        "at": "2026-09-01T00:00:00Z",
        "base_branch": "main",
    });
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    // A second, perfectly healthy molecule is now refused — the debt is a
    // property of the kernel, not of the molecule being asked about.
    let asked = nucleate(&dir, "asked");
    assert!(cs_armed(&dir)
        .args(["complete", &asked, "--reason", "work done"])
        .output()
        .expect("cs complete")
        .status
        .success());

    let out = land(&dir, &asked);
    assert_eq!(
        out.status.code(),
        Some(DoorRefusal::BacklogFull.exit_code()),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ceiling 1"),
        "the refusal must show the bound it hit: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}
