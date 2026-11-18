// SPDX-License-Identifier: AGPL-3.0-only

//! `cs presence detach` through the shipped CLI (issue #212, W5).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};
use std::time::SystemTime;

/// What a file looks like from outside: its bytes and its modification time.
type Listing = BTreeMap<String, (u64, SystemTime, Vec<u8>)>;

struct State {
    dir: tempfile::TempDir,
}

impl State {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("isolated state"),
        }
    }

    fn cs(&self, args: &[&str]) -> Output {
        // The nested binary gets only the identity this fixture supplies.
        Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/no-pilot-env.sh"
        ))
        .arg("env")
        .arg("COSMON_SESSION_ID=detach-self")
        .arg(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(self.dir.path())
        .args(args)
        .current_dir(self.dir.path())
        .output()
        .expect("run cs")
    }

    fn ping(&self, sid: &str) {
        success(&self.cs(&["presence", "ping", "--session", sid, "--headline", "here"]));
    }

    fn live(&self) -> Vec<String> {
        success(&self.cs(&["presence", "ls", "--json"]))
            .lines()
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).expect("row");
                v["session_id"].as_str().expect("sid").to_owned()
            })
            .collect()
    }

    /// Every file under the state directory with size, mtime and bytes.
    fn listing(&self) -> Listing {
        fn visit(root: &Path, path: &Path, out: &mut Listing) {
            for entry in std::fs::read_dir(path).expect("read dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else {
                    let meta = std::fs::metadata(&path).expect("metadata");
                    out.insert(
                        path.strip_prefix(root)
                            .expect("relative")
                            .display()
                            .to_string(),
                        (
                            meta.len(),
                            meta.modified().expect("mtime"),
                            std::fs::read(&path).expect("bytes"),
                        ),
                    );
                }
            }
        }
        let mut out = Listing::new();
        visit(self.dir.path(), self.dir.path(), &mut out);
        out
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
fn ping_ls_detach_ls_removes_the_session_from_the_registry() {
    let s = State::new();
    s.ping("detach-self");
    s.ping("detach-peer");
    assert!(s.live().contains(&"detach-self".to_owned()));

    // No --session: the current session, resolved as `ping` resolves it.
    let out = success(&s.cs(&["presence", "detach"]));
    assert!(out.contains("detach-self"), "{out}");
    assert_eq!(s.live(), vec!["detach-peer".to_owned()]);
}

#[test]
fn explicit_session_detaches_that_session_only() {
    let s = State::new();
    s.ping("detach-self");
    s.ping("detach-peer");
    success(&s.cs(&["presence", "detach", "--session", "detach-peer"]));
    assert_eq!(s.live(), vec!["detach-self".to_owned()]);
}

#[test]
fn second_detach_succeeds_says_so_and_changes_no_file() {
    let s = State::new();
    s.ping("detach-self");
    s.ping("detach-peer");
    success(&s.cs(&["presence", "detach"]));
    let before = s.listing();

    let out = success(&s.cs(&["presence", "detach"]));
    assert!(out.contains("nothing to detach"), "{out}");
    assert_eq!(before, s.listing(), "repeat must not touch any file");

    // A session that never pinged behaves the same way.
    let out = success(&s.cs(&["presence", "detach", "--session", "never-seen"]));
    assert!(out.contains("nothing to detach"), "{out}");
    assert_eq!(before, s.listing());
}

#[test]
fn detach_keeps_mailbox_checkpoints_and_logs_readable() {
    let s = State::new();
    s.ping("detach-self");
    success(&s.cs(&[
        "sessions",
        "send",
        "--to",
        "detach-self",
        "--from",
        "detach-sender",
        "--message",
        "still here",
    ]));
    success(&s.cs(&[
        "sessions",
        "checkpoint",
        "publish",
        "--mission",
        "m-1",
        "--session",
        "detach-self",
        "--id",
        "cp-1",
        "--done",
        "wrote the test",
    ]));
    // The whisper log channel belongs to the session, not to its presence.
    let log = s.dir.path().join("presence/session-detach-self.log");
    std::fs::write(&log, "a whisper\n").expect("seed log");

    let before = s.listing();
    success(&s.cs(&["presence", "detach"]));
    let after = s.listing();

    let removed: Vec<_> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    assert_eq!(removed.len(), 1, "only the snapshot goes: {removed:?}");
    assert!(
        removed[0].ends_with("session-detach-self.json"),
        "{removed:?}"
    );
    let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert!(added.is_empty(), "detach writes nothing: {added:?}");
    for (path, entry) in &after {
        assert_eq!(&before[path], entry, "{path} changed");
    }
    assert_eq!(std::fs::read_to_string(&log).expect("log"), "a whisper\n");

    let peek = json(&s.cs(&["sessions", "inbox", "--peek", "--json"]));
    assert_eq!(peek["body"], "still here");
    let cp = success(&s.cs(&["sessions", "checkpoint", "list", "--mission", "m-1"]));
    assert!(cp.contains("cp-1"), "{cp}");
    let shown = success(&s.cs(&[
        "sessions",
        "checkpoint",
        "show",
        "--mission",
        "m-1",
        "--id",
        "cp-1",
    ]));
    assert!(shown.contains("wrote the test"), "{shown}");
}

#[test]
fn detach_neither_requests_nor_grants_a_lease() {
    let s = State::new();
    s.ping("detach-self");
    let before = s.listing();
    success(&s.cs(&["presence", "detach"]));
    for path in s.listing().keys().chain(before.keys()) {
        assert!(!path.contains("lease"), "unexpected lease file {path}");
    }
}

#[test]
fn json_reports_detached_and_nothing_to_detach() {
    let s = State::new();
    s.ping("detach-self");
    let first = json(&s.cs(&["presence", "detach", "--json"]));
    assert_eq!(first["session"], "detach-self");
    assert_eq!(first["detached"], true);
    let second = json(&s.cs(&["presence", "detach", "--json"]));
    assert_eq!(second["session"], "detach-self");
    assert_eq!(second["detached"], false);
}

/// Whether any file under the state directory holds an `operator_present` event.
fn records_operator_present(s: &State) -> bool {
    s.listing()
        .values()
        .any(|(_, _, bytes)| String::from_utf8_lossy(bytes).contains("operator_present"))
}

/// A ping is a lifecycle write and stamps "operator present" in the event log;
/// `detach` withdraws the session and must not, so the pair pins the carve-out.
#[test]
fn ping_stamps_operator_presence_and_detach_does_not() {
    let s = State::new();
    assert!(!records_operator_present(&s));
    s.ping("detach-self");
    assert!(records_operator_present(&s), "ping must stamp presence");

    let fresh = State::new();
    fresh.ping("detach-self");
    let log = fresh
        .listing()
        .into_iter()
        .find(|(_, (_, _, bytes))| String::from_utf8_lossy(bytes).contains("operator_present"))
        .expect("the ping's event log");
    let stamps = |s: &State| {
        String::from_utf8_lossy(&s.listing()[&log.0].2)
            .matches("operator_present")
            .count()
    };
    let before = stamps(&fresh);
    success(&fresh.cs(&["presence", "detach"]));
    assert_eq!(before, stamps(&fresh), "detach must not stamp presence");
}

/// The global `--json` flag, given before the subcommand, selects the JSON form
/// exactly as the verb's own `--json` does.
#[test]
fn global_json_flag_selects_the_json_form() {
    let s = State::new();
    s.ping("detach-self");
    let first = json(&s.cs(&["--json", "presence", "detach"]));
    assert_eq!(first["detached"], true);
    let second = json(&s.cs(&["--json", "presence", "detach"]));
    assert_eq!(second["detached"], false);
}
