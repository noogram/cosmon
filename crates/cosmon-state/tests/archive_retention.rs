// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end integration test for archive retention + prune (ADR-030 M4).
//!
//! Builds a realistic archive layout — three molecules, one young decision
//! that references an old task as `DecayedFrom` — and runs the full
//! `scan → plan → execute` pipeline against the actual filesystem. The
//! test verifies:
//!
//! 1. The scan recovers the kind, parents, and archived-at timestamp from
//!    `molecule.json` + `events.jsonl`.
//! 2. The plan respects `keep_kinds`, `max_age_days`, and the hash-chain
//!    integrity guard (a parent of a kept molecule is promoted, never
//!    deleted).
//! 3. `execute` deletes exactly the expected directories on disk and
//!    leaves everything else intact.

use std::fs;
use std::path::Path;

use chrono::{TimeZone, Utc};
use cosmon_core::config::RetentionConfig;
use cosmon_state::archive::retention::{execute, plan, scan_entries, Fate};

#[allow(clippy::too_many_arguments)]
fn write_entry(
    archive_root: &Path,
    year: i32,
    month: u32,
    mol_id: &str,
    kind: &str,
    archived_at: &str,
    typed_links_json: &str,
    response_bytes: &[u8],
) {
    let dir = archive_root
        .join(format!("{year:04}"))
        .join(format!("{month:02}"))
        .join(mol_id);
    fs::create_dir_all(dir.join("responses")).unwrap();
    fs::write(
        dir.join("molecule.json"),
        format!(r#"{{"id":"{mol_id}","kind":"{kind}","typed_links":{typed_links_json}}}"#),
    )
    .unwrap();
    fs::write(
        dir.join("events.jsonl"),
        format!("{{\"at\":\"{archived_at}\"}}\n"),
    )
    .unwrap();
    fs::write(
        dir.join("manifest.json"),
        format!(
            r#"{{"schema_version":"1","formula_pin":"task-work","molecule_id":"{mol_id}","status":"completed"}}"#
        ),
    )
    .unwrap();
    fs::write(dir.join("responses/body.md"), response_bytes).unwrap();
}

#[test]
fn prune_respects_keep_kinds_age_and_integrity() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("archive");

    // Very old task — deletion candidate under max_age_days.
    write_entry(
        &archive,
        2024,
        1,
        "task-20240101-aaaa",
        "task",
        "2024-01-15T00:00:00Z",
        "[]",
        b"old task response body\n",
    );
    // Recent decision — kept by keep_kinds, references the old task as
    // DecayedFrom. Integrity must promote the task back to KeptByIntegrity.
    write_entry(
        &archive,
        2026,
        3,
        "decision-20260315-bbbb",
        "decision",
        "2026-03-15T00:00:00Z",
        r#"[{"rel":"decayed_from","id":"task-20240101-aaaa"}]"#,
        b"decision body\n",
    );
    // Unrelated old task — no references, deletion candidate.
    write_entry(
        &archive,
        2024,
        2,
        "task-20240215-cccc",
        "task",
        "2024-02-15T00:00:00Z",
        "[]",
        b"orphan task body\n",
    );

    let entries = scan_entries(&archive).unwrap();
    assert_eq!(entries.len(), 3);

    let mut policy = RetentionConfig::default();
    policy.keep_all = false;
    policy.max_age_days = 180;
    policy.max_total_mb = 0;
    policy.keep_kinds = vec!["decision".to_owned()];

    let now = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
    let plan = plan(&entries, &policy, now);

    assert_eq!(plan.deletions, 1, "only the orphan task should delete");
    assert_eq!(plan.promoted, 1, "DecayedFrom parent must be promoted");

    let decision_row = plan
        .rows
        .iter()
        .find(|r| r.entry.molecule_id == "decision-20260315-bbbb")
        .unwrap();
    assert_eq!(decision_row.fate, Fate::KeptByPolicy);

    let old_task_row = plan
        .rows
        .iter()
        .find(|r| r.entry.molecule_id == "task-20240101-aaaa")
        .unwrap();
    assert_eq!(old_task_row.fate, Fate::KeptByIntegrity);
    assert!(old_task_row.reason.contains("parent"));

    let orphan_row = plan
        .rows
        .iter()
        .find(|r| r.entry.molecule_id == "task-20240215-cccc")
        .unwrap();
    assert_eq!(orphan_row.fate, Fate::Delete);

    // Execute.
    let deleted = execute(&plan, |id, e| panic!("unexpected failure for {id}: {e}"));
    assert_eq!(deleted, vec!["task-20240215-cccc".to_owned()]);
    assert!(!archive.join("2024/02/task-20240215-cccc").exists());
    assert!(archive.join("2024/01/task-20240101-aaaa").is_dir());
    assert!(archive.join("2026/03/decision-20260315-bbbb").is_dir());

    // Re-scan: the plan on a pruned archive has zero deletions.
    let after = scan_entries(&archive).unwrap();
    assert_eq!(after.len(), 2);
    let second_plan = plan2(&after, &policy, now);
    assert_eq!(second_plan.deletions, 0);
}

fn plan2(
    entries: &[cosmon_state::archive::retention::ArchiveEntry],
    policy: &RetentionConfig,
    now: chrono::DateTime<Utc>,
) -> cosmon_state::archive::retention::Plan {
    cosmon_state::archive::retention::plan(entries, policy, now)
}

#[test]
fn prune_is_idempotent_in_dry_mode_semantics() {
    // Dry-run semantics are enforced by the CLI layer (it doesn't call
    // execute()). But scan + plan on the same archive must produce the
    // same plan across invocations.
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("archive");
    write_entry(
        &archive,
        2024,
        1,
        "task-20240101-aaaa",
        "task",
        "2024-01-15T00:00:00Z",
        "[]",
        b"body\n",
    );

    let mut policy = RetentionConfig::default();
    policy.keep_all = false;
    policy.max_age_days = 30;

    let now = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
    let entries = scan_entries(&archive).unwrap();
    let p1 = plan(&entries, &policy, now);
    let p2 = plan(&entries, &policy, now);
    assert_eq!(p1.deletions, p2.deletions);
    assert_eq!(p1.bytes_before, p2.bytes_before);
    assert_eq!(p1.rows.len(), p2.rows.len());
    assert_eq!(p1.rows[0].fate, p2.rows[0].fate);
}

#[test]
fn prune_keeps_everything_under_default_policy() {
    // The default policy is keep_all = true. Drop a very old task and
    // confirm the pipeline refuses to schedule it for deletion.
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("archive");
    write_entry(
        &archive,
        2019,
        1,
        "task-20190101-aaaa",
        "task",
        "2019-01-15T00:00:00Z",
        "[]",
        b"ancient\n",
    );

    let entries = scan_entries(&archive).unwrap();
    let plan = plan(&entries, &RetentionConfig::default(), Utc::now());
    assert_eq!(plan.deletions, 0);
    assert!(plan.rows[0].reason.contains("keep_all"));
}
