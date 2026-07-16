// SPDX-License-Identifier: AGPL-3.0-only

//! Proof-of-work manifest — capture and replay artifact hashes and formula
//! gate outcomes for completed molecules.
//!
//! `cs complete` seals a `verify.json` manifest containing BLAKE3 hashes of
//! every markdown artifact produced by the molecule (prompt, briefing,
//! synthesis, responses, log). `cs verify` later recomputes those hashes
//! and replays shell/native gates declared in the formula, producing a
//! per-artifact PASS/FAIL chain in `verify-report.md`.
//!
//! Editing any sealed artifact after completion (e.g. tampering with
//! `synthesis.md`) produces a hash divergence that `cs verify` reports
//! and exits non-zero on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cosmon_hash::Hash;
use serde::{Deserialize, Serialize};

/// Markdown artifacts hashed by default. Missing files are skipped.
const ARTIFACTS: &[&str] = &[
    "prompt.md",
    "briefing.md",
    "frame.md",
    "synthesis.md",
    "log.md",
];

/// Sealed manifest written by `cs complete` and verified by `cs verify`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyManifest {
    /// Molecule ID this manifest seals.
    pub molecule_id: String,
    /// Formula ID the molecule was completed under.
    pub formula_id: String,
    /// RFC3339 UTC timestamp of sealing.
    pub sealed_at: String,
    /// Map of `relative/path.md` → blake3 hex hash.
    pub artifacts: BTreeMap<String, String>,
}

/// Path to the sealed manifest inside a molecule directory.
#[must_use]
pub fn manifest_path(mol_dir: &Path) -> PathBuf {
    mol_dir.join("verify.json")
}

/// Compute blake3 hashes over every tracked artifact present in `mol_dir`.
#[must_use]
pub fn compute_artifact_hashes(mol_dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in ARTIFACTS {
        let p = mol_dir.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            out.insert((*name).to_owned(), Hash::of_bytes(&bytes).to_hex());
        }
    }
    let responses = mol_dir.join("responses");
    if responses.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&responses) {
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for e in entries {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("md") {
                    if let Ok(bytes) = std::fs::read(&p) {
                        let rel = format!("responses/{}", e.file_name().to_string_lossy());
                        out.insert(rel, Hash::of_bytes(&bytes).to_hex());
                    }
                }
            }
        }
    }
    out
}

/// Seal a manifest into `mol_dir` — captures current artifact state.
///
/// # Errors
/// Returns an error if the manifest cannot be serialized or written.
pub fn seal(mol_dir: &Path, molecule_id: &str, formula_id: &str) -> anyhow::Result<VerifyManifest> {
    let manifest = VerifyManifest {
        molecule_id: molecule_id.to_owned(),
        formula_id: formula_id.to_owned(),
        sealed_at: chrono::Utc::now().to_rfc3339(),
        artifacts: compute_artifact_hashes(mol_dir),
    };
    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(manifest_path(mol_dir), json)?;
    Ok(manifest)
}

/// Read a sealed manifest from `mol_dir`, if present and parseable.
#[must_use]
pub fn read(mol_dir: &Path) -> Option<VerifyManifest> {
    let bytes = std::fs::read(manifest_path(mol_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn seal_and_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("prompt.md"), "hello").unwrap();
        std::fs::write(tmp.path().join("synthesis.md"), "world").unwrap();

        let sealed = seal(tmp.path(), "task-1", "task-work").unwrap();
        assert_eq!(sealed.artifacts.len(), 2);

        let read_back = read(tmp.path()).unwrap();
        assert_eq!(read_back.artifacts, sealed.artifacts);
    }

    #[test]
    fn tamper_changes_hash() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("synthesis.md"), "original").unwrap();
        let sealed = seal(tmp.path(), "m", "f").unwrap();
        std::fs::write(tmp.path().join("synthesis.md"), "tampered").unwrap();
        let current = compute_artifact_hashes(tmp.path());
        assert_ne!(sealed.artifacts["synthesis.md"], current["synthesis.md"]);
    }

    #[test]
    fn responses_directory_hashed() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("responses")).unwrap();
        std::fs::write(tmp.path().join("responses/wheeler.md"), "w").unwrap();
        std::fs::write(tmp.path().join("responses/feynman.md"), "f").unwrap();
        let h = compute_artifact_hashes(tmp.path());
        assert!(h.contains_key("responses/wheeler.md"));
        assert!(h.contains_key("responses/feynman.md"));
    }
}
