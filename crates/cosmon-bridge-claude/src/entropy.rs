// SPDX-License-Identifier: AGPL-3.0-only

//! Entropy instrumentation for agent systems.
//!
//! Implements three computable entropy sources from THESIS.md Part XII:
//!
//! 1. **Compression ratio** — `compressed_bytes / raw_bytes`, a proxy for
//!    Shannon information density. High ratio (near 1.0) means high entropy
//!    (incompressible data); low ratio means redundancy.
//!
//! 2. **Code entropy** — compression ratio of source files in a directory tree.
//!    Tracks the "initial entropy of the universe" (the codebase itself).
//!
//! 3. **State entropy** — Boltzmann entropy `S = log₂(W)` where `W` is the
//!    number of reachable fleet configurations.
//!
//! All functions are standalone (ADR-COS-001). Results are captured in
//! [`EntropyRecord`] and appended to a JSONL log.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Error type for entropy operations.
#[derive(Debug, thiserror::Error)]
pub enum EntropyError {
    /// An I/O error reading files or writing the log.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Which entropy source produced the measurement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntropySource {
    /// Compression ratio of an arbitrary byte stream (message log, etc.).
    Compression,
    /// Compression ratio of source code files.
    Code,
    /// Boltzmann state entropy of fleet configuration.
    State,
}

/// A single entropy measurement, serialized to the JSONL log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntropyRecord {
    /// When the measurement was taken.
    pub timestamp: DateTime<Utc>,
    /// Which entropy source.
    pub source: EntropySource,
    /// The computed entropy value (bits or ratio, depending on source).
    pub value: f64,
    /// Raw byte count (for compression-based measurements).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_bytes: Option<u64>,
    /// Compressed byte count (for compression-based measurements).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compressed_bytes: Option<u64>,
    /// Number of files measured (for code entropy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,
    /// Free-form label for context (e.g. directory path, worker set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Compute the compression ratio of a byte slice using DEFLATE.
///
/// Returns `compressed_len / raw_len`. A value near 1.0 means the data is
/// already high-entropy (incompressible). A value near 0.0 means heavy
/// redundancy.
///
/// Returns 0.0 for empty input.
///
/// # Examples
///
/// ```
/// use cosmon_bridge_claude::entropy::compression_ratio;
///
/// // Highly redundant data compresses well (low ratio).
/// let redundant = vec![b'A'; 10_000];
/// let ratio = compression_ratio(&redundant);
/// assert!(ratio < 0.1, "redundant data should compress well: {ratio}");
///
/// // Empty input returns 0.0.
/// assert!((compression_ratio(&[]) - 0.0).abs() < f64::EPSILON);
/// ```
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn compression_ratio(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }

    let compressed = miniz_oxide::deflate::compress_to_vec(data, 6);
    compressed.len() as f64 / data.len() as f64
}

/// Compute code entropy: the compression ratio of all source files under a
/// directory tree.
///
/// Recursively collects `*.rs`, `*.toml`, and `*.md` files, concatenates their
/// contents, and returns the compression ratio. This tracks the "initial
/// entropy of the universe" — the codebase complexity.
///
/// # Errors
///
/// Returns [`EntropyError::Io`] if the directory cannot be read.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use cosmon_bridge_claude::entropy::code_entropy;
///
/// let record = code_entropy(Path::new("crates/")).unwrap();
/// assert!(record.value > 0.0);
/// assert!(record.file_count.unwrap() > 0);
/// ```
pub fn code_entropy(dir: &Path) -> Result<EntropyRecord, EntropyError> {
    let mut buf = Vec::new();
    let mut file_count = 0usize;

    collect_source_files(dir, &mut buf, &mut file_count)?;

    let ratio = compression_ratio(&buf);

    Ok(EntropyRecord {
        timestamp: Utc::now(),
        source: EntropySource::Code,
        value: ratio,
        raw_bytes: Some(buf.len() as u64),
        compressed_bytes: Some(if buf.is_empty() {
            0
        } else {
            miniz_oxide::deflate::compress_to_vec(&buf, 6).len() as u64
        }),
        file_count: Some(file_count),
        label: Some(dir.display().to_string()),
    })
}

/// Compute Boltzmann state entropy of the fleet: `S = log₂(W)`.
///
/// `W` is the number of reachable configurations, estimated as:
///
/// ```text
/// W = (worker_states ^ worker_count) × (molecule_states ^ molecule_count)
/// ```
///
/// where `worker_states` and `molecule_states` are the number of distinct
/// statuses each entity can occupy.
///
/// # Examples
///
/// ```
/// use cosmon_bridge_claude::entropy::state_entropy;
///
/// // 3 workers with 4 possible states, 5 molecules with 6 possible states:
/// let record = state_entropy(3, 4, 5, 6);
/// // W = 4^3 × 6^5 = 64 × 7776 = 497664
/// // S = log₂(497664) ≈ 18.92
/// assert!((record.value - 18.92).abs() < 0.1);
/// ```
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn state_entropy(
    worker_count: usize,
    worker_states: usize,
    molecule_count: usize,
    molecule_states: usize,
) -> EntropyRecord {
    let w = if worker_states == 0 || molecule_states == 0 {
        0.0
    } else {
        // log₂(a^n × b^m) = n·log₂(a) + m·log₂(b)
        worker_count as f64 * (worker_states as f64).log2()
            + molecule_count as f64 * (molecule_states as f64).log2()
    };

    EntropyRecord {
        timestamp: Utc::now(),
        source: EntropySource::State,
        value: w,
        raw_bytes: None,
        compressed_bytes: None,
        file_count: None,
        label: Some(format!(
            "workers={worker_count}×{worker_states} molecules={molecule_count}×{molecule_states}"
        )),
    }
}

/// Append an [`EntropyRecord`] to a JSONL file.
///
/// Creates the file and parent directories if they do not exist.
///
/// # Errors
///
/// Returns [`EntropyError`] on I/O or serialization failure.
pub fn append_record(path: &Path, record: &EntropyRecord) -> Result<(), EntropyError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

/// Read all entropy records from a JSONL file.
///
/// Returns an empty `Vec` if the file does not exist.
///
/// # Errors
///
/// Returns [`EntropyError`] on I/O or parse failure.
pub fn read_records(path: &Path) -> Result<Vec<EntropyRecord>, EntropyError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path)?;
    let mut records = Vec::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        records.push(serde_json::from_str(line)?);
    }
    Ok(records)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Recursively collect source file contents into `buf`.
fn collect_source_files(
    dir: &Path,
    buf: &mut Vec<u8>,
    count: &mut usize,
) -> Result<(), EntropyError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skip target/ and hidden directories.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" {
                continue;
            }
            collect_source_files(&path, buf, count)?;
        } else if is_source_file(&path) {
            buf.extend_from_slice(&fs::read(&path)?);
            *count += 1;
        }
    }
    Ok(())
}

/// Check if a path has a source-file extension we care about.
fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "toml" | "md")
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_compression_ratio_empty() {
        assert!((compression_ratio(&[]) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compression_ratio_redundant() {
        let data = vec![b'A'; 10_000];
        let ratio = compression_ratio(&data);
        assert!(ratio < 0.1, "redundant data should compress well: {ratio}");
    }

    #[test]
    fn test_compression_ratio_varied() {
        // Data with all 256 byte values repeated — moderate entropy.
        let data: Vec<u8> = (0..256)
            .cycle()
            .take(10_000)
            .map(|b: i32| u8::try_from(b & 0xFF).unwrap_or(0))
            .collect();
        let ratio = compression_ratio(&data);
        // Should compress less than pure repetition but still significantly.
        assert!(ratio > 0.0 && ratio < 1.0, "varied data ratio: {ratio}");
    }

    #[test]
    fn test_code_entropy_tempdir() {
        let dir = tempfile::tempdir().unwrap();

        // Create a few source files.
        fs::write(
            dir.path().join("lib.rs"),
            "fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        let record = code_entropy(dir.path()).unwrap();
        assert_eq!(record.source, EntropySource::Code);
        assert!(record.value > 0.0);
        assert_eq!(record.file_count, Some(2));
        assert!(record.raw_bytes.unwrap() > 0);
    }

    #[test]
    fn test_code_entropy_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let record = code_entropy(dir.path()).unwrap();
        assert!((record.value - 0.0).abs() < f64::EPSILON);
        assert_eq!(record.file_count, Some(0));
    }

    #[test]
    fn test_code_entropy_skips_hidden_and_target() {
        let dir = tempfile::tempdir().unwrap();

        // Hidden dir.
        let hidden = dir.path().join(".git");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("config.rs"), "should be skipped").unwrap();

        // Target dir.
        let target = dir.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("debug.rs"), "should be skipped").unwrap();

        // Visible source.
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let record = code_entropy(dir.path()).unwrap();
        assert_eq!(record.file_count, Some(1));
    }

    #[test]
    fn test_state_entropy_basic() {
        // 3 workers × 4 states, 5 molecules × 6 states.
        // S = 3·log₂(4) + 5·log₂(6) = 6 + 12.925 = 18.925
        let record = state_entropy(3, 4, 5, 6);
        assert_eq!(record.source, EntropySource::State);
        assert!((record.value - 18.925).abs() < 0.01, "got {}", record.value);
    }

    #[test]
    fn test_state_entropy_zero_states() {
        let record = state_entropy(5, 0, 3, 4);
        assert!((record.value - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_state_entropy_zero_entities() {
        let record = state_entropy(0, 4, 0, 6);
        assert!((record.value - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_append_and_read_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops/log/entropy.jsonl");

        let r1 = EntropyRecord {
            timestamp: Utc::now(),
            source: EntropySource::Compression,
            value: 0.42,
            raw_bytes: Some(1000),
            compressed_bytes: Some(420),
            file_count: None,
            label: Some("test".to_owned()),
        };

        let r2 = state_entropy(2, 3, 4, 5);

        append_record(&path, &r1).unwrap();
        append_record(&path, &r2).unwrap();

        let records = read_records(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source, EntropySource::Compression);
        assert_eq!(records[1].source, EntropySource::State);
    }

    #[test]
    fn test_read_records_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        let records = read_records(&path).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_entropy_record_serde_roundtrip() {
        let record = EntropyRecord {
            timestamp: Utc::now(),
            source: EntropySource::Code,
            value: 0.65,
            raw_bytes: Some(50_000),
            compressed_bytes: Some(32_500),
            file_count: Some(42),
            label: Some("crates/".to_owned()),
        };
        let json = serde_json::to_string(&record).unwrap();
        let back: EntropyRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }
}
