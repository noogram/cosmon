// SPDX-License-Identifier: Apache-2.0

//! `GalaxyKind` — typed taxonomy of the four families of galaxies.
//!
//! Wheeler: two galaxies share a kind iff the bits they exist to move
//! flow in the same direction across the galaxy's boundary.
//!
//! - [`GalaxyKind::Infra`]: bits flow *inward* — the galaxy enables its sisters.
//! - [`GalaxyKind::Project`]: bits flow *through* — artefacts + illuminated principles.
//! - [`GalaxyKind::SocialHub`]: bits flow *laterally* — human-to-human coordination.
//! - [`GalaxyKind::Editorial`]: bits flow *outward* — one-way publication to strangers.
//!
//! The enum is closed; cross-cutting attributes (`lifecycle:ephemeral`,
//! `status:frozen`, `status:nascent`) live in tags, not here.

use serde::{Deserialize, Serialize};

/// The four families of galaxies.
///
/// Stored in `repos.galaxy_kind` as the kebab-case string form
/// (`infra | project | social-hub | editorial`). A `NULL` column
/// means the galaxy has not yet been classified — report it as
/// `nascent` in observability surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GalaxyKind {
    /// Bits flow inward — the galaxy enables its sisters.
    Infra,
    /// Bits flow through — artefacts and illuminated principles.
    Project,
    /// Bits flow laterally — human-to-human coordination.
    SocialHub,
    /// Bits flow outward — one-way publication to strangers.
    Editorial,
}

impl GalaxyKind {
    /// All four variants, in declaration order (Infra → Editorial).
    pub fn all() -> &'static [GalaxyKind] {
        &[Self::Infra, Self::Project, Self::SocialHub, Self::Editorial]
    }

    /// Kebab-case canonical string — the exact byte shape stored in
    /// `repos.galaxy_kind` and surfaced by `--json`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Infra => "infra",
            Self::Project => "project",
            Self::SocialHub => "social-hub",
            Self::Editorial => "editorial",
        }
    }

    /// Parse from the kebab-case canonical string. Returns `None`
    /// for any unknown token; callers are responsible for emitting
    /// a usable diagnostic.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "infra" => Some(Self::Infra),
            "project" => Some(Self::Project),
            "social-hub" => Some(Self::SocialHub),
            "editorial" => Some(Self::Editorial),
            _ => None,
        }
    }
}

impl std::fmt::Display for GalaxyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classify a galaxy name against the canonical migration table of
/// the 10 existing galaxies.
///
/// Returns `None` for names outside the known set — those galaxies
/// remain `nascent` until observable tests (W=28d) classify them.
#[must_use]
pub fn classify_known_galaxy(name: &str) -> Option<GalaxyKind> {
    // The canonical 10-galaxy migration (synthesis §4 Q2). Any new
    // galaxy should earn its kind via the observable tests, not by
    // being tacked onto this table.
    match name {
        "cosmon" => Some(GalaxyKind::Infra),
        "showroom" | "earshot" | "cadence" | "crunch-audio" | "mailroom" | "sandbox"
        | "peerco-integration" => Some(GalaxyKind::Project),
        "demo-squad" => Some(GalaxyKind::SocialHub),
        "chancery" => Some(GalaxyKind::Editorial),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_enum_variants_round_trip_through_strings() {
        for k in GalaxyKind::all() {
            assert_eq!(Some(*k), GalaxyKind::from_str(k.as_str()));
        }
    }

    #[test]
    fn unknown_string_parses_to_none() {
        assert_eq!(GalaxyKind::from_str("archive"), None);
        assert_eq!(GalaxyKind::from_str("experiment"), None);
        assert_eq!(GalaxyKind::from_str(""), None);
    }

    #[test]
    fn display_is_kebab_case_canonical_form() {
        assert_eq!(GalaxyKind::Infra.to_string(), "infra");
        assert_eq!(GalaxyKind::SocialHub.to_string(), "social-hub");
    }

    #[test]
    fn serde_uses_kebab_case() {
        let j = serde_json::to_string(&GalaxyKind::SocialHub).unwrap();
        assert_eq!(j, "\"social-hub\"");
        let back: GalaxyKind = serde_json::from_str("\"social-hub\"").unwrap();
        assert_eq!(back, GalaxyKind::SocialHub);
    }

    #[test]
    fn classify_known_galaxies_matches_delib_5168() {
        assert_eq!(classify_known_galaxy("cosmon"), Some(GalaxyKind::Infra));
        assert_eq!(classify_known_galaxy("showroom"), Some(GalaxyKind::Project));
        assert_eq!(classify_known_galaxy("earshot"), Some(GalaxyKind::Project));
        assert_eq!(classify_known_galaxy("cadence"), Some(GalaxyKind::Project));
        assert_eq!(
            classify_known_galaxy("crunch-audio"),
            Some(GalaxyKind::Project)
        );
        assert_eq!(classify_known_galaxy("mailroom"), Some(GalaxyKind::Project));
        assert_eq!(classify_known_galaxy("sandbox"), Some(GalaxyKind::Project));
        assert_eq!(
            classify_known_galaxy("peerco-integration"),
            Some(GalaxyKind::Project)
        );
        assert_eq!(
            classify_known_galaxy("demo-squad"),
            Some(GalaxyKind::SocialHub)
        );
        assert_eq!(
            classify_known_galaxy("chancery"),
            Some(GalaxyKind::Editorial)
        );
    }

    #[test]
    fn classify_covers_every_named_galaxy_in_delib_5168() {
        // delib-20260419-5168 §4 Q2 enumerates the named galaxies; the
        // neutralization folded two of them onto one placeholder, so the
        // public table pins 10 distinct names. The test keeps future drift
        // in this table loud.
        let known = [
            "cosmon",
            "showroom",
            "earshot",
            "cadence",
            "crunch-audio",
            "mailroom",
            "sandbox",
            "peerco-integration",
            "demo-squad",
            "chancery",
        ];
        let classified = known
            .iter()
            .filter(|n| classify_known_galaxy(n).is_some())
            .count();
        assert_eq!(classified, 10, "all 10 named galaxies must classify");
    }

    #[test]
    fn unknown_galaxy_name_is_nascent() {
        assert_eq!(classify_known_galaxy("workshop"), None);
        assert_eq!(classify_known_galaxy("new-galaxy"), None);
    }
}
