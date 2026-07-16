// SPDX-License-Identifier: Apache-2.0

//! Pure JSON comparator for the cs ↔ cs-thin parity test.
//!
//! [`compare`] walks two `serde_json::Value` trees in lock-step,
//! reporting every difference as a [`Diff`] entry with a dotted path
//! (e.g. `molecule.tags.0`). The optional [`Allowlist`] suppresses
//! known-divergent paths — `request_id` (server-generated UUID),
//! `created_at` (per-call wall-clock), `molecule_dir` (filesystem
//! path absent on the wire), and so on.
//!
//! # Why pure
//!
//! The comparator must be auditable in isolation: a future operator
//! reading a CI failure should be able to re-run the exact diff
//! locally without booting the rpp-adapter. Keeping it I/O-free and
//! `serde_json::Value`-only makes the failure mode reproducible from
//! the captured byte streams alone.
//!
//! # Allowlist semantics
//!
//! The allowlist is **explicit** by design — there is no
//! "ignore-everything-that-differs" escape hatch. Every entry names a
//! specific path and a written rationale. The TOML form lives at
//! `tests/parity-allowlist.toml`; see
//! `docs/guides/cs-thin-parity.md` for the audit narrative.
//!
//! Each entry binds a `verb` (or `*` for any verb) to a `path`
//! template. Path templates accept three forms:
//!
//! - Literal dotted path: `molecule.tags.0` — matches exactly.
//! - Wildcard segment `*`: `molecule.tags.*` — matches any single
//!   index/key in that position.
//! - Trailing `**`: `molecule.**` — matches any descendants.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// One divergence between two JSON values at a specific path.
///
/// `path` is a dotted breadcrumb starting at the root (`""` for the
/// root itself). Object keys appear verbatim; array indices appear as
/// numeric segments (`tags.0`). The shape is intentionally flat so
/// failures are scannable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// Dotted path to the divergent node (`""` = root).
    pub path: String,
    /// Left-hand value (typically `cs --json` output) at `path`,
    /// rendered as compact JSON. `None` if the key/index was absent.
    pub left: Option<String>,
    /// Right-hand value (typically `cs-thin` output) at `path`.
    pub right: Option<String>,
    /// Human-readable kind of divergence — `missing_left`,
    /// `missing_right`, `type_mismatch`, `value_mismatch`.
    pub kind: DiffKind,
}

/// Categorical reason a [`Diff`] was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Right side has the path; left side does not.
    MissingLeft,
    /// Left side has the path; right side does not.
    MissingRight,
    /// Both sides have the path but with incompatible JSON types
    /// (object vs array, number vs string, …).
    TypeMismatch,
    /// Both sides have the path with the same type but different
    /// values.
    ValueMismatch,
}

impl DiffKind {
    /// Short string label for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingLeft => "missing_left",
            Self::MissingRight => "missing_right",
            Self::TypeMismatch => "type_mismatch",
            Self::ValueMismatch => "value_mismatch",
        }
    }
}

/// Allowlist of paths permitted to differ between `cs` and `cs-thin`.
///
/// Built either programmatically (`Allowlist::new`) or loaded from
/// `tests/parity-allowlist.toml` via [`Allowlist::from_toml_str`].
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    entries: Vec<AllowedEntry>,
}

#[derive(Debug, Clone)]
struct AllowedEntry {
    /// Verb name (e.g. `"observe"`, `"nucleate"`, `"tag"`) or `"*"`.
    verb: String,
    /// Path template (literal, `*`, or trailing `**`).
    path: String,
    /// Human rationale — recorded for audit purposes; never silently
    /// dropped from the test report on failure.
    #[allow(dead_code)]
    reason: String,
}

#[derive(Debug, Deserialize)]
struct AllowlistFile {
    allowed: Vec<AllowlistFileEntry>,
}

#[derive(Debug, Deserialize)]
struct AllowlistFileEntry {
    verb: String,
    path: String,
    reason: String,
}

impl Allowlist {
    /// Build an empty allowlist. Useful for round-trip tests of the
    /// comparator itself (every divergence is reported).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a single (verb, path, reason) entry.
    pub fn allow(
        &mut self,
        verb: impl Into<String>,
        path: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.entries.push(AllowedEntry {
            verb: verb.into(),
            path: path.into(),
            reason: reason.into(),
        });
    }

    /// Parse an allowlist from a TOML document. The expected schema is
    /// the `[[allowed]]` table-of-tables form documented in
    /// `tests/parity-allowlist.toml`.
    ///
    /// # Errors
    ///
    /// Returns the underlying `toml::de::Error` if the document fails
    /// to parse against [`AllowlistFile`].
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        let file: AllowlistFile = toml::from_str(s)?;
        Ok(Self {
            entries: file
                .allowed
                .into_iter()
                .map(|e| AllowedEntry {
                    verb: e.verb,
                    path: e.path,
                    reason: e.reason,
                })
                .collect(),
        })
    }

    /// True if the given (verb, path) combination is permitted to
    /// differ. `verb = "*"` entries match any verb.
    #[must_use]
    pub fn allows(&self, verb: &str, path: &str) -> bool {
        self.entries
            .iter()
            .any(|e| (e.verb == "*" || e.verb == verb) && path_matches(&e.path, path))
    }

    /// Number of entries — exposed so the test report can summarise
    /// the audit surface (`"compared with N allowlist entries"`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Compare two JSON values, returning every divergence not silenced
/// by the allowlist.
///
/// `verb` names the cs verb under test (e.g. `"observe"`); allowlist
/// entries are scoped per-verb (`"*"` = any).
///
/// The walk is order-stable: object keys are visited in
/// `BTreeMap`-sorted order, array indices in their natural sequence.
/// This makes the diff output deterministic across runs.
#[must_use]
pub fn compare(left: &Value, right: &Value, verb: &str, allowlist: &Allowlist) -> Vec<Diff> {
    let mut diffs = Vec::new();
    walk(left, right, "", verb, allowlist, &mut diffs);
    diffs
}

fn walk(
    left: &Value,
    right: &Value,
    path: &str,
    verb: &str,
    allowlist: &Allowlist,
    out: &mut Vec<Diff>,
) {
    match (left, right) {
        (Value::Object(l), Value::Object(r)) => {
            // Sort keys so the diff output is stable.
            let l_sorted: BTreeMap<&String, &Value> = l.iter().collect();
            let r_sorted: BTreeMap<&String, &Value> = r.iter().collect();
            let mut all_keys: Vec<&String> = l_sorted.keys().copied().collect();
            for k in r_sorted.keys() {
                if !all_keys.contains(k) {
                    all_keys.push(*k);
                }
            }
            all_keys.sort();
            for key in all_keys {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (l_sorted.get(key), r_sorted.get(key)) {
                    (Some(lv), Some(rv)) => walk(lv, rv, &child_path, verb, allowlist, out),
                    (Some(lv), None) => {
                        if !allowlist.allows(verb, &child_path) {
                            out.push(Diff {
                                path: child_path,
                                left: Some(compact(lv)),
                                right: None,
                                kind: DiffKind::MissingRight,
                            });
                        }
                    }
                    (None, Some(rv)) => {
                        if !allowlist.allows(verb, &child_path) {
                            out.push(Diff {
                                path: child_path,
                                left: None,
                                right: Some(compact(rv)),
                                kind: DiffKind::MissingLeft,
                            });
                        }
                    }
                    (None, None) => unreachable!("key drawn from union of left+right"),
                }
            }
        }
        (Value::Array(l), Value::Array(r)) => {
            let max = l.len().max(r.len());
            for i in 0..max {
                let child_path = if path.is_empty() {
                    i.to_string()
                } else {
                    format!("{path}.{i}")
                };
                match (l.get(i), r.get(i)) {
                    (Some(lv), Some(rv)) => walk(lv, rv, &child_path, verb, allowlist, out),
                    (Some(lv), None) => {
                        if !allowlist.allows(verb, &child_path) {
                            out.push(Diff {
                                path: child_path,
                                left: Some(compact(lv)),
                                right: None,
                                kind: DiffKind::MissingRight,
                            });
                        }
                    }
                    (None, Some(rv)) => {
                        if !allowlist.allows(verb, &child_path) {
                            out.push(Diff {
                                path: child_path,
                                left: None,
                                right: Some(compact(rv)),
                                kind: DiffKind::MissingLeft,
                            });
                        }
                    }
                    (None, None) => unreachable!("i bounded by max"),
                }
            }
        }
        (lv, rv) if same_kind(lv, rv) && lv == rv => { /* identical leaves */ }
        (lv, rv) if same_kind(lv, rv) => {
            if !allowlist.allows(verb, path) {
                out.push(Diff {
                    path: path.to_owned(),
                    left: Some(compact(lv)),
                    right: Some(compact(rv)),
                    kind: DiffKind::ValueMismatch,
                });
            }
        }
        (lv, rv) => {
            if !allowlist.allows(verb, path) {
                out.push(Diff {
                    path: path.to_owned(),
                    left: Some(compact(lv)),
                    right: Some(compact(rv)),
                    kind: DiffKind::TypeMismatch,
                });
            }
        }
    }
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "<unrenderable>".to_owned())
}

fn same_kind(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Null, Value::Null)
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::String(_), Value::String(_))
            | (Value::Array(_), Value::Array(_))
            | (Value::Object(_), Value::Object(_))
    )
}

/// Match a path template against an actual dotted path.
///
/// Templates support:
/// - exact segment match (`foo` matches only `foo`),
/// - single-segment wildcard (`*` matches any one segment),
/// - trailing `**` (matches zero or more segments at the end).
fn path_matches(template: &str, actual: &str) -> bool {
    let t_segs: Vec<&str> = template.split('.').collect();
    let a_segs: Vec<&str> = actual.split('.').collect();
    matches_at(&t_segs, &a_segs)
}

fn matches_at(template: &[&str], actual: &[&str]) -> bool {
    match (template.first(), actual.first()) {
        (None, None) => true,
        // Trailing `**` greedily matches the remainder, including the empty
        // suffix. Anything *after* `**` is a template-author error so we
        // reject it conservatively (only the last segment may be `**`).
        (Some(&"**"), _) => template.len() == 1,
        (Some(&"*"), Some(_)) => matches_at(&template[1..], &actual[1..]),
        (Some(t), Some(a)) if t == a => matches_at(&template[1..], &actual[1..]),
        // Length mismatch (None on one side only) or literal mismatch.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_allowlist() -> Allowlist {
        Allowlist::new()
    }

    #[test]
    fn identical_values_produce_no_diff() {
        let v = json!({"a": 1, "b": [true, "x"]});
        assert!(compare(&v, &v, "observe", &no_allowlist()).is_empty());
    }

    #[test]
    fn value_mismatch_is_reported_at_dotted_path() {
        let l = json!({"a": {"b": 1}});
        let r = json!({"a": {"b": 2}});
        let diffs = compare(&l, &r, "observe", &no_allowlist());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "a.b");
        assert_eq!(diffs[0].kind, DiffKind::ValueMismatch);
        assert_eq!(diffs[0].left.as_deref(), Some("1"));
        assert_eq!(diffs[0].right.as_deref(), Some("2"));
    }

    #[test]
    fn missing_left_and_missing_right_are_distinguished() {
        let l = json!({"a": 1});
        let r = json!({"a": 1, "b": 2});
        let diffs = compare(&l, &r, "observe", &no_allowlist());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::MissingLeft);
        assert_eq!(diffs[0].path, "b");

        let l = json!({"a": 1, "b": 2});
        let r = json!({"a": 1});
        let diffs = compare(&l, &r, "observe", &no_allowlist());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::MissingRight);
        assert_eq!(diffs[0].path, "b");
    }

    #[test]
    fn type_mismatch_is_reported() {
        let l = json!({"a": 1});
        let r = json!({"a": "1"});
        let diffs = compare(&l, &r, "observe", &no_allowlist());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::TypeMismatch);
    }

    #[test]
    fn array_index_appears_in_path() {
        let l = json!({"tags": ["a", "b"]});
        let r = json!({"tags": ["a", "c"]});
        let diffs = compare(&l, &r, "tag", &no_allowlist());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "tags.1");
    }

    #[test]
    fn allowlist_silences_a_specific_path() {
        let mut al = Allowlist::new();
        al.allow("observe", "request_id", "server-generated");
        let l = json!({"request_id": "X", "id": "m1"});
        let r = json!({"request_id": "Y", "id": "m1"});
        let diffs = compare(&l, &r, "observe", &al);
        assert!(diffs.is_empty(), "request_id should be silenced");

        // But for a different verb, the same path is NOT silenced.
        let diffs = compare(&l, &r, "nucleate", &al);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "request_id");
    }

    #[test]
    fn allowlist_wildcard_verb_silences_for_all() {
        let mut al = Allowlist::new();
        al.allow("*", "request_id", "server-generated, any verb");
        let l = json!({"request_id": "X"});
        let r = json!({"request_id": "Y"});
        assert!(compare(&l, &r, "observe", &al).is_empty());
        assert!(compare(&l, &r, "nucleate", &al).is_empty());
    }

    #[test]
    fn allowlist_wildcard_segment_matches() {
        let mut al = Allowlist::new();
        al.allow("observe", "tags.*", "tag set per-call");
        let l = json!({"tags": ["a"]});
        let r = json!({"tags": ["b"]});
        assert!(compare(&l, &r, "observe", &al).is_empty());
    }

    #[test]
    fn allowlist_double_star_matches_descendants() {
        let mut al = Allowlist::new();
        al.allow("observe", "energy.**", "energy metrics per-call");
        let l = json!({"energy": {"in": 1, "out": 2}});
        let r = json!({"energy": {"in": 9, "out": 9}});
        assert!(compare(&l, &r, "observe", &al).is_empty());
    }

    #[test]
    fn from_toml_str_round_trips() {
        let toml = r#"
[[allowed]]
verb = "*"
path = "request_id"
reason = "server-generated UUID per request"

[[allowed]]
verb = "observe"
path = "molecule_dir"
reason = "filesystem path absent on the wire"
"#;
        let al = Allowlist::from_toml_str(toml).unwrap();
        assert_eq!(al.len(), 2);
        assert!(al.allows("observe", "request_id"));
        assert!(al.allows("nucleate", "request_id"));
        assert!(al.allows("observe", "molecule_dir"));
        assert!(!al.allows("nucleate", "molecule_dir"));
    }

    #[test]
    fn path_matches_handles_three_template_forms() {
        assert!(path_matches("a.b", "a.b"));
        assert!(!path_matches("a.b", "a.c"));
        assert!(path_matches("a.*", "a.b"));
        assert!(path_matches("a.*", "a.0"));
        assert!(!path_matches("a.*", "a.b.c"), "single * is one segment");
        assert!(path_matches("a.**", "a.b"));
        assert!(path_matches("a.**", "a.b.c"));
        assert!(path_matches("a.**", "a"));
    }
}
