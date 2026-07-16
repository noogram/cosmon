// SPDX-License-Identifier: AGPL-3.0-only

//! Surface configuration — parsed from `.cosmon/surfaces.toml`.

use cosmon_core::kind::MoleculeKind;
use serde::{Deserialize, Serialize};

/// The full surfaces configuration file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceConfig {
    /// List of surface projection targets.
    #[serde(default)]
    pub surface: Vec<Surface>,
}

/// A single surface projection target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surface {
    /// The neurion referent this surface materializes.
    pub referent: String,
    /// What kind of surface (markdown file, directory index, etc.).
    pub kind: SurfaceKind,
    /// File path relative to the project root (not required for github-issues).
    #[serde(default)]
    pub path: String,
    /// Optional template name (for future extensibility).
    #[serde(default)]
    pub template: Option<String>,
    /// GitHub repository (owner/repo) — required for github-issues kind.
    #[serde(default)]
    pub repo: Option<String>,
    /// Labels to apply to created GitHub issues.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Filter projection to only these molecule kinds.
    ///
    /// When empty (the default), all kinds are included.
    /// When specified, only molecules matching one of the listed kinds
    /// are projected onto this surface.
    #[serde(default)]
    pub molecule_kinds: Vec<MoleculeKind>,
    /// How visible cosmon branding is on the rendered surface.
    ///
    /// Defaults to [`Branding::HostNative`] — the host project owns the
    /// surface, cosmon is invisible plumbing. See [`Branding`] for the full
    /// ladder of modes.
    #[serde(default)]
    pub branding: Branding,
}

/// Controls how visible cosmon vocabulary is on a rendered surface.
///
/// Cosmon projects molecule state onto host-owned files (STATUS.md,
/// GitHub Issues, etc.). The default stance is that the **host project
/// owns the surface** — cosmon is the tool that generated it, the same way
/// `rustc` generates `.o` files without stamping its own name on every one.
///
/// # The ladder
///
/// - [`Branding::Attributed`] — visible cosmon header/footer, metadata
///   block (Molecule/Kind/Formula/Status/Progress/Fleet), explicit
///   *Projected by cosmon surface* attribution. Use when you want the
///   tool to be visible to readers of the surface.
/// - [`Branding::HostNative`] — **default**. No visible cosmon vocabulary
///   in the body. A minimum footer declares the file is auto-generated and
///   points at the source directory, but makes no mention of cosmon.
/// - [`Branding::None`] — no footer at all. Use when the surface is
///   embedded in a larger host document where a generation notice would
///   be redundant.
///
/// # Why host-native is the default
///
/// The operator wants the host project to own its surfaces, not to hide
/// cosmon. Tools should disappear into the artifacts they produce. If you
/// need the tool to announce itself, flip to `attributed` explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Branding {
    /// Current cosmon-branded behavior with metadata block and attribution.
    Attributed,
    /// Host-native: no visible cosmon vocabulary, minimum neutral footer.
    /// This is the default.
    #[default]
    HostNative,
    /// No footer at all. Metadata block still dropped.
    None,
}

/// The kind of surface projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SurfaceKind {
    /// A single Markdown file (STATUS.md, ISSUES.md).
    Markdown,
    /// A directory index (docs/adr/ → renders an index of files).
    Directory,
    /// GitHub Issues (future).
    GithubIssues,
}

impl Surface {
    /// Check if a molecule kind passes this surface's kind filter.
    ///
    /// Returns `true` if `molecule_kinds` is empty (no filter) or if the
    /// given kind is in the list.
    #[must_use]
    pub fn accepts_kind(&self, kind: MoleculeKind) -> bool {
        self.molecule_kinds.is_empty() || self.molecule_kinds.contains(&kind)
    }
}

impl SurfaceConfig {
    /// Parse a surfaces.toml string.
    ///
    /// # Errors
    ///
    /// Returns an error if the TOML is invalid.
    pub fn parse(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }

    /// Load from a file path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse(&content)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_surfaces_toml() {
        let config = SurfaceConfig::parse(
            r#"
            [[surface]]
            referent = "project.status"
            kind = "markdown"
            path = "STATUS.md"

            [[surface]]
            referent = "project.issues"
            kind = "markdown"
            path = "ISSUES.md"

            [[surface]]
            referent = "project.decisions"
            kind = "directory"
            path = "docs/adr/"
            "#,
        )
        .unwrap();
        assert_eq!(config.surface.len(), 3);
        assert_eq!(config.surface[0].referent, "project.status");
        assert_eq!(config.surface[0].kind, SurfaceKind::Markdown);
        assert_eq!(config.surface[1].path, "ISSUES.md");
        assert_eq!(config.surface[2].kind, SurfaceKind::Directory);
    }

    #[test]
    fn test_parse_molecule_kinds_filter() {
        let config = SurfaceConfig::parse(
            r#"
            [[surface]]
            referent = "project.issues"
            kind = "markdown"
            path = "ISSUES.md"
            molecule_kinds = ["task", "issue"]
            "#,
        )
        .unwrap();
        assert_eq!(config.surface[0].molecule_kinds.len(), 2);
        assert!(config.surface[0].accepts_kind(MoleculeKind::Task));
        assert!(config.surface[0].accepts_kind(MoleculeKind::Issue));
        assert!(!config.surface[0].accepts_kind(MoleculeKind::Idea));
        assert!(!config.surface[0].accepts_kind(MoleculeKind::Signal));
    }

    #[test]
    fn test_parse_ideas_surface() {
        let config = SurfaceConfig::parse(
            r#"
            [[surface]]
            referent = "project.ideas"
            kind = "markdown"
            path = "IDEAS.md"
            molecule_kinds = ["idea"]
            "#,
        )
        .unwrap();
        assert_eq!(config.surface[0].referent, "project.ideas");
        assert_eq!(config.surface[0].path, "IDEAS.md");
        assert!(config.surface[0].accepts_kind(MoleculeKind::Idea));
        assert!(!config.surface[0].accepts_kind(MoleculeKind::Task));
        assert!(!config.surface[0].accepts_kind(MoleculeKind::Issue));
    }

    #[test]
    fn test_parse_deliberations_surface() {
        let config = SurfaceConfig::parse(
            r#"
            [[surface]]
            referent = "project.deliberations"
            kind = "markdown"
            path = "DELIBERATIONS.md"
            molecule_kinds = ["deliberation"]
            "#,
        )
        .unwrap();
        assert_eq!(config.surface[0].referent, "project.deliberations");
        assert_eq!(config.surface[0].path, "DELIBERATIONS.md");
        assert!(config.surface[0].accepts_kind(MoleculeKind::Deliberation));
        assert!(!config.surface[0].accepts_kind(MoleculeKind::Idea));
        assert!(!config.surface[0].accepts_kind(MoleculeKind::Task));
    }

    #[test]
    fn test_empty_molecule_kinds_accepts_all() {
        let config = SurfaceConfig::parse(
            r#"
            [[surface]]
            referent = "project.status"
            kind = "markdown"
            path = "STATUS.md"
            "#,
        )
        .unwrap();
        assert!(config.surface[0].molecule_kinds.is_empty());
        assert!(config.surface[0].accepts_kind(MoleculeKind::Idea));
        assert!(config.surface[0].accepts_kind(MoleculeKind::Signal));
        assert!(config.surface[0].accepts_kind(MoleculeKind::Decision));
    }
}
