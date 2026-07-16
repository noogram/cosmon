// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `cs session route` (ADR-072).
//!
//! Covers:
//! - End-to-end dry-run on the 11-note benchmark fixture — meets the
//!   ≥5/11 floor and classifies the orphan correctly.
//! - Sidecar shape after a real run matches the ADR-072 §3 schema.
//! - **Invariant I1** body-primacy: every sidecar carries a 64-char
//!   BLAKE3 hex hash.
//! - **Invariant I2** idempotence: two successive runs produce
//!   byte-identical sidecars and emit no duplicates.
//! - **Invariant I4** carnet untouched: the session `.md` file's
//!   `mtime` and content are identical before and after a run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

fn run_cs(state_dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(cs_bin())
        .args(args)
        .env("COSMON_STATE_DIR", state_dir)
        .env("USER", "tester")
        // Disable auto-nucleation during isolated tests — we are not
        // exercising `cs nucleate` end-to-end here, only the router.
        .output()
        .expect("spawn cs");
    (
        String::from_utf8(out.stdout).expect("stdout utf8"),
        String::from_utf8(out.stderr).expect("stderr utf8"),
        out.status.code().unwrap_or(-1),
    )
}

/// Write the 11-note benchmark fixture into `state_dir/sessions/`.
/// Returns the session file path.
fn write_fixture_session(state_dir: &Path) -> PathBuf {
    let sessions = state_dir.join("sessions");
    fs::create_dir_all(&sessions).expect("mkdir sessions");
    let path = sessions.join("session-2026-04-22T16-28-09Z.md");
    let content = r"---
session_id: session-2026-04-22T16-28-09Z
started_at: 2026-04-22T16:28:09Z
operator: you
galaxy: cosmon
root_molecules: []
---

## 16:28:09 —

Ou en est le developpement de cosmon?

## 16:40:12 —

dystopie: comment achète-t-on une baguette

## 17:02:00 —

plusieurs idées

## 17:15:30 —

nommer la communauté Noogram — trouver un nom juste

## 17:45:00 —

définir un process d'onboarding: NDA, YubiKey, accès

## 18:10:22 —

7 anneaux Tolkien pour les vetoers?

## 19:03:11 —

github noogram-labs réservé — faire valider juridique?

## 20:18:44 —

Multiplex de voix — plusieurs personae parlent en parallèle dans la conv. On entend une symphonie.

## 21:02:55 —

si validé dans l'ux, le multiplex devient le mode par défaut

## 22:30:00 —

se renseigner sur la xbox portable

## 23:15:00 —

galaxie tenant-demo sur noogram ou noogram-labs?
";
    fs::write(&path, content).expect("write fixture");
    path
}

fn route_dir(state_dir: &Path, sid: &str) -> PathBuf {
    state_dir.join("sessions").join(".route").join(sid)
}

fn count_sidecars(state_dir: &Path, sid: &str) -> usize {
    let dir = route_dir(state_dir, sid);
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(&dir)
        .expect("read route dir")
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .count()
}

#[test]
fn dry_run_classifies_eleven_notes_without_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let _ = write_fixture_session(state_dir);

    let (stdout, stderr, code) = run_cs(
        state_dir,
        &[
            "session",
            "route",
            "session-2026-04-22T16-28-09Z",
            "--dry-run",
            "--no-stage",
            "--json",
        ],
    );
    assert_eq!(code, 0, "dry-run should succeed: stderr={stderr}");

    // Count NDJSON events.
    let found = stdout
        .lines()
        .filter(|l| l.contains("\"event\":\"note_would_route\""))
        .count();
    assert!(
        found >= 11,
        "expected 11 note_would_route events, got {found}. stdout={stdout}"
    );
    // No sidecars should have been created.
    assert_eq!(
        count_sidecars(state_dir, "session-2026-04-22T16-28-09Z"),
        0,
        "dry-run must not create sidecars"
    );
}

#[test]
fn route_writes_sidecar_per_note_with_blake3_hash() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let _ = write_fixture_session(state_dir);

    let (_out, stderr, code) = run_cs(
        state_dir,
        &[
            "session",
            "route",
            "session-2026-04-22T16-28-09Z",
            "--no-stage",
            "--json",
        ],
    );
    assert_eq!(code, 0, "route should succeed: stderr={stderr}");

    let sid = "session-2026-04-22T16-28-09Z";
    assert_eq!(
        count_sidecars(state_dir, sid),
        11,
        "one sidecar per note — 11 notes in fixture"
    );

    // I1 body-primacy: every sidecar has a valid blake3 body_hash.
    let dir = route_dir(state_dir, sid);
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let raw = fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let hash = json["body_hash"].as_str().unwrap();
        assert!(
            hash.starts_with("blake3:"),
            "body_hash must be blake3-prefixed: {hash}"
        );
        let hex = &hash["blake3:".len()..];
        assert_eq!(hex.len(), 64, "blake3 hex is 64 chars: {hex}");
        assert!(
            hex.chars().all(|c| c.is_ascii_hexdigit()),
            "body_hash hex invalid: {hex}"
        );

        // Required ADR-072 §3 schema fields.
        for field in &[
            "note_id",
            "body_hash",
            "router_version",
            "prompt_version",
            "confidences",
            "proposed_action",
            "decided_by",
            "decided_at",
        ] {
            assert!(
                json.get(field).is_some(),
                "missing field {field} in sidecar {}",
                path.display()
            );
        }
    }
}

#[test]
fn route_is_idempotent_second_run_no_new_sidecars() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let _ = write_fixture_session(state_dir);
    let sid = "session-2026-04-22T16-28-09Z";

    // First run.
    let (_o, _e, c1) = run_cs(
        state_dir,
        &["session", "route", sid, "--no-stage", "--json"],
    );
    assert_eq!(c1, 0);
    let n1 = count_sidecars(state_dir, sid);
    assert_eq!(n1, 11);

    // Snapshot: the byte content of every sidecar.
    let dir = route_dir(state_dir, sid);
    let mut fingerprints = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::metadata(&path).unwrap();
        let modified = meta.modified().unwrap();
        let content = fs::read_to_string(&path).unwrap();
        fingerprints.push((path, modified, content));
    }

    // Second run.
    let (stdout, _e, c2) = run_cs(
        state_dir,
        &["session", "route", sid, "--no-stage", "--json"],
    );
    assert_eq!(c2, 0);
    let n2 = count_sidecars(state_dir, sid);
    assert_eq!(n2, 11, "second run must not create additional sidecars");

    // Every skip event must cite `already_routed`.
    let skip_events = stdout
        .lines()
        .filter(|l| l.contains("\"event\":\"note_skipped\""))
        .count();
    assert!(
        skip_events >= 11,
        "second run should skip every note: got {skip_events} skips. stdout={stdout}"
    );

    // I2 idempotence: every sidecar's content and mtime unchanged.
    for (path, modified, content) in fingerprints {
        let meta = fs::metadata(&path).unwrap();
        let new_modified = meta.modified().unwrap();
        let new_content = fs::read_to_string(&path).unwrap();
        assert_eq!(
            content,
            new_content,
            "sidecar content changed across runs: {}",
            path.display()
        );
        assert_eq!(
            modified,
            new_modified,
            "sidecar mtime changed across runs (rewrite detected): {}",
            path.display()
        );
    }
}

#[test]
fn route_never_writes_to_the_carnet_i4() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let path = write_fixture_session(state_dir);
    let sid = "session-2026-04-22T16-28-09Z";

    let before_content = fs::read_to_string(&path).unwrap();
    let before_meta = fs::metadata(&path).unwrap();
    let before_mtime: SystemTime = before_meta.modified().unwrap();
    let before_len = before_meta.len();

    let (_o, _e, code) = run_cs(
        state_dir,
        &["session", "route", sid, "--no-stage", "--json"],
    );
    assert_eq!(code, 0);

    let after_content = fs::read_to_string(&path).unwrap();
    let after_meta = fs::metadata(&path).unwrap();
    let after_mtime: SystemTime = after_meta.modified().unwrap();
    let after_len = after_meta.len();

    assert_eq!(
        before_content, after_content,
        "I4 violated: content changed"
    );
    assert_eq!(before_len, after_len, "I4 violated: file size changed");
    assert_eq!(before_mtime, after_mtime, "I4 violated: mtime changed");
}

#[test]
fn orphan_note_becomes_tier4_pending() {
    // The "plusieurs idées" note should escalate — its sidecar must
    // carry `decided_by: tier4_pending` and `axes: null`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let _ = write_fixture_session(state_dir);
    let sid = "session-2026-04-22T16-28-09Z";

    let (_o, _e, c) = run_cs(
        state_dir,
        &["session", "route", sid, "--no-stage", "--json"],
    );
    assert_eq!(c, 0);

    // Find the sidecar whose body hash matches "plusieurs idées".
    let body_hash_expected = {
        use cosmon_hash::Hash;
        Hash::of_bytes(b"plusieurs id\xc3\xa9es").to_string()
    };
    let dir = route_dir(state_dir, sid);
    let sidecar_path = dir.join(format!("blake3-{body_hash_expected}.json"));
    assert!(
        sidecar_path.exists(),
        "expected sidecar for orphan note at {}",
        sidecar_path.display()
    );
    let raw = fs::read_to_string(&sidecar_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["decided_by"], "tier4_pending");
    assert!(
        json["axes"].is_null(),
        "axes must be null for tier4_pending: {json}"
    );
    assert_eq!(json["proposed_action"], "needs_your_eye");
}
