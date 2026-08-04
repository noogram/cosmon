// SPDX-License-Identifier: AGPL-3.0-only

//! A co-pilot without the lease is refused by the **mechanism**, not by its
//! brief — ADR-168 §D6, friction F9 of the M7 dogfood.
//!
//! # What the dogfood found
//!
//! `task-20260731-bd92` ran a real Codex co-pilot beside a Claude primary on a
//! real mission and recorded zero mutations by the co-pilot. Nine commands,
//! every one enumerated from the co-pilot's own event stream, HEAD identical
//! before and after. The number was true.
//!
//! It was also, as §8 F9 says in as many words, *tenu et vérifié a posteriori*
//! and not *garanti a priori*: the co-pilot abstained because a hand-written
//! brief told it to and because it obeyed. The lease guard existed and refused
//! five falsifiers — but all five went through `cs sessions takeover check`,
//! and not one of the nine gestures did. Nothing mechanical stood between that
//! co-pilot and `cs evolve`.
//!
//! # What this file pins
//!
//! End to end, through the real `cs` binary: on a mission whose PRIMARY lease
//! an operator has granted, a **different** session issuing a lifecycle verb
//! is refused with the typed exit code, and the molecule does not move. Delete
//! the `refuse_unleased_pilot_gesture` call from any of the five verbs and its
//! case here goes red — the verb runs and exits 0.
//!
//! Two counter-tests keep the first from being vacuous, and both assert
//! **success**, not merely "not exit 16" — a guard that refused everything
//! would satisfy the weaker form, and that is a worse bug than the one being
//! fixed:
//!
//! - **the holder flies** — a lifecycle verb from the lease-holding session,
//!   on the same leased mission, succeeds and moves the molecule;
//! - **an unleased mission is untouched** — a molecule nobody has granted
//!   behaves exactly as it did before this guard existed, which is every
//!   molecule on every fleet until an operator types `takeover grant`.
//!
//! That second one is the load-bearing one for the fleet. The guard is scoped
//! to co-piloted missions on purpose; if it fired on the absence of a lease it
//! would be a global kill-switch for `cs evolve`, which no ADR asks for.
//!
//! `cs collapse` is the verb the two counter-tests use, because it is the one
//! that both mutates and is legal on a `pending` molecule. `cs evolve` needs a
//! `running` one, which needs a dispatch, which needs a worker — and measuring
//! a worker is not what these two are for.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_minisign_testkit::Operator;

/// Exit code for `GuardError::UnleasedPilotGesture`, restated here as the
/// contract a script or an external scheduler branches on. A change to the
/// constant that is not a deliberate contract change fails this file.
const UNLEASED_PILOT_GESTURE: i32 = 16;

const PRIMARY: &str = "claude-primary-6158";
const COPILOT: &str = "codex-copilot-6158";

/// A `cs` invocation with the ambient cosmon session stripped, so the run is
/// hermetic: no inherited worker depth, no adapter hammer, and — the one that
/// matters here — no `COSMON_SESSION_ID` from the session running the suite,
/// which would otherwise decide who this test's caller is.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_DEFAULT_MODEL",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
        "ANTHROPIC_MODEL",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

/// The human this suite stands in for — the one who holds the secret key.
///
/// A fixed seed, so a failing run re-signs the same bytes.
fn operator() -> Operator {
    Operator::from_seed(9)
}

/// A galaxy with one formula whose steps need nothing but a state directory,
/// and an operator trust root pinned where a real galaxy commits it.
///
/// The pin is not decoration. Since ADR-171 a grant is honoured only if its
/// signature checks against a pinned key, so a galaxy without one is a galaxy
/// where `takeover grant` writes nothing and every mission below would read
/// back as unleased — which is precisely the state in which this file's
/// falsifier passes while proving nothing.
fn galaxy(tmp: &Path) -> PathBuf {
    let root = tmp.join("galaxy");
    fs::create_dir_all(&root).unwrap();
    assert!(cs(&root).arg("init").status().unwrap().success());
    fs::write(
        root.join(".cosmon").join("takeover.pub"),
        operator().public_key_file(),
    )
    .unwrap();

    let formulas = root.join(".cosmon").join("formulas");
    fs::create_dir_all(&formulas).unwrap();
    fs::write(
        formulas.join("flight.formula.toml"),
        r#"formula = "flight"
version = 1
description = "A mission with two steps, so `cs evolve` has somewhere to go."
id_prefix = "task"

[vars.topic]
description = "The task."
required = true

[[steps]]
id = "one"
title = "One"
description = "The first leg."

[[steps]]
id = "two"
title = "Two"
description = "The second leg."
"#,
    )
    .unwrap();
    root
}

/// Nucleate a `flight` molecule and return its id.
fn nucleate(root: &Path) -> String {
    let out = cs(root)
        .args([
            "--json",
            "nucleate",
            "flight",
            "--kind",
            "task",
            "--var",
            "topic=fly the aeroplane",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("`cs nucleate --json` must emit json")
        .get("id")
        .and_then(|v| v.as_str())
        .expect("`cs nucleate --json` must carry the molecule id")
        .to_owned()
}

/// The operator seats `PRIMARY` on `mission`, and `PRIMARY` publishes the
/// presence snapshot that carries its claim — the two halves of a lease, as
/// the M7 dogfood performed them at 14:29:27 and 14:29:37.
fn seat_primary(root: &Path, mission: &str) {
    grant(root, mission, PRIMARY);

    let seated = cs(root)
        .args([
            "presence",
            "ping",
            "--session",
            PRIMARY,
            "--role",
            "primary",
            "--mission",
            mission,
            "--epoch",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        seated.status.success(),
        "the primary seat must be writable once the lease is granted: {}",
        String::from_utf8_lossy(&seated.stderr),
    );
}

/// `cs evolve`'s two required flags, spelled once.
///
/// They are required by clap, so an invocation missing them exits 2 at parse
/// time and never reaches the guard — a refusal that would be indistinguishable
/// from the one under test and would make every assertion here vacuous.
fn evolve_verb() -> Vec<&'static str> {
    vec![
        "evolve",
        "--evidence",
        "the leg is flown",
        "--formula",
        ".cosmon/formulas/flight.formula.toml",
    ]
}

/// Run one lifecycle verb against `mission` as `session`.
fn gesture(root: &Path, session: &str, mission: &str, verb: &[&str]) -> std::process::Output {
    let mut cmd = cs(root);
    cmd.env("COSMON_SESSION_ID", session);
    cmd.args(verb);
    cmd.arg(mission);
    // Every verb below needs one more argument or none; the caller passes the
    // flags inside `verb` and the molecule id lands last, which is where each
    // of these five takes it.
    cmd.output().unwrap()
}

/// Read a molecule's status, without going through a verb that might mutate.
fn status(root: &Path, mission: &str) -> String {
    let out = cs(root)
        .args(["--json", "observe", mission])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "observe failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("observe json");
    v.get("status")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned()
}

// ---------------------------------------------------------------------------
// The falsifier: a co-pilot that TRIES to mutate must be refused by the guard.
// ---------------------------------------------------------------------------

#[test]
fn a_copilot_without_the_lease_is_refused_by_the_mechanism() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mission = nucleate(&root);
    seat_primary(&root, &mission);

    let before = status(&root, &mission);

    // The four verbs that move a mission, plus dispatch. Each is attempted by
    // a session that holds nothing — exactly the co-pilot of the dogfood, with
    // the brief removed.
    for verb in [
        evolve_verb(),
        vec!["complete", "--reason", "seizing the controls"],
        vec!["collapse", "--reason", "seizing the controls"],
        vec!["done"],
        vec!["tackle", "--dry-run"],
    ] {
        let out = gesture(&root, COPILOT, &mission, &verb);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        assert_eq!(
            out.status.code(),
            Some(UNLEASED_PILOT_GESTURE),
            "`cs {}` from a session with no lease must be refused by the guard \
             with the typed exit code.\nstderr: {stderr}",
            verb.join(" "),
        );
        // The refusal must be THIS one. Another gate that also exits non-zero
        // would make the assertion above true and meaningless.
        assert!(
            stderr.contains("under co-pilotage") && stderr.contains(PRIMARY),
            "the refusal must name the co-pilotage and the holder, so the \
             operator knows the next move is a grant.\nstderr: {stderr}",
        );
    }

    assert_eq!(
        status(&root, &mission),
        before,
        "a refused gesture is refused BEFORE it takes effect (ADR-168 §D6, \
         third bullet) — the mission must not have moved",
    );
}

// ---------------------------------------------------------------------------
// Counter-tests: without these, the one above is satisfied by a guard that
// refuses everything, which would be a worse bug than the one it fixes.
// ---------------------------------------------------------------------------

#[test]
fn the_lease_holder_still_flies_the_mission() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mission = nucleate(&root);
    seat_primary(&root, &mission);

    let out = gesture(&root, PRIMARY, &mission, &["collapse", "--reason", "flown"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // `success`, not merely "not exit 16". A weaker assertion would still hold
    // if the guard refused every gesture and something else exited non-zero
    // first, which is the failure mode this counter-test exists to catch.
    assert!(
        out.status.success(),
        "the session holding the lease, presenting the epoch its own seat \
         records, must fly the mission.\nstderr: {stderr}",
    );
    assert_eq!(
        status(&root, &mission),
        "collapsed",
        "and the mission must actually have moved",
    );
}

#[test]
fn a_mission_nobody_has_leased_is_untouched_by_the_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mission = nucleate(&root);
    // No grant, no seat: the state of every molecule on every fleet today.

    let out = gesture(&root, COPILOT, &mission, &["collapse", "--reason", "flown"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        out.status.success(),
        "the guard is scoped to co-piloted missions. Firing on the absence of \
         a lease would make the lease ledger a global kill-switch for \
         `cs evolve`, which ADR-168 does not ask for and the M4 note declines \
         in as many words.\nstderr: {stderr}",
    );
    assert_eq!(status(&root, &mission), "collapsed");
}

/// Grant the mission's controls to `to` without going through a request —
/// the operator's three steps: read the challenge, sign it out of band, record
/// the grant with the signature attached (ADR-171 §b).
fn grant(root: &Path, mission: &str, to: &str) {
    let challenge = cs(root)
        .args([
            "sessions",
            "takeover",
            "challenge",
            "--mission",
            mission,
            "--to",
            to,
            "--by",
            "operator",
        ])
        .output()
        .unwrap();
    assert!(
        challenge.status.success(),
        "takeover challenge for {to} failed: {}",
        String::from_utf8_lossy(&challenge.stderr),
    );

    // The signature is produced outside `cs` on purpose: the shipped tree owns
    // no signing path, which is the whole mechanism of ADR-171.
    let sig = root.join(format!("grant-{to}.minisig"));
    fs::write(&sig, operator().sign(&challenge.stdout)).unwrap();

    let out = cs(root)
        .args([
            "sessions",
            "takeover",
            "grant",
            "--mission",
            mission,
            "--to",
            to,
            "--by",
            "operator",
            "--attestation",
        ])
        .arg(&sig)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "takeover grant to {to} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn a_pilot_whose_lease_was_transferred_away_loses_the_gesture() {
    // The former primary keeps a live session and a seat that still says
    // `primary`. What it does not keep is the lease, and nobody had to tell it.
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mission = nucleate(&root);
    seat_primary(&root, &mission);
    grant(&root, &mission, COPILOT);

    let out = gesture(&root, PRIMARY, &mission, &evolve_verb());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(
        out.status.code(),
        Some(UNLEASED_PILOT_GESTURE),
        "the transfer must take the gesture away from the former holder.\n\
         stderr: {stderr}",
    );
    assert!(
        stderr.contains(&format!("the lease is held by {COPILOT}")),
        "the refusal must name who holds the controls now, so the operator \
         reads a next move and not a dead end.\nstderr: {stderr}",
    );
}

#[test]
fn a_stale_epoch_is_refused_even_when_the_seat_came_back() {
    // ADR-168 falsifier 3 — "a pilot mutates after its epoch has been
    // superseded" — in its sharpest form: the caller *is* the current holder,
    // so identity alone would let it through. What refuses it is the epoch its
    // own snapshot records, which is the generation before the round trip.
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mission = nucleate(&root);
    seat_primary(&root, &mission);

    // Controls away and back. The ledger is at epoch 3; `PRIMARY`'s seat was
    // written at epoch 1 and no ping has refreshed it.
    grant(&root, &mission, COPILOT);
    grant(&root, &mission, PRIMARY);

    let out = gesture(&root, PRIMARY, &mission, &evolve_verb());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(
        out.status.code(),
        Some(UNLEASED_PILOT_GESTURE),
        "holding the lease is not enough — D6 says hold it *and say so*, and \
         what this seat says is a generation that has been superseded.\n\
         stderr: {stderr}",
    );
    assert!(
        stderr.contains("stale epoch 1") && stderr.contains("epoch 3"),
        "the refusal must say which generation the caller believed and which \
         one is true — that is what makes a stale primary diagnosable rather \
         than merely wrong.\nstderr: {stderr}",
    );
}

// ---------------------------------------------------------------------------
// The drift that made all of the above vacuous, pinned at the source level.
// ---------------------------------------------------------------------------

/// Production code must build the lease ledger through `presence::leases_at`,
/// never through `PilotLeaseStore::new` directly.
///
/// This is not style. A store built without a pinned trust root honours **no**
/// grant, so every leased mission reads back as a mission nobody ever granted
/// — and a guard that asks "is this mission leased?" answers "no" and waves
/// the gesture through. That is exactly what happened when the F1 (signature)
/// and F9 (guard) branches were developed in parallel and merged: both were
/// green on their own branch, and on merged `main` an unleased co-pilot could
/// collapse a leased mission while `cs sessions takeover check`, reading the
/// same ledger through the trusting constructor, refused the very same
/// session. The M8 relève exercise found it by trying.
///
/// The behavioural tests above now catch the regression, and this one catches
/// the *shape* of it — including at a call site that no test happens to cover
/// yet.
#[test]
fn the_lease_ledger_is_only_ever_built_with_its_trust_root() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir");
    let mut offenders = Vec::new();
    walk(crates_dir, &mut |path| {
        if path.extension().is_none_or(|e| e != "rs") {
            return;
        }
        let as_str = path.display().to_string();
        // Tests may build a bare store to assert what an unpinned one does,
        // and the filestore crate is where the constructor itself lives.
        if as_str.contains("/tests/") || as_str.contains("cosmon-filestore") {
            return;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            return;
        };
        // A `#[cfg(test)]` module may seed a ledger with a bare store: that is
        // a fixture, not an authority check. Only the shipped half is scanned.
        let shipped = body.split("#[cfg(test)]").next().unwrap_or_default();
        for (n, line) in shipped.lines().enumerate() {
            // Prose about the constructor is not a call to it — this very rule
            // is explained in a comment at one of the sites it governs.
            if line.trim_start().starts_with("//") {
                continue;
            }
            // The one legitimate call site is the trusting constructor, which
            // pins the key on the very next lines.
            if line.contains("PilotLeaseStore::new") && !as_str.ends_with("presence.rs") {
                offenders.push(format!("{as_str}:{}", n + 1));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "the lease ledger must be built by `presence::leases_at`, which pins \
         the operator trust root; a bare `PilotLeaseStore::new` honours no \
         grant and silently turns every authority check into a pass. \
         Offending call sites: {offenders:?}",
    );
}

/// Recursive walk, as `takeover_unforgeable` does it.
fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&path, f);
        } else {
            f(&path);
        }
    }
}
