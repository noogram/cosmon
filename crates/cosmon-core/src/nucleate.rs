// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule nucleation — creating a new molecule instance from a formula.
//!
//! Nucleation is the domain operation that produces the initial state for a
//! molecule from a parsed `Formula`, user-supplied variables, and an optional
//! worker assignment. This module contains no I/O and no persistence types;
//! callers convert the result into their persistence representation.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rand::Rng;

use crate::formula::Formula;
use crate::id::{FormulaId, IdError, MoleculeId, WorkerId};
use crate::molecule::MoleculeStatus;

/// Errors specific to the nucleation process.
#[derive(Debug, thiserror::Error)]
pub enum NucleateError {
    /// The generated molecule ID was invalid (bad prefix).
    #[error("id generation failed: {0}")]
    IdGeneration(#[from] IdError),

    /// A required formula variable was not provided and has no default.
    #[error("missing required variable: {0}")]
    MissingVariable(String),

    /// A required formula variable was provided, but its value is blank
    /// (empty or whitespace-only). A blank required variable carries no
    /// operator intent — nucleating it would birth a briefless molecule that
    /// the dispatch guard would later refuse to tackle. Rejecting it at the
    /// source is the nucleation half of the briefless-molecule defence
    /// (task-20260711-919a); the dispatch half lives in `cs tackle`.
    #[error("required variable is empty: {0} (provide a non-blank value)")]
    EmptyVariable(String),
}

/// Configuration for nucleating a molecule.
pub struct NucleateRequest<'a> {
    /// The parsed formula to instantiate.
    pub formula: &'a Formula,
    /// Variable bindings (key=value pairs).
    pub variables: HashMap<String, String>,
    /// Optional worker to assign immediately.
    pub assign: Option<WorkerId>,
}

/// The result of a successful nucleation — all data needed to persist a molecule.
#[derive(Debug, Clone)]
pub struct NucleateResult {
    /// The generated molecule identifier.
    pub id: MoleculeId,
    /// The formula this molecule was instantiated from.
    pub formula_id: FormulaId,
    /// Initial lifecycle status (`Pending` if unassigned, `Queued` if assigned).
    pub status: MoleculeStatus,
    /// Resolved variable bindings (user-supplied merged with defaults).
    pub variables: HashMap<String, String>,
    /// Worker assigned at creation time, if any.
    pub assigned_worker: Option<WorkerId>,
    /// Timestamp of molecule creation.
    pub created_at: DateTime<Utc>,
    /// Number of steps in the formula.
    pub total_steps: usize,
}

/// Nucleate a new molecule from a formula.
///
/// Generates a unique `MoleculeId`, validates required variables, applies
/// defaults, and returns a [`NucleateResult`] ready for persistence.
///
/// The RNG is injected by the caller so the domain stays pure
/// (INV-DOMAIN-PURE-NO-IO, ADR-082): the boundary owns the entropy source.
///
/// # Errors
/// Returns `NucleateError::MissingVariable` if a required variable has no
/// value and no default. Returns `NucleateError::IdGeneration` if the
/// formula's `id_prefix` is invalid.
pub fn nucleate<R: Rng + ?Sized>(
    req: NucleateRequest<'_>,
    rng: &mut R,
) -> Result<NucleateResult, NucleateError> {
    let formula = req.formula;

    // Determine prefix: use formula's id_prefix if non-empty, else "mol".
    let prefix = if formula.id_prefix.is_empty() {
        "mol"
    } else {
        &formula.id_prefix
    };

    let id = MoleculeId::generate(prefix, rng)?;

    // Resolve variables: user-supplied > default > error if required.
    let mut variables = HashMap::new();
    for (key, var_def) in &formula.variables {
        if let Some(val) = req.variables.get(key) {
            // A required variable provided blank (empty / whitespace-only)
            // carries no intent — reject it rather than birthing a briefless
            // molecule. Optional variables may legitimately be empty.
            if var_def.required && var_def.default.is_none() && val.trim().is_empty() {
                return Err(NucleateError::EmptyVariable(key.clone()));
            }
            variables.insert(key.clone(), val.clone());
        } else if let Some(ref default) = var_def.default {
            variables.insert(key.clone(), default.clone());
        } else if var_def.required {
            return Err(NucleateError::MissingVariable(key.clone()));
        }
    }
    // Include user-supplied variables not declared in the formula (pass-through).
    for (key, val) in &req.variables {
        variables.entry(key.clone()).or_insert_with(|| val.clone());
    }

    let now = Utc::now();

    let status = if req.assign.is_some() {
        MoleculeStatus::Queued
    } else {
        MoleculeStatus::Pending
    };

    Ok(NucleateResult {
        id,
        formula_id: formula.name.clone(),
        status,
        variables,
        assigned_worker: req.assign,
        created_at: now,
        total_steps: formula.steps.len(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;
    use crate::formula::Formula;

    fn test_rng() -> StdRng {
        // Seeded RNG keeps tests deterministic and avoids touching ambient
        // entropy (preserves INV-DOMAIN-PURE-NO-IO inside cosmon-core/src).
        StdRng::seed_from_u64(0)
    }

    fn minimal_formula() -> Formula {
        Formula::parse(
            r#"
formula = "test-formula"
version = 1
description = "A test formula"
id_prefix = "tf"

[[steps]]
id = "step-1"
title = "First step"
description = "Do the first thing."
acceptance = "First thing done"

[[steps]]
id = "step-2"
title = "Second step"
description = "Do the second thing."
needs = ["step-1"]
"#,
        )
        .unwrap()
    }

    fn formula_with_vars() -> Formula {
        Formula::parse(
            r#"
formula = "vars-formula"
version = 1
id_prefix = "vf"

[vars.target]
description = "Deploy target"
required = true

[vars.verbose]
description = "Enable verbose output"
type = "bool"
default = "false"

[[steps]]
id = "only-step"
title = "Step"
description = "Do it."
"#,
        )
        .unwrap()
    }

    #[test]
    fn test_nucleate_creates_valid_molecule() {
        let formula = minimal_formula();
        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: HashMap::new(),
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.formula_id.as_str(), "test-formula");
        assert_eq!(result.status, MoleculeStatus::Pending);
        assert_eq!(result.total_steps, 2);
        assert!(result.assigned_worker.is_none());
    }

    #[test]
    fn test_nucleate_generates_valid_id() {
        let formula = minimal_formula();
        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: HashMap::new(),
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.id.prefix(), "tf");
        assert_eq!(result.id.date().len(), 8);
        assert_eq!(result.id.suffix().len(), 4);
        // ID should round-trip
        let reparsed = MoleculeId::new(result.id.as_str()).unwrap();
        assert_eq!(reparsed, result.id);
    }

    /// A formula whose `id_prefix` contains a hyphen (`bug-closure`, the
    /// `drift-*` family) must nucleate. Regression for task-20260705-6c3a:
    /// `MoleculeId::generate` used to reject any non-alphanumeric prefix, so
    /// these formulas failed at ID generation and could never be nucleated.
    #[test]
    fn test_nucleate_hyphenated_id_prefix() {
        let formula = Formula::parse(
            r#"
formula = "bug-closure"
version = 1
description = "A meta-formula with a hyphenated id_prefix"
id_prefix = "bug-closure"

[[steps]]
id = "step-1"
title = "First step"
description = "Do the first thing."
"#,
        )
        .unwrap();

        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: HashMap::new(),
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.id.prefix(), "bug-closure");
        // The generated ID round-trips through the parser.
        let reparsed = MoleculeId::new(result.id.as_str()).unwrap();
        assert_eq!(reparsed, result.id);
    }

    #[test]
    fn test_nucleate_with_variables() {
        let formula = formula_with_vars();
        let mut vars = HashMap::new();
        vars.insert("target".to_string(), "production".to_string());

        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: vars,
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.variables.get("target").unwrap(), "production");
        // Default should be applied
        assert_eq!(result.variables.get("verbose").unwrap(), "false");
    }

    #[test]
    fn test_nucleate_missing_required_variable_errors() {
        let formula = formula_with_vars();
        let err = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: HashMap::new(),
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap_err();

        assert!(matches!(err, NucleateError::MissingVariable(ref k) if k == "target"));
    }

    #[test]
    fn test_nucleate_empty_required_variable_errors() {
        // A required variable provided blank must be rejected — this is the
        // nucleation half of the briefless-molecule guard (919a). `--var
        // target=""` and `--var target="   "` are both blank.
        let formula = formula_with_vars();
        for blank in ["", "   ", "\t\n"] {
            let mut vars = HashMap::new();
            vars.insert("target".to_string(), blank.to_string());
            let err = nucleate(
                NucleateRequest {
                    formula: &formula,
                    variables: vars,
                    assign: None,
                },
                &mut test_rng(),
            )
            .unwrap_err();
            assert!(
                matches!(err, NucleateError::EmptyVariable(ref k) if k == "target"),
                "blank {blank:?} should be rejected as EmptyVariable, got {err:?}"
            );
        }
    }

    #[test]
    fn test_nucleate_optional_variable_may_be_empty() {
        // The `verbose` variable is optional (has a default); passing it blank
        // is allowed — the empty-var guard only fires on effectively-required
        // variables.
        let formula = formula_with_vars();
        let mut vars = HashMap::new();
        vars.insert("target".to_string(), "production".to_string());
        vars.insert("verbose".to_string(), String::new());
        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: vars,
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();
        assert_eq!(result.variables.get("verbose").unwrap(), "");
    }

    #[test]
    fn test_nucleate_with_worker_assignment() {
        let formula = minimal_formula();
        let worker = WorkerId::new("vault").unwrap();
        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: HashMap::new(),
                assign: Some(worker),
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.assigned_worker.as_ref().unwrap().as_str(), "vault");
    }

    #[test]
    fn test_nucleate_default_prefix_when_empty() {
        let formula = Formula::parse(
            r#"
formula = "no-prefix"
version = 1

[[steps]]
id = "s"
title = "S"
description = "."
"#,
        )
        .unwrap();

        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: HashMap::new(),
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.id.prefix(), "mol");
    }

    #[test]
    fn test_nucleate_passthrough_variables() {
        let formula = minimal_formula();
        let mut vars = HashMap::new();
        vars.insert("extra_key".to_string(), "extra_value".to_string());

        let result = nucleate(
            NucleateRequest {
                formula: &formula,
                variables: vars,
                assign: None,
            },
            &mut test_rng(),
        )
        .unwrap();

        assert_eq!(result.variables.get("extra_key").unwrap(), "extra_value");
    }
}
