// SPDX-License-Identifier: AGPL-3.0-only

//! Reproducibility classification for agent computations.
//!
//! Not all agent work is equally reproducible. A pure code formatter will
//! produce identical output on every run; an LLM-backed researcher may not.
//! [`ReproducibilityClass`] captures this spectrum so the system can make
//! informed decisions about caching, retry, and verification strategies.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::reproducibility::ReproducibilityClass;
//!
//! let class: ReproducibilityClass = "deterministic".parse().unwrap();
//! assert_eq!(class, ReproducibilityClass::Deterministic);
//! assert_eq!(class.to_string(), "deterministic");
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;

/// How reproducible a computation's output is given the same inputs.
///
/// This classification drives downstream decisions: deterministic steps can be
/// cached aggressively; approximate ones need tolerance-based comparison;
/// non-reproducible ones must be re-evaluated on every run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproducibilityClass {
    /// Identical output for identical input, every time.
    ///
    /// Examples: code formatting, static analysis, pure transformations.
    Deterministic,

    /// Identical output when the same random seed is supplied.
    ///
    /// Examples: stochastic simulations, shuffled sampling, Monte Carlo methods.
    SeedDeterministic,

    /// Output varies within a bounded tolerance.
    ///
    /// Examples: floating-point numerics across platforms, approximate algorithms.
    Approximate,

    /// Output may differ arbitrarily between runs.
    ///
    /// Examples: LLM generation, real-time data queries, human-in-the-loop steps.
    NonReproducible,
}

impl fmt::Display for ReproducibilityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deterministic => f.write_str("deterministic"),
            Self::SeedDeterministic => f.write_str("seed_deterministic"),
            Self::Approximate => f.write_str("approximate"),
            Self::NonReproducible => f.write_str("non_reproducible"),
        }
    }
}

impl FromStr for ReproducibilityClass {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "deterministic" => Ok(Self::Deterministic),
            "seed_deterministic" => Ok(Self::SeedDeterministic),
            "approximate" => Ok(Self::Approximate),
            "non_reproducible" => Ok(Self::NonReproducible),
            _ => Err(ParseEnumError {
                type_name: "ReproducibilityClass",
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reproducibility_class_display_roundtrip() {
        let classes = [
            ReproducibilityClass::Deterministic,
            ReproducibilityClass::SeedDeterministic,
            ReproducibilityClass::Approximate,
            ReproducibilityClass::NonReproducible,
        ];
        for class in classes {
            let s = class.to_string();
            let parsed: ReproducibilityClass = s.parse().unwrap();
            assert_eq!(parsed, class);
        }
    }

    #[test]
    fn test_reproducibility_class_json_roundtrip() {
        let classes = [
            ReproducibilityClass::Deterministic,
            ReproducibilityClass::SeedDeterministic,
            ReproducibilityClass::Approximate,
            ReproducibilityClass::NonReproducible,
        ];
        for class in classes {
            let json = serde_json::to_string(&class).unwrap();
            let back: ReproducibilityClass = serde_json::from_str(&json).unwrap();
            assert_eq!(back, class);
        }
    }

    #[test]
    fn test_reproducibility_class_parse_unknown_variant() {
        let result = "unknown".parse::<ReproducibilityClass>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.type_name, "ReproducibilityClass");
        assert_eq!(err.value, "unknown");
    }

    #[test]
    fn test_reproducibility_class_exhaustive() {
        let classes = [
            ReproducibilityClass::Deterministic,
            ReproducibilityClass::SeedDeterministic,
            ReproducibilityClass::Approximate,
            ReproducibilityClass::NonReproducible,
        ];
        let mut seen = std::collections::HashSet::new();
        for class in classes {
            let label = match class {
                ReproducibilityClass::Deterministic => "deterministic",
                ReproducibilityClass::SeedDeterministic => "seed_deterministic",
                ReproducibilityClass::Approximate => "approximate",
                ReproducibilityClass::NonReproducible => "non_reproducible",
            };
            assert!(seen.insert(label), "duplicate class: {label}");
        }
        assert_eq!(seen.len(), 4);
    }
}
