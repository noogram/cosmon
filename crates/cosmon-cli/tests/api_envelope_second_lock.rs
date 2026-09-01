// SPDX-License-Identifier: AGPL-3.0-only

//! ADR-080 §3.5 — the **second lock**, against the real `cs` binary.
//!
//! §3.5 requires `cs` to detect `COSMON_API_REQUEST=1` and refuse the
//! operator-only verbs of §5.1 *at parse time*, as a second lock behind
//! the adapter's `OPERATOR_ONLY_VERBS` admission check. Before
//! `task-20260831-294e` that refusal did not exist anywhere in `crates/`:
//! the marker was read three times, always to suppress a `cb` probe or to
//! project the exposed egress posture, never to refuse. The announced
//! defence in depth was one layer, and a single mis-wired route traversed
//! all of it.
//!
//! The pure decision is unit-tested in `cosmon_core::api_envelope`. This
//! file pins the two things a unit test cannot see:
//!
//! 1. the refusal is reached **before any state mutation** — the process
//!    exits with the reserved code and the molecule is untouched;
//! 2. the two exposed §8p paths that spawn `cs` again as a *local*
//!    gesture (ADR-124's drain teardown, and a tackled worker) are not
//!    collaterally bricked by it.
//!
//! Point 2 is the falsifier that shaped the design: `cs done`,
//! `cs evolve` and `cs complete` are all on the closed list, and all
//! three are invoked, legitimately, downstream of an *exposed* verb.

use std::path::Path;
use std::process::Command;

use cosmon_core::api_envelope::{EXIT_OPERATOR_ONLY_VERB_IN_API, REQUEST_ENV, REQUEST_ID_ENV};

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove(REQUEST_ENV)
        .env_remove(REQUEST_ID_ENV);
    cmd
}

fn cs_in(state_dir: &Path) -> Command {
    let mut cmd = cs();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .current_dir(state_dir);
    cmd
}

/// A bare state dir is enough: the refusal must fire before `cs` resolves
/// a molecule, so there is nothing to nucleate.
fn state_dir(tmp: &Path) -> std::path::PathBuf {
    let dir = tmp.join(".cosmon/state");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn every_operator_only_verb_is_refused_under_the_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = state_dir(tmp.path());

    // One argv per verb that parses cleanly — the gate must fire on the
    // verb, not on a clap usage error, so each invocation is one that
    // would otherwise reach a handler. (An argv clap rejects exits 2
    // before the gate is consulted; that is still a refusal with no state
    // mutation, but it would make this assertion measure clap.)
    let invocations: &[&[&str]] = &[
        &["done", "task-20260101-aaaa"],
        &[
            "evolve",
            "task-20260101-aaaa",
            "--evidence",
            "e",
            "--formula",
            "f",
        ],
        &["complete", "task-20260101-aaaa"],
        &["security", "status"],
        &["kill", "task-20260101-aaaa"],
        &["purge"],
        &["reconcile"],
        &["verify"],
        &["whisper", "task-20260101-aaaa", "-m", "hi"],
        &["drop", "note"],
    ];

    for argv in invocations {
        let out = cs_in(&dir)
            .env(REQUEST_ENV, "1")
            .args(*argv)
            .output()
            .expect("cs spawn");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(EXIT_OPERATOR_ONLY_VERB_IN_API),
            "cs {argv:?} must exit with the reserved §3.5 code.\nstderr={stderr}"
        );
        assert!(
            stderr.contains("operator-only verb") && stderr.contains("ADR-080"),
            "the refusal must name the rule and cite the ADR.\nstderr={stderr}"
        );
    }
}

/// The refusal is *parse-time*: nothing on disk moves. Verified on the
/// event log, which every state-mutating verb writes to — an empty (or
/// absent) log after a refused `cs done` is the honest witness that the
/// gate sits ahead of the handler, not inside it.
#[test]
fn the_refusal_precedes_every_state_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = state_dir(tmp.path());
    let events = dir.join("events.jsonl");

    let out = cs_in(&dir)
        .env(REQUEST_ENV, "1")
        .args(["done", "task-20260101-aaaa"])
        .output()
        .expect("cs spawn");
    assert_eq!(out.status.code(), Some(EXIT_OPERATOR_ONLY_VERB_IN_API));

    let written = std::fs::read_to_string(&events).unwrap_or_default();
    assert!(
        written.is_empty(),
        "a refused verb must write nothing — found: {written}"
    );
}

/// `--json` callers get the refusal in the shape they parse. The RPP
/// envelope sets `--json` on every subprocess (§3.5), so a bare human
/// line here would reach a tenant as a parse error instead of a reason.
#[test]
fn the_refusal_is_json_when_json_was_asked_for() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = state_dir(tmp.path());

    let out = cs_in(&dir)
        .env(REQUEST_ENV, "1")
        .args(["--json", "done", "task-20260101-aaaa"])
        .output()
        .expect("cs spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(stderr.trim()).unwrap_or_else(|e| panic!("not JSON: {stderr} ({e})"));
    assert!(
        parsed["error"]
            .as_str()
            .is_some_and(|m| m.contains("operator-only verb")),
        "the JSON refusal must carry the reason: {parsed}"
    );
}

/// The exposed §8p surface is untouched. `run` is the load-bearing case:
/// it left the closed list through the §5.2 successor path (ADR-124), so
/// a second lock that refused it would silently un-ship a shipped route.
#[test]
fn the_exposed_surface_still_parses_under_the_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = state_dir(tmp.path());

    for argv in [
        vec!["observe", "task-20260101-aaaa"],
        vec!["ensemble"],
        vec!["run", "task-20260101-aaaa", "--help"],
        vec!["tackle", "--help"],
    ] {
        let out = cs_in(&dir)
            .env(REQUEST_ENV, "1")
            .args(&argv)
            .output()
            .expect("cs spawn");
        assert_ne!(
            out.status.code(),
            Some(EXIT_OPERATOR_ONLY_VERB_IN_API),
            "exposed verb {argv:?} must never hit the operator-only refusal.\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A local operator is unaffected — the whole closed list still parses
/// with no envelope in the environment. This is the regression that would
/// hurt most: the second lock keying on a broader condition than the
/// marker would brick `cs done` on every developer machine.
#[test]
fn no_envelope_no_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = state_dir(tmp.path());

    for value in [None, Some("0"), Some("")] {
        let mut cmd = cs_in(&dir);
        if let Some(v) = value {
            cmd.env(REQUEST_ENV, v);
        }
        let out = cmd
            .args(["done", "task-20260101-aaaa"])
            .output()
            .expect("cs spawn");
        assert_ne!(
            out.status.code(),
            Some(EXIT_OPERATOR_ONLY_VERB_IN_API),
            "COSMON_API_REQUEST={value:?} is not an envelope and must not refuse"
        );
    }
}

/// THE FALSIFIER (design-shaping). `POST /v1/molecules/{id}/run` is an
/// exposed verb (ADR-124) whose resident loop calls `cs done` to tear a
/// completed molecule down, and `POST .../tackle` spawns a worker whose
/// whole job is `cs evolve` / `cs complete`. All three verbs are on the
/// closed list, so a naively-inherited envelope would have made the
/// second lock break both shipped routes.
///
/// The hand-off ([`cosmon_core::api_envelope::hand_off_to_local_child`])
/// is what prevents that, and this pins its two halves at once: the
/// markers are gone from the child, and the egress posture is not.
#[test]
fn a_local_hand_off_child_escapes_the_lock_without_losing_its_confinement() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = state_dir(tmp.path());

    // `cs paths` is an ordinary read-only verb: it runs to completion, so
    // a non-refusal exit code here is a real "the child ran", not an
    // accident of a different early exit.
    let mut child = cs_in(&dir);
    child
        .env(REQUEST_ENV, "1")
        .env(REQUEST_ID_ENV, "req-falsifier")
        .env("COSMON_EGRESS_POLICY", "deny-external")
        .env("COSMON_EGRESS_EXPOSED", "1")
        .args(["done", "task-20260101-aaaa"]);

    // Without the hand-off the envelope is inherited and the lock fires…
    let before = child.output().expect("cs spawn");
    assert_eq!(
        before.status.code(),
        Some(EXIT_OPERATOR_ONLY_VERB_IN_API),
        "control: an inherited envelope must refuse, or this test measures nothing"
    );

    // …and with it, the same child runs its verb.
    cosmon_core::api_envelope::hand_off_to_local_child(&mut child);
    let after = child.output().expect("cs spawn");
    assert_ne!(
        after.status.code(),
        Some(EXIT_OPERATOR_ONLY_VERB_IN_API),
        "a local hand-off child must not be refused as if it were the request.\nstderr={}",
        String::from_utf8_lossy(&after.stderr)
    );

    // The confinement travels with it. Asserted on the command's own env
    // rather than on behaviour, because the posture's *effect* is
    // host-dependent (netns) while the hand-off's contract is not.
    let kept: Vec<String> = child
        .get_envs()
        .filter(|(_, v)| v.is_some())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for posture in ["COSMON_EGRESS_POLICY", "COSMON_EGRESS_EXPOSED"] {
        assert!(
            kept.contains(&posture.to_owned()),
            "the hand-off must never relax {posture}: {kept:?}"
        );
    }
}
