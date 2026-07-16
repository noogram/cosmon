// SPDX-License-Identifier: AGPL-3.0-only

//! Typed labels for molecules.
//!
//! A [`Tag`] is a short, typed label attached to a molecule. Tags follow
//! a `key:value` convention where the key is required (kebab-case) and
//! the value is optional (free-form, but printable ASCII without `:` or
//! whitespace). Tags form a small, orderly vocabulary shared across a
//! project: `deferred:yes`, `priority:high`, `area:cli`, `bug`.
//!
//! # Format
//!
//! ```text
//! key[:value]
//! ```
//!
//! - `key` must be non-empty and match `[a-z][a-z0-9-]*` (kebab-case).
//! - `value`, when present, is any sequence of non-colon, non-whitespace
//!   printable ASCII characters (1..=64).
//! - The full tag (including `:` and value) is at most 128 bytes.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::tag::Tag;
//!
//! let t = Tag::new("deferred:yes").unwrap();
//! assert_eq!(t.key(), "deferred");
//! assert_eq!(t.value(), Some("yes"));
//!
//! let bare = Tag::new("bug").unwrap();
//! assert_eq!(bare.key(), "bug");
//! assert_eq!(bare.value(), None);
//!
//! assert!(Tag::new("").is_err());
//! assert!(Tag::new("Bad Key").is_err());
//! assert!(Tag::new("key:bad value").is_err());
//! ```

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Maximum full tag length (including `:` and value, in bytes).
pub const MAX_TAG_LEN: usize = 128;

/// A typed label attached to a molecule.
///
/// Immutable once constructed. The canonical text form is `key` or
/// `key:value`. Comparison is string-based so tags order lexically
/// regardless of whether a value is present, making `BTreeSet<Tag>`
/// render in a stable, human-friendly order.
#[derive(Debug, Clone, Eq)]
pub struct Tag {
    /// Full `key[:value]` text.
    raw: String,
    /// Byte index of the `:` separator, if any.
    sep: Option<usize>,
}

impl std::hash::Hash for Tag {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

/// Error returned when parsing a tag fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set will grow; external callers must keep a `_ =>` arm
pub enum TagError {
    /// Input was empty or contained no key.
    #[error("tag key must be non-empty")]
    EmptyKey,
    /// Tag exceeds [`MAX_TAG_LEN`] bytes.
    #[error("tag length {0} exceeds maximum {MAX_TAG_LEN}")]
    TooLong(usize),
    /// Key does not match `[a-z][a-z0-9-]*`.
    #[error("tag key `{0}` is not kebab-case (must start with [a-z] and contain [a-z0-9-])")]
    InvalidKey(String),
    /// Value contains a forbidden character (whitespace or `:`) or is too long.
    #[error("tag value `{0}` is invalid (no whitespace, no `:`, 1..=64 printable ASCII)")]
    InvalidValue(String),
}

impl Tag {
    /// Parse a tag from its canonical text form.
    ///
    /// # Errors
    /// Returns [`TagError`] if the input is empty, too long, or violates
    /// the key/value format rules.
    pub fn new(raw: impl Into<String>) -> Result<Self, TagError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(TagError::EmptyKey);
        }
        if raw.len() > MAX_TAG_LEN {
            return Err(TagError::TooLong(raw.len()));
        }

        let sep = raw.find(':');
        let (key, value) = match sep {
            Some(i) => (&raw[..i], Some(&raw[i + 1..])),
            None => (raw.as_str(), None),
        };

        validate_key(key)?;
        if let Some(v) = value {
            validate_value(v)?;
        }

        Ok(Self { raw, sep })
    }

    /// The key portion (before `:`).
    #[must_use]
    pub fn key(&self) -> &str {
        match self.sep {
            Some(i) => &self.raw[..i],
            None => &self.raw,
        }
    }

    /// The value portion (after `:`), if any.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        self.sep.map(|i| &self.raw[i + 1..])
    }

    /// Full canonical string, `key[:value]`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Match this tag against a glob pattern (`*` wildcard only).
    ///
    /// The pattern is matched against the canonical string. `*` matches
    /// any sequence of characters (including empty). Used by
    /// `cs ensemble --tag deferred:*`.
    #[must_use]
    pub fn matches_glob(&self, pattern: &str) -> bool {
        glob_match(pattern, &self.raw)
    }
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl PartialOrd for Tag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for Tag {
    type Err = TagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_owned())
    }
}

impl Serialize for Tag {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_key(key: &str) -> Result<(), TagError> {
    if key.is_empty() {
        return Err(TagError::EmptyKey);
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(TagError::InvalidKey(key.to_owned()));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(TagError::InvalidKey(key.to_owned()));
        }
    }
    Ok(())
}

fn validate_value(value: &str) -> Result<(), TagError> {
    if value.is_empty() || value.len() > 64 {
        return Err(TagError::InvalidValue(value.to_owned()));
    }
    for c in value.chars() {
        if c == ':' || c.is_whitespace() || !c.is_ascii() || c.is_ascii_control() {
            return Err(TagError::InvalidValue(value.to_owned()));
        }
    }
    Ok(())
}

/// Minimal glob matcher: `*` is the only wildcard.
///
/// Dynamic-programming is overkill for tag-sized strings — a recursive
/// match with early termination is clearer and fast enough.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    glob_match_bytes(p, 0, t, 0)
}

fn glob_match_bytes(p: &[u8], mut pi: usize, t: &[u8], mut ti: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            b'*' => {
                // Collapse adjacent stars.
                while pi + 1 < p.len() && p[pi + 1] == b'*' {
                    pi += 1;
                }
                if pi + 1 == p.len() {
                    return true;
                }
                // Try every tail position.
                for k in ti..=t.len() {
                    if glob_match_bytes(p, pi + 1, t, k) {
                        return true;
                    }
                }
                return false;
            }
            c => {
                if ti >= t.len() || t[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == t.len()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_key() {
        let t = Tag::new("bug").unwrap();
        assert_eq!(t.key(), "bug");
        assert!(t.value().is_none());
        assert_eq!(t.as_str(), "bug");
    }

    #[test]
    fn key_value() {
        let t = Tag::new("priority:high").unwrap();
        assert_eq!(t.key(), "priority");
        assert_eq!(t.value(), Some("high"));
    }

    #[test]
    fn kebab_key() {
        assert!(Tag::new("sub-system:cli").is_ok());
        assert!(Tag::new("a1-b2").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(Tag::new(""), Err(TagError::EmptyKey));
    }

    #[test]
    fn rejects_uppercase_key() {
        assert!(matches!(
            Tag::new("Priority:high"),
            Err(TagError::InvalidKey(_))
        ));
    }

    #[test]
    fn rejects_key_starting_with_digit() {
        assert!(matches!(Tag::new("1bad:x"), Err(TagError::InvalidKey(_))));
    }

    #[test]
    fn rejects_value_with_whitespace() {
        assert!(matches!(
            Tag::new("key:has space"),
            Err(TagError::InvalidValue(_))
        ));
    }

    #[test]
    fn rejects_value_with_colon() {
        assert!(matches!(
            Tag::new("key:a:b"),
            Err(TagError::InvalidValue(_))
        ));
    }

    #[test]
    fn rejects_too_long() {
        let long = "k:".to_string() + &"x".repeat(MAX_TAG_LEN);
        assert!(matches!(Tag::new(long), Err(TagError::TooLong(_))));
    }

    #[test]
    fn roundtrip_serde() {
        let t = Tag::new("area:cli").unwrap();
        let j = serde_json::to_string(&t).unwrap();
        assert_eq!(j, "\"area:cli\"");
        let back: Tag = serde_json::from_str(&j).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn ord_is_lexical() {
        let a = Tag::new("area:cli").unwrap();
        let b = Tag::new("priority:high").unwrap();
        assert!(a < b);
    }

    #[test]
    fn glob_exact() {
        let t = Tag::new("deferred:yes").unwrap();
        assert!(t.matches_glob("deferred:yes"));
        assert!(!t.matches_glob("deferred:no"));
    }

    #[test]
    fn glob_wildcard_suffix() {
        let t = Tag::new("deferred:yes").unwrap();
        assert!(t.matches_glob("deferred:*"));
        assert!(t.matches_glob("*"));
        assert!(t.matches_glob("def*"));
        assert!(!t.matches_glob("priority:*"));
    }

    #[test]
    fn glob_wildcard_middle() {
        let t = Tag::new("area:cli").unwrap();
        assert!(t.matches_glob("a*li"));
        assert!(t.matches_glob("*cli"));
    }
}
