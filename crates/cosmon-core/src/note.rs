// SPDX-License-Identifier: AGPL-3.0-only

//! Append-only notes on molecules.
//!
//! A [`Note`] is a timestamped comment attached to a molecule. Notes are
//! stored as individual Markdown files under
//! `.cosmon/state/fleets/<fleet>/molecules/<id>/notes/NNN-author.md`,
//! where `NNN` is a zero-padded monotonic sequence number and `author`
//! is the worker id or the literal `human`.
//!
//! # Append-only discipline
//!
//! Notes form an audit trail — once written, a note **must not** be edited
//! or deleted. The file name carries the sequence number so insertion order
//! is preserved even if timestamps skew, and the author field records the
//! actor so both human operators and worker agents leave traceable footprints.
//!
//! # Wire format
//!
//! Each note file is YAML frontmatter + Markdown body:
//!
//! ```text
//! ---
//! seq: 12
//! author: worker-onyx
//! timestamp: 2026-04-11T15:04:05Z
//! ---
//! first observation on this molecule
//! ```

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::WorkerId;

/// Who authored a note — a worker, or a human operator.
///
/// Serialized as a plain string: `"human"` for human-authored notes,
/// or the worker id (e.g. `"onyx"`) otherwise. Using a tagged union
/// would collide with the worker id named `"human"`, so the sentinel
/// is decoded first and falls through to worker parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteAuthor {
    /// Authored by a worker agent. Carries the worker's identity.
    Worker(WorkerId),
    /// Authored by a human (direct CLI invocation). Sentinel string.
    Human(HumanMarker),
}

impl Serialize for NoteAuthor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Human(_) => serializer.serialize_str("human"),
            Self::Worker(w) => serializer.serialize_str(w.as_str()),
        }
    }
}

impl<'de> Deserialize<'de> for NoteAuthor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s == "human" {
            return Ok(Self::Human(HumanMarker));
        }
        WorkerId::new(s)
            .map(Self::Worker)
            .map_err(serde::de::Error::custom)
    }
}

/// Marker for human-authored notes.
///
/// Serialized as the literal string `"human"`. This is a newtype rather
/// than a bare string so `NoteAuthor::Human` stays symbolic rather than
/// accidentally collecting arbitrary text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanMarker;

impl Serialize for HumanMarker {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("human")
    }
}

impl<'de> Deserialize<'de> for HumanMarker {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s == "human" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "expected literal \"human\", got {s:?}"
            )))
        }
    }
}

impl NoteAuthor {
    /// Returns the author as a filesystem-safe string used in the note file
    /// name: either the worker id or the literal `human`.
    #[must_use]
    pub fn slug(&self) -> &str {
        match self {
            Self::Worker(w) => w.as_str(),
            Self::Human(_) => "human",
        }
    }
}

impl fmt::Display for NoteAuthor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Worker(w) => write!(f, "worker:{w}"),
            Self::Human(_) => f.write_str("human"),
        }
    }
}

/// A single append-only note on a molecule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    /// Monotonic sequence number (1-based). Zero-padded in file names.
    pub seq: u32,
    /// Author — worker agent or human.
    pub author: NoteAuthor,
    /// When the note was written.
    pub timestamp: DateTime<Utc>,
    /// Free-form Markdown body (may be empty in theory, but commands
    /// reject empty input).
    pub body: String,
}

impl Note {
    /// Build a new note. Callers typically compute `seq` from the current
    /// notes directory (see `cosmon-filestore`).
    #[must_use]
    pub fn new(seq: u32, author: NoteAuthor, body: impl Into<String>) -> Self {
        Self {
            seq,
            author,
            timestamp: Utc::now(),
            body: body.into(),
        }
    }

    /// The base file name for this note, e.g. `003-worker-onyx.md`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{:03}-{}.md", self.seq, sanitize_slug(self.author.slug()))
    }

    /// Render the note as a full Markdown file with YAML frontmatter.
    #[must_use]
    pub fn render(&self) -> String {
        let author_field = match &self.author {
            NoteAuthor::Worker(w) => format!("worker:{w}"),
            NoteAuthor::Human(_) => "human".to_owned(),
        };
        format!(
            "---\nseq: {seq}\nauthor: {author}\ntimestamp: {ts}\n---\n{body}\n",
            seq = self.seq,
            author = author_field,
            ts = self.timestamp.to_rfc3339(),
            body = self.body.trim_end(),
        )
    }
}

/// Replace filesystem-unfriendly characters in a slug with `-`.
fn sanitize_slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_author_slug() {
        let n = Note::new(1, NoteAuthor::Human(HumanMarker), "hi");
        assert_eq!(n.author.slug(), "human");
        assert_eq!(n.file_name(), "001-human.md");
    }

    #[test]
    fn worker_author_slug() {
        let w = WorkerId::new("onyx").unwrap();
        let n = Note::new(7, NoteAuthor::Worker(w), "hi");
        assert_eq!(n.file_name(), "007-onyx.md");
    }

    #[test]
    fn render_contains_frontmatter_and_body() {
        let n = Note::new(2, NoteAuthor::Human(HumanMarker), "hello world");
        let text = n.render();
        assert!(text.starts_with("---\n"));
        assert!(text.contains("seq: 2"));
        assert!(text.contains("author: human"));
        assert!(text.contains("hello world"));
    }

    #[test]
    fn author_serde_roundtrip_worker() {
        let w = WorkerId::new("ruby").unwrap();
        let a = NoteAuthor::Worker(w);
        let j = serde_json::to_string(&a).unwrap();
        let back: NoteAuthor = serde_json::from_str(&j).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn author_serde_roundtrip_human() {
        let a = NoteAuthor::Human(HumanMarker);
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"human\"");
        let back: NoteAuthor = serde_json::from_str(&j).unwrap();
        assert_eq!(a, back);
    }
}
