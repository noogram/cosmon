// SPDX-License-Identifier: AGPL-3.0-only

//! Dispatch-claim ownership for a molecule — the anti-preemption lease.
//!
//! # Why this module exists
//!
//! `cs tackle` is **human-only** by command perimeter (see
//! `docs/architectural-invariants.md`). The resident runtime (`cs run`)
//! also dispatches molecules — and the two are concurrent writers racing on
//! the same `Pending → Active` transition. The runtime polls every few
//! seconds while a human types with fingers, so the runtime almost always
//! wins the race and **raffles a molecule a human manually reached for**.
//! This is a scope bug of the convoy-cascade family: the runtime resurrects
//! or steals work the operator deliberately claimed.
//!
//! The fix is a **dispatch lease recorded in the molecule's own state**,
//! using the git ownership-by-claim model (ownership recorded in a field,
//! conflict *detected* at re-read — not a lock-file or a mutex). The
//! missing datum was *who* dispatched, not *which* worker: `assigned_worker`
//! answers "which worker", [`TackledBy`] answers "which actor class".
//!
//! # "Manual always wins" — a binary owner field, no clock
//!
//! The lease is deliberately a binary owner field, **not** a tunable
//! "runtime never touches a molecule a human touched in the last N seconds".
//! A clock introduces a window you must calibrate; the owner field needs no
//! tuning. The walker honours it by skipping any candidate whose
//! `tackled_by == TackledBy::Human`, even if the molecule briefly returns to
//! `Pending` on a revision. A human claim is sticky; a runtime claim is not
//! (so the runtime may freely re-dispatch its own stranded work).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The actor class that holds the dispatch claim on a molecule.
///
/// Serialises as a flat string on the wire — `"human"` or
/// `"runtime:<pid>"` — so a `state.json` reader (and the human eye reading
/// the file) sees the claim at a glance, and legacy molecules that predate
/// the field deserialise as absent (`None`) rather than failing.
///
/// ```
/// use cosmon_core::tackle::TackledBy;
/// assert_eq!(TackledBy::Human.to_string(), "human");
/// assert_eq!(TackledBy::runtime(4242).to_string(), "runtime:4242");
/// assert_eq!("human".parse::<TackledBy>().unwrap(), TackledBy::Human);
/// assert_eq!(
///     "runtime:4242".parse::<TackledBy>().unwrap(),
///     TackledBy::runtime(4242)
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum TackledBy {
    /// A human ran `cs tackle` directly. This is the **sticky** lease the
    /// resident runtime must never preempt.
    Human,
    /// The resident runtime (`cs run`) dispatched this molecule, carrying
    /// its own OS process id for forensics. A runtime claim does **not**
    /// block re-dispatch — only human claims are sticky.
    Runtime {
        /// OS process id of the `cs run` walker that recorded the claim.
        pid: u32,
    },
}

impl TackledBy {
    /// Construct a runtime claim carrying the walker's process id.
    #[must_use]
    pub fn runtime(pid: u32) -> Self {
        Self::Runtime { pid }
    }

    /// Returns `true` when a human holds the (sticky) dispatch claim.
    ///
    /// This is the single predicate the walker consults to enforce
    /// "manual always wins".
    #[must_use]
    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human)
    }
}

impl fmt::Display for TackledBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Human => f.write_str("human"),
            Self::Runtime { pid } => write!(f, "runtime:{pid}"),
        }
    }
}

/// Error returned when a `tackled_by` wire string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TackledByParseError {
    /// The string was empty or whitespace-only.
    #[error("empty tackled_by actor string")]
    Empty,
    /// The `runtime:` prefix was present but the pid did not parse as `u32`.
    #[error("runtime actor has an invalid pid in '{0}' (expected 'runtime:<u32>')")]
    BadPid(String),
    /// The string matched neither `human` nor `runtime:<pid>`.
    #[error("unknown tackled_by actor class '{0}' (expected 'human' or 'runtime:<pid>')")]
    Unknown(String),
}

impl FromStr for TackledBy {
    type Err = TackledByParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(TackledByParseError::Empty);
        }
        if s == "human" {
            return Ok(Self::Human);
        }
        if let Some(pid_str) = s.strip_prefix("runtime:") {
            let pid = pid_str
                .trim()
                .parse::<u32>()
                .map_err(|_| TackledByParseError::BadPid(s.to_owned()))?;
            return Ok(Self::Runtime { pid });
        }
        Err(TackledByParseError::Unknown(s.to_owned()))
    }
}

impl From<TackledBy> for String {
    fn from(value: TackledBy) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for TackledBy {
    type Error = TackledByParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_round_trips_through_string() {
        let v = TackledBy::Human;
        let s = v.to_string();
        assert_eq!(s, "human");
        assert_eq!(s.parse::<TackledBy>().unwrap(), v);
    }

    #[test]
    fn runtime_round_trips_through_string() {
        let v = TackledBy::runtime(31337);
        let s = v.to_string();
        assert_eq!(s, "runtime:31337");
        assert_eq!(s.parse::<TackledBy>().unwrap(), v);
    }

    #[test]
    fn is_human_distinguishes_the_sticky_lease() {
        assert!(TackledBy::Human.is_human());
        assert!(!TackledBy::runtime(1).is_human());
    }

    #[test]
    fn serde_emits_flat_strings() {
        let json = serde_json::to_string(&TackledBy::Human).unwrap();
        assert_eq!(json, "\"human\"");
        let json = serde_json::to_string(&TackledBy::runtime(7)).unwrap();
        assert_eq!(json, "\"runtime:7\"");
    }

    #[test]
    fn serde_parses_flat_strings() {
        let v: TackledBy = serde_json::from_str("\"human\"").unwrap();
        assert_eq!(v, TackledBy::Human);
        let v: TackledBy = serde_json::from_str("\"runtime:99\"").unwrap();
        assert_eq!(v, TackledBy::runtime(99));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!("".parse::<TackledBy>(), Err(TackledByParseError::Empty));
        assert!(matches!(
            "robot".parse::<TackledBy>(),
            Err(TackledByParseError::Unknown(_))
        ));
        assert!(matches!(
            "runtime:notapid".parse::<TackledBy>(),
            Err(TackledByParseError::BadPid(_))
        ));
    }
}
