// SPDX-License-Identifier: AGPL-3.0-only

//! `prime` — the step context of a running molecule.
//!
//! Read-only. Shared by `cs prime` and `GET /v1/molecules/:id/prime`: both
//! call [`prime`] and render its [`PrimeView`], so a worker re-primed by
//! the `SessionStart` hook and a remote caller asking for the same context
//! read the same text.
//!
//! A terminal molecule has nothing to prime. That is a typed answer
//! ([`PrimeView::Terminal`]), not an error: the caller asked a well-formed
//! question about a molecule that exists.

use std::path::Path;

use cosmon_core::error::CosmonError;
use cosmon_core::formula::Formula;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::ops::error::OpsError;
use crate::StateStore;

/// Errors returned by [`prime`].
#[derive(Debug, thiserror::Error)]
pub enum PrimeError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// The state store could not be read.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl PrimeError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for PrimeError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Where a formula step stands relative to the molecule's current step.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StepPosition {
    /// Already past.
    Done,
    /// The step the molecule is on.
    Current,
    /// Not reached yet.
    Pending,
}

/// One step of the formula, as it appears in the reminder.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrimeStep {
    /// 1-based position of the step.
    pub number: usize,
    /// Step title.
    pub title: String,
    /// Position relative to the current step.
    pub position: StepPosition,
    /// Step description. Present on the current step only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Exit criteria. Present on the current step only, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_criteria: Option<String>,
}

/// The step context of a non-terminal molecule.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrimeContext {
    /// Molecule the context belongs to.
    pub molecule: String,
    /// Molecule status (`running`, `pending`, `frozen`, …).
    pub status: String,
    /// 1-based number of the current step.
    pub step: usize,
    /// Total number of steps.
    pub total_steps: usize,
    /// The formula's steps. `None` when the formula cannot be read; the
    /// reminder then degrades to a bare progress line.
    pub steps: Option<Vec<PrimeStep>>,
}

/// Result of a [`prime`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimeView {
    /// The molecule is `Completed` or `Collapsed`: nothing to prime.
    Terminal {
        /// The molecule asked about.
        molecule: MoleculeId,
        /// Its terminal status.
        status: MoleculeStatus,
    },
    /// The molecule is alive; this is its current step context.
    Context(PrimeContext),
}

/// Wire shape of a [`PrimeView`] — what the route returns. `cs prime`
/// prints [`PrimeContext::render`] instead; the text is the same
/// information.
#[derive(Debug, Clone, Serialize)]
pub struct PrimeJson {
    /// Molecule asked about.
    pub molecule: String,
    /// `true` when the molecule is terminal and there is nothing to prime.
    pub terminal: bool,
    /// Molecule status.
    pub status: String,
    /// The step context. Absent for a terminal molecule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<PrimeContext>,
    /// The reminder text `cs prime` prints. Absent for a terminal molecule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder: Option<String>,
}

impl PrimeJson {
    /// Project a view onto its wire shape.
    #[must_use]
    pub fn from_view(view: &PrimeView) -> Self {
        match view {
            PrimeView::Terminal { molecule, status } => Self {
                molecule: molecule.as_str().to_owned(),
                terminal: true,
                status: status.to_string(),
                context: None,
                reminder: None,
            },
            PrimeView::Context(ctx) => Self {
                molecule: ctx.molecule.clone(),
                terminal: false,
                status: ctx.status.clone(),
                context: Some(ctx.clone()),
                reminder: Some(ctx.render()),
            },
        }
    }
}

impl PrimeContext {
    /// The propulsion reminder, byte for byte what `cs prime` prints.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;

        let mut out = format!(
            "🚨 PROPULSION REMINDER — You are executing molecule `{}`.\n\n",
            self.molecule
        );
        if let Some(steps) = &self.steps {
            let _ = write!(
                out,
                "**Progress:** step {}/{}\n\n",
                self.step, self.total_steps
            );
            for s in steps {
                match s.position {
                    StepPosition::Done => {
                        let _ = writeln!(out, "- [x] Step {}: {}", s.number, s.title);
                    }
                    StepPosition::Current => {
                        let _ = writeln!(
                            out,
                            "- [>] **Step {}: {}** ◀ EXECUTE NOW",
                            s.number, s.title
                        );
                        let _ = writeln!(out, "  {}", s.description.as_deref().unwrap_or(""));
                        if let Some(criteria) = &s.exit_criteria {
                            let _ = writeln!(out, "  Exit criteria: {criteria}");
                        }
                    }
                    StepPosition::Pending => {
                        let _ = writeln!(out, "- [ ] Step {}: {}", s.number, s.title);
                    }
                }
            }
        } else {
            let _ = writeln!(
                out,
                "Step {}/{} — continue execution.",
                self.step, self.total_steps
            );
        }
        let _ = writeln!(
            out,
            "\n**Do NOT pause. Execute the current step immediately.**"
        );
        let _ = writeln!(
            out,
            "When ALL steps done: `cs complete {} --reason \"<summary>\"`",
            self.molecule
        );
        out
    }
}

/// Build the step context of a molecule.
///
/// `formulas_dir` is where `<formula_id>.formula.toml` is resolved; an
/// unreadable or unparsable formula degrades the context to a bare progress
/// line rather than failing, because a worker that lost its formula still
/// needs to be told to continue.
///
/// # Errors
///
/// [`PrimeError::MoleculeNotFound`] when the molecule is absent,
/// [`PrimeError::StoreUnavailable`] when the store cannot be read.
///
/// # `#[verb]` registration
///
/// Annotated as `GET /v1/molecules/:id/prime` (principal `tenant`).
#[cosmon_thin_macro::verb(method = "GET", path = "/v1/molecules/:id/prime", principal = "tenant")]
pub fn prime(
    store: &dyn StateStore,
    formulas_dir: &Path,
    id: &MoleculeId,
) -> Result<PrimeView, PrimeError> {
    let mol = store
        .load_molecule(id)
        .map_err(|e| PrimeError::from_cosmon(id, e))?;
    if mol.status.is_terminal() {
        return Ok(PrimeView::Terminal {
            molecule: id.clone(),
            status: mol.status,
        });
    }

    let formula_path = formulas_dir.join(format!("{}.formula.toml", mol.formula_id));
    let formula = std::fs::read_to_string(&formula_path)
        .ok()
        .and_then(|t| Formula::parse(&t).ok());

    let steps = formula.map(|formula| {
        formula
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let position = match i.cmp(&mol.current_step) {
                    std::cmp::Ordering::Less => StepPosition::Done,
                    std::cmp::Ordering::Equal => StepPosition::Current,
                    std::cmp::Ordering::Greater => StepPosition::Pending,
                };
                let current = position == StepPosition::Current;
                PrimeStep {
                    number: i + 1,
                    title: step.title.clone(),
                    position,
                    description: current.then(|| step.description.clone()),
                    exit_criteria: if current {
                        step.exit_criteria.clone()
                    } else {
                        None
                    },
                }
            })
            .collect()
    });

    Ok(PrimeView::Context(PrimeContext {
        molecule: id.as_str().to_owned(),
        status: mol.status.to_string(),
        step: mol.current_step + 1,
        total_steps: mol.total_steps,
        steps,
    }))
}
