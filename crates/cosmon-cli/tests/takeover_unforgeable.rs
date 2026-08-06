// SPDX-License-Identifier: AGPL-3.0-only

//! The operator gesture is unforgeable by its beneficiary — M7 friction F1.
//!
//! # What this file is falsifying
//!
//! The M7 dogfood (`task-20260731-bd92` §8) found that
//! `cs sessions takeover grant --to X --by emmanuel` took `--by` as a free
//! string, and that in the dogfood itself the agent that typed the grant was
//! the agent the grant seated. TAKEOVER-SUPERVISED therefore rested on a
//! convention, and a supervision falsifiable by its beneficiary is not one.
//!
//! So the claim under test is not "the happy path works". It is:
//!
//! > **An agent holding everything the beneficiary holds — the state
//! > directory, the ledger file, the `cs` binary, the pinned public key, a
//! > shell — cannot produce authority for itself.**
//!
//! Every test below is a move that agent can actually make, and each asserts a
//! refusal. `an_operator_who_signs_seats_the_pilot` is the control that keeps
//! the rest honest: without it, "refuse everything" would pass this file.
//!
//! # Why these go red if the fix is retired
//!
//! [`a_hand_written_ledger_line_seats_nobody`] does not go through `cs` at
//! all — it appends JSON to the grants file, which is what an agent would
//! actually do once the CLI refuses it. Only the **read-time** check in
//! `PilotLeaseStore::grants` refuses that line. Remove it and this test seats
//! the agent, which is exactly the failure it exists to catch. The others
//! cover the write-time and canonical-bytes halves.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cosmon_minisign_testkit::Operator;

const MISSION: &str = "task-20260731-e4d0";
const BENEFICIARY: &str = "agent-that-wants-the-seat";

/// A galaxy with a pinned operator key, and an agent living inside it.
struct World {
    _tmp: tempfile::TempDir,
    state: PathBuf,
    repo: PathBuf,
    pubkey: PathBuf,
    operator: Operator,
}

fn world() -> World {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&state).expect("state");
    std::fs::create_dir_all(repo.join(".git")).expect("repo");

    let operator = Operator::from_seed(7);
    let pubkey = tmp.path().join("takeover.pub");
    // The trust root is readable by the agent, on purpose. Reading it confers
    // nothing; that asymmetry is the whole mechanism.
    std::fs::write(&pubkey, operator.public_key_file()).expect("pin the operator key");

    World {
        _tmp: tmp,
        state,
        repo,
        pubkey,
        operator,
    }
}

impl World {
    /// `cs --config <state> sessions …`, run with exactly the environment the
    /// beneficiary would have.
    fn cs(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(&self.repo)
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env("COSMON_TAKEOVER_PUBKEY", &self.pubkey)
            .arg("--config")
            .arg(&self.state)
            .arg("sessions")
            .args(args);
        cmd.output().expect("spawn cs")
    }

    /// The bytes the operator would have to sign for this transfer.
    fn challenge(&self, holder: &str, by: &str) -> String {
        let out = self.cs(&[
            "takeover",
            "challenge",
            "--mission",
            MISSION,
            "--to",
            holder,
            "--by",
            by,
        ]);
        assert!(
            out.status.success(),
            "challenge failed:\n{}\n{}",
            text(&out.stdout),
            text(&out.stderr),
        );
        text(&out.stdout)
    }

    /// Write a signature file and return its path.
    fn write_sig(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.repo.join(name);
        std::fs::write(&path, contents).expect("write signature");
        path
    }

    fn grants_path(&self) -> PathBuf {
        self.state
            .join("pilot-lease")
            .join(format!("{MISSION}.grants.jsonl"))
    }

    /// Does the ledger seat `holder` at `epoch`, as the guard sees it?
    fn seats(&self, holder: &str, epoch: &str) -> bool {
        self.cs(&[
            "takeover",
            "check",
            "--mission",
            MISSION,
            "--session",
            holder,
            "--epoch",
            epoch,
        ])
        .status
        .success()
    }
}

fn text(raw: &[u8]) -> String {
    String::from_utf8(raw.to_vec()).expect("utf8")
}

fn grant(w: &World, holder: &str, by: &str, sig: &Path) -> Output {
    w.cs(&[
        "takeover",
        "grant",
        "--mission",
        MISSION,
        "--to",
        holder,
        "--by",
        by,
        "--attestation",
        &sig.display().to_string(),
    ])
}

// ── The control: the mechanism is not "refuse everything" ──────────────────

#[test]
fn an_operator_who_signs_seats_the_pilot() {
    let w = world();
    let challenge = w.challenge("claude-primary", "emmanuel");
    let sig = w.write_sig("ok.minisig", &w.operator.sign(challenge.as_bytes()));

    let out = grant(&w, "claude-primary", "emmanuel", &sig);
    assert!(
        out.status.success(),
        "a signed grant must be honoured:\n{}\n{}",
        text(&out.stdout),
        text(&out.stderr),
    );
    assert!(w.seats("claude-primary", "1"));
}

// ── The falsifiers ─────────────────────────────────────────────────────────

/// The exact gesture of the M7 dogfood: the agent types the grant that seats
/// itself, naming a human in `--by`.
#[test]
fn the_beneficiary_cannot_grant_itself_the_seat_by_naming_a_human() {
    let w = world();
    let out = w.cs(&[
        "takeover",
        "grant",
        "--mission",
        MISSION,
        "--to",
        BENEFICIARY,
        "--by",
        "emmanuel",
    ]);
    assert!(!out.status.success(), "an unsigned grant must be refused");
    let why = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(
        why.contains("attestation"),
        "the refusal must name what is missing:\n{why}"
    );
    assert!(!w.seats(BENEFICIARY, "1"));
    assert!(
        !w.grants_path().exists(),
        "a refused grant must leave no line behind"
    );
}

/// The move the agent makes *after* the CLI refuses it: skip the CLI. This is
/// the test that pins the read-time check specifically — nothing else refuses
/// a line somebody echoed into the ledger.
#[test]
fn a_hand_written_ledger_line_seats_nobody() {
    let w = world();
    std::fs::create_dir_all(w.grants_path().parent().expect("parent")).expect("mkdir");
    let forged = serde_json::json!({
        "mission_id": MISSION,
        "holder_session_id": BENEFICIARY,
        "epoch": 1,
        "granted_by": "emmanuel",
        "granted_at": "2026-08-04T10:00:00Z",
    });
    std::fs::write(w.grants_path(), format!("{forged}\n")).expect("forge a grant");

    assert!(
        !w.seats(BENEFICIARY, "1"),
        "a ledger line no operator signed must confer nothing"
    );
    let shown = text(&w.cs(&["takeover", "show", "--mission", MISSION]).stdout);
    assert!(
        shown.contains("nobody holds the controls"),
        "the forged line must not read as a lease:\n{shown}"
    );
}

/// …and it must still be *visible*. A forgery that disappears from every view
/// is a forgery nobody investigates.
#[test]
fn the_forged_line_is_reported_by_an_audit_rather_than_hidden() {
    let w = world();
    std::fs::create_dir_all(w.grants_path().parent().expect("parent")).expect("mkdir");
    let forged = serde_json::json!({
        "mission_id": MISSION,
        "holder_session_id": BENEFICIARY,
        "epoch": 1,
        "granted_by": "emmanuel",
        "granted_at": "2026-08-04T10:00:00Z",
    });
    std::fs::write(w.grants_path(), format!("{forged}\n")).expect("forge a grant");

    let history = text(
        &w.cs(&["takeover", "show", "--mission", MISSION, "--history"])
            .stdout,
    );
    assert!(
        history.contains("NOT AN OPERATOR GESTURE"),
        "the audit must name the refused line:\n{history}"
    );
}

/// The agent has a keypair of its own — generating one costs nothing. It is
/// not the pinned one.
#[test]
fn a_grant_signed_by_the_agents_own_key_is_refused() {
    let w = world();
    let impostor = Operator::from_seed(200);
    let challenge = w.challenge(BENEFICIARY, "emmanuel");
    let sig = w.write_sig("mine.minisig", &impostor.sign(challenge.as_bytes()));

    let out = grant(&w, BENEFICIARY, "emmanuel", &sig);
    assert!(!out.status.success(), "a foreign key must not authorise");
    assert!(!w.seats(BENEFICIARY, "1"));
}

/// Capture-and-replay: the agent lifts the operator's real signature off the
/// epoch-1 transfer and presents it for the next one. The epoch is inside the
/// signed bytes, so it does not fit.
#[test]
fn an_operator_signature_cannot_be_replayed_onto_the_next_epoch() {
    let w = world();
    let challenge = w.challenge("claude-primary", "emmanuel");
    let sig = w.write_sig("epoch1.minisig", &w.operator.sign(challenge.as_bytes()));
    assert!(grant(&w, "claude-primary", "emmanuel", &sig)
        .status
        .success());

    // Same signature, now presented for epoch 2 and for a different holder.
    let out = grant(&w, BENEFICIARY, "emmanuel", &sig);
    assert!(
        !out.status.success(),
        "a replayed signature must be refused"
    );
    assert!(!w.seats(BENEFICIARY, "2"));
    assert!(
        w.seats("claude-primary", "1"),
        "and the honest lease must be untouched"
    );
}

/// A signature the operator produced for one pilot must not seat another.
#[test]
fn a_signature_for_one_pilot_does_not_seat_a_different_one() {
    let w = world();
    let challenge = w.challenge("claude-primary", "emmanuel");
    let sig = w.write_sig("for-claude.minisig", &w.operator.sign(challenge.as_bytes()));

    let out = grant(&w, BENEFICIARY, "emmanuel", &sig);
    assert!(
        !out.status.success(),
        "the holder is inside the signed bytes"
    );
    assert!(!w.seats(BENEFICIARY, "1"));
}

/// The operator signed a grant naming themselves. The agent presents the same
/// signature under a different `--by`, hoping the label is decoration.
#[test]
fn the_operator_name_is_covered_by_the_signature() {
    let w = world();
    let challenge = w.challenge("claude-primary", "emmanuel");
    let sig = w.write_sig(
        "by-emmanuel.minisig",
        &w.operator.sign(challenge.as_bytes()),
    );

    let out = grant(&w, "claude-primary", "somebody-else", &sig);
    assert!(
        !out.status.success(),
        "--by is inside the challenge, not beside it"
    );
}

/// Editing the *trusted comment* of a valid signature must invalidate it —
/// otherwise the only place minisign records provenance would be forgeable.
#[test]
fn editing_the_signed_comment_invalidates_the_attestation() {
    let w = world();
    let challenge = w.challenge("claude-primary", "emmanuel");
    let honest = w.operator.sign(challenge.as_bytes());
    let tampered = honest.replace(
        "trusted comment: signed by the operator, in this test",
        "trusted comment: signed by somebody else entirely",
    );
    assert_ne!(honest, tampered, "the fixture comment must be present");
    let sig = w.write_sig("tampered.minisig", &tampered);

    assert!(!grant(&w, "claude-primary", "emmanuel", &sig)
        .status
        .success());
}

/// Deleting the trust root must stop transfers, not unlock them. If absence
/// meant "unverified", the bypass would be one `rm`.
#[test]
fn removing_the_pinned_key_refuses_grants_rather_than_waving_them_through() {
    let w = world();
    let challenge = w.challenge("claude-primary", "emmanuel");
    let sig = w.write_sig("ok.minisig", &w.operator.sign(challenge.as_bytes()));
    assert!(grant(&w, "claude-primary", "emmanuel", &sig)
        .status
        .success());

    std::fs::remove_file(&w.pubkey).expect("the agent deletes the trust root");
    // `--config` still points at the same state; only the key is gone.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(&w.repo)
        .env_remove("COSMON_TAKEOVER_PUBKEY")
        .arg("--config")
        .arg(&w.state)
        .args([
            "sessions",
            "takeover",
            "check",
            "--mission",
            MISSION,
            "--session",
            "claude-primary",
            "--epoch",
            "1",
        ]);
    let out = cmd.output().expect("spawn cs");
    assert!(
        !out.status.success(),
        "with no trust root nothing is authorised:\n{}",
        text(&out.stdout)
    );
}

/// `takeover trust` must say which key is in force, so an operator can compare
/// it by eye with what `minisign -G` printed.
#[test]
fn the_pinned_key_is_reportable() {
    let w = world();
    let shown = text(&w.cs(&["takeover", "trust"]).stdout);
    assert!(
        shown.contains(&w.operator.key_id_display()),
        "trust must name the key it trusts:\n{shown}"
    );
}

/// The challenge is a description, not a capability: printing it must not
/// change any authority. An agent may read it all day.
#[test]
fn printing_a_challenge_confers_nothing() {
    let w = world();
    let first = w.challenge(BENEFICIARY, "emmanuel");
    let second = w.challenge(BENEFICIARY, "emmanuel");
    assert_eq!(first, second, "the challenge is a pure function of the ask");
    assert!(first.starts_with("cosmon-takeover-grant-v1\n"));
    assert!(!w.seats(BENEFICIARY, "1"));
    assert!(!w.grants_path().exists());
}

// ── `--sign-with`: one command, and still not a stamp ──────────────────────
//
// The operator's verdict of 2026-08-05 was that three commands, a temp file
// and a `--by` typed twice is laboratory ergonomics. `--sign-with` folds them
// into one. These tests exist because folding a *relay* into the command must
// not fold a *signer* into it: the claim under test is unchanged — an agent
// holding the binary, the ledger, the pinned key and now the `--sign-with`
// flag still cannot produce authority without the passphrase.

/// Stand a script in for `minisign(1)`. `body` receives the challenge path as
/// `$MFILE` and is what decides whether a signature appears.
fn stub_signer(w: &World, name: &str, body: &str) -> PathBuf {
    let path = w.repo.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-m\" ]; then MFILE=\"$2\"; fi\n  shift\ndone\n{body}\n"),
    )
    .expect("write stub signer");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

/// `cs … takeover grant --sign-with <key>`, with `signer` standing in for the
/// operator's minisign.
fn grant_signing_with(w: &World, holder: &str, by: &str, signer: &Path, key: &Path) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(&w.repo)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("COSMON_TAKEOVER_PUBKEY", &w.pubkey)
        .env("COSMON_MINISIGN_BIN", signer)
        .arg("--config")
        .arg(&w.state)
        .args([
            "sessions",
            "takeover",
            "grant",
            "--mission",
            MISSION,
            "--to",
            holder,
            "--by",
            by,
            "--sign-with",
        ])
        .arg(key);
    cmd.output().expect("spawn cs")
}

/// The control for this half of the file: when the operator's signer does
/// answer the prompt, one command seats the pilot.
#[test]
fn one_command_seats_the_pilot_when_the_operator_signs() {
    let w = world();
    // What the real minisign would produce, precomputed because the challenge
    // is a pure function of the ask (`printing_a_challenge_confers_nothing`).
    let signed = w.repo.join("operator-answered.minisig");
    std::fs::write(
        &signed,
        w.operator
            .sign(w.challenge("claude-primary", "emmanuel").as_bytes()),
    )
    .expect("write signature");
    let signer = stub_signer(
        &w,
        "minisign-ok.sh",
        &format!("cp {} \"$MFILE.minisig\"", signed.display()),
    );
    let key = w.repo.join("operator.key");
    std::fs::write(&key, "an opaque secret only minisign opens\n").expect("write key");

    let out = grant_signing_with(&w, "claude-primary", "emmanuel", &signer, &key);
    assert!(
        out.status.success(),
        "the one-command form must seat the pilot:\n{}\n{}",
        text(&out.stdout),
        text(&out.stderr),
    );
    assert!(w.seats("claude-primary", "1"));

    // The transfer is shown before the passphrase is asked for: signing blind
    // is not a gesture. Stderr, so `--json` stays parseable.
    let shown = text(&out.stderr);
    assert!(
        shown.contains("cosmon-takeover-grant-v1")
            && shown.contains("holder=claude-primary")
            && shown.contains("epoch=1"),
        "the operator must be shown what they are authorising:\n{shown}"
    );

    // And nothing is left to clean up: the operator asked for one command.
    let leftovers: Vec<_> = std::fs::read_dir(&w.repo)
        .expect("read repo")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".minisig") && n != "operator-answered.minisig")
        .collect();
    assert!(
        leftovers.is_empty(),
        "--sign-with must leave no signature file behind: {leftovers:?}"
    );
}

/// **The falsifier.** An agent has the binary, the ledger, the pinned key and
/// `--sign-with`. What it does not have is the passphrase, so the signer it
/// invokes refuses — and the seat must be exactly as empty as before.
#[test]
fn sign_with_confers_nothing_without_the_passphrase() {
    let w = world();
    // A minisign that was asked for a passphrase and did not get a usable one.
    let signer = stub_signer(
        &w,
        "minisign-refuses.sh",
        "echo 'Password: ' >&2\necho 'Wrong password for that key' >&2\nexit 1",
    );
    let key = w.repo.join("operator.key");
    std::fs::write(&key, "an opaque secret only minisign opens\n").expect("write key");

    let out = grant_signing_with(&w, BENEFICIARY, "emmanuel", &signer, &key);
    assert!(
        !out.status.success(),
        "a signer that did not sign must not seat anybody:\n{}",
        text(&out.stdout),
    );
    let why = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(
        why.contains("did not sign"),
        "the refusal must name the unsigned attempt:\n{why}"
    );
    assert!(!w.seats(BENEFICIARY, "1"));
    assert!(
        !w.grants_path().exists(),
        "a grant nobody signed must leave no line behind"
    );
}

/// The other half of the same falsifier: a signer that *runs* but holds the
/// wrong secret. Being able to invoke a signer is not being able to authorise.
#[test]
fn sign_with_confers_nothing_under_a_key_this_galaxy_does_not_trust() {
    let w = world();
    let impostor = Operator::from_seed(9);
    let signed = w.repo.join("impostor.minisig");
    std::fs::write(
        &signed,
        impostor.sign(w.challenge(BENEFICIARY, "emmanuel").as_bytes()),
    )
    .expect("write signature");
    let signer = stub_signer(
        &w,
        "minisign-impostor.sh",
        &format!("cp {} \"$MFILE.minisig\"", signed.display()),
    );
    let key = w.repo.join("impostor.key");
    std::fs::write(&key, "the wrong secret\n").expect("write key");

    let out = grant_signing_with(&w, BENEFICIARY, "emmanuel", &signer, &key);
    assert!(!out.status.success(), "an untrusted key must seat nobody");
    let why = format!("{}{}", text(&out.stdout), text(&out.stderr));
    // A rotation must read as a rotation and not as a corrupt file: both key
    // ids, and where the pinned one is read from.
    assert!(
        why.contains("does not recognise") && why.contains(&w.operator.key_id_display()),
        "the refusal must name the key this galaxy expects:\n{why}"
    );
    assert!(!w.seats(BENEFICIARY, "1"));
}

/// M8 friction F12: `--by` omitted made `granted_by` default to `$USER`, so a
/// challenge signed as someone else covered a different transfer and the grant
/// was refused with no hint as to why. The refusal must now name the cause.
#[test]
fn a_grant_missing_by_is_refused_by_a_message_that_names_by() {
    let w = world();
    // Signed for the operator's real name…
    let sig = w.write_sig(
        "signed-as-emmanuel.minisig",
        &w.operator
            .sign(w.challenge("claude-primary", "emmanuel").as_bytes()),
    );
    // …but granted without `--by`, so cosmon fills `$USER` in instead.
    let out = w.cs(&[
        "takeover",
        "grant",
        "--mission",
        MISSION,
        "--to",
        "claude-primary",
        "--attestation",
        &sig.display().to_string(),
    ]);
    assert!(!out.status.success(), "the signature covers another name");
    let why = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(
        why.contains("--by") && why.contains("$USER"),
        "the refusal must name the omitted flag and where the name came from:\n{why}"
    );
    assert!(!w.seats("claude-primary", "1"));
}

/// The passphrase must never enter cosmon's address space. Structural, like
/// the no-signing-path test below: capturing the child's streams — to prettify
/// its prompt, to log it — is the change that would break the property, so it
/// is asserted against the source rather than assumed.
#[test]
fn the_relay_hands_the_passphrase_prompt_straight_to_the_terminal() {
    let relay = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/operator_signature_relay.rs");
    let body = std::fs::read_to_string(&relay).expect("read the relay");
    for captured in ["Stdio::piped", ".output()", "stdin(", "read_passphrase"] {
        assert!(
            !body.contains(captured),
            "the relay must inherit stdio so the passphrase is between the operator \
             and minisign; found `{captured}`"
        );
    }
    assert!(
        body.contains(".status()"),
        "the relay is expected to run the signer with inherited stdio"
    );
}

/// The shipped tree must contain no way to sign a takeover challenge. This is
/// the property the whole design rests on, so it is asserted rather than
/// trusted: a future `cs sessions takeover sign` would hand the beneficiary
/// the stamp back, and would land here as a red test.
#[test]
fn the_shipped_tree_owns_no_signing_path_for_the_operator_key() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir");
    let mut offenders = Vec::new();
    walk(src, &mut |path| {
        if path.extension().is_none_or(|e| e != "rs") {
            return;
        }
        // Two places are allowed to hold a signer, and both are structural
        // rather than conventional: the test harness, and the `publish = false`
        // testkit crate that only ever appears in `[dev-dependencies]`.
        let as_str = path.display().to_string();
        if as_str.contains("/tests/") || as_str.contains("cosmon-minisign-testkit") {
            return;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            return;
        };
        if body.contains("SigningKey") && body.contains("minisign") {
            offenders.push(as_str);
        }
    });
    assert!(
        offenders.is_empty(),
        "cosmon must verify operator signatures and never produce one; found: {offenders:?}"
    );
}

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
