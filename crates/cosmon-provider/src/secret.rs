// SPDX-License-Identifier: AGPL-3.0-only

//! Minimal redacting newtype for sensitive provider state.
//!
//! The pre-fix shape was a
//! credential-exfiltration surface: every provider adapter exposed
//! `pub api_key: String` and derived `Debug`, so a single
//! `tracing::debug!("{:?}", provider)` splattered the key into logs.
//!
//! [`Secret<String>`] is the load-bearing fix. The newtype:
//!
//! 1. Has a hand-written `Debug` and `Display` that always print
//!    `"<redacted>"` regardless of the inner value — the type system
//!    enforces redaction at every `{:?}` / `{}` format site.
//! 2. Has no derived `Serialize` — secrets never accidentally land in
//!    a `serde_json::to_string(&provider)` call.
//! 3. Exposes the inner value only through [`Secret::expose`], a
//!    grep-friendly call site every audit can find.
//!
//! Sized to ~30 lines on purpose: we do *not* pull in the `secrecy`
//! crate (extra dep, extra unsafe surface) when the only operation we
//! need is "redact on every format trait". A future zeroize / mlock
//! upgrade is a single field swap behind the same external API.

use std::fmt;

/// A wrapper that suppresses every implicit log / serialize path for
/// its inner value. The only way to read the secret is to call
/// [`Self::expose`], which is greppable.
///
/// `T` is most commonly `String` (API keys) but any inner type is
/// permitted; only `Debug` / `Display` are intercepted.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap a value as a [`Secret`]. The inner value is moved in.
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Borrow the inner value. The call site name is grep-bait by
    /// design — auditors can locate every reveal.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl<S: Into<String>> From<S> for Secret<String> {
    fn from(value: S) -> Self {
        Self(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak_inner_value() {
        let s: Secret<String> = "sk-very-secret-token".to_owned().into();
        let formatted = format!("{s:?}");
        assert!(
            !formatted.contains("sk-very-secret-token"),
            "Debug must not contain the inner value; got: {formatted}"
        );
        assert!(
            formatted.contains("redacted"),
            "Debug should mark the value as redacted; got: {formatted}"
        );
    }

    #[test]
    fn display_does_not_leak_inner_value() {
        let s: Secret<String> = "sk-very-secret-token".to_owned().into();
        let formatted = format!("{s}");
        assert!(
            !formatted.contains("sk-very-secret-token"),
            "Display must not contain the inner value; got: {formatted}"
        );
        assert!(
            formatted.contains("redacted"),
            "Display should mark the value as redacted; got: {formatted}"
        );
    }

    #[test]
    fn expose_returns_inner_value() {
        let s: Secret<String> = "exposable".to_owned().into();
        assert_eq!(s.expose(), "exposable");
    }

    #[test]
    fn clone_preserves_inner_value_through_expose() {
        let s: Secret<String> = "shared".to_owned().into();
        let t = s.clone();
        assert_eq!(t.expose(), "shared");
        assert_eq!(s.expose(), "shared");
    }
}
