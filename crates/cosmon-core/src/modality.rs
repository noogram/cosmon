// SPDX-License-Identifier: AGPL-3.0-only

//! The seven knowledge modalities — shared vocabulary across the ecosystem.
//!
//! Each modality represents a distinct information topology and access pattern.
//! This enum is the shared vocabulary used by Cosmon (agent definitions),
//! Neurion (bearer classification), and Topon (structural map selection).
//!
//! The routing logic (how to reach a modality, which bearer to use) belongs
//! to Neurion, not here. This module defines only the vocabulary.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The seven knowledge modalities.
///
/// Variants are ordered by retrieval latency (cheapest first) to support
/// cascade strategies.
///
/// # Examples
///
/// ```
/// use cosmon_core::modality::Modality;
///
/// let m: Modality = "wiki".parse().unwrap();
/// assert_eq!(m, Modality::Wiki);
/// assert_eq!(m.to_string(), "wiki");
/// assert_eq!(Modality::all().len(), 7);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    /// Memoized query results with TTL — cheapest retrieval.
    Cache,
    /// Queryable metadata catalog across all modalities.
    Index,
    /// Structural code maps (tree-sitter symbols, `PageRank` ranking).
    /// Backend: Topon.
    Cfs,
    /// Wikilink-based knowledge graphs (Obsidian vault notes).
    /// Backend: Topon.
    Wiki,
    /// Academic references and citation metadata (Zotero library).
    /// Backend: Almanac (cosmon-lab substrate channel — see ADR-076).
    Zotero,
    /// Unprocessed source files, web pages, and raw artifacts.
    Raw,
    /// Embedding-based semantic representations — nearest-neighbor search.
    Vector,
}

impl Modality {
    /// Return all seven modalities in cascade order (cheapest first).
    #[must_use]
    pub fn all() -> &'static [Modality] {
        &[
            Self::Cache,
            Self::Index,
            Self::Cfs,
            Self::Wiki,
            Self::Zotero,
            Self::Raw,
            Self::Vector,
        ]
    }

    /// Whether this modality is topological (graph-based, deterministic).
    /// Topological modalities are served by Topon.
    #[must_use]
    pub fn is_topological(self) -> bool {
        matches!(self, Self::Wiki | Self::Cfs)
    }

    /// Whether this modality produces results from external systems.
    #[must_use]
    pub fn is_external(self) -> bool {
        matches!(self, Self::Zotero | Self::Raw)
    }
}

impl fmt::Display for Modality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw => f.write_str("raw"),
            Self::Wiki => f.write_str("wiki"),
            Self::Cfs => f.write_str("cfs"),
            Self::Zotero => f.write_str("zotero"),
            Self::Vector => f.write_str("vector"),
            Self::Cache => f.write_str("cache"),
            Self::Index => f.write_str("index"),
        }
    }
}

/// Error returned when parsing a [`Modality`] from a string fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown modality: {value:?} (expected one of: raw, wiki, cfs, zotero, vector, cache, index)"
)]
pub struct ParseModalityError {
    /// The string value that failed to parse.
    pub value: String,
}

impl FromStr for Modality {
    type Err = ParseModalityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "raw" => Ok(Self::Raw),
            "wiki" => Ok(Self::Wiki),
            "cfs" => Ok(Self::Cfs),
            "zotero" => Ok(Self::Zotero),
            "vector" => Ok(Self::Vector),
            "cache" => Ok(Self::Cache),
            "index" => Ok(Self::Index),
            _ => Err(ParseModalityError {
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_returns_seven() {
        assert_eq!(Modality::all().len(), 7);
    }

    #[test]
    fn test_display_roundtrip() {
        for &m in Modality::all() {
            let s = m.to_string();
            let parsed: Modality = s.parse().unwrap();
            assert_eq!(parsed, m);
        }
    }

    #[test]
    fn test_parse_unknown() {
        let err = "foobar".parse::<Modality>().unwrap_err();
        assert!(err.to_string().contains("foobar"));
    }

    #[test]
    fn test_serde_roundtrip() {
        for &m in Modality::all() {
            let json = serde_json::to_string(&m).unwrap();
            let back: Modality = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn test_topological() {
        assert!(Modality::Wiki.is_topological());
        assert!(Modality::Cfs.is_topological());
        assert!(!Modality::Raw.is_topological());
        assert!(!Modality::Vector.is_topological());
    }

    #[test]
    fn test_cascade_order() {
        let all = Modality::all();
        assert_eq!(all[0], Modality::Cache);
        assert_eq!(all[6], Modality::Vector);
    }
}
