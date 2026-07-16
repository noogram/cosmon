// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule declarations — git-trackable intent descriptions.
//!
//! A [`MoleculeDeclaration`] captures *what* work to do without any runtime
//! state. It is serialized as TOML and stored in `.cosmon/molecules/`, where
//! it can be committed to git. When a collaborator clones the repo and runs
//! `cs nucleate --from .cosmon/molecules/`, the declarations are hydrated
//! into runtime [`MoleculeData`](cosmon_state::MoleculeData) instances.
//!
//! This separation follows the thesis principle: "Desired state is persisted;
//! observed state is derived from reality."

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A molecule declaration — declarative intent, no runtime state.
///
/// This is the git-trackable counterpart to `MoleculeData`. It describes
/// *what* to create, not *what happened*.
///
/// # TOML example
///
/// ```toml
/// id_prefix = "wiki"
/// formula = "report-writing"
/// description = "Write wiki article about compressor DSP"
///
/// [variables]
/// topic = "Feed-forward RMS compressor with 6 modes"
/// audience = "audio engineer"
/// format = "markdown"
///
/// links = ["wiki-20260407-bcd5"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoleculeDeclaration {
    /// Prefix for the generated molecule ID (e.g., "wiki", "build").
    pub id_prefix: String,
    /// Formula to instantiate.
    pub formula: String,
    /// Human-readable description of what this molecule does.
    #[serde(default)]
    pub description: String,
    /// Variable bindings for the formula.
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// Links to other molecules or external resources.
    #[serde(default)]
    pub links: Vec<String>,
    /// Cognitive nature: "idea", "task", "decision", "issue", "signal".
    /// Determines interaction rules and surface projection target.
    #[serde(default)]
    pub kind: Option<String>,
    /// Worker to assign (optional — omit for pending).
    #[serde(default)]
    pub assign: Option<String>,
}

/// Errors from parsing molecule declarations.
#[derive(Debug, thiserror::Error)]
pub enum DeclarationError {
    /// TOML parse error.
    #[error("failed to parse declaration TOML: {0}")]
    Toml(#[from] toml::de::Error),
}

impl MoleculeDeclaration {
    /// Parse a declaration from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns [`DeclarationError::Toml`] if the TOML is invalid.
    pub fn parse(toml_str: &str) -> Result<Self, DeclarationError> {
        let decl: Self = toml::from_str(toml_str)?;
        Ok(decl)
    }

    /// Serialize this declaration to TOML.
    ///
    /// # Panics
    ///
    /// Cannot panic in practice — all fields are serializable.
    #[must_use]
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("declaration is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal() {
        let decl = MoleculeDeclaration::parse(
            r#"
            id_prefix = "wiki"
            formula = "report-writing"
            "#,
        )
        .unwrap();
        assert_eq!(decl.id_prefix, "wiki");
        assert_eq!(decl.formula, "report-writing");
        assert!(decl.variables.is_empty());
        assert!(decl.links.is_empty());
        assert!(decl.assign.is_none());
    }

    #[test]
    fn test_parse_full() {
        let decl = MoleculeDeclaration::parse(
            r#"
            id_prefix = "build"
            formula = "plugin-build"
            description = "Build the audio plugin"
            assign = "builder"
            links = ["wiki-20260407-abc1"]

            [variables]
            target = "release"
            platform = "macos"
            "#,
        )
        .unwrap();
        assert_eq!(decl.formula, "plugin-build");
        assert_eq!(decl.description, "Build the audio plugin");
        assert_eq!(decl.assign.as_deref(), Some("builder"));
        assert_eq!(decl.variables.get("target").unwrap(), "release");
        assert_eq!(decl.links, vec!["wiki-20260407-abc1"]);
    }

    #[test]
    fn test_roundtrip_toml() {
        let decl = MoleculeDeclaration {
            id_prefix: "test".to_string(),
            formula: "test-formula".to_string(),
            description: "A test molecule".to_string(),
            variables: {
                let mut m = HashMap::new();
                m.insert("key".to_string(), "value".to_string());
                m
            },
            links: vec!["link-1".to_string()],
            kind: Some("task".to_string()),
            assign: Some("worker-1".to_string()),
        };

        let toml_str = decl.to_toml();
        let reparsed = MoleculeDeclaration::parse(&toml_str).unwrap();
        assert_eq!(reparsed.id_prefix, "test");
        assert_eq!(reparsed.formula, "test-formula");
        assert_eq!(reparsed.assign.as_deref(), Some("worker-1"));
    }
}
