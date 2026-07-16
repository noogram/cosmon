// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical JSON serialization.
//!
//! Stable byte sequence for any `serde::Serialize` value:
//!
//! * Object keys sorted lexicographically (UTF-8 byte order — same as
//!   `BTreeMap<String, _>` iteration).
//! * No whitespace.
//! * Numbers re-emitted via `serde_json` (which uses shortest-round-trip
//!   for floats).
//! * UTF-8 strings escaped exactly as `serde_json` does.
//!
//! Roughly 100 lines, as Torvalds asked. We avoid pulling in the
//! `serde_json_canonicalizer` crate so that our hash recipe stays
//! transparent: read this file and you see exactly what gets hashed.

use serde::Serialize;
use serde_json::{Map, Value};

/// Canonical-serialization error.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    /// The value could not be serialized to `serde_json::Value`.
    #[error("serde_json error: {0}")]
    Json(#[from] serde_json::Error),
    /// UTF-8 encoding failed when emitting the canonical form.
    #[error("utf-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

/// Serialize `value` to canonical JSON bytes.
///
/// # Errors
///
/// Returns [`CanonicalError`] if the value cannot be turned into a
/// `serde_json::Value` (e.g. map with non-string keys).
pub fn canonical_serialize<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    let v: Value = serde_json::to_value(value)?;
    let mut out = Vec::with_capacity(128);
    write_canonical(&v, &mut out)?;
    Ok(out)
}

fn write_canonical(v: &Value, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => write_object(map, out)?,
    }
    Ok(())
}

fn write_object(map: &Map<String, Value>, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_unstable();
    out.push(b'{');
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_string(k, out);
        out.push(b':');
        write_canonical(&map[*k], out)?;
    }
    out.push(b'}');
    Ok(())
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    // Delegate to serde_json so escape rules match exactly what callers
    // see when they look at JSONL on disk.
    let encoded = serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""));
    out.extend_from_slice(encoded.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_sorted_lexicographically() {
        let v = json!({"b": 1, "a": 2, "c": 3});
        let bytes = canonical_serialize(&v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"a":2,"b":1,"c":3}"#
        );
    }

    #[test]
    fn nested_objects_sorted() {
        let v = json!({"z": {"y": 1, "x": 2}, "a": [3, 2, 1]});
        let bytes = canonical_serialize(&v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"a":[3,2,1],"z":{"x":2,"y":1}}"#
        );
    }

    #[test]
    fn no_whitespace() {
        let v = json!({"a": 1, "b": [1, 2]});
        let bytes = canonical_serialize(&v).unwrap();
        assert!(!bytes.iter().any(|b| matches!(*b, b' ' | b'\n' | b'\t')));
    }

    #[test]
    fn arrays_preserve_order() {
        let v = json!([3, 1, 2]);
        let bytes = canonical_serialize(&v).unwrap();
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "[3,1,2]");
    }

    #[test]
    fn strings_escaped_as_serde_json() {
        let v = json!({"k": "hello\nworld\""});
        let bytes = canonical_serialize(&v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"k":"hello\nworld\""}"#
        );
    }

    #[test]
    fn null_bool_number() {
        let v = json!({"n": null, "b": true, "x": 42});
        let bytes = canonical_serialize(&v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"b":true,"n":null,"x":42}"#
        );
    }

    proptest::proptest! {
        #[test]
        fn canonical_is_deterministic(s in "[a-z]{1,8}", n in 0i64..1000) {
            let v = json!({"s": s, "n": n});
            let a = canonical_serialize(&v).unwrap();
            let b = canonical_serialize(&v).unwrap();
            proptest::prop_assert_eq!(a, b);
        }

        #[test]
        fn canonical_roundtrips_through_json(
            keys in proptest::collection::vec("[a-z]{1,4}", 0..6),
            vals in proptest::collection::vec(0i64..1000, 0..6),
        ) {
            let mut map = serde_json::Map::new();
            for (k, v) in keys.iter().zip(vals.iter()) {
                map.insert(k.clone(), json!(v));
            }
            let v = Value::Object(map);
            let bytes = canonical_serialize(&v).unwrap();
            let parsed: Value = serde_json::from_slice(&bytes).unwrap();
            // Reparse and re-canonicalize must give same bytes.
            let bytes2 = canonical_serialize(&parsed).unwrap();
            proptest::prop_assert_eq!(bytes, bytes2);
        }
    }
}
