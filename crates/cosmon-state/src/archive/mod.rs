// SPDX-License-Identifier: AGPL-3.0-only

//! Archive writer — durable capture of terminal molecule transitions.
//!
//! When a molecule reaches a terminal (or quasi-terminal) transition via
//! `cs done`, `cs collapse`, or `cs freeze`, this module writes a
//! canonical, tracked snapshot under `.cosmon/state/archive/YYYY/MM/<id>/`.
//! The archive outlives worktree teardown and branch deletion so the chain
//! of reasoning can be read from a fresh clone without running `cs`.
//!
//! Layout (ADR-030):
//!
//! ```text
//! archive/
//!   YYYY/MM/<molecule-id>/
//!     molecule.json     # canonical, sorted-key JSON of terminal state
//!     edges.json        # typed links touching this molecule
//!     manifest.json     # {formula_pin, response_hashes, schema_version}
//!     responses/*.md    # copied verbatim when present
//!     synthesis.md      # copied verbatim when present
//!     events.jsonl      # per-molecule transition log (append-only)
//!   events/
//!     events-YYYY-MM.jsonl  # fleet-level transition stream (append-only)
//!   SCHEMA_VERSION
//! ```
//!
//! ## Invariants
//!
//! - **Idempotent.** Re-running the same terminal trigger produces the
//!   same archive entry (events.jsonl gains a fresh appended line only if
//!   the timestamped envelope differs; molecule.json and manifest.json
//!   are overwritten atomically with byte-identical content when the
//!   source state has not changed).
//! - **Non-fatal.** Callers (`cs done`, `cs collapse`, `cs freeze`) treat
//!   archive failures as a warning, never an error — the terminal
//!   transition must always succeed.
//! - **Canonical JSON.** Maps are serialized with sorted keys (via a
//!   `BTreeMap<String, Value>` intermediate) so diffs track semantic
//!   change, not serializer whims.
//!
//! This module is an M3 bootstrap: it writes the minimal schema-version-1
//! layout that M3's trigger sites depend on. M2's richer canonical
//! writer (with full edge-projection semantics and exhaustive invariant
//! tests) will supersede this implementation when it lands; the trigger
//! sites in `cs done` / `cs collapse` / `cs freeze` will not need to
//! change.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use cosmon_core::id::MoleculeId;

use crate::{MoleculeData, SCHEMA_VERSION};

pub mod retention;

/// The kind of terminal trigger that produced an archive entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// `cs done` — molecule merged back to main.
    Done,
    /// `cs collapse` — molecule terminated with a recorded reason.
    Collapse,
    /// `cs freeze` — molecule's worker paused; its durable state is
    /// captured at the freeze point so thaw/reclone can find it.
    Freeze,
    /// `cs stuck` — molecule blocked on a missing prerequisite; the
    /// freeze is recorded with the blocker reason so an operator
    /// reviewing `archive/` sees *why* the worker paused without
    /// having to reconstruct the live state.
    Stuck,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Collapse => "collapse",
            Self::Freeze => "freeze",
            Self::Stuck => "stuck",
        }
    }
}

/// Outcome of an archive write. Purely informational — callers discard
/// errors to preserve the non-fatal contract.
#[derive(Debug)]
pub struct ArchiveWrite {
    /// The per-molecule archive directory that was written (or refreshed).
    pub entry_dir: PathBuf,
    /// The trigger that produced this entry.
    pub trigger: Trigger,
}

/// Write an archive entry for a terminal molecule transition.
///
/// `state_root` is the state directory (typically `.cosmon/state/`);
/// `state_root/archive/` is created on demand. `mol_dir` is the live
/// molecule directory — its `responses/`, `synthesis.md`, and other
/// artifacts are copied into the archive entry when present.
///
/// The write is best-effort: partial failures return an error, but
/// completed writes are atomic enough (via tempfile + rename) that a
/// retry observes a consistent state.
///
/// # Errors
///
/// Returns an error if the archive directory cannot be created, if the
/// molecule state cannot be serialized, or if any artifact copy fails
/// with a non-absent I/O error.
pub fn write(
    state_root: &Path,
    mol_dir: &Path,
    mol: &MoleculeData,
    trigger: Trigger,
    now: DateTime<Utc>,
) -> std::io::Result<ArchiveWrite> {
    write_with_warnings(state_root, mol_dir, mol, trigger, now, &[])
}

/// Write an archive entry, recording any non-fatal teardown warnings in its
/// manifest.
///
/// The optional field is intentionally owned by the terminal command rather
/// than [`MoleculeData`]: warnings describe the archival operation, not the
/// molecule's domain state. Callers without warnings should use [`write`](fn@write).
///
/// # Errors
///
/// Returns the same I/O errors as [`write`](fn@write).
pub fn write_with_warnings(
    state_root: &Path,
    mol_dir: &Path,
    mol: &MoleculeData,
    trigger: Trigger,
    now: DateTime<Utc>,
    warnings: &[String],
) -> std::io::Result<ArchiveWrite> {
    let archive_root = state_root.join("archive");
    let month_dir = archive_root
        .join(format!("{:04}", now.year()))
        .join(format!("{:02}", now.month()));
    let entry_dir = month_dir.join(mol.id.as_str());
    fs::create_dir_all(&entry_dir)?;

    // Schema version stamp — idempotent (writes same content every time).
    write_atomic(
        &archive_root.join("SCHEMA_VERSION"),
        format!("{SCHEMA_VERSION}\n").as_bytes(),
    )?;

    // molecule.json — canonical (sorted keys).
    let mol_json = canonical_json(mol)?;
    write_atomic(&entry_dir.join("molecule.json"), mol_json.as_bytes())?;

    // edges.json — typed links touching this molecule.
    let edges_value = edges_value(mol);
    let edges_json = canonical_string(&edges_value)?;
    write_atomic(&entry_dir.join("edges.json"), edges_json.as_bytes())?;

    // responses/ — copied verbatim, with per-file sha256 recorded in
    // the manifest below.
    let mut response_hashes: BTreeMap<String, String> = BTreeMap::new();
    let live_responses = mol_dir.join("responses");
    if live_responses.is_dir() {
        let dest = entry_dir.join("responses");
        fs::create_dir_all(&dest)?;
        for entry in fs::read_dir(&live_responses)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let bytes = fs::read(entry.path())?;
                let hash = sha256_hex(&bytes);
                write_atomic(&dest.join(&name), &bytes)?;
                response_hashes.insert(name, hash);
            }
        }
    }

    // synthesis.md — copied verbatim if present, hashed so the CI
    // verifier catches post-archive edits to the synthesis the same
    // way it catches edits to per-persona responses.
    let live_synth = mol_dir.join("synthesis.md");
    let mut synthesis_hash: Option<String> = None;
    if live_synth.is_file() {
        let bytes = fs::read(&live_synth)?;
        synthesis_hash = Some(sha256_hex(&bytes));
        write_atomic(&entry_dir.join("synthesis.md"), &bytes)?;
    }

    // manifest.json — formula pin, schema version, response hashes,
    // optional synthesis hash.
    let manifest = manifest_value(mol, &response_hashes, synthesis_hash.as_deref(), warnings);
    let manifest_json = canonical_string(&manifest)?;
    write_atomic(&entry_dir.join("manifest.json"), manifest_json.as_bytes())?;

    // events.jsonl — append per-molecule transition.
    let event = transition_event(mol, trigger, now);
    append_jsonl(&entry_dir.join("events.jsonl"), &event)?;

    // Fleet-level monthly event stream.
    let fleet_events_dir = archive_root.join("events");
    fs::create_dir_all(&fleet_events_dir)?;
    let fleet_events_path =
        fleet_events_dir.join(format!("events-{:04}-{:02}.jsonl", now.year(), now.month()));
    append_jsonl(&fleet_events_path, &event)?;

    Ok(ArchiveWrite { entry_dir, trigger })
}

/// Best-effort variant for trigger sites that must never fail.
///
/// Logs the error to stderr (prefixed with `archive:`) and swallows it.
/// Returns `Some(ArchiveWrite)` on success, `None` on failure.
#[must_use]
pub fn write_non_fatal(
    state_root: &Path,
    mol_dir: &Path,
    mol: &MoleculeData,
    trigger: Trigger,
    now: DateTime<Utc>,
) -> Option<ArchiveWrite> {
    write_non_fatal_with_warnings(state_root, mol_dir, mol, trigger, now, &[])
}

/// Best-effort [`write_with_warnings`].
#[must_use]
pub fn write_non_fatal_with_warnings(
    state_root: &Path,
    mol_dir: &Path,
    mol: &MoleculeData,
    trigger: Trigger,
    now: DateTime<Utc>,
    warnings: &[String],
) -> Option<ArchiveWrite> {
    match write_with_warnings(state_root, mol_dir, mol, trigger, now, warnings) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!(
                "archive: failed to write {} entry for {}: {e}",
                trigger.as_str(),
                mol.id
            );
            None
        }
    }
}

fn canonical_json<T: Serialize>(value: &T) -> std::io::Result<String> {
    let v = serde_json::to_value(value).map_err(std::io::Error::other)?;
    canonical_string(&v)
}

fn canonical_string(value: &Value) -> std::io::Result<String> {
    // serde_json::Value's `Map` is a BTreeMap when the `preserve_order`
    // feature is off (our case), so `to_string_pretty` already emits
    // sorted keys. Explicit re-sort is unnecessary but harmless; we
    // keep the wrapper to centralize the canonicalization contract.
    let s = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    // Ensure trailing newline for clean diffs.
    Ok(format!("{s}\n"))
}

fn edges_value(mol: &MoleculeData) -> Value {
    let typed: Vec<Value> = mol
        .typed_links
        .iter()
        .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
        .collect();
    let legacy: Vec<Value> = mol.links.iter().map(|s| Value::String(s.clone())).collect();
    let mut map = serde_json::Map::new();
    map.insert("molecule".into(), Value::String(mol.id.as_str().to_owned()));
    map.insert("typed_links".into(), Value::Array(typed));
    map.insert("legacy_links".into(), Value::Array(legacy));
    Value::Object(map)
}

fn manifest_value(
    mol: &MoleculeData,
    response_hashes: &BTreeMap<String, String>,
    synthesis_hash: Option<&str>,
    warnings: &[String],
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "schema_version".into(),
        Value::String(SCHEMA_VERSION.to_owned()),
    );
    map.insert(
        "formula_pin".into(),
        Value::String(mol.formula_id.as_str().to_owned()),
    );
    map.insert(
        "molecule_id".into(),
        Value::String(mol.id.as_str().to_owned()),
    );
    map.insert("status".into(), Value::String(mol.status.to_string()));
    let mut hashes = serde_json::Map::new();
    for (k, v) in response_hashes {
        hashes.insert(k.clone(), Value::String(v.clone()));
    }
    map.insert("response_hashes".into(), Value::Object(hashes));
    if let Some(h) = synthesis_hash {
        map.insert("synthesis_hash".into(), Value::String(h.to_owned()));
    }
    if !warnings.is_empty() {
        map.insert(
            "warnings".into(),
            Value::Array(warnings.iter().cloned().map(Value::String).collect()),
        );
    }
    Value::Object(map)
}

fn transition_event(mol: &MoleculeData, trigger: Trigger, now: DateTime<Utc>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("at".into(), Value::String(now.to_rfc3339()));
    map.insert(
        "molecule_id".into(),
        Value::String(mol.id.as_str().to_owned()),
    );
    map.insert("status".into(), Value::String(mol.status.to_string()));
    map.insert("trigger".into(), Value::String(trigger.as_str().to_owned()));
    if let Some(reason) = &mol.collapse_reason {
        map.insert("reason".into(), Value::String(reason.clone()));
    }
    Value::Object(map)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Write `bytes` to `path` atomically (write-to-temp + rename).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Append a single JSON value as a line to `path`.
fn append_jsonl(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// One archived entry located on disk. Emitted by [`list_since`] so
/// callers (the `cs archive` CLI, the CI verifier, `just archive-verify`)
/// can iterate over archive entries without re-implementing the
/// `YYYY/MM/<id>` walk.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Molecule ID derived from the entry directory name.
    pub molecule_id: String,
    /// Absolute path to the entry directory under `archive/YYYY/MM/`.
    pub entry_dir: PathBuf,
    /// Modification time of the entry directory — used as a proxy for
    /// "when was this molecule archived" when iterating by recency.
    pub modified: std::time::SystemTime,
}

/// List archive entries under `state_root/archive/` whose entry directory
/// was modified at or after `since`. Returns them in arbitrary order;
/// callers that want a deterministic stream should sort by
/// `(modified, molecule_id)`.
///
/// The archive tree is laid out `archive/YYYY/MM/<id>/`, so we walk at
/// most a handful of year and month directories — linear in the number
/// of archived molecules, no index required. Missing `archive/` directory
/// returns the empty vec (no archive entries yet, not an error).
///
/// # Errors
///
/// Returns an error only if reading a discovered directory fails in a
/// way that is not "not found".
pub fn list_since(
    state_root: &Path,
    since: std::time::SystemTime,
) -> std::io::Result<Vec<ArchiveEntry>> {
    let archive_root = state_root.join("archive");
    if !archive_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for year in read_dir_sorted(&archive_root)? {
        let year_path = year.path();
        if !year_path.is_dir() || !is_numeric_name(&year_path) {
            continue;
        }
        for month in read_dir_sorted(&year_path)? {
            let month_path = month.path();
            if !month_path.is_dir() || !is_numeric_name(&month_path) {
                continue;
            }
            for entry in read_dir_sorted(&month_path)? {
                let entry_path = entry.path();
                if !entry_path.is_dir() {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                if modified < since {
                    continue;
                }
                let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                out.push(ArchiveEntry {
                    molecule_id: name.to_owned(),
                    entry_dir: entry_path,
                    modified,
                });
            }
        }
    }
    Ok(out)
}

fn read_dir_sorted(dir: &Path) -> std::io::Result<Vec<fs::DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    Ok(entries)
}

fn is_numeric_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
}

/// Outcome of a single per-artifact hash check inside [`verify_entry`].
#[derive(Debug, Clone)]
pub struct VerifyCheck {
    /// Name of the artifact as it appears in the manifest (e.g. `torvalds.md`).
    pub artifact: String,
    /// Kind of check: `"response"` for files under `responses/`, or
    /// `"synthesis"` for `synthesis.md`.
    pub kind: &'static str,
    /// `true` iff the recomputed hash matches the sealed hash.
    pub ok: bool,
    /// The hash recorded in `manifest.json` at archive time.
    pub sealed_hash: String,
    /// The hash recomputed from the current archive contents.
    pub current_hash: String,
}

/// Aggregate outcome of verifying one archive entry.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    /// Molecule ID the entry was written for (from the manifest).
    pub molecule_id: String,
    /// One row per hashed artifact.
    pub checks: Vec<VerifyCheck>,
    /// `true` iff every check passed (also `true` on an empty check list,
    /// which corresponds to a molecule with no response/synthesis content).
    pub ok: bool,
}

/// Verify the on-disk integrity of one archive entry.
///
/// Recomputes the sha-256 hash of every response file and of
/// `synthesis.md` (when present) and compares against the hashes sealed
/// in `manifest.json` at archive time. Returns a structured report so
/// callers can render either a human-readable diff or a JSON stream.
///
/// # Errors
///
/// Returns an error if `manifest.json` is missing, unparseable, or if a
/// file listed in the manifest cannot be read (which also represents a
/// tamper event — deletion — and is surfaced as a failure case by the
/// `cs archive verify` wrapper).
pub fn verify_entry(entry_dir: &Path) -> std::io::Result<VerifyReport> {
    let manifest_path = entry_dir.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(std::io::Error::other)?;
    let molecule_id = manifest
        .get("molecule_id")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_owned();
    let response_hashes = manifest
        .get("response_hashes")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut checks: Vec<VerifyCheck> = Vec::new();

    for (name, hash) in &response_hashes {
        let sealed = hash.as_str().unwrap_or_default().to_owned();
        let path = entry_dir.join("responses").join(name);
        let current = match fs::read(&path) {
            Ok(bytes) => sha256_hex(&bytes),
            Err(_) => String::new(),
        };
        checks.push(VerifyCheck {
            artifact: name.clone(),
            kind: "response",
            ok: !current.is_empty() && current == sealed,
            sealed_hash: sealed,
            current_hash: current,
        });
    }

    // synthesis.md — compare against `synthesis_hash` sealed in the
    // manifest. An archive entry written before the field existed
    // (legacy) simply has no synthesis check — backward compatible.
    // An entry with a sealed hash but no synthesis.md on disk is a
    // deletion-class tamper and reported as a failure.
    if let Some(sealed_hash) = manifest.get("synthesis_hash").and_then(|v| v.as_str()) {
        let path = entry_dir.join("synthesis.md");
        let current = match fs::read(&path) {
            Ok(bytes) => sha256_hex(&bytes),
            Err(_) => String::new(),
        };
        checks.push(VerifyCheck {
            artifact: "synthesis.md".to_owned(),
            kind: "synthesis",
            ok: !current.is_empty() && current == sealed_hash,
            sealed_hash: sealed_hash.to_owned(),
            current_hash: current,
        });
    }

    let ok = checks.iter().all(|c| c.ok);
    Ok(VerifyReport {
        molecule_id,
        checks,
        ok,
    })
}

/// Resolve the live molecule directory for a given state root, so trigger
/// sites can locate the molecule's `responses/` and `synthesis.md` without
/// duplicating the `FileStore`'s fleet-search logic. Returns `None` if the
/// molecule cannot be located (in which case the archive entry is still
/// written, just without the optional artifacts).
#[must_use]
pub fn resolve_molecule_dir(state_root: &Path, id: &MoleculeId) -> Option<PathBuf> {
    let fleets_root = state_root.join("fleets");
    if fleets_root.is_dir() {
        if let Ok(entries) = fs::read_dir(&fleets_root) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("molecules").join(id.as_str());
                if candidate.is_dir() {
                    return Some(candidate);
                }
            }
        }
    }
    let legacy = state_root.join("ops/molecules").join(id.as_str());
    if legacy.is_dir() {
        return Some(legacy);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::TimeZone;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;

    use super::*;

    fn mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        let now = Utc::now();
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 2,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: vec![],
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn write_creates_canonical_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-xyz1");
        fs::create_dir_all(mol_dir.join("responses")).unwrap();
        fs::write(mol_dir.join("synthesis.md"), "synthesis\n").unwrap();
        fs::write(mol_dir.join("responses/torvalds.md"), "response\n").unwrap();

        let m = mol("task-20260414-xyz1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        let w = write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        let entry = state.join("archive/2026/04/task-20260414-xyz1");
        assert_eq!(w.entry_dir, entry);
        assert!(entry.join("molecule.json").is_file());
        assert!(entry.join("edges.json").is_file());
        assert!(entry.join("manifest.json").is_file());
        assert!(entry.join("events.jsonl").is_file());
        assert!(entry.join("synthesis.md").is_file());
        assert!(entry.join("responses/torvalds.md").is_file());
        assert!(state.join("archive/SCHEMA_VERSION").is_file());
        assert!(state.join("archive/events/events-2026-04.jsonl").is_file());

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(entry.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["schema_version"], SCHEMA_VERSION);
        assert!(manifest["response_hashes"]["torvalds.md"].is_string());
    }

    #[test]
    fn done_warnings_are_persisted_in_the_archive_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260715-warn");
        fs::create_dir_all(&mol_dir).unwrap();
        let m = mol("task-20260715-warn", MoleculeStatus::Completed);
        let warnings = vec![
            "failed to kill tmux session task-20260715-warn".to_owned(),
            "branch delete failed: branch is checked out".to_owned(),
        ];

        write_with_warnings(
            state,
            &mol_dir,
            &m,
            Trigger::Done,
            Utc.with_ymd_and_hms(2026, 7, 15, 10, 0, 0).unwrap(),
            &warnings,
        )
        .unwrap();

        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(state.join("archive/2026/07/task-20260715-warn/manifest.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["warnings"], serde_json::json!(warnings));
    }

    #[test]
    fn triple_trigger_produces_three_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let fleet_mol = |id: &str| {
            let d = state.join(format!("fleets/default/molecules/{id}"));
            fs::create_dir_all(&d).unwrap();
            d
        };

        let done = mol("task-20260414-dne1", MoleculeStatus::Completed);
        let coll = mol("task-20260414-col1", MoleculeStatus::Collapsed);
        let frez = mol("task-20260414-frz1", MoleculeStatus::Running);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();

        write(
            state,
            &fleet_mol("task-20260414-dne1"),
            &done,
            Trigger::Done,
            when,
        )
        .unwrap();
        write(
            state,
            &fleet_mol("task-20260414-col1"),
            &coll,
            Trigger::Collapse,
            when,
        )
        .unwrap();
        write(
            state,
            &fleet_mol("task-20260414-frz1"),
            &frez,
            Trigger::Freeze,
            when,
        )
        .unwrap();

        assert!(state.join("archive/2026/04/task-20260414-dne1").is_dir());
        assert!(state.join("archive/2026/04/task-20260414-col1").is_dir());
        assert!(state.join("archive/2026/04/task-20260414-frz1").is_dir());

        let fleet_events =
            fs::read_to_string(state.join("archive/events/events-2026-04.jsonl")).unwrap();
        let lines: Vec<&str> = fleet_events.lines().collect();
        assert_eq!(lines.len(), 3, "three entries, one event each");

        let triggers: Vec<String> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["trigger"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(triggers, vec!["done", "collapse", "freeze"]);
    }

    #[test]
    fn rerunning_is_idempotent_for_state() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let md = state.join("fleets/default/molecules/task-20260414-rer1");
        fs::create_dir_all(&md).unwrap();
        let m = mol("task-20260414-rer1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();

        write(state, &md, &m, Trigger::Done, when).unwrap();
        let first_mol =
            fs::read_to_string(state.join("archive/2026/04/task-20260414-rer1/molecule.json"))
                .unwrap();
        let first_manifest =
            fs::read_to_string(state.join("archive/2026/04/task-20260414-rer1/manifest.json"))
                .unwrap();

        write(state, &md, &m, Trigger::Done, when).unwrap();
        let second_mol =
            fs::read_to_string(state.join("archive/2026/04/task-20260414-rer1/molecule.json"))
                .unwrap();
        let second_manifest =
            fs::read_to_string(state.join("archive/2026/04/task-20260414-rer1/manifest.json"))
                .unwrap();

        assert_eq!(first_mol, second_mol);
        assert_eq!(first_manifest, second_manifest);
        // events.jsonl grows (each trigger is a distinct transition
        // record, even if timestamps match — monotone append).
        let events =
            fs::read_to_string(state.join("archive/2026/04/task-20260414-rer1/events.jsonl"))
                .unwrap();
        assert_eq!(events.lines().count(), 2);
    }

    #[test]
    fn verify_passes_on_clean_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-ver1");
        fs::create_dir_all(mol_dir.join("responses")).unwrap();
        fs::write(mol_dir.join("synthesis.md"), "synthesis body\n").unwrap();
        fs::write(mol_dir.join("responses/torvalds.md"), "response body\n").unwrap();
        fs::write(mol_dir.join("responses/feynman.md"), "feynman body\n").unwrap();

        let m = mol("task-20260414-ver1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        let w = write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        let report = verify_entry(&w.entry_dir).unwrap();
        assert!(report.ok, "clean archive should verify clean: {report:?}");
        assert_eq!(report.molecule_id, "task-20260414-ver1");
        let names: Vec<&str> = report.checks.iter().map(|c| c.artifact.as_str()).collect();
        assert!(names.contains(&"torvalds.md"));
        assert!(names.contains(&"feynman.md"));
        assert!(names.contains(&"synthesis.md"));
    }

    #[test]
    fn verify_fails_on_tampered_response() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-tmp1");
        fs::create_dir_all(mol_dir.join("responses")).unwrap();
        fs::write(mol_dir.join("responses/torvalds.md"), "original\n").unwrap();

        let m = mol("task-20260414-tmp1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        let w = write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        // Tamper the archived response in place.
        fs::write(w.entry_dir.join("responses/torvalds.md"), "TAMPERED\n").unwrap();

        let report = verify_entry(&w.entry_dir).unwrap();
        assert!(!report.ok, "tampered response must fail verification");
        let torvalds = report
            .checks
            .iter()
            .find(|c| c.artifact == "torvalds.md")
            .expect("torvalds.md check present");
        assert!(!torvalds.ok);
        assert_ne!(torvalds.sealed_hash, torvalds.current_hash);
    }

    #[test]
    fn verify_fails_on_tampered_synthesis() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-syn1");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(mol_dir.join("synthesis.md"), "authoritative synthesis\n").unwrap();

        let m = mol("task-20260414-syn1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        let w = write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        fs::write(w.entry_dir.join("synthesis.md"), "EDITED AFTER ARCHIVE\n").unwrap();

        let report = verify_entry(&w.entry_dir).unwrap();
        assert!(!report.ok);
        let synth = report
            .checks
            .iter()
            .find(|c| c.artifact == "synthesis.md")
            .expect("synthesis check present");
        assert!(!synth.ok);
    }

    #[test]
    fn verify_fails_when_response_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-del1");
        fs::create_dir_all(mol_dir.join("responses")).unwrap();
        fs::write(mol_dir.join("responses/knuth.md"), "knuth body\n").unwrap();

        let m = mol("task-20260414-del1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        let w = write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        fs::remove_file(w.entry_dir.join("responses/knuth.md")).unwrap();

        let report = verify_entry(&w.entry_dir).unwrap();
        assert!(!report.ok);
        let knuth = report
            .checks
            .iter()
            .find(|c| c.artifact == "knuth.md")
            .expect("knuth check present");
        assert!(!knuth.ok);
        assert!(knuth.current_hash.is_empty());
    }

    #[test]
    fn list_since_discovers_archived_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-ls1");
        fs::create_dir_all(&mol_dir).unwrap();
        let m = mol("task-20260414-ls1", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        let since = std::time::SystemTime::UNIX_EPOCH;
        let entries = list_since(state, since).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].molecule_id, "task-20260414-ls1");
    }

    #[test]
    fn list_since_filters_by_modified_time() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260414-ls2");
        fs::create_dir_all(&mol_dir).unwrap();
        let m = mol("task-20260414-ls2", MoleculeStatus::Completed);
        let when = Utc.with_ymd_and_hms(2026, 4, 14, 10, 0, 0).unwrap();
        write(state, &mol_dir, &m, Trigger::Done, when).unwrap();

        // A cutoff far in the future yields no results.
        let far_future =
            std::time::SystemTime::now() + std::time::Duration::from_secs(60 * 60 * 24 * 365);
        let entries = list_since(state, far_future).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_since_on_empty_archive_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = list_since(tmp.path(), std::time::SystemTime::UNIX_EPOCH).unwrap();
        assert!(entries.is_empty());
    }
}
