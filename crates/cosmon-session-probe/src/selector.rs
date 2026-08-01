// SPDX-License-Identifier: Apache-2.0

//! The canonical key of a provider session: `<provider>:<native-session-id>`.
//!
//! PROVIDER-ID-NATIVE, in three types. The mission's falsifier list contains
//! *"two unnamed sessions in the same cwd are confused"* and *"a worktree named
//! like the root is chosen by substring"*; both are failures of **keying**, not
//! of parsing. So the key is a type, it is built from the identifier the
//! provider itself minted, and the display name is a separate `Option` field
//! that no comparison ever reads.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::ProbeError;

/// A provider name — `claude`, `codex`, and whatever an adapter author adds
/// next.
///
/// This is a validated string and **not** an enum on purpose. Mission
/// falsifier 10 is *"adding a third provider requires editing `cs sessions`"*;
/// a closed enum here would make that falsifier true by construction, because
/// every `match` in the cockpit would have to grow an arm.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderName(String);

impl ProviderName {
    /// Build a provider name from `raw`.
    ///
    /// Accepts lowercase ASCII alphanumerics, `-` and `_`; rejects everything
    /// else so a name can never contain the `:` that separates it from the
    /// native id in a selector.
    ///
    /// # Errors
    ///
    /// [`ProbeError::InvalidIdentifier`] if `raw` is empty or contains a byte
    /// outside that set.
    pub fn new(raw: impl Into<String>) -> Result<Self, ProbeError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(ProbeError::InvalidIdentifier(
                "provider name is empty".to_string(),
            ));
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return Err(ProbeError::InvalidIdentifier(format!(
                "provider name {raw:?} is not [a-z0-9_-]+"
            )));
        }
        Ok(Self(raw))
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The identifier the provider itself minted for a session.
///
/// Claude puts it in the log filename *and* in every record's `sessionId`;
/// Codex puts it in `session_meta.payload.session_id`. Neither is derived from
/// a title, a tmux pane name or a directory name, which is exactly why the
/// protocol keys on it: a `/rename` must not move a session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NativeSessionId(String);

impl NativeSessionId {
    /// Build a native session id from `raw`.
    ///
    /// # Errors
    ///
    /// [`ProbeError::InvalidIdentifier`] if `raw` is empty or contains
    /// whitespace or a `:` — both would make a selector ambiguous.
    pub fn new(raw: impl Into<String>) -> Result<Self, ProbeError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(ProbeError::InvalidIdentifier(
                "native session id is empty".to_string(),
            ));
        }
        if raw.chars().any(|c| c.is_whitespace() || c == ':') {
            return Err(ProbeError::InvalidIdentifier(format!(
                "native session id {raw:?} contains whitespace or ':'"
            )));
        }
        Ok(Self(raw))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NativeSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The canonical selector of a session: `<provider>:<native-session-id>`.
///
/// Round-trips through [`Display`](fmt::Display) and [`FromStr`], so an
/// operator can copy one out of a listing and paste it into the next command
/// without an intermediate lookup table.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionSelector {
    /// The provider that minted the session.
    pub provider: ProviderName,
    /// The provider's own id for it.
    pub native_session_id: NativeSessionId,
}

impl SessionSelector {
    /// Compose a selector from its two halves.
    #[must_use]
    pub fn new(provider: ProviderName, native_session_id: NativeSessionId) -> Self {
        Self {
            provider,
            native_session_id,
        }
    }
}

impl fmt::Display for SessionSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.native_session_id)
    }
}

impl FromStr for SessionSelector {
    type Err = ProbeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((provider, native)) = s.split_once(':') else {
            return Err(ProbeError::InvalidSelector {
                input: s.to_string(),
                reason: "expected <provider>:<native-session-id>",
            });
        };
        let provider = ProviderName::new(provider).map_err(|_| ProbeError::InvalidSelector {
            input: s.to_string(),
            reason: "provider half is not [a-z0-9_-]+",
        })?;
        let native_session_id =
            NativeSessionId::new(native).map_err(|_| ProbeError::InvalidSelector {
                input: s.to_string(),
                reason: "native-session-id half is empty or contains whitespace",
            })?;
        Ok(Self::new(provider, native_session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_round_trips_through_text() {
        let text = "claude:4940f28e-0000-4000-8000-000000000001";
        let parsed: SessionSelector = text.parse().unwrap();
        assert_eq!(parsed.provider.as_str(), "claude");
        assert_eq!(parsed.to_string(), text);
    }

    #[test]
    fn a_provider_name_may_not_hide_a_separator() {
        assert!(ProviderName::new("clau:de").is_err());
        assert!(ProviderName::new("Claude").is_err(), "uppercase rejected");
        assert!(ProviderName::new("").is_err());
    }

    #[test]
    fn a_native_id_may_not_contain_whitespace_or_separator() {
        assert!(NativeSessionId::new("has space").is_err());
        assert!(NativeSessionId::new("has:colon").is_err());
        assert!(NativeSessionId::new("rollout-2026").is_ok());
    }

    #[test]
    fn a_selector_without_a_separator_is_refused() {
        assert!("just-an-id".parse::<SessionSelector>().is_err());
    }
}
