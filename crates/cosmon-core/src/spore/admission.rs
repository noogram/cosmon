// SPDX-License-Identifier: AGPL-3.0-only

//! Pure admission for work types declared by a spore (ADR-183).
//! The shell resolves refs and reads files; this module makes the refusal.

use serde::{Deserialize, Serialize};

/// The versioned contract in `[spore.admission]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AdmissionSpec {
    /// Admission schema version. Only version 1 is understood.
    pub version: u32,
    /// Work types this recipe can honestly process.
    pub work_types: Vec<String>,
    /// Named fields the applicant must supply in an admission record.
    pub inputs: Vec<String>,
    /// Gate commands the admitted run must execute.
    pub gates: Vec<String>,
    /// Reviewer capabilities required by the recipe.
    pub reviewer_capabilities: Vec<String>,
    /// Execution substrates required by the recipe.
    pub execution_substrates: Vec<String>,
}

/// Operator supplied facts, kept separate from the spore's topology variables.
#[derive(Debug, Clone, Deserialize)]
pub struct AdmissionInput {
    /// Admission record schema version.
    pub version: u32,
    /// Explicit work classification; an issue title is never classified by guesswork.
    pub work_type: String,
    /// A ref naming the affected commit, pinned by the shell before admission.
    pub baseline: String,
    /// Intended integration ref, pinned by the shell before admission.
    pub target_base: String,
    /// Reporter supplied verbatim symptom.
    pub reporter_symptom: Option<String>,
    /// Reporter environment and version fingerprint.
    pub reporter_environment: Option<String>,
    /// Reporter reproduction transcript or equivalent evidence.
    pub reporter_transcript: Option<String>,
    /// Proposed files or path patterns, if known.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Capabilities available for independent review, as operator attestations.
    #[serde(default)]
    pub reviewer_capabilities: Vec<String>,
    /// Substrates available for the recipe's checks, as operator attestations.
    #[serde(default)]
    pub execution_substrates: Vec<String>,
}

/// Admission verdict displayed before expansion and persisted by the caller if desired.
#[derive(Debug, Clone, Serialize)]
pub struct AdmissionRecord {
    /// Admission schema version.
    pub version: u32,
    /// Vehicle chosen by the manifest.
    pub vehicle: String,
    /// Accepted work type.
    pub work_type: String,
    /// Resolved affected commit.
    pub baseline: String,
    /// Resolved integration commit.
    pub target_base: String,
    /// Declared paths; an empty list means none were declared.
    pub paths: Vec<String>,
    /// Conservative risk classification derived from the declared paths.
    pub risk: String,
    /// Applicable gate commands.
    pub gates: Vec<String>,
    /// Required reviewer capabilities.
    pub reviewer_capabilities: Vec<String>,
    /// Required execution substrates.
    pub execution_substrates: Vec<String>,
    /// Initial allocation, excluding later children.
    pub expected_molecules: usize,
}

/// Typed reasons a spore must refuse before allocating anything.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdmissionError {
    /// The declaration or supplied record uses an unknown version.
    #[error("unsupported admission version {0}; expected 1")]
    Version(u32),
    /// A different vehicle must be chosen for the request.
    #[error("work type `{0}` is not accepted by this spore; use a plan followed by scoped task-work for a feature")]
    WrongWorkType(String),
    /// One required fact is absent.
    #[error("admission input `{0}` is missing or empty")]
    MissingInput(String),
    /// A declared capability is not attested as available.
    #[error("required reviewer capability `{0}` is unavailable")]
    MissingReviewer(String),
    /// A declared execution substrate is not attested as available.
    #[error("required execution substrate `{0}` is unavailable")]
    MissingSubstrate(String),
}

/// Decide admission using only declared data and already pinned commit identities.
///
/// # Errors
/// Returns a typed refusal for unknown versions, mismatched work, absent
/// evidence, or unavailable declared capabilities.
pub fn preflight(
    spec: &AdmissionSpec,
    input: &AdmissionInput,
    vehicle: &str,
    baseline: String,
    target_base: String,
    expected_molecules: usize,
) -> Result<AdmissionRecord, AdmissionError> {
    if spec.version != 1 {
        return Err(AdmissionError::Version(spec.version));
    }
    if input.version != 1 {
        return Err(AdmissionError::Version(input.version));
    }
    if !spec.work_types.contains(&input.work_type) {
        return Err(AdmissionError::WrongWorkType(input.work_type.clone()));
    }
    for field in &spec.inputs {
        let present = match field.as_str() {
            "baseline" => !input.baseline.trim().is_empty(),
            "target_base" => !input.target_base.trim().is_empty(),
            "reporter_symptom" => input
                .reporter_symptom
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            "reporter_environment" => input
                .reporter_environment
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            "reporter_transcript" => input
                .reporter_transcript
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            "paths" => !input.paths.is_empty(),
            _ => false,
        };
        if !present {
            return Err(AdmissionError::MissingInput(field.clone()));
        }
    }
    for capability in &spec.reviewer_capabilities {
        if !input.reviewer_capabilities.contains(capability) {
            return Err(AdmissionError::MissingReviewer(capability.clone()));
        }
    }
    let risk = if input.paths.is_empty()
        || input.paths.iter().any(|path| {
            [
                "auth",
                "credential",
                "token",
                "key",
                "sign",
                "egress",
                "root_spawn_policy",
                "docs/specs/",
                "scripts/publish.sh",
            ]
            .iter()
            .any(|needle| path.to_ascii_lowercase().contains(needle))
        }) {
        "security"
    } else {
        "normal"
    };
    let mut required_reviewers = spec.reviewer_capabilities.clone();
    if risk == "security" {
        required_reviewers.push("third provider family".to_owned());
    }
    if risk == "security"
        && !input
            .reviewer_capabilities
            .iter()
            .any(|capability| capability == "third provider family")
    {
        return Err(AdmissionError::MissingReviewer(
            "third provider family".to_owned(),
        ));
    }
    for substrate in &spec.execution_substrates {
        if !input.execution_substrates.contains(substrate) {
            return Err(AdmissionError::MissingSubstrate(substrate.clone()));
        }
    }
    Ok(AdmissionRecord {
        version: 1,
        vehicle: vehicle.to_owned(),
        work_type: input.work_type.clone(),
        baseline,
        target_base,
        paths: input.paths.clone(),
        risk: risk.to_owned(),
        gates: spec.gates.clone(),
        reviewer_capabilities: required_reviewers,
        execution_substrates: spec.execution_substrates.clone(),
        expected_molecules,
    })
}

#[cfg(test)]
mod tests {
    use super::{preflight, AdmissionError, AdmissionInput, AdmissionSpec};

    #[test]
    fn security_paths_widen_the_reported_reviewer_floor() {
        let spec = AdmissionSpec {
            version: 1,
            work_types: vec!["defect".to_owned()],
            inputs: vec!["reporter_symptom".to_owned()],
            gates: vec!["verify".to_owned()],
            reviewer_capabilities: vec!["independent refuter".to_owned()],
            execution_substrates: Vec::new(),
        };
        let mut input = AdmissionInput {
            version: 1,
            work_type: "defect".to_owned(),
            baseline: "base".to_owned(),
            target_base: "target".to_owned(),
            reporter_symptom: Some("observed failure".to_owned()),
            reporter_environment: None,
            reporter_transcript: None,
            paths: vec!["crates/cosmon-core/src/auth.rs".to_owned()],
            reviewer_capabilities: vec!["independent refuter".to_owned()],
            execution_substrates: Vec::new(),
        };
        let decide = |input: &AdmissionInput| {
            preflight(
                &spec,
                input,
                "cosmon-dev",
                "base".to_owned(),
                "target".to_owned(),
                14,
            )
        };
        assert_eq!(
            decide(&input).expect_err("third family is required"),
            AdmissionError::MissingReviewer("third provider family".to_owned())
        );
        input
            .reviewer_capabilities
            .push("third provider family".to_owned());
        let record = decide(&input).expect("widened review admitted");
        assert_eq!(record.risk, "security");
        assert!(record
            .reviewer_capabilities
            .contains(&"third provider family".to_owned()));
    }
}
