// SPDX-License-Identifier: AGPL-3.0-only

//! Tiny dot-path evaluator for `[steps.query]` expressions.
//!
//! This module implements a deliberately small subset of `JSONPath` suitable
//! for the historical `cs --json observe … | jq …` shell-outs that motivated
//! it. The grammar is:
//!
//! ```text
//! expr   ::= '.' | path
//! path   ::= segment (segment)*
//! segment::= '.' ident | '[' index ']'
//! ident  ::= [A-Za-z_][A-Za-z0-9_-]*
//! index  ::= integer
//! ```
//!
//! Examples:
//!
//! - `.` — the root document.
//! - `.id` — the `id` field of an object.
//! - `.variables.versions` — chained field access.
//! - `.steps[0].name` — array indexing then field access.
//!
//! Anything more elaborate (filters, slices, recursive descent) is an
//! intentional non-goal: when a real query language is needed, callers
//! reach for SQL via the event store, not this evaluator.

use serde_json::Value;

/// Errors from [`evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DotPathError {
    /// The expression failed to parse (malformed segment, unbalanced
    /// bracket, …). The string carries a short remediation hint.
    Parse(String),
    /// The expression resolved a field on a non-object.
    NotObject(String),
    /// The expression resolved an index on a non-array.
    NotArray(String),
    /// A field name did not exist in the object at this position.
    MissingField(String),
    /// An index was out of bounds for the array at this position.
    IndexOutOfBounds {
        /// 0-based index that overflowed.
        index: usize,
        /// Length of the array at the resolution site.
        len: usize,
    },
}

impl std::fmt::Display for DotPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "dot-path parse error: {msg}"),
            Self::NotObject(field) => {
                write!(f, "dot-path: cannot resolve field `{field}` on non-object")
            }
            Self::NotArray(idx) => {
                write!(f, "dot-path: cannot index `[{idx}]` on non-array")
            }
            Self::MissingField(field) => {
                write!(f, "dot-path: object has no field `{field}`")
            }
            Self::IndexOutOfBounds { index, len } => {
                write!(
                    f,
                    "dot-path: index {index} out of bounds for array of length {len}",
                )
            }
        }
    }
}

impl std::error::Error for DotPathError {}

/// Parsed segment of a dot-path expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Field(String),
    Index(usize),
}

fn parse(expr: &str) -> Result<Vec<Segment>, DotPathError> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err(DotPathError::Parse(
            "empty expression — use `.` to select the root".to_owned(),
        ));
    }
    if trimmed == "." {
        return Ok(Vec::new());
    }
    let bytes = trimmed.as_bytes();
    if bytes[0] != b'.' && bytes[0] != b'[' {
        return Err(DotPathError::Parse(format!(
            "expression must start with `.` or `[`; got `{}`",
            &trimmed[..1]
        )));
    }
    let mut segments = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    let c = bytes[i];
                    let ok = c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
                    if !ok {
                        return Err(DotPathError::Parse(format!(
                            "unexpected character `{}` in field name",
                            c as char,
                        )));
                    }
                    i += 1;
                }
                if i == start {
                    return Err(DotPathError::Parse("empty field name after `.`".to_owned()));
                }
                segments.push(Segment::Field(trimmed[start..i].to_owned()));
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    if !bytes[i].is_ascii_digit() {
                        return Err(DotPathError::Parse(format!(
                            "non-digit `{}` inside `[...]` index",
                            bytes[i] as char,
                        )));
                    }
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(DotPathError::Parse(
                        "unbalanced `[` — missing `]`".to_owned(),
                    ));
                }
                let idx: usize = trimmed[start..i].parse().map_err(|_| {
                    DotPathError::Parse(format!(
                        "invalid index `{}` in `[...]`",
                        &trimmed[start..i],
                    ))
                })?;
                segments.push(Segment::Index(idx));
                i += 1;
            }
            other => {
                return Err(DotPathError::Parse(format!(
                    "unexpected character `{}` between segments",
                    other as char,
                )));
            }
        }
    }
    Ok(segments)
}

/// Evaluate a dot-path expression against a JSON [`Value`].
///
/// Returns a borrowed reference into the input value; clone if the caller
/// needs ownership.
///
/// # Errors
/// Returns [`DotPathError`] on parse failure or when the path cannot be
/// resolved against the document (missing field, out-of-bounds index, type
/// mismatch).
pub fn evaluate<'a>(expr: &str, value: &'a Value) -> Result<&'a Value, DotPathError> {
    let segments = parse(expr)?;
    let mut cur = value;
    for seg in &segments {
        match seg {
            Segment::Field(name) => {
                let obj = cur
                    .as_object()
                    .ok_or_else(|| DotPathError::NotObject(name.clone()))?;
                cur = obj
                    .get(name)
                    .ok_or_else(|| DotPathError::MissingField(name.clone()))?;
            }
            Segment::Index(idx) => {
                let arr = cur
                    .as_array()
                    .ok_or_else(|| DotPathError::NotArray(idx.to_string()))?;
                cur = arr.get(*idx).ok_or(DotPathError::IndexOutOfBounds {
                    index: *idx,
                    len: arr.len(),
                })?;
            }
        }
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn root_selects_whole_document() {
        let v = json!({"a": 1});
        assert_eq!(evaluate(".", &v).unwrap(), &v);
    }

    #[test]
    fn field_access() {
        let v = json!({"id": "abc", "n": 7});
        assert_eq!(evaluate(".id", &v).unwrap(), &json!("abc"));
        assert_eq!(evaluate(".n", &v).unwrap(), &json!(7));
    }

    #[test]
    fn nested_field() {
        let v = json!({"variables": {"versions": ["1.0", "1.1"]}});
        assert_eq!(
            evaluate(".variables.versions", &v).unwrap(),
            &json!(["1.0", "1.1"]),
        );
    }

    #[test]
    fn index_then_field() {
        let v = json!({"steps": [{"name": "a"}, {"name": "b"}]});
        assert_eq!(evaluate(".steps[0].name", &v).unwrap(), &json!("a"));
        assert_eq!(evaluate(".steps[1].name", &v).unwrap(), &json!("b"));
    }

    #[test]
    fn missing_field_errors() {
        let v = json!({});
        let err = evaluate(".missing", &v).unwrap_err();
        assert!(matches!(err, DotPathError::MissingField(_)));
    }

    #[test]
    fn index_out_of_bounds_errors() {
        let v = json!([1, 2]);
        let err = evaluate("[5]", &v).unwrap_err();
        assert!(matches!(
            err,
            DotPathError::IndexOutOfBounds { index: 5, len: 2 },
        ));
    }

    #[test]
    fn empty_expression_rejected() {
        assert!(matches!(
            evaluate("", &json!(0)),
            Err(DotPathError::Parse(_))
        ));
    }

    #[test]
    fn malformed_index_rejected() {
        assert!(matches!(
            evaluate(".a[", &json!({})),
            Err(DotPathError::Parse(_)),
        ));
        assert!(matches!(
            evaluate(".a[ab]", &json!({})),
            Err(DotPathError::Parse(_)),
        ));
    }

    #[test]
    fn dash_in_field_name_supported() {
        let v = json!({"my-field": 1});
        assert_eq!(evaluate(".my-field", &v).unwrap(), &json!(1));
    }
}
