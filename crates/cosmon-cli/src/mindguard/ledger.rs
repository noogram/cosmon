// SPDX-License-Identifier: AGPL-3.0-only

//! Append-only ledger for mindguard overrides.
//!
//! Every `--override-mindguard-down` invocation lands here *before* the
//! wrapped operation runs. The file is JSONL (one record per line) at
//! `~/.cosmon/audit/mindguard-overrides.jsonl`. Never rewritten,
//! never trimmed — a mindguard one can contour by default when it is
//! down is a mindguard agents learn to make fall.
//!
//! Records carry:
//! - `timestamp` — UTC ISO-8601.
//! - `gate` — which gate was bypassed (currently `"surface_visual"`).
//! - `molecule_id` — the molecule whose claim was forced through.
//! - `justification` — operator-supplied free-form rationale.
//! - `underlying_error` — the [`MindguardError::Unavailable`] payload
//!   that triggered the override (so the audit reader can see *why*
//!   the gate was unreachable, not just *that* it was bypassed).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::id::MoleculeId;
use serde::{Deserialize, Serialize};

/// One line in the override ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverrideRecord {
    pub timestamp: chrono::DateTime<Utc>,
    pub gate: String,
    pub molecule_id: String,
    pub justification: String,
    pub underlying_error: String,
}

/// Default ledger path: `~/.cosmon/audit/mindguard-overrides.jsonl`.
///
/// Overridable for tests via `$COSMON_MINDGUARD_OVERRIDE_LEDGER`.
#[must_use]
pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("COSMON_MINDGUARD_OVERRIDE_LEDGER") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    Path::new(&home)
        .join(".cosmon")
        .join("audit")
        .join("mindguard-overrides.jsonl")
}

/// Append a record to the ledger at the default path.
///
/// # Errors
///
/// Returns the IO error if the parent directory cannot be created or
/// the append fails. Callers must propagate the error and *refuse the
/// override* in that case — never silently let the override proceed
/// when the ledger cannot record it.
pub fn append(
    gate: &str,
    mol_id: &MoleculeId,
    justification: &str,
    underlying_error: &str,
) -> std::io::Result<OverrideRecord> {
    append_to(
        &default_path(),
        gate,
        mol_id,
        justification,
        underlying_error,
    )
}

/// Append to an explicit path (tests, alternate audit roots).
///
/// # Errors
///
/// See [`append`].
pub fn append_to(
    path: &Path,
    gate: &str,
    mol_id: &MoleculeId,
    justification: &str,
    underlying_error: &str,
) -> std::io::Result<OverrideRecord> {
    let record = OverrideRecord {
        timestamp: Utc::now(),
        gate: gate.to_owned(),
        molecule_id: mol_id.as_str().to_owned(),
        justification: justification.to_owned(),
        underlying_error: underlying_error.to_owned(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(&record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use tempfile::TempDir;

    #[test]
    fn append_creates_parent_dirs_and_writes_one_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested/audit/overrides.jsonl");
        let mol = MoleculeId::new("task-20260527-1234").unwrap();
        let rec = append_to(&path, "surface_visual", &mol, "ledger missing", "io error").unwrap();
        assert_eq!(rec.gate, "surface_visual");
        assert_eq!(rec.molecule_id, "task-20260527-1234");

        let f = fs::File::open(&path).unwrap();
        let lines: Vec<String> = std::io::BufReader::new(f)
            .lines()
            .map(Result::unwrap)
            .collect();
        assert_eq!(lines.len(), 1);
        let parsed: OverrideRecord = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed.justification, "ledger missing");
    }

    #[test]
    fn append_is_append_only() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("overrides.jsonl");
        let mol = MoleculeId::new("task-20260527-5678").unwrap();
        append_to(&path, "surface_visual", &mol, "first", "x").unwrap();
        append_to(&path, "surface_visual", &mol, "second", "y").unwrap();
        let lines = fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(lines.contains("\"first\""));
        assert!(lines.contains("\"second\""));
    }
}
