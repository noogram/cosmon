// SPDX-License-Identifier: Apache-2.0

//! The byte cursor: resuming a read of a file that is still being written.
//!
//! This module is the answer to probe P7 of ADR-168 — `claudion::parse_session`
//! reads the whole file and returns `Err` on the first line that is not
//! complete JSON, so observing a live session means re-reading from byte zero
//! every poll and failing outright whenever the sample lands mid-append — and
//! to probes P4/P5, which are the *other* half of the same defect in the
//! session mailbox: a stale seek past a rotated end silently swallows the
//! backlog, and a seek landing inside a multi-byte character panics the reader.
//!
//! Three rules, one each:
//!
//! 1. **Only complete lines are consumed.** The cursor advances to just past
//!    the last `\n` actually read; a half-written trailing line is left where
//!    it is and picked up by the next read.
//! 2. **A shrunken or replaced file rewinds to zero** and says so, instead of
//!    reporting success while reading nothing. Replacement is detected by a
//!    fingerprint of the file's head, which a pure append cannot change.
//! 3. **Every slice is a byte slice.** Text is decoded per complete line with
//!    [`String::from_utf8_lossy`], so no cursor value — however stale, however
//!    foreign — can land inside a codepoint and take the reading process down.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ProbeError;

/// How many bytes of the file head the generation fingerprint covers, at most.
///
/// The head of a provider session log is its `session_meta` / first record —
/// long enough that two different sessions practically never share it, short
/// enough that fingerprinting costs one small read per poll.
const HEAD_SAMPLE_BYTES: u64 = 512;

/// Which file generation a cursor was minted against.
///
/// The sample **length** travels with the hash, and re-sampling uses the
/// recorded length rather than the current one. That detail is the whole
/// correctness of the scheme: a short log that grows past the sample size
/// would otherwise change its own fingerprint by being appended to, and every
/// poll of a young session would report a spurious rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Generation {
    /// Bytes of head that were hashed.
    sampled: u64,
    /// The hash of those bytes.
    hash: u64,
}

/// A resumable position in a provider session log.
///
/// Serializable because a co-pilot that restarts must resume where it stopped
/// rather than replay the session (CHECKPOINT-NOT-SCROLLBACK, applied to the
/// probe itself).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Byte offset of the first byte not yet consumed. Always sits just past a
    /// `\n`, or at 0.
    offset: u64,
    /// Fingerprint of the file head as it was when this cursor was minted.
    /// `None` on a fresh cursor, which therefore trusts whatever it finds.
    generation: Option<Generation>,
}

impl Cursor {
    /// A cursor at the start of a log that has never been read.
    #[must_use]
    pub const fn start() -> Self {
        Self {
            offset: 0,
            generation: None,
        }
    }

    /// The byte offset this cursor will resume from.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Rebuild a cursor from a persisted offset, without a generation
    /// fingerprint.
    ///
    /// This is the deliberately *weak* constructor: an offset alone cannot
    /// tell a rotated file from an appended one, so a cursor built this way
    /// detects truncation (the file is shorter than the offset) but not
    /// same-length replacement. Prefer carrying the whole [`Cursor`] across a
    /// restart; use this only when reading back a legacy seek file.
    #[must_use]
    pub const fn from_offset(offset: u64) -> Self {
        Self {
            offset,
            generation: None,
        }
    }
}

/// Why a read restarted from byte zero instead of resuming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestartCause {
    /// The file is shorter than the cursor — it was truncated in place.
    Truncated,
    /// The file's head changed — it was rotated or rewritten wholesale.
    Rotated,
}

/// What happened to continuity between the previous read and this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Continuity {
    /// First read of this log: everything from byte zero is new.
    Fresh,
    /// The cursor was honoured; the events are strictly the new tail.
    Resumed,
    /// The cursor was abandoned and the log re-read from zero. The caller is
    /// looking at events it may have seen before — MESSAGE-TRACE puts the
    /// burden of idempotent consumption on the consumer, and this field is
    /// what tells it to apply it.
    Restarted(RestartCause),
}

/// One complete line, with the byte offset it starts at.
///
/// The offset is a stable address inside a generation: a caller can cite it in
/// a finding and another reader can seek back to the same line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawLine {
    /// Byte offset of the first byte of the line.
    pub offset: u64,
    /// The line's text, lossily decoded, without its trailing `\n`.
    pub text: String,
}

/// The result of an incremental read.
#[derive(Clone, Debug)]
pub struct LineBatch {
    /// The complete lines read, in file order.
    pub lines: Vec<RawLine>,
    /// The cursor to pass to the next read.
    pub cursor: Cursor,
    /// Whether this read resumed, started fresh, or had to rewind.
    pub continuity: Continuity,
    /// Bytes of a partial trailing line deliberately left unconsumed.
    pub pending_bytes: u64,
}

/// Read the complete lines a log has gained since `cursor`.
///
/// Opens `path` read-only and never writes, renames, locks or stats-then-touches
/// it: OBSERVATION-NEUTRE means an observed session cannot tell it is being
/// observed.
///
/// # Errors
///
/// [`ProbeError::Io`] if the file cannot be opened, measured, sought or read.
/// A malformed *content* line is not an error here — this layer returns text
/// and lets the adapter decide (see [`crate::event`]).
pub fn read_lines_from(path: &Path, cursor: Cursor) -> Result<LineBatch, ProbeError> {
    let io = |source| ProbeError::Io {
        path: path.to_path_buf(),
        source,
    };

    let mut file = std::fs::File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();

    let continuity = match cursor.generation {
        None if cursor.offset == 0 => Continuity::Fresh,
        // An offset without a generation — a legacy seek value. It can still
        // detect a shrunken file, which is what P4 was about.
        None if cursor.offset > len => Continuity::Restarted(RestartCause::Truncated),
        None => Continuity::Resumed,
        Some(prior) if cursor.offset > len || len < prior.sampled => {
            Continuity::Restarted(RestartCause::Truncated)
        }
        Some(prior) => {
            let head = read_head(&mut file, prior.sampled).map_err(io)?;
            if fingerprint(&head) == prior.hash {
                Continuity::Resumed
            } else {
                Continuity::Restarted(RestartCause::Rotated)
            }
        }
    };

    let start = match continuity {
        Continuity::Restarted(_) | Continuity::Fresh => 0,
        Continuity::Resumed => cursor.offset,
    };

    file.seek(SeekFrom::Start(start)).map_err(io)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(io)?;

    // Consume up to and including the last newline; anything after it is a
    // line still being written.
    let consumed = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let (complete, partial) = buf.split_at(consumed);

    let mut lines = Vec::new();
    let mut at = start;
    for chunk in complete.split_inclusive(|b| *b == b'\n') {
        let text = String::from_utf8_lossy(chunk)
            .trim_end_matches('\n')
            .to_string();
        if !text.trim().is_empty() {
            lines.push(RawLine { offset: at, text });
        }
        at += chunk.len() as u64;
    }

    // Mint the generation of the file as it is *now*, over as much head as it
    // currently has.
    let sampled = len.min(HEAD_SAMPLE_BYTES);
    let head = read_head(&mut file, sampled).map_err(io)?;

    Ok(LineBatch {
        lines,
        cursor: Cursor {
            offset: start + consumed as u64,
            generation: Some(Generation {
                sampled,
                hash: fingerprint(&head),
            }),
        },
        continuity,
        pending_bytes: partial.len() as u64,
    })
}

/// Read exactly `want` bytes from the start of an open file, leaving the
/// caller to seek wherever it wants afterwards. The caller guarantees the file
/// is at least that long.
fn read_head(file: &mut std::fs::File, want: u64) -> std::io::Result<Vec<u8>> {
    let want = usize::try_from(want).unwrap_or(usize::MAX);
    let mut head = vec![0_u8; want];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head)?;
    Ok(head)
}

/// FNV-1a over the head sample.
///
/// Not a security primitive and not content addressing — its only job is to
/// answer *"is this the same file generation the cursor was minted against?"*.
/// A pure append cannot change it; a rotation or a wholesale rewrite almost
/// always does. `cosmon-hash` is deliberately not pulled in for this: the port
/// stays a leaf crate an external adapter author can depend on.
fn fingerprint(head: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in head {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn a_resumed_read_returns_only_the_new_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.jsonl");
        append(&log, "one\ntwo\n");

        let first = read_lines_from(&log, Cursor::start()).unwrap();
        assert_eq!(first.continuity, Continuity::Fresh);
        assert_eq!(first.lines.len(), 2);

        append(&log, "three\n");
        let second = read_lines_from(&log, first.cursor).unwrap();
        assert_eq!(second.continuity, Continuity::Resumed);
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].text, "three");
    }

    #[test]
    fn a_half_written_trailing_line_is_left_for_the_next_read() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.jsonl");
        append(&log, "complete\n{\"partial\":");

        let first = read_lines_from(&log, Cursor::start()).unwrap();
        assert_eq!(first.lines.len(), 1, "the partial line is not delivered");
        assert!(first.pending_bytes > 0);

        append(&log, "true}\n");
        let second = read_lines_from(&log, first.cursor).unwrap();
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].text, "{\"partial\":true}");
    }

    #[test]
    fn a_truncated_file_rewinds_instead_of_reporting_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.jsonl");
        append(&log, "one\ntwo\nthree\n");
        let first = read_lines_from(&log, Cursor::start()).unwrap();

        std::fs::write(&log, "fresh\n").unwrap();
        let second = read_lines_from(&log, first.cursor).unwrap();
        assert_eq!(
            second.continuity,
            Continuity::Restarted(RestartCause::Truncated)
        );
        assert_eq!(second.lines[0].text, "fresh");
    }

    #[test]
    fn a_rotated_file_of_the_same_length_is_still_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.jsonl");
        std::fs::write(&log, "aaaa\nbbbb\n").unwrap();
        let first = read_lines_from(&log, Cursor::start()).unwrap();

        // Same byte length, different content: a length check alone sees
        // nothing, and the reader would sit past the end forever.
        std::fs::write(&log, "cccc\ndddd\n").unwrap();
        let second = read_lines_from(&log, first.cursor).unwrap();
        assert_eq!(
            second.continuity,
            Continuity::Restarted(RestartCause::Rotated)
        );
        assert_eq!(second.lines.len(), 2);
    }

    #[test]
    fn a_stale_offset_inside_a_codepoint_does_not_panic() {
        // Probe P5: `&content[seek..]` on a byte offset inside 'é' panics the
        // reading process. Here the same offset is merely a byte position.
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.jsonl");
        let mut bytes = vec![b'a'; 113];
        bytes.extend("é".as_bytes());
        bytes.extend(b"\ntail\n");
        std::fs::write(&log, bytes).unwrap();

        let batch = read_lines_from(&log, Cursor::from_offset(114)).unwrap();
        assert_eq!(batch.continuity, Continuity::Resumed);
        assert!(batch.lines.iter().any(|l| l.text == "tail"));
    }

    #[test]
    fn an_empty_log_yields_nothing_and_a_usable_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.jsonl");
        std::fs::write(&log, "").unwrap();
        let batch = read_lines_from(&log, Cursor::start()).unwrap();
        assert!(batch.lines.is_empty());
        assert_eq!(batch.cursor.offset(), 0);
    }
}
