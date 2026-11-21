// SPDX-License-Identifier: AGPL-3.0-only

//! The local `cs sessions send` takes the mission's mailbox lock, the same
//! lock the remote S-SEND writer takes (issue #212, W7b follow-up).
//!
//! The recipient is a local session whose presence snapshot names a mission.
//! The test holds that mission's mailbox lock the way a remote writer does
//! and drives the shipped binary: a local send that ignores the lock writes
//! at once, one that honours it waits for the release.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use cosmon_core::id::{MoleculeId, SessionId};
use cosmon_core::paths::CosmonPath;
use cosmon_core::presence::Presence;
use cosmon_filestore::PresenceStore;

const MISSION: &str = "task-20261121-aaaa";
const RECIPIENT: &str = "lock-reader";

fn cs_send(state: &Path, body: &str) -> Child {
    Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/no-pilot-env.sh"
    ))
    .arg(env!("CARGO_BIN_EXE_cs"))
    .arg("--config")
    .arg(state)
    .args([
        "sessions",
        "send",
        "--to",
        RECIPIENT,
        "--from",
        "lock-sender",
        "--message",
        body,
        "--json",
    ])
    .current_dir(state)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn cs")
}

fn register_recipient(state: &Path, mission: Option<&str>) {
    let mut presence = Presence::new(
        SessionId::new(RECIPIENT).expect("session id"),
        "galaxy",
        state,
        std::process::id(),
        Utc::now(),
    );
    presence.mission = mission.map(|m| MoleculeId::new(m).expect("mission id"));
    PresenceStore::new(state)
        .upsert(&presence)
        .expect("write presence");
}

fn lock_path(state: &Path) -> PathBuf {
    let mission = MoleculeId::new(MISSION).expect("mission id");
    state.join(CosmonPath::RemoteMailboxLock { mission: &mission }.rel())
}

fn hold_lock(state: &Path) -> std::fs::File {
    let path = lock_path(state);
    std::fs::create_dir_all(path.parent().expect("lock dir")).expect("lock dir");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("open lock");
    fs2::FileExt::lock_exclusive(&file).expect("take lock");
    file
}

fn inbox_lines(state: &Path) -> usize {
    let sid = SessionId::new(RECIPIENT).expect("session id");
    let path = state.join(CosmonPath::PresenceInbox { session: &sid }.rel());
    std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

#[test]
fn a_local_send_to_a_mission_pilot_waits_for_the_mailbox_lock() {
    let dir = tempfile::tempdir().expect("state");
    register_recipient(dir.path(), Some(MISSION));
    let lock = hold_lock(dir.path());

    let mut child = cs_send(dir.path(), "waits for the lock");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        child.try_wait().expect("poll cs").is_none(),
        "the local send finished while the mission mailbox lock was held"
    );
    assert_eq!(
        inbox_lines(dir.path()),
        0,
        "nothing is written under the lock"
    );

    drop(lock);
    let deadline = Instant::now() + Duration::from_secs(60);
    let output = loop {
        if child.try_wait().expect("poll cs").is_some() {
            break child.wait_with_output().expect("cs output");
        }
        assert!(Instant::now() < deadline, "the send never resumed");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(value["sequence"], 1);
    assert_eq!(value["delivered"], true);
    assert_eq!(inbox_lines(dir.path()), 1);
}

#[test]
fn a_local_send_to_a_session_without_a_mission_takes_no_lock() {
    let dir = tempfile::tempdir().expect("state");
    register_recipient(dir.path(), None);
    let _lock = hold_lock(dir.path());

    let output = cs_send(dir.path(), "no mission")
        .wait_with_output()
        .expect("cs output");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(inbox_lines(dir.path()), 1);
}

#[test]
fn a_local_send_to_an_unregistered_session_still_delivers() {
    let dir = tempfile::tempdir().expect("state");
    let output = cs_send(dir.path(), "unregistered")
        .wait_with_output()
        .expect("cs output");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(inbox_lines(dir.path()), 1);
}
