// SPDX-License-Identifier: AGPL-3.0-only

//! Retention policy for the archive — the engine behind `cs archive prune`.
//!
//! The archive grows forever by design: every terminal molecule lands a
//! canonical, hash-sealed entry under `.cosmon/state/archive/YYYY/MM/<id>/`.
//! On a long-running project this accumulates quickly — a fleet that ships
//! a hundred molecules a month carries hundreds of megabytes of prompt /
//! briefing / synthesis markdown after a year.
//!
//! Retention is the controlled escape valve: operators declare a policy in
//! `config.toml` ([`RetentionConfig`]), and `cs archive prune` uses this
//! module to pick candidates, respect integrity, and execute the deletes.
//!
//! # Pipeline
//!
//! ```text
//! scan_entries(root)  ──►  Vec<ArchiveEntry>
//!         │
//!         ▼
//! plan(entries, policy)  ──►  Plan { deletions, kept, protected }
//!         │
//!         ▼
//! execute(plan)          ──►  fs::remove_dir_all on each deletion
//! ```
//!
//! # Integrity
//!
//! Every candidate is checked against the hash-chain relationship of kept
//! entries. If molecule *B* is kept and *B*'s archived state references
//! molecule *A* as a parent (`DecayedFrom`, `BlockedBy`, or `MergedFrom`),
//! then *A* is promoted to kept even if policy would otherwise have
//! deleted it. The BFS closure runs until fixpoint so a chain
//! *C* → *B* → *A* survives when only *C* passes the policy.
//!
//! # Non-destructive planning
//!
//! [`plan`] is a pure function — it never touches the filesystem beyond
//! scanning. `cs archive prune --dry-run` prints the plan and exits;
//! `cs archive prune` then calls [`execute`]. Splitting scan/plan/execute
//! keeps the retention logic fully covered by unit tests without having
//! to carve fake filesystems.
//!
//! [`RetentionConfig`]: cosmon_core::config::RetentionConfig

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use cosmon_core::config::RetentionConfig;
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::kind::MoleculeKind;
use serde::{Deserialize, Serialize};

/// One archive entry, as discovered by [`scan_entries`].
///
/// Carries everything [`plan`] needs to decide the entry's fate:
/// identity, position in the YYYY/MM layout, size on disk, archived
/// timestamp, kind, and the parent references that must survive as
/// hash-chain integrity anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Canonical path: `<archive_root>/YYYY/MM/<molecule_id>/`.
    pub path: PathBuf,
    /// Molecule identifier (the directory's file name).
    pub molecule_id: String,
    /// Archive year (from the `YYYY/` directory).
    pub year: i32,
    /// Archive month, 1..=12.
    pub month: u32,
    /// Total size of every regular file in the entry directory, in bytes.
    pub size_bytes: u64,
    /// Time the entry was archived, as recovered from `events.jsonl`'s
    /// oldest line. Falls back to midnight on the 1st of the
    /// directory's month when the events file is missing or unparseable.
    pub archived_at: DateTime<Utc>,
    /// Molecule kind, as recovered from `molecule.json`. `None` when
    /// the kind was not set (legacy molecules).
    pub kind: Option<MoleculeKind>,
    /// Molecule identifiers this entry references as parents in the
    /// hash-chain sense: `DecayedFrom.id`, `BlockedBy.source`,
    /// `MergedFrom.ids`. `plan` uses this list to run the integrity
    /// closure — no kept molecule is ever orphaned.
    pub parents: Vec<MoleculeId>,
}

/// Categorisation of a scanned entry in a retention plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fate {
    /// The entry is **not** a deletion candidate under the policy.
    KeptByPolicy,
    /// The entry would have been deleted, but is the parent of a kept
    /// entry — promoted by the hash-chain integrity guard.
    KeptByIntegrity,
    /// The entry is scheduled for deletion.
    Delete,
}

/// One row of the retention plan — `(entry, fate, reason)`.
///
/// `reason` is a human-readable sentence explaining *why* `fate` was
/// assigned. `cs archive prune --dry-run` prints these verbatim so the
/// operator can audit the policy before arming it.
#[derive(Debug, Clone)]
pub struct PlanRow {
    /// The scanned entry.
    pub entry: ArchiveEntry,
    /// Decision assigned to this entry.
    pub fate: Fate,
    /// Human-readable explanation.
    pub reason: String,
}

/// The full retention plan — every scanned entry plus aggregate stats.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Every scanned entry, in deterministic (`archived_at`, id) order.
    pub rows: Vec<PlanRow>,
    /// Archive total size before the plan executes, in bytes.
    pub bytes_before: u64,
    /// Archive total size after executing every [`Fate::Delete`] row, in bytes.
    pub bytes_after: u64,
    /// Count of rows by fate.
    pub kept: usize,
    /// Count of rows promoted from deletion to kept by integrity.
    pub promoted: usize,
    /// Count of rows scheduled for deletion.
    pub deletions: usize,
}

impl Plan {
    /// Returns the subset of rows whose fate is [`Fate::Delete`].
    #[must_use]
    pub fn deletions(&self) -> Vec<&PlanRow> {
        self.rows
            .iter()
            .filter(|r| r.fate == Fate::Delete)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// Walk `.cosmon/state/archive/` and return one [`ArchiveEntry`] per
/// molecule directory.
///
/// Returns entries sorted by (year, month, `molecule_id`) so a subsequent
/// plan is deterministic even across filesystem orderings. A missing
/// archive root is **not** an error — it yields an empty vector so
/// `cs archive prune --dry-run` on a fresh project prints "nothing to
/// prune" instead of stack-tracing.
///
/// # Errors
///
/// Propagates `std::io::Error` only when a subdirectory cannot be read
/// after the root was found (permission denied, broken symlink, etc.).
pub fn scan_entries(archive_root: &Path) -> std::io::Result<Vec<ArchiveEntry>> {
    let mut out = Vec::new();
    if !archive_root.is_dir() {
        return Ok(out);
    }
    for year_dir in read_sorted_dirs(archive_root)? {
        let Some(year) = parse_year(&year_dir) else {
            continue;
        };
        for month_dir in read_sorted_dirs(&year_dir)? {
            let Some(month) = parse_month(&month_dir) else {
                continue;
            };
            for mol_dir in read_sorted_dirs(&month_dir)? {
                if !mol_dir.is_dir() {
                    continue;
                }
                let molecule_id = mol_dir
                    .file_name()
                    .and_then(|n| n.to_str().map(str::to_owned))
                    .unwrap_or_default();
                if molecule_id.is_empty() {
                    continue;
                }
                let size_bytes = dir_size_bytes(&mol_dir)?;
                let archived_at = read_archived_at(&mol_dir, year, month);
                let (kind, parents) = read_molecule_json(&mol_dir);
                out.push(ArchiveEntry {
                    path: mol_dir,
                    molecule_id,
                    year,
                    month,
                    size_bytes,
                    archived_at,
                    kind,
                    parents,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.archived_at
            .cmp(&b.archived_at)
            .then_with(|| a.molecule_id.cmp(&b.molecule_id))
    });
    Ok(out)
}

fn read_sorted_dirs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut v: Vec<_> = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    v.sort();
    Ok(v)
}

fn parse_year(path: &Path) -> Option<i32> {
    path.file_name()?.to_str()?.parse().ok()
}

fn parse_month(path: &Path) -> Option<u32> {
    let s = path.file_name()?.to_str()?;
    let m: u32 = s.parse().ok()?;
    (1..=12).contains(&m).then_some(m)
}

fn dir_size_bytes(root: &Path) -> std::io::Result<u64> {
    let mut total: u64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

fn read_archived_at(entry_dir: &Path, year: i32, month: u32) -> DateTime<Utc> {
    // Prefer the first line of events.jsonl — that was the archival
    // transition. Fall back to the month directory (which has day-level
    // ambiguity only, but suffices for max_age_days policies).
    if let Ok(bytes) = fs::read(entry_dir.join("events.jsonl")) {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(at) = v.get("at").and_then(|a| a.as_str()) {
                        if let Ok(dt) = DateTime::parse_from_rfc3339(at) {
                            return dt.with_timezone(&Utc);
                        }
                    }
                }
            }
        }
    }
    // Deterministic fallback: midnight on the 1st of the archive month.
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

fn read_molecule_json(entry_dir: &Path) -> (Option<MoleculeKind>, Vec<MoleculeId>) {
    let Ok(bytes) = fs::read(entry_dir.join("molecule.json")) else {
        return (None, Vec::new());
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return (None, Vec::new());
    };
    let kind = value
        .get("kind")
        .and_then(|k| k.as_str())
        .and_then(|s| s.parse::<MoleculeKind>().ok());
    let mut parents = Vec::new();
    if let Some(arr) = value.get("typed_links").and_then(|l| l.as_array()) {
        for link_value in arr {
            if let Ok(link) = serde_json::from_value::<MoleculeLink>(link_value.clone()) {
                match link {
                    MoleculeLink::DecayedFrom { id } => parents.push(id),
                    MoleculeLink::BlockedBy { source } => parents.push(source),
                    MoleculeLink::MergedFrom { ids } => parents.extend(ids),
                    _ => {}
                }
            }
        }
    }
    (kind, parents)
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// Compute a retention plan from the scanned entries and the operator's
/// policy.
///
/// Pure function: no filesystem mutation. Ordering guarantees:
///
/// 1. Entries whose kind is in `policy.keep_kinds` are always kept.
/// 2. With `keep_all = true`, every entry is kept regardless of age or size.
/// 3. Entries older than `policy.max_age_days` (when that is `> 0`) become
///    deletion candidates.
/// 4. If the total size still exceeds `policy.max_total_mb * 1 MiB` after
///    the age sweep, the oldest remaining non-kept entries become
///    deletion candidates until the total drops under the budget.
/// 5. Finally, the hash-chain integrity closure runs: any parent of a
///    kept entry is promoted back to kept ([`Fate::KeptByIntegrity`]).
///
/// `now` is injected so tests can freeze time; production callers pass
/// `Utc::now()`.
#[must_use]
pub fn plan(entries: &[ArchiveEntry], policy: &RetentionConfig, now: DateTime<Utc>) -> Plan {
    let bytes_before: u64 = entries.iter().map(|e| e.size_bytes).sum();
    let keep_kinds: HashSet<MoleculeKind> = policy
        .keep_kinds
        .iter()
        .filter_map(|s| s.parse::<MoleculeKind>().ok())
        .collect();

    let mut status = classify_by_age(entries, policy, &keep_kinds, now);
    apply_size_budget(entries, &mut status, policy, &keep_kinds, bytes_before);
    let promoted = apply_integrity_closure(entries, &mut status);

    let mut rows = Vec::with_capacity(entries.len());
    for e in entries {
        let (fate, reason) = status
            .remove(&e.molecule_id)
            .unwrap_or((Fate::KeptByPolicy, "uncategorized".to_owned()));
        rows.push(PlanRow {
            entry: e.clone(),
            fate,
            reason,
        });
    }

    let kept = rows.iter().filter(|r| r.fate != Fate::Delete).count();
    let deletions = rows.len() - kept;
    let bytes_after: u64 = rows
        .iter()
        .filter(|r| r.fate != Fate::Delete)
        .map(|r| r.entry.size_bytes)
        .sum();

    Plan {
        rows,
        bytes_before,
        bytes_after,
        kept,
        promoted,
        deletions,
    }
}

/// First pass: classify each entry by `keep_all`, `keep_kinds`, then age.
fn classify_by_age(
    entries: &[ArchiveEntry],
    policy: &RetentionConfig,
    keep_kinds: &HashSet<MoleculeKind>,
    now: DateTime<Utc>,
) -> BTreeMap<String, (Fate, String)> {
    let mut status: BTreeMap<String, (Fate, String)> = BTreeMap::new();
    let max_age = policy.max_age_days;
    for e in entries {
        if policy.keep_all {
            status.insert(
                e.molecule_id.clone(),
                (Fate::KeptByPolicy, "keep_all = true".to_owned()),
            );
            continue;
        }
        if let Some(k) = e.kind {
            if keep_kinds.contains(&k) {
                status.insert(
                    e.molecule_id.clone(),
                    (
                        Fate::KeptByPolicy,
                        format!("kind `{k}` listed in keep_kinds"),
                    ),
                );
                continue;
            }
        }
        let age_days = age_in_days(now, e.archived_at);
        if max_age > 0 && age_days > i64::from(max_age) {
            status.insert(
                e.molecule_id.clone(),
                (
                    Fate::Delete,
                    format!("older than max_age_days ({age_days} > {max_age})"),
                ),
            );
            continue;
        }
        status.insert(
            e.molecule_id.clone(),
            (Fate::KeptByPolicy, "within retention window".to_owned()),
        );
    }
    status
}

/// Second pass: evict oldest non-kept entries until the running total
/// drops under `max_total_mb`. Protected kinds are never evicted by
/// size pressure.
fn apply_size_budget(
    entries: &[ArchiveEntry],
    status: &mut BTreeMap<String, (Fate, String)>,
    policy: &RetentionConfig,
    keep_kinds: &HashSet<MoleculeKind>,
    bytes_before: u64,
) {
    if policy.keep_all {
        return;
    }
    let max_bytes: u64 = policy.max_total_mb.saturating_mul(1024 * 1024);
    if max_bytes == 0 {
        return;
    }
    let mut running: u64 = entries
        .iter()
        .filter(|e| status.get(&e.molecule_id).map(|(f, _)| *f) != Some(Fate::Delete))
        .map(|e| e.size_bytes)
        .sum();
    if running <= max_bytes {
        return;
    }
    for e in entries {
        if running <= max_bytes {
            break;
        }
        let Some((fate, _)) = status.get(&e.molecule_id) else {
            continue;
        };
        if *fate != Fate::KeptByPolicy {
            continue;
        }
        if let Some(k) = e.kind {
            if keep_kinds.contains(&k) {
                continue;
            }
        }
        running = running.saturating_sub(e.size_bytes);
        status.insert(
            e.molecule_id.clone(),
            (
                Fate::Delete,
                format!(
                    "archive total {} > max_total_mb ({} MiB)",
                    format_mib(bytes_before),
                    policy.max_total_mb,
                ),
            ),
        );
    }
}

/// Third pass: BFS from every currently-kept entry, promoting each of
/// their in-archive parents back to [`Fate::KeptByIntegrity`]. Returns
/// the number of entries that were promoted.
fn apply_integrity_closure(
    entries: &[ArchiveEntry],
    status: &mut BTreeMap<String, (Fate, String)>,
) -> usize {
    let id_index: BTreeMap<String, &ArchiveEntry> =
        entries.iter().map(|e| (e.molecule_id.clone(), e)).collect();
    let mut kept_ids: HashSet<String> = status
        .iter()
        .filter(|(_, (f, _))| *f != Fate::Delete)
        .map(|(id, _)| id.clone())
        .collect();
    let mut queue: VecDeque<String> = kept_ids.iter().cloned().collect();
    let mut promoted: BTreeSet<String> = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        let Some(entry) = id_index.get(&id) else {
            continue;
        };
        for parent in &entry.parents {
            let parent_str = parent.as_str().to_owned();
            if !id_index.contains_key(&parent_str) {
                continue;
            }
            if kept_ids.insert(parent_str.clone()) {
                if let Some(slot) = status.get_mut(&parent_str) {
                    if slot.0 == Fate::Delete {
                        promoted.insert(parent_str.clone());
                        *slot = (
                            Fate::KeptByIntegrity,
                            format!("referenced as parent by kept molecule `{id}`"),
                        );
                    }
                }
                queue.push_back(parent_str);
            }
        }
    }
    promoted.len()
}

fn age_in_days(now: DateTime<Utc>, archived_at: DateTime<Utc>) -> i64 {
    (now - archived_at).num_days()
}

fn format_mib(bytes: u64) -> String {
    // Precision for display only; archives are bounded by operator
    // config well below 2^53 bytes (8 EiB).
    #[allow(clippy::cast_precision_loss)]
    let mib = bytes as f64 / (1024.0 * 1024.0);
    format!("{mib:.1} MiB")
}

// ---------------------------------------------------------------------------
// Execute
// ---------------------------------------------------------------------------

/// Remove every [`Fate::Delete`] entry from disk. Returns the list of
/// molecule ids that were actually deleted.
///
/// Failures on a single entry are reported via the `warnings` callback
/// and do not short-circuit the sweep — a permission error on one
/// directory must not block the prune from reclaiming the rest.
pub fn execute<F>(plan: &Plan, mut warnings: F) -> Vec<String>
where
    F: FnMut(&str, std::io::Error),
{
    let mut deleted = Vec::with_capacity(plan.deletions);
    for row in plan.deletions() {
        match fs::remove_dir_all(&row.entry.path) {
            Ok(()) => deleted.push(row.entry.molecule_id.clone()),
            Err(e) => warnings(&row.entry.molecule_id, e),
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    use cosmon_core::id::MoleculeId;

    fn entry(
        id: &str,
        year: i32,
        month: u32,
        size: u64,
        archived_at: DateTime<Utc>,
        kind: Option<MoleculeKind>,
        parents: Vec<&str>,
    ) -> ArchiveEntry {
        ArchiveEntry {
            path: PathBuf::from(format!("/archive/{year:04}/{month:02}/{id}")),
            molecule_id: id.to_owned(),
            year,
            month,
            size_bytes: size,
            archived_at,
            kind,
            parents: parents
                .into_iter()
                .map(|s| MoleculeId::new(s).unwrap())
                .collect(),
        }
    }

    fn default_policy() -> RetentionConfig {
        RetentionConfig::default()
    }

    fn policy(
        keep_all: bool,
        max_age_days: u32,
        max_total_mb: u64,
        keep_kinds: &[&str],
    ) -> RetentionConfig {
        let mut p = RetentionConfig::default();
        p.keep_all = keep_all;
        p.max_age_days = max_age_days;
        p.max_total_mb = max_total_mb;
        p.keep_kinds = keep_kinds.iter().map(|s| (*s).to_owned()).collect();
        p
    }

    #[test]
    fn keep_all_true_keeps_everything() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![entry(
            "task-20200101-aaaa",
            2020,
            1,
            1024,
            old,
            Some(MoleculeKind::Task),
            vec![],
        )];
        let plan = plan(&entries, &default_policy(), now);
        assert_eq!(plan.deletions, 0);
        assert_eq!(plan.rows[0].fate, Fate::KeptByPolicy);
        assert!(plan.rows[0].reason.contains("keep_all"));
    }

    #[test]
    fn max_age_days_evicts_old_tasks() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let old = Utc.with_ymd_and_hms(2029, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2029, 12, 15, 0, 0, 0).unwrap();
        let entries = vec![
            entry(
                "task-20290101-aaaa",
                2029,
                1,
                10,
                old,
                Some(MoleculeKind::Task),
                vec![],
            ),
            entry(
                "task-20291215-bbbb",
                2029,
                12,
                10,
                recent,
                Some(MoleculeKind::Task),
                vec![],
            ),
        ];
        let pol = policy(false, 30, 0, &[]);
        let plan = plan(&entries, &pol, now);
        assert_eq!(plan.deletions, 1);
        assert_eq!(plan.rows[0].fate, Fate::Delete);
        assert_eq!(plan.rows[0].entry.molecule_id, "task-20290101-aaaa");
        assert_eq!(plan.rows[1].fate, Fate::KeptByPolicy);
    }

    #[test]
    fn keep_kinds_overrides_age() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let ancient = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![entry(
            "decision-20200101-abcd",
            2020,
            1,
            10,
            ancient,
            Some(MoleculeKind::Decision),
            vec![],
        )];
        let pol = policy(false, 30, 0, &["decision"]);
        let plan = plan(&entries, &pol, now);
        assert_eq!(plan.deletions, 0);
        assert_eq!(plan.rows[0].fate, Fate::KeptByPolicy);
        assert!(plan.rows[0].reason.contains("keep_kinds"));
    }

    #[test]
    fn integrity_promotes_parent_of_kept_molecule() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2029, 12, 15, 0, 0, 0).unwrap();
        // Parent (old task) would be deleted; child (recent decision)
        // references it via DecayedFrom and is kept by keep_kinds.
        // Integrity must promote the parent.
        let entries = vec![
            entry(
                "task-20200101-ppp1",
                2020,
                1,
                10,
                old,
                Some(MoleculeKind::Task),
                vec![],
            ),
            entry(
                "decision-20291215-ccc1",
                2029,
                12,
                10,
                recent,
                Some(MoleculeKind::Decision),
                vec!["task-20200101-ppp1"],
            ),
        ];
        let pol = policy(false, 30, 0, &["decision"]);
        let plan = plan(&entries, &pol, now);
        assert_eq!(plan.deletions, 0, "parent must not be deleted");
        assert_eq!(plan.promoted, 1);
        let parent_row = plan
            .rows
            .iter()
            .find(|r| r.entry.molecule_id == "task-20200101-ppp1")
            .unwrap();
        assert_eq!(parent_row.fate, Fate::KeptByIntegrity);
        assert!(parent_row.reason.contains("referenced as parent"));
    }

    #[test]
    fn integrity_is_transitive() {
        // Grandparent → parent → child chain, only grandchild is kept
        // by keep_kinds. Both ancestors must be promoted.
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let ancient = Utc.with_ymd_and_hms(2019, 1, 1, 0, 0, 0).unwrap();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2029, 12, 15, 0, 0, 0).unwrap();
        let entries = vec![
            entry(
                "task-20190101-aaaa",
                2019,
                1,
                10,
                ancient,
                Some(MoleculeKind::Task),
                vec![],
            ),
            entry(
                "task-20200101-bbbb",
                2020,
                1,
                10,
                old,
                Some(MoleculeKind::Task),
                vec!["task-20190101-aaaa"],
            ),
            entry(
                "decision-20291215-cccc",
                2029,
                12,
                10,
                recent,
                Some(MoleculeKind::Decision),
                vec!["task-20200101-bbbb"],
            ),
        ];
        let pol = policy(false, 30, 0, &["decision"]);
        let plan = plan(&entries, &pol, now);
        assert_eq!(plan.deletions, 0);
        assert_eq!(plan.promoted, 2);
    }

    #[test]
    fn max_total_mb_evicts_oldest_first() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2029, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2029, 6, 1, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2029, 12, 1, 0, 0, 0).unwrap();
        let mib: u64 = 1024 * 1024;
        let entries = vec![
            entry(
                "task-20290101-aaaa",
                2029,
                1,
                2 * mib,
                t1,
                Some(MoleculeKind::Task),
                vec![],
            ),
            entry(
                "task-20290601-bbbb",
                2029,
                6,
                2 * mib,
                t2,
                Some(MoleculeKind::Task),
                vec![],
            ),
            entry(
                "task-20291201-cccc",
                2029,
                12,
                2 * mib,
                t3,
                Some(MoleculeKind::Task),
                vec![],
            ),
        ];
        // Total = 6 MiB; cap at 3 MiB. Expect 2 oldest deleted.
        let pol = policy(false, 0, 3, &[]);
        let plan = plan(&entries, &pol, now);
        assert_eq!(plan.deletions, 2);
        assert_eq!(plan.rows[0].fate, Fate::Delete); // oldest
        assert_eq!(plan.rows[1].fate, Fate::Delete);
        assert_eq!(plan.rows[2].fate, Fate::KeptByPolicy);
    }

    #[test]
    fn max_total_mb_does_not_delete_keep_kinds() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2029, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2029, 6, 1, 0, 0, 0).unwrap();
        let mib: u64 = 1024 * 1024;
        let entries = vec![
            entry(
                "decision-20290101-aaaa",
                2029,
                1,
                5 * mib,
                t1,
                Some(MoleculeKind::Decision),
                vec![],
            ),
            entry(
                "task-20290601-bbbb",
                2029,
                6,
                mib,
                t2,
                Some(MoleculeKind::Task),
                vec![],
            ),
        ];
        let pol = policy(false, 0, 2, &["decision"]);
        let plan = plan(&entries, &pol, now);
        // Decision is 5 MiB, protected; task is 1 MiB — well under cap
        // *for the non-protected pool*. Cap is on total; the protected
        // decision eats the whole budget. Task stays (it is within
        // retention window + no age rule active).
        let decision_row = plan
            .rows
            .iter()
            .find(|r| r.entry.molecule_id == "decision-20290101-aaaa")
            .unwrap();
        assert_eq!(decision_row.fate, Fate::KeptByPolicy);
    }

    #[test]
    fn entries_with_unknown_kind_are_not_protected() {
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let entries = vec![entry("task-20200101-aaaa", 2020, 1, 10, old, None, vec![])];
        let pol = policy(false, 30, 0, &["decision"]);
        let plan = plan(&entries, &pol, now);
        assert_eq!(plan.deletions, 1);
    }

    #[test]
    fn plan_is_deterministic_order() {
        // Out-of-order input must not change the row order: scan_entries
        // sorts, plan() preserves.
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let t_early = Utc.with_ymd_and_hms(2029, 1, 1, 0, 0, 0).unwrap();
        let t_late = Utc.with_ymd_and_hms(2029, 12, 1, 0, 0, 0).unwrap();
        // Intentionally reversed by timestamp, then sorted by scan_entries.
        let mut entries = vec![
            entry(
                "task-20291201-bbbb",
                2029,
                12,
                10,
                t_late,
                Some(MoleculeKind::Task),
                vec![],
            ),
            entry(
                "task-20290101-aaaa",
                2029,
                1,
                10,
                t_early,
                Some(MoleculeKind::Task),
                vec![],
            ),
        ];
        entries.sort_by(|a, b| {
            a.archived_at
                .cmp(&b.archived_at)
                .then_with(|| a.molecule_id.cmp(&b.molecule_id))
        });
        let plan = plan(&entries, &default_policy(), now);
        assert_eq!(plan.rows[0].entry.molecule_id, "task-20290101-aaaa");
        assert_eq!(plan.rows[1].entry.molecule_id, "task-20291201-bbbb");
    }

    #[test]
    fn scan_missing_root_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("does-not-exist");
        let entries = scan_entries(&root).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn scan_reads_real_archive_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        let entry_dir = root.join("2029/01/task-20290101-aaaa");
        fs::create_dir_all(&entry_dir).unwrap();
        fs::write(
            entry_dir.join("molecule.json"),
            r#"{"id":"task-20290101-aaaa","kind":"task","typed_links":[{"rel":"blocked_by","source":"task-20280101-bbbb"}]}"#,
        )
        .unwrap();
        fs::write(
            entry_dir.join("events.jsonl"),
            "{\"at\":\"2029-01-15T12:00:00Z\"}\n",
        )
        .unwrap();
        let entries = scan_entries(&root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, Some(MoleculeKind::Task));
        assert_eq!(entries[0].parents.len(), 1);
        assert_eq!(
            entries[0].archived_at,
            Utc.with_ymd_and_hms(2029, 1, 15, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn execute_removes_deletion_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let target = root.join("2020/01/task-20200101-aaaa");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("molecule.json"), "{}").unwrap();
        let plan = Plan {
            rows: vec![PlanRow {
                entry: ArchiveEntry {
                    path: target.clone(),
                    molecule_id: "task-20200101-aaaa".to_owned(),
                    year: 2020,
                    month: 1,
                    size_bytes: 2,
                    archived_at: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
                    kind: Some(MoleculeKind::Task),
                    parents: vec![],
                },
                fate: Fate::Delete,
                reason: "test".to_owned(),
            }],
            bytes_before: 2,
            bytes_after: 0,
            kept: 0,
            promoted: 0,
            deletions: 1,
        };
        let deleted = execute(&plan, |_, _| {});
        assert_eq!(deleted, vec!["task-20200101-aaaa".to_owned()]);
        assert!(!target.exists());
    }
}
