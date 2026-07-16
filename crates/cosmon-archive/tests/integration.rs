// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the canonical archive writer (ADR-030 M2 `DoD`).
//!
//! Spec from the parent task:
//!
//! > Integration test creates a fake molecule dir, runs `archive::write`,
//! > verifies files + manifest + hashes.
//!
//! This test builds a realistic molecule directory (prompt + briefing +
//! synthesis + responses + log + events + state), runs the archive
//! writer twice, and checks three properties:
//!
//! 1. Every source artifact shows up in the archive entry with
//!    byte-identical content.
//! 2. `manifest.json` is well-formed, lists every archived file, records
//!    the right `SCHEMA_VERSION` and `trigger`, and carries a chain head
//!    consistent with the per-file content hashes.
//! 3. Re-running on an unchanged molecule directory is idempotent for
//!    file contents and manifest hashes (clocks aside).

use std::fs;

use chrono::{TimeZone, Utc};
use cosmon_archive::{recompute_chain_head, write, Manifest, Trigger, SCHEMA_VERSION};
use cosmon_hash::Hash;

const ARCHIVED_FILES: &[&str] = &[
    "prompt.md",
    "briefing.md",
    "synthesis.md",
    "log.md",
    "events.jsonl",
    "state.json",
    "responses/torvalds.md",
    "responses/knuth.md",
];

fn populate_fake_molecule(mol: &std::path::Path) {
    fs::create_dir_all(mol.join("responses")).unwrap();
    fs::write(mol.join("prompt.md"), b"# prompt\ntopic: archive M2\n").unwrap();
    fs::write(mol.join("briefing.md"), b"# briefing\nStep 1: write\n").unwrap();
    fs::write(mol.join("synthesis.md"), b"# synthesis\npanel agrees\n").unwrap();
    fs::write(mol.join("log.md"), b"# log\nworker started\n").unwrap();
    fs::write(
        mol.join("events.jsonl"),
        b"{\"kind\":\"spawn\"}\n{\"kind\":\"step\"}\n",
    )
    .unwrap();
    fs::write(
        mol.join("state.json"),
        b"{\"molecule_id\":\"task-20260413-dfd8\",\"status\":\"completed\"}\n",
    )
    .unwrap();
    fs::write(mol.join("responses/torvalds.md"), b"# torvalds\nack\n").unwrap();
    fs::write(mol.join("responses/knuth.md"), b"# knuth\nack\n").unwrap();
}

fn assert_round_trips(mol: &std::path::Path, entry: &std::path::Path) {
    for name in ARCHIVED_FILES {
        assert!(entry.join(name).is_file(), "missing archived file: {name}");
        let orig = fs::read(mol.join(name)).unwrap();
        let arch = fs::read(entry.join(name)).unwrap();
        assert_eq!(orig, arch, "archive drift for {name}");
    }
    assert!(entry.join("manifest.json").is_file(), "manifest missing");
}

fn assert_chain_is_valid(manifest: &Manifest, entry: &std::path::Path) {
    let mut prev = Hash::of_bytes(&[]);
    for (i, f) in manifest.files.iter().enumerate() {
        let bytes = fs::read(entry.join(&f.name)).unwrap();
        assert_eq!(
            f.content_hash,
            Hash::of_bytes(&bytes),
            "content hash mismatch at index {i} ({})",
            f.name,
        );
        assert_eq!(f.size, bytes.len() as u64);
        assert_eq!(
            f.prev_hash, prev,
            "prev_hash broken at index {i} ({})",
            f.name,
        );
        prev = f.chain_hash;
    }
    assert_eq!(manifest.chain_head, prev);
    assert_eq!(recompute_chain_head(&manifest.files), manifest.chain_head);
}

#[test]
fn write_captures_all_artifacts_and_hash_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join(".cosmon").join("archive");
    let mol = tmp.path().join("mol-task-20260413-dfd8");
    populate_fake_molecule(&mol);

    let when = Utc.with_ymd_and_hms(2026, 4, 13, 18, 0, 0).unwrap();
    let out = write(&archive, &mol, "task-20260413-dfd8", when, Trigger::Done).unwrap();

    let entry = archive.join("2026/04/task-20260413-dfd8");
    assert_eq!(out.entry_dir, entry);
    assert_round_trips(&mol, &entry);

    // SCHEMA_VERSION stamped at archive root.
    assert_eq!(
        fs::read_to_string(archive.join("SCHEMA_VERSION")).unwrap(),
        format!("{SCHEMA_VERSION}\n"),
    );

    let manifest_bytes = fs::read(entry.join("manifest.json")).unwrap();
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();

    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert_eq!(manifest.molecule_id, "task-20260413-dfd8");
    assert_eq!(manifest.trigger, Trigger::Done);
    assert_eq!(manifest.archived_at, when);

    let mut expected: Vec<String> = ARCHIVED_FILES.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    let names: Vec<String> = manifest.files.iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, expected);

    assert_chain_is_valid(&manifest, &entry);

    // Idempotent re-run on unchanged sources.
    let again = write(&archive, &mol, "task-20260413-dfd8", when, Trigger::Done).unwrap();
    assert_eq!(again.manifest, manifest);
    let again_bytes = fs::read(entry.join("manifest.json")).unwrap();
    let again_manifest: Manifest = serde_json::from_slice(&again_bytes).unwrap();
    assert_eq!(again_manifest.chain_head, manifest.chain_head);
    assert_eq!(
        again_manifest.files, manifest.files,
        "hash chain must be deterministic for unchanged sources",
    );
}

#[test]
fn missing_optional_files_are_skipped_not_errored() {
    // A deliberation molecule may have `synthesis.md` but no `responses/`.
    // A task molecule may have `log.md` but no `synthesis.md`. Either
    // way, the writer must accept the subset and emit a manifest
    // listing only the files that exist.
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("archive");
    let mol = tmp.path().join("partial");
    fs::create_dir_all(&mol).unwrap();
    fs::write(mol.join("prompt.md"), b"p\n").unwrap();
    fs::write(mol.join("state.json"), b"{}\n").unwrap();

    let when = Utc.with_ymd_and_hms(2026, 4, 13, 12, 0, 0).unwrap();
    let out = write(&archive, &mol, "task-partial", when, Trigger::Collapse).unwrap();

    let names: Vec<_> = out.manifest.files.iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, vec!["prompt.md".to_owned(), "state.json".to_owned()]);
    assert_eq!(out.manifest.trigger, Trigger::Collapse);
    assert_eq!(
        recompute_chain_head(&out.manifest.files),
        out.manifest.chain_head
    );
}
