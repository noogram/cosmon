// SPDX-License-Identifier: Apache-2.0

//! Lightweight energy types for session measurement.
//!
//! These are claudion-local newtypes for token counting and costing.
//! claudion is a self-contained frontier crate (ADR-092): rather than
//! depend on `cosmon-core`, it defines its own copies so a third party
//! can `cargo add claudion` without pulling the AGPL core. Consumers that
//! also use `cosmon-core` can convert via `.get()` / `::new()`.

use std::fmt;
use std::ops::Add;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A count of tokens (input or output). Wraps `u64`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TokenCount(u64);

impl TokenCount {
    /// Create a new token count.
    #[must_use]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    /// Return the inner value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Add for TokenCount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for TokenCount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl fmt::Display for TokenCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} tokens", self.0)
    }
}

/// A monetary cost in currency units (e.g. USD). Wraps `f64`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenCost(f64);

impl TokenCost {
    /// Create a new token cost.
    #[must_use]
    pub fn new(amount: f64) -> Self {
        Self(amount)
    }

    /// Return the inner value.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Add for TokenCost {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl fmt::Display for TokenCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.4}", self.0)
    }
}

/// Identifies a session. Non-empty string newtype.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

/// Error when constructing an ID from an empty string.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("SessionId cannot be empty")]
pub struct SessionIdError;

impl SessionId {
    /// Create a new session ID, validating non-emptiness.
    ///
    /// # Errors
    /// Returns [`SessionIdError`] if the string is empty.
    pub fn new(s: impl Into<String>) -> Result<Self, SessionIdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SessionIdError);
        }
        Ok(Self(s))
    }

    /// Return the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SessionId {
    type Err = SessionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for SessionId {
    type Error = SessionIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<SessionId> for String {
    fn from(id: SessionId) -> Self {
        id.0
    }
}
