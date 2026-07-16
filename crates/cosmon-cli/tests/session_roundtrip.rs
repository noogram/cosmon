// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end test for `cs session start → note → note → end`.
//!
//! Asserts that the sealed session file has the expected structure,
//! that the BLAKE3 seal over the body matches a fresh re-hash, and
//! that the two error paths (already-open on `start`, no-open on
//! `note`/`end`) surface the documented exit codes (2 / 3).

use std::path::PathBuf;
use std::process::Command;

fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

fn run_cs(state_dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(cs_bin())
        .args(args)
        .env("COSMON_STATE_DIR", state_dir)
        .env("USER", "tester")
        .output()
        .expect("spawn cs");
    (
        String::from_utf8(out.stdout).expect("stdout utf8"),
        String::from_utf8(out.stderr).expect("stderr utf8"),
        out.status.code().unwrap_or(-1),
    )
}

fn sessions_dir(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("sessions")
}

fn first_session_file(state_dir: &std::path::Path) -> PathBuf {
    let dir = sessions_dir(state_dir);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read sessions dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let stem_ok = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-"));
            let ext_ok = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"));
            stem_ok && ext_ok
        })
        .collect();
    files.sort();
    files.into_iter().next().expect("no session file written")
}

#[test]
fn full_round_trip_session_start_note_note_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();

    // start
    let (stdout, _stderr, code) = run_cs(state_dir, &["session", "start", "--galaxy", "cosmon"]);
    assert_eq!(code, 0, "start failed: {stdout}");
    let session_id = stdout.trim().to_owned();
    assert!(session_id.starts_with("session-"));

    // note (untagged) + note (tagged)
    let (_o1, _e1, c1) = run_cs(state_dir, &["session", "note", "first body"]);
    assert_eq!(c1, 0);
    let (_o2, _e2, c2) = run_cs(
        state_dir,
        &["session", "note", "--tag", "insight", "second body"],
    );
    assert_eq!(c2, 0);

    // end
    let (_o3, _e3, c3) = run_cs(state_dir, &["session", "end"]);
    assert_eq!(c3, 0);

    // Inspect the sealed file.
    let path = first_session_file(state_dir);
    let content = std::fs::read_to_string(&path).expect("read session");

    assert!(content.starts_with("---\nsession_id: "));
    assert!(content.contains("galaxy: cosmon"));
    assert!(content.contains("operator: tester"));
    assert!(
        content.contains("## "),
        "expected at least one note heading"
    );
    assert!(
        content.contains(" — insight"),
        "tag missing from note heading"
    );
    assert!(content.contains("first body"));
    assert!(content.contains("second body"));
    assert!(content.contains("note_count: 2"));
    assert!(content.contains("seal: blake3:"));

    // The seal must match a fresh re-hash of the body slice.
    let rest = content.strip_prefix("---\n").expect("frontmatter");
    let fm_close = rest.find("\n---\n").expect("fm close");
    let after_fm = &rest[fm_close + 5..];
    // The footer opens at the first `\n---\n` inside `after_fm`. Using
    // `rfind` would land on the closing marker, which is why the body
    // slice must use `find` (first match).
    let footer_open = after_fm.find("\n---\n").expect("footer open");
    let body = &after_fm[..footer_open];
    let body_trimmed = body.trim_end_matches(['\n', ' ', '\t']);
    let expected = cosmon_hash::Hash::of_bytes(body_trimmed.as_bytes()).to_string();

    let seal_line = content
        .lines()
        .find(|l| l.starts_with("seal: blake3:"))
        .expect("seal line");
    let seal_hex = seal_line.trim_start_matches("seal: blake3:");
    assert_eq!(seal_hex, expected, "seal does not match body re-hash");
}

#[test]
fn start_refuses_when_session_already_open_with_exit_code_2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let (_o, _e, c0) = run_cs(state_dir, &["session", "start"]);
    assert_eq!(c0, 0);
    let (_o, _e, c1) = run_cs(state_dir, &["session", "start"]);
    assert_eq!(c1, 2, "expected exit code 2 when a session is already open");
}

#[test]
fn note_and_end_fail_with_exit_code_3_when_no_session() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let (_o, _e, c_note) = run_cs(state_dir, &["session", "note", "orphan"]);
    assert_eq!(c_note, 3);
    let (_o, _e, c_end) = run_cs(state_dir, &["session", "end"]);
    assert_eq!(c_end, 3);
}

#[test]
fn note_with_cause_flags_writes_subline_backward_compatible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();

    // start
    let (_o, _e, c0) = run_cs(state_dir, &["session", "start"]);
    assert_eq!(c0, 0);

    // one legacy note (no cause flags) — must render without `cause:` subline
    let (_o, _e, c1) = run_cs(state_dir, &["session", "note", "legacy body"]);
    assert_eq!(c1, 0);

    // one causal note — oracle-suggestion via keyboard
    let (_o, _e, c2) = run_cs(
        state_dir,
        &[
            "session",
            "note",
            "--tag",
            "insight",
            "--cause-kind",
            "oracle-suggestion",
            "--cause-agent",
            "apfel-oracle-rococo",
            "--cause-channel",
            "keyboard",
            "causal body",
        ],
    );
    assert_eq!(c2, 0);

    // one voice-transcription note, custom channel Other
    let (_o, _e, c3) = run_cs(
        state_dir,
        &[
            "session",
            "note",
            "--cause-kind",
            "transcription",
            "--cause-agent",
            "matrix:@tenant_auditor:hs",
            "--cause-channel",
            "voice",
            "spoken body",
        ],
    );
    assert_eq!(c3, 0);

    let (_o, _e, c4) = run_cs(state_dir, &["session", "end"]);
    assert_eq!(c4, 0);

    let path = first_session_file(state_dir);
    let content = std::fs::read_to_string(&path).expect("read session");

    // Legacy note renders without cause subline.
    assert!(content.contains("legacy body"));
    // The legacy note's block must NOT contain the cause line. Check by
    // looking for the exact sequence "legacy body" appearing *without* a
    // preceding cause line.
    let legacy_idx = content.find("legacy body").expect("legacy body present");
    let before = &content[..legacy_idx];
    let last_header = before.rfind("## ").expect("legacy header");
    assert!(
        !content[last_header..legacy_idx].contains("cause:"),
        "legacy note must not carry a cause line: {}",
        &content[last_header..legacy_idx]
    );

    // Causal notes render the cause subline.
    assert!(content.contains(
        "cause: {kind: oracle-suggestion, agent: apfel-oracle-rococo, channel: keyboard}"
    ));
    assert!(
        content.contains("cause: {kind: transcription, agent: matrix:@tenant_auditor:hs, channel: voice}")
    );

    assert!(content.contains("note_count: 3"));
}

#[test]
fn end_with_no_seal_still_writes_footer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let (_o, _e, c0) = run_cs(state_dir, &["session", "start"]);
    assert_eq!(c0, 0);
    let (_o, _e, c1) = run_cs(state_dir, &["session", "note", "scratch"]);
    assert_eq!(c1, 0);
    let (_o, _e, c2) = run_cs(state_dir, &["session", "end", "--no-seal"]);
    assert_eq!(c2, 0);

    let path = first_session_file(state_dir);
    let content = std::fs::read_to_string(path).expect("read session");
    assert!(content.contains("ended_at: "));
    assert!(content.contains("note_count: 1"));
    assert!(
        !content.contains("seal: "),
        "--no-seal must not write a seal line"
    );
}
