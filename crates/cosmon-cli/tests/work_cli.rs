// SPDX-License-Identifier: AGPL-3.0-only

//! Binary-level examples of a declared work exchange.

use std::process::Command;
use std::{fs, path::Path};

const OWNER: &str = "task-20260928-a001";
const A: &str = "task-20260928-a002";
const B: &str = "task-20260928-a003";
const OUTSIDER: &str = "task-20260928-a004";

fn mol_dir(state: &Path, id: &str) -> std::path::PathBuf {
    state.join("fleets/default/molecules").join(id)
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp galaxy");
    let state = dir.path().join(".cosmon/state");
    for id in [OWNER, A, B, OUTSIDER] {
        let path = mol_dir(&state, id);
        fs::create_dir_all(&path).expect("molecule dir");
        fs::write(path.join("state.json"), format!("state-{id}\n")).expect("state sentinel");
    }
    fs::write(state.join("events.jsonl"), b"event sentinel\n").expect("event sentinel");
    dir
}

fn cs(dir: &Path, caller: Option<&str>, args: &[&str]) -> std::process::Output {
    let state = dir.join(".cosmon/state");
    let mut command = Command::new(env!("CARGO_BIN_EXE_cs"));
    command
        .current_dir(dir)
        .args(["--config", state.to_str().expect("state path")])
        .args(args);
    if let Some(id) = caller {
        command.env("COSMON_MOL_DIR", mol_dir(&state, id));
    } else {
        command.env_remove("COSMON_MOL_DIR");
    }
    command.output().expect("run cs")
}

fn ok(dir: &Path, caller: Option<&str>, args: &[&str]) -> String {
    let output = cs(dir, caller, args);
    assert!(
        output.status.success(),
        "cs {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}

fn declare(dir: &Path) {
    ok(
        dir,
        None,
        &[
            "work",
            "declare",
            OWNER,
            "--seat",
            &format!("a={A}"),
            "--seat",
            &format!("b={B}"),
        ],
    );
}

#[test]
fn work_help_lists_the_verbs() {
    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args(["help", "work"])
        .output()
        .expect("run cs help work");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    for verb in ["declare", "send", "inbox", "ack", "list"] {
        assert!(help.contains(verb), "missing {verb} from {help}");
    }
}

#[test]
fn declare_send_inbox_ack_list_round_trip() {
    let fixture = fixture();
    let dir = fixture.path();
    declare(dir);
    let sent = ok(
        dir,
        Some(A),
        &[
            "work",
            "send",
            "--to",
            "b",
            "--text",
            "Review line 4",
            "--key",
            "finding-1",
        ],
    );
    assert!(
        sent.contains("finding-1") && sent.contains("admitted"),
        "{sent}"
    );
    let inbox = ok(dir, Some(B), &["work", "inbox"]);
    assert!(
        inbox.contains("finding-1") && inbox.contains("Review line 4"),
        "{inbox}"
    );
    ok(dir, Some(B), &["work", "ack", "finding-1", "--considered"]);
    let listed: serde_json::Value =
        serde_json::from_str(&ok(dir, None, &["--json", "work", "list", OWNER]))
            .expect("list JSON");
    let view = &listed["envelopes"]["finding-1"];
    assert!(view["admitted"].is_string());
    assert_eq!(view["delivery_attempts"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        view["context_delivered"]["observation"]["status"],
        "unknown"
    );
    assert_eq!(view["consumed"]["disposition"], "considered");
}

#[test]
fn redeclaring_the_same_roster_does_not_rewrite_scope() {
    let fixture = fixture();
    let dir = fixture.path();
    declare(dir);
    let scope = mol_dir(&dir.join(".cosmon/state"), OWNER).join("work/scope.json");
    let before = fs::read(&scope).expect("scope");
    #[cfg(unix)]
    let inode_before = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&scope).expect("metadata").ino()
    };
    declare(dir);
    assert_eq!(fs::read(&scope).expect("scope"), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&scope).expect("metadata").ino(), inode_before);
    }
}

#[test]
fn non_member_sender_exits_two_and_writes_nothing() {
    let fixture = fixture();
    let dir = fixture.path();
    declare(dir);
    let work = mol_dir(&dir.join(".cosmon/state"), OWNER).join("work");
    let before = fs::read(work.join("scope.json")).expect("scope");
    let events_before = fs::read(dir.join(".cosmon/state/events.jsonl")).expect("events");
    let output = cs(
        dir,
        Some(OUTSIDER),
        &["work", "send", "--to", "b", "--text", "intrusion"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(work.join("scope.json")).expect("scope"), before);
    assert_eq!(
        fs::read(dir.join(".cosmon/state/events.jsonl")).expect("events"),
        events_before
    );
    assert!(!work.join("envelopes").exists());
    assert!(!work.join("receipts").exists());
    assert!(!mol_dir(&dir.join(".cosmon/state"), OUTSIDER)
        .join("work-ref.json")
        .exists());
}

#[test]
fn peek_records_no_receipt() {
    let fixture = fixture();
    let dir = fixture.path();
    declare(dir);
    ok(
        dir,
        Some(A),
        &[
            "work", "send", "--to", "b", "--text", "question", "--key", "peek-1",
        ],
    );
    let receipts = mol_dir(&dir.join(".cosmon/state"), OWNER).join("work/receipts/peek-1.jsonl");
    let before = fs::read(&receipts).expect("admission receipt");
    let output = ok(dir, Some(B), &["work", "inbox", "--peek"]);
    assert!(output.contains("peek-1"));
    assert_eq!(fs::read(receipts).expect("receipts"), before);
}

#[test]
fn messaging_causes_no_lifecycle_transition() {
    let fixture = fixture();
    let dir = fixture.path();
    let state = dir.join(".cosmon/state");
    declare(dir);
    let events_before = fs::read(state.join("events.jsonl")).expect("events");
    let states_before: Vec<_> = [OWNER, A, B]
        .iter()
        .map(|id| fs::read(mol_dir(&state, id).join("state.json")).expect("state"))
        .collect();

    ok(
        dir,
        Some(A),
        &[
            "work", "send", "--to", "b", "--text", "first", "--key", "q1",
        ],
    );
    ok(dir, Some(B), &["work", "inbox"]);
    ok(dir, Some(B), &["work", "ack", "q1", "--considered"]);
    ok(
        dir,
        Some(B),
        &[
            "work",
            "send",
            "--to",
            "a",
            "--text",
            "reply",
            "--key",
            "r1",
            "--reply-to",
            "q1",
        ],
    );
    ok(dir, Some(A), &["work", "inbox"]);
    ok(dir, Some(A), &["work", "ack", "r1", "--deferred"]);
    ok(
        dir,
        Some(A),
        &[
            "work", "send", "--to", "b", "--text", "second", "--key", "q2",
        ],
    );
    ok(dir, Some(B), &["work", "inbox"]);
    ok(dir, Some(B), &["work", "ack", "q2", "--rejected"]);
    let listed: serde_json::Value =
        serde_json::from_str(&ok(dir, None, &["--json", "work", "list", OWNER]))
            .expect("list JSON");
    assert_eq!(
        listed["envelopes"].as_object().map(serde_json::Map::len),
        Some(3)
    );
    assert_eq!(listed["envelopes"]["r1"]["envelope"]["reply_to"], "q1");
    assert_eq!(
        fs::read(state.join("events.jsonl")).expect("events"),
        events_before
    );
    for (id, before) in [OWNER, A, B].iter().zip(states_before) {
        assert_eq!(
            fs::read(mol_dir(&state, id).join("state.json")).expect("state"),
            before,
            "{id}"
        );
    }
}

fn send_finding(dir: &Path) {
    ok(
        dir,
        Some(A),
        &[
            "work",
            "send",
            "--to",
            "b",
            "--text",
            "Review line 4",
            "--key",
            "finding-1",
        ],
    );
}

/// Re-declaring the owner with seat `b` bound to another molecule must not
/// hand the old holder's pending envelope to the new one (#162, item 1).
#[test]
fn redeclared_seat_does_not_inherit_the_previous_holders_mail() {
    let fixture = fixture();
    let dir = fixture.path();
    declare(dir);
    send_finding(dir);
    ok(
        dir,
        None,
        &[
            "work",
            "declare",
            OWNER,
            "--seat",
            &format!("a={A}"),
            "--seat",
            &format!("b={OUTSIDER}"),
        ],
    );

    let inbox = ok(dir, Some(OUTSIDER), &["work", "inbox"]);
    assert!(
        !inbox.contains("finding-1") && !inbox.contains("Review line 4"),
        "new holder was offered the old holder's envelope: {inbox}"
    );
    let receipts = fs::read_to_string(
        mol_dir(&dir.join(".cosmon/state"), OWNER).join("work/receipts/finding-1.jsonl"),
    )
    .expect("admission receipt");
    assert!(
        !receipts.contains("delivery_attempted"),
        "no delivery attempt may be recorded for the new holder: {receipts}"
    );
    // The old holder can no longer pull either: it is not in the roster.
    assert!(!cs(dir, Some(B), &["work", "inbox"]).status.success());

    // The envelope is not lost: it stays visible, with a finding.
    let listed: serde_json::Value =
        serde_json::from_str(&ok(dir, None, &["--json", "work", "list", OWNER]))
            .expect("list JSON");
    assert!(listed["envelopes"]["finding-1"].is_object(), "{listed}");
    assert_eq!(listed["envelopes"]["finding-1"]["recipient_bound"], false);
    let findings = listed["findings"].as_array().expect("findings");
    assert!(
        findings
            .iter()
            .any(|f| f["finding"] == "recipient_not_bound" && f["key"] == "finding-1"),
        "{listed}"
    );
}

/// Revising the roster without changing who holds the recipient seat keeps
/// earlier mail deliverable: the check is about the molecule, not the revision.
#[test]
fn redeclaring_with_an_added_seat_keeps_pending_mail_deliverable() {
    let fixture = fixture();
    let dir = fixture.path();
    declare(dir);
    send_finding(dir);
    ok(
        dir,
        None,
        &[
            "work",
            "declare",
            OWNER,
            "--seat",
            &format!("a={A}"),
            "--seat",
            &format!("b={B}"),
            "--seat",
            &format!("c={OUTSIDER}"),
        ],
    );
    let inbox = ok(dir, Some(B), &["work", "inbox"]);
    assert!(inbox.contains("finding-1"), "{inbox}");
}
