// SPDX-License-Identifier: AGPL-3.0-only

//! [`Transcript`] — the on-disk conversation artifact.
//!
//! ## Transcript, not Session (ADR-096)
//!
//! claw-code persists a resumable `Session` JSON and reloads it with
//! `--resume`. cosmon deliberately does **not** copy that shape: ADR-096
//! refuses `Session`-as-context, and ADR-016 forbids a process that
//! carries state across invocations. What survives a `cosmon-pilot` run is
//! a **transcript** — an append-only *record* of what was said, never a
//! resumable context. A second invocation starts a fresh
//! [`cosmon_agent_harness::InteractiveSession`]; the transcript is read by
//! humans and future audits, not re-fed to the model.
//!
//! ## Append-only, write-through
//!
//! The REPL renders the live scrollback from
//! [`cosmon_agent_harness::InteractiveSession::transcript`] (the in-memory
//! projection). This type is the *durable* mirror: after each turn the
//! REPL hands the full current entry list to [`Transcript::append_new`],
//! which writes only the *new* tail (entries past the high-water mark it
//! already flushed) to the file. That keeps the write idempotent across
//! turns without re-reading the file or re-writing earlier entries.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use cosmon_agent_harness::{TranscriptEntry, TranscriptRole};

/// An append-only on-disk record of a pilot conversation.
///
/// Holds the target path and a high-water mark (`flushed`) — the count of
/// [`TranscriptEntry`] values already written. Each
/// [`Self::append_new`] writes the slice past the mark and advances it, so
/// repeatedly handing the whole growing transcript back is cheap and never
/// duplicates a line.
#[derive(Debug)]
pub struct Transcript {
    path: PathBuf,
    flushed: usize,
}

impl Transcript {
    /// Create (truncating any prior file) a fresh transcript at `path` and
    /// write a one-line header.
    ///
    /// Truncation is the honest default for a stateless driver: each run
    /// is a new conversation (ADR-016 — nothing persists across
    /// invocations but the record itself), so a run owns its transcript
    /// file rather than appending into a previous run's. A caller that
    /// wants per-run files passes a per-run path.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the file cannot be
    /// created or the header cannot be written (e.g. the parent directory
    /// does not exist).
    pub fn create(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let mut file = File::create(&path)?;
        writeln!(file, "# cosmon-pilot transcript\n")?;
        Ok(Self { path, flushed: 0 })
    }

    /// The path this transcript writes to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append every entry past the high-water mark to the file, then
    /// advance the mark. Returns the number of entries written this call
    /// (zero when `entries` has not grown since the last call).
    ///
    /// The caller passes the *whole* current transcript
    /// ([`cosmon_agent_harness::InteractiveSession::transcript`]); this
    /// method slices off the already-flushed prefix so only fresh entries
    /// hit the disk.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the file cannot be
    /// reopened for appending or a write fails.
    pub fn append_new(&mut self, entries: &[TranscriptEntry]) -> std::io::Result<usize> {
        if entries.len() <= self.flushed {
            return Ok(0);
        }
        let fresh = &entries[self.flushed..];
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        for entry in fresh {
            writeln!(file, "## {}", role_label(entry.role))?;
            writeln!(file, "{}\n", entry.content)?;
        }
        let written = fresh.len();
        self.flushed = entries.len();
        Ok(written)
    }
}

/// Stable, render-ready label for a [`TranscriptRole`] — used as the
/// markdown section header for each transcript entry.
fn role_label(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::System => "SYSTEM",
        TranscriptRole::Operator => "OPERATOR",
        TranscriptRole::Assistant => "ASSISTANT",
        TranscriptRole::Tool => "TOOL",
        // `TranscriptRole` is `#[non_exhaustive]`; an unforeseen future
        // role renders under a neutral label rather than failing to build.
        _ => "ENTRY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_new_writes_only_the_fresh_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.md");
        let mut transcript = Transcript::create(&path).unwrap();

        let first = vec![
            TranscriptEntry::new(TranscriptRole::System, "briefing"),
            TranscriptEntry::new(TranscriptRole::Operator, "hello"),
        ];
        assert_eq!(transcript.append_new(&first).unwrap(), 2);

        // The same two entries plus one new one — only the new one writes.
        let mut second = first.clone();
        second.push(TranscriptEntry::new(TranscriptRole::Assistant, "hi there"));
        assert_eq!(transcript.append_new(&second).unwrap(), 1);

        // Idempotent: re-handing the same list writes nothing.
        assert_eq!(transcript.append_new(&second).unwrap(), 0);

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# cosmon-pilot transcript"));
        assert!(body.contains("## OPERATOR\nhello"));
        assert!(body.contains("## ASSISTANT\nhi there"));
        // "hi there" must appear exactly once despite three append calls.
        assert_eq!(body.matches("hi there").count(), 1);
    }
}
