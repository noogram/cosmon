// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem adapter for the noogram `attestor-events.jsonl` ledger.
//!
//! The pure parse logic lives in [`cosmon_core::attestor_event_v1`]; this
//! module owns the I/O seam — open the file, stream lines — so the domain
//! crate stays zero-I/O (INV-DOMAIN-PURE-NO-IO, ADR-082). It is the
//! attestor-ledger sibling of [`crate::event_log::read_all`], which performs
//! the same role for the `EventV2` envelope. Before this split the domain
//! crate carried a duplicate `std::fs::File::open` reader; the canonical
//! adapter home for that I/O is here.

use std::io::{BufRead, BufReader};
use std::path::Path;

use cosmon_core::attestor_event_v1::{parse_line, AttestorEnvelope};

/// Read every line of an `attestor-events.jsonl` file into envelopes.
///
/// Empty lines are skipped; the first malformed or wrong-version line fails
/// the whole read (the audit is not a recovery tool). Parsing is delegated
/// to the pure [`cosmon_core::attestor_event_v1::parse_line`]; this function
/// is only the filesystem seam.
///
/// # Errors
///
/// Returns an [`std::io::Error`] on filesystem failure, or with kind
/// `InvalidData` on a malformed JSON line / a row whose schema version is
/// not `1`.
pub fn read_all(path: &Path) -> std::io::Result<Vec<AttestorEnvelope>> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        let parsed = parse_line(i + 1, &line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        if let Some(env) = parsed {
            out.push(env);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_all_rejects_unknown_version() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("evt.jsonl");
        std::fs::write(
            &p,
            "{\"v\":2,\"seq\":0,\"ts\":\"2026-05-16T12:00:00Z\",\"kind\":\"attestor_enrol\",\"attestor\":\"a1\",\"t\":\"2026-05-16T12:00:00Z\"}\n",
        )
        .unwrap();
        let err = read_all(&p).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_all_parses_valid_rows_and_skips_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("evt.jsonl");
        std::fs::write(
            &p,
            "\n{\"v\":1,\"seq\":0,\"ts\":\"2026-05-16T12:00:00Z\",\"kind\":\"attestor_enrol\",\"attestor\":\"a1\",\"t\":\"2026-05-16T12:00:00Z\"}\n\n",
        )
        .unwrap();
        let rows = read_all(&p).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 0);
    }
}
