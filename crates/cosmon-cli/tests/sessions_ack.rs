// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit pilot-mailbox acknowledgments through the shipped CLI.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

struct Mailbox {
    dir: tempfile::TempDir,
}

impl Mailbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("isolated mailbox"),
        }
    }

    fn cs(&self, args: &[&str]) -> Output {
        // Strip the dispatch environment before invoking the nested binary;
        // this fixture supplies only its own caller identity.
        Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/no-pilot-env.sh"
        ))
        .arg("env")
        .arg("COSMON_SESSION_ID=ack-reader")
        .arg(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(self.dir.path())
        .arg("sessions")
        .args(args)
        .current_dir(self.dir.path())
        .output()
        .expect("run cs")
    }

    fn send(&self, to: &str, body: &str) -> String {
        let value = json(&self.cs(&[
            "send",
            "--to",
            to,
            "--from",
            "ack-sender",
            "--message",
            body,
            "--json",
        ]));
        value["id"].as_str().expect("message id").to_owned()
    }

    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        fn visit(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(path).expect("read mailbox directory") {
                let path = entry.expect("mailbox entry").path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(root)
                            .expect("relative path")
                            .display()
                            .to_string(),
                        std::fs::read(&path).expect("mailbox bytes"),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(self.dir.path(), self.dir.path(), &mut files);
        files
    }
}

fn success(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).expect("UTF-8 output")
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_str(&success(output)).expect("JSON output")
}

#[test]
fn peek_then_ack_consumes_exactly_one_envelope_and_duplicate_writes_nothing() {
    let m = Mailbox::new();
    let key = m.send("ack-reader", "first envelope");
    let other = m.send("ack-reader", "leave pending");
    let before = m.files();
    let peek = success(&m.cs(&["inbox", "--peek", "--json"]));
    assert_eq!(peek.lines().count(), 2);
    assert_eq!(before, m.files(), "peek must not write");

    let ack = json(&m.cs(&["ack", &key, "--json"]));
    assert_eq!(ack["id"], key);
    assert_eq!(ack["session"], "ack-reader");
    assert_eq!(ack["duplicate"], false);
    let mut files = m.files();
    let record = files
        .remove("presence/ack-reader.inbox.ack.jsonl")
        .expect("the existing acknowledgment ledger");
    assert_eq!(before, files, "only the acknowledgment ledger may change");
    let record: serde_json::Value = serde_json::from_slice(&record).expect("one ack record");
    assert_eq!(record["id"], key);
    let after = m.files();
    let entries = success(&m.cs(&["inbox", "--all", "--peek", "--json"]));
    let entries: Vec<serde_json::Value> = entries
        .lines()
        .map(|line| serde_json::from_str(line).expect("entry"))
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], key);
    assert_eq!(entries[0]["state"], "read");
    assert!(entries[0]["read_at"].is_string());
    assert_eq!(entries[0]["read_at"], record["read_at"]);
    assert_eq!(entries[1]["id"], other);
    assert_eq!(entries[1]["state"], "queued");
    assert!(entries[1]["read_at"].is_null());

    let duplicate = json(&m.cs(&["ack", &key, "--json"]));
    assert_eq!(duplicate["id"], key);
    assert_eq!(duplicate["session"], "ack-reader");
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(
        after,
        m.files(),
        "duplicate must leave all files byte-identical"
    );
}

#[test]
fn unknown_or_other_mailbox_key_is_refused_without_writing() {
    let m = Mailbox::new();
    let foreign = m.send("another-reader", "not yours");
    for key in ["msg-unknown", foreign.as_str()] {
        let before = m.files();
        let output = m.cs(&["ack", key, "--json"]);
        assert!(!output.status.success(), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(key), "{error}");
        assert_eq!(before, m.files(), "refusal must not write an ack");
    }
}

#[test]
fn explicit_session_and_text_output_report_recorded_and_duplicate() {
    let m = Mailbox::new();
    let key = m.send("explicit-reader", "explicit mailbox");
    let output = success(&m.cs(&["ack", &key, "--session", "explicit-reader"]));
    assert!(output.contains(&key), "{output}");
    assert!(output.contains("acknowledged"), "{output}");
    let before = m.files();
    let output = success(&m.cs(&["ack", &key, "--session", "explicit-reader"]));
    assert!(output.contains(&key), "{output}");
    assert!(output.contains("duplicate"), "{output}");
    assert_eq!(before, m.files());
}

#[test]
fn plain_inbox_still_acknowledges_and_explicit_ack_then_reports_duplicate() {
    let m = Mailbox::new();
    let key = m.send("ack-reader", "ordinary read");
    let read = json(&m.cs(&["inbox", "--json"]));
    assert_eq!(read["id"], key);
    assert_eq!(read["body"], "ordinary read");
    assert!(success(&m.cs(&["inbox", "--json"])).is_empty());
    let before = m.files();
    assert_eq!(json(&m.cs(&["ack", &key, "--json"]))["duplicate"], true);
    assert_eq!(before, m.files());
    let read = json(&m.cs(&["inbox", "--all", "--json"]));
    assert_eq!(read["state"], "read");
    assert!(read["read_at"].is_string());
    assert_eq!(before, m.files());
}
