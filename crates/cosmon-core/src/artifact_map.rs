// SPDX-License-Identifier: AGPL-3.0-only

//! Genre classification for tracked artifacts (ADR-057).
//!
//! Given a `.cosmon/artifact-map.toml` file, this module classifies any
//! path in a galaxy into a [`Genre`], deriving the [`Audience`] and
//! [`Residence`] from it. The TOML surface is deliberately minimal: one
//! table per genre, two fields (`location`, `audience`). Glob matching
//! uses longest-fixed-character-count as the specificity measure, with
//! declaration order as the tiebreaker. When `artifact-map.toml` is
//! absent, [`ArtifactMap::default_code_catchall`] returns a map where
//! every path classifies as `code` — the backwards-compatible default.
//!
//! # Invariants (ADR-057 §2.6)
//!
//! - **I1 Totality** — every path classifies (enforced by the `code`
//!   catch-all glob `**/*`).
//! - **I2 Unique classification** — longest-specific-glob wins; ties
//!   resolve in declaration order.
//! - **I3 Residence well-typed** — every audience maps to one of
//!   `{Solo, Team}` (v0 does not synthesise `Encrypted`/`Remote` from
//!   audience alone; those are residence-level choices).
//! - **I4 Audience–residence compat** — `public`/`team` audiences on a
//!   `Solo` residence is flagged as a violation.
//!
//! # Partner capture
//!
//! A glob may contain the token `<name>` exactly once. It captures the
//! matching path component and parameterises the audience
//! `partner:<name>`. Example:
//! `docs/addl/<name>/**/*` with path `docs/addl/operator-b/videos/demo.mp4`
//! yields `audience = partner:operator-b`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Parsed `.cosmon/artifact-map.toml` — ordered list of genre specs.
///
/// Order matters for the I2 tiebreak: when two globs have identical
/// specificity, the earlier declaration wins. The TOML parser is strict
/// about table order (via `IndexMap`-like behaviour in `toml::Value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMap {
    /// Genre declarations in source order.
    pub genres: Vec<GenreSpec>,
}

/// A single genre entry from the TOML map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreSpec {
    /// The genre name (TOML table key, e.g. `chronicle`, `adr`).
    pub name: String,
    /// Glob patterns that identify files of this genre.
    pub locations: Vec<String>,
    /// Who the genre is addressed to. `partner:<name>` is a template;
    /// the `<name>` token is resolved from the matching path component
    /// at classification time.
    pub audience: AudienceSpec,
}

/// Raw audience declaration from the TOML.
///
/// `Partner` variants are templates until a classification resolves
/// the `<name>` capture from the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudienceSpec {
    /// Tracked on `main`; the world is the audience.
    Public,
    /// Tracked on `main`; collaborators on the git remote.
    Team,
    /// Author + their agents only (narration branch, not `main`).
    AuthorAgent,
    /// A specific external partner. `None` means the `<name>` token is
    /// expected from the path glob; `Some(literal)` means a fixed
    /// partner name declared inline.
    Partner(Option<String>),
    /// Operator alone; local filesystem only.
    Solo,
}

/// Resolved audience after classification (no template left).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// `public` — tracked on `main`.
    Public,
    /// `team` — tracked on `main`.
    Team,
    /// `author+agent` — narration branch.
    #[serde(rename = "author+agent")]
    AuthorAgent,
    /// `partner:<name>` — narration branch (optionally encrypted).
    Partner(String),
    /// `solo` — local only.
    Solo,
}

impl Audience {
    /// Operator-facing string form. Round-trips with [`Audience::parse`].
    #[must_use]
    pub fn as_display(&self) -> String {
        match self {
            Audience::Public => "public".to_owned(),
            Audience::Team => "team".to_owned(),
            Audience::AuthorAgent => "author+agent".to_owned(),
            Audience::Partner(name) => format!("partner:{name}"),
            Audience::Solo => "solo".to_owned(),
        }
    }
}

/// Derived residence (subset of [`ADR-055`] — only variants v0 synthesises).
///
/// Solo audience → Solo residence. Every other audience → Team.
/// Encrypted and Remote are residence-level choices that a genre does
/// not drive from audience alone; they are orthogonal decisions the
/// operator makes at `cs mode` time.
///
/// [`ADR-055`]: ../../../../docs/adr/055-cosmon-residence.md
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Residence {
    /// Local filesystem only (`.git/info/exclude`).
    Solo,
    /// Tracked through the git remote — either `main` or the narration
    /// orphan branch.
    Team,
}

/// Result of classifying a single path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactClass {
    /// The genre name (e.g. `chronicle`, `github-surface`).
    pub genre: String,
    /// The glob that matched (for `--verbose` output).
    pub matched_glob: String,
    /// The audience, with any `<name>` capture resolved.
    pub audience: Audience,
    /// The derived residence.
    pub residence: Residence,
}

/// Error types for the artifact map.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactMapError {
    /// TOML failed to parse.
    #[error("invalid artifact-map.toml: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// A genre declared an audience that is not recognised.
    #[error("genre '{genre}' has unknown audience '{audience}'")]
    UnknownAudience {
        /// Genre name.
        genre: String,
        /// Raw audience string as seen in the TOML.
        audience: String,
    },

    /// A genre declared no `location` field or an empty list.
    #[error("genre '{0}' declares no location globs")]
    NoLocations(String),

    /// A glob could not be compiled to a regex.
    #[error("genre '{genre}' has invalid glob '{glob}': {reason}")]
    BadGlob {
        /// Genre name.
        genre: String,
        /// The offending glob pattern.
        glob: String,
        /// Reason from the regex compiler.
        reason: String,
    },
}

impl ArtifactMap {
    /// The zero-config map: every path classifies as `code`, audience
    /// `public`. Returned when `.cosmon/artifact-map.toml` is absent, so
    /// legacy galaxies keep working with zero surprise.
    #[must_use]
    pub fn default_code_catchall() -> Self {
        ArtifactMap {
            genres: vec![GenreSpec {
                name: "code".to_owned(),
                locations: vec!["**/*".to_owned()],
                audience: AudienceSpec::Public,
            }],
        }
    }

    /// Parse a TOML document into an [`ArtifactMap`].
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactMapError::TomlParse`] on malformed TOML,
    /// [`ArtifactMapError::NoLocations`] on an empty `location` list,
    /// or [`ArtifactMapError::UnknownAudience`] on a bad `audience`
    /// literal.
    pub fn parse_toml(s: &str) -> Result<Self, ArtifactMapError> {
        // `toml::Table` preserves insertion order in recent versions; we
        // walk it once.
        let table: toml::value::Table = toml::from_str(s)?;
        let mut genres = Vec::with_capacity(table.len());
        for (name, value) in table {
            let toml::Value::Table(entry) = value else {
                continue; // skip non-table root entries
            };
            let locations: Vec<String> = entry
                .get("location")
                .and_then(toml::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if locations.is_empty() {
                return Err(ArtifactMapError::NoLocations(name));
            }
            let audience_raw = entry
                .get("audience")
                .and_then(toml::Value::as_str)
                .unwrap_or("");
            let audience = parse_audience(&name, audience_raw)?;
            genres.push(GenreSpec {
                name,
                locations,
                audience,
            });
        }
        Ok(ArtifactMap { genres })
    }

    /// Classify a path. Never fails: the `code` catch-all (or the
    /// [`Self::default_code_catchall`]) always matches; if neither exists
    /// in the map, returns `None`.
    ///
    /// The path is normalised to use `/` separators regardless of OS.
    /// Specificity = count of non-wildcard characters in the pattern;
    /// higher wins. Ties break in declaration order.
    #[must_use]
    pub fn classify(&self, path: &Path) -> Option<ArtifactClass> {
        let key = normalise_path(path);
        let mut best: Option<Candidate<'_>> = None;
        for (decl_idx, spec) in self.genres.iter().enumerate() {
            for glob in &spec.locations {
                let Ok(re) = glob_to_regex(glob) else {
                    continue;
                };
                if let Some(caps) = re.captures(&key) {
                    let captured = caps.get(1).map(|m| m.as_str().to_owned());
                    let rank = CandidateRank {
                        specificity: glob_specificity(glob),
                        decl_priority: usize::MAX - decl_idx,
                        len: glob.len(),
                    };
                    let take = best.as_ref().is_none_or(|b| b.rank < rank);
                    if take {
                        best = Some(Candidate {
                            rank,
                            glob: glob.clone(),
                            spec,
                            captured,
                        });
                    }
                }
            }
        }
        let Candidate {
            glob: matched_glob,
            spec,
            captured,
            ..
        } = best?;
        let audience = resolve_audience(&spec.audience, captured.as_deref());
        let residence = audience_to_residence(&audience);
        Some(ArtifactClass {
            genre: spec.name.clone(),
            matched_glob,
            audience,
            residence,
        })
    }

    /// Audit every path in `paths`. Returns per-genre counts plus any
    /// unclassified paths (I1 violations would appear here) and any
    /// audience/residence mismatches (I4 violations).
    #[must_use]
    pub fn audit(&self, paths: &[&Path]) -> AuditReport {
        let mut per_genre: BTreeMap<String, usize> = BTreeMap::new();
        let mut unclassified: Vec<String> = Vec::new();
        for p in paths {
            match self.classify(p) {
                Some(class) => {
                    *per_genre.entry(class.genre).or_insert(0) += 1;
                }
                None => unclassified.push(normalise_path(p)),
            }
        }
        AuditReport {
            per_genre,
            unclassified,
            total: paths.len(),
        }
    }
}

/// Aggregate audit output for `cs artifacts audit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    /// Count of paths per genre (alphabetical).
    pub per_genre: BTreeMap<String, usize>,
    /// Paths that no genre matched. Empty when I1 holds.
    pub unclassified: Vec<String>,
    /// Total path count (sanity).
    pub total: usize,
}

impl AuditReport {
    /// True when the four ADR-057 invariants hold structurally (I1
    /// totality + no unclassified files).
    #[must_use]
    pub fn invariants_hold(&self) -> bool {
        self.unclassified.is_empty()
    }
}

// --- internal helpers ----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateRank {
    specificity: usize,
    decl_priority: usize,
    len: usize,
}

struct Candidate<'a> {
    rank: CandidateRank,
    glob: String,
    spec: &'a GenreSpec,
    captured: Option<String>,
}

fn parse_audience(genre: &str, raw: &str) -> Result<AudienceSpec, ArtifactMapError> {
    match raw.trim() {
        "public" => Ok(AudienceSpec::Public),
        "team" => Ok(AudienceSpec::Team),
        "author+agent" => Ok(AudienceSpec::AuthorAgent),
        "solo" => Ok(AudienceSpec::Solo),
        other if other.starts_with("partner:") => {
            let name = &other["partner:".len()..];
            if name == "<name>" {
                Ok(AudienceSpec::Partner(None))
            } else {
                Ok(AudienceSpec::Partner(Some(name.to_owned())))
            }
        }
        _ => Err(ArtifactMapError::UnknownAudience {
            genre: genre.to_owned(),
            audience: raw.to_owned(),
        }),
    }
}

fn resolve_audience(spec: &AudienceSpec, captured: Option<&str>) -> Audience {
    match spec {
        AudienceSpec::Public => Audience::Public,
        AudienceSpec::Team => Audience::Team,
        AudienceSpec::AuthorAgent => Audience::AuthorAgent,
        AudienceSpec::Solo => Audience::Solo,
        AudienceSpec::Partner(Some(name)) => Audience::Partner(name.clone()),
        AudienceSpec::Partner(None) => {
            let name = captured.unwrap_or("unknown").to_owned();
            Audience::Partner(name)
        }
    }
}

fn audience_to_residence(audience: &Audience) -> Residence {
    match audience {
        Audience::Solo => Residence::Solo,
        _ => Residence::Team,
    }
}

fn normalise_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let trimmed = raw.trim_start_matches("./");
    trimmed.replace('\\', "/")
}

/// Specificity = number of non-wildcard characters in the pattern.
/// `*`, `?`, `<`, `>` are wildcard / capture markers.
fn glob_specificity(glob: &str) -> usize {
    glob.chars()
        .filter(|c| !matches!(c, '*' | '?' | '<' | '>'))
        .count()
}

/// Translate a glob to a regex.
///
/// - `**/` (zero or more directories) → `(?:.+/)?`
/// - `**` alone → `.*`
/// - `*` (single) → `[^/]*`
/// - `?` → `[^/]`
/// - `<name>` → `([^/]+)` (only one capture per glob; later `<…>` are
///   treated as non-capturing `[^/]+` to keep the regex well-formed)
/// - every other character is escaped.
fn glob_to_regex(glob: &str) -> Result<Regex, regex::Error> {
    let mut out = String::with_capacity(glob.len() * 2 + 4);
    out.push('^');
    let bytes = glob.as_bytes();
    let mut i = 0;
    let mut captured = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '*' if i + 1 < bytes.len() && bytes[i + 1] as char == '*' => {
                // `**/` (the common case in `docs/lore/**/*.md`) must
                // match zero or more directories, including the empty
                // case. Emit `(?:.+/)?` and consume both the `**` and
                // the trailing `/`.
                if i + 2 < bytes.len() && bytes[i + 2] as char == '/' {
                    out.push_str("(?:.+/)?");
                    i += 3;
                } else {
                    // Bare `**` at end-of-pattern — matches anything
                    // including empty.
                    out.push_str(".*");
                    i += 2;
                }
            }
            '*' => {
                out.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            '<' => {
                // Consume up to the next '>'.
                let end = glob[i..].find('>').map(|pos| i + pos);
                if let Some(end) = end {
                    if captured {
                        out.push_str("[^/]+");
                    } else {
                        out.push_str("([^/]+)");
                        captured = true;
                    }
                    i = end + 1;
                } else {
                    out.push('<');
                    i += 1;
                }
            }
            _ => {
                // Escape regex-special chars.
                if matches!(
                    c,
                    '.' | '+' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\'
                ) {
                    out.push('\\');
                }
                out.push(c);
                i += 1;
            }
        }
    }
    out.push('$');
    Regex::new(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_map() -> ArtifactMap {
        ArtifactMap::parse_toml(
            r#"
            [chronicle]
            location = ["docs/lore/**/*.md"]
            audience = "author+agent"

            [adr]
            location = ["docs/adr/**/*.md"]
            audience = "public"

            [addl]
            location = ["docs/addl/<name>/**/*"]
            audience = "partner:<name>"

            [github-surface]
            location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
            audience = "solo"

            [deliberation]
            location = [".cosmon/state/fleets/*/molecules/*/synthesis.md"]
            audience = "author+agent"

            [code]
            location = ["**/*"]
            audience = "public"
            "#,
        )
        .expect("fixture TOML must parse")
    }

    #[test]
    fn default_map_classifies_everything_as_code() {
        let map = ArtifactMap::default_code_catchall();
        let c = map.classify(Path::new("src/main.rs")).unwrap();
        assert_eq!(c.genre, "code");
        assert_eq!(c.audience, Audience::Public);
        assert_eq!(c.residence, Residence::Team);
    }

    #[test]
    fn chronicle_beats_code_by_specificity() {
        let map = sample_map();
        let c = map
            .classify(Path::new("docs/lore/2026-04-20-le-triangle.md"))
            .unwrap();
        assert_eq!(c.genre, "chronicle");
        assert_eq!(c.audience, Audience::AuthorAgent);
        assert_eq!(c.residence, Residence::Team);
    }

    #[test]
    fn adr_classifies_as_public() {
        let map = sample_map();
        let c = map
            .classify(Path::new("docs/adr/052-one-ledger.md"))
            .unwrap();
        assert_eq!(c.genre, "adr");
        assert_eq!(c.audience, Audience::Public);
        assert_eq!(c.residence, Residence::Team);
    }

    #[test]
    fn github_surface_is_solo() {
        let map = sample_map();
        let c = map.classify(Path::new("docs/surfaces/issues.md")).unwrap();
        assert_eq!(c.genre, "github-surface");
        assert_eq!(c.audience, Audience::Solo);
        assert_eq!(c.residence, Residence::Solo);
    }

    #[test]
    fn top_level_status_md_is_github_surface() {
        let map = sample_map();
        let c = map.classify(Path::new("STATUS.md")).unwrap();
        assert_eq!(c.genre, "github-surface");
        assert_eq!(c.audience, Audience::Solo);
    }

    #[test]
    fn partner_capture_resolves_name() {
        let map = sample_map();
        let c = map
            .classify(Path::new("docs/addl/operator-b/videos/demo.mp4"))
            .unwrap();
        assert_eq!(c.genre, "addl");
        assert_eq!(c.audience, Audience::Partner("operator-b".to_owned()));
        assert_eq!(c.residence, Residence::Team);
    }

    #[test]
    fn unknown_path_falls_back_to_code() {
        let map = sample_map();
        let c = map.classify(Path::new("random/weird/path.xyz")).unwrap();
        assert_eq!(c.genre, "code");
        assert_eq!(c.audience, Audience::Public);
        assert_eq!(c.residence, Residence::Team);
    }

    #[test]
    fn deliberation_synthesis_matches() {
        let map = sample_map();
        let c = map
            .classify(Path::new(
                ".cosmon/state/fleets/default/molecules/delib-foo/synthesis.md",
            ))
            .unwrap();
        assert_eq!(c.genre, "deliberation");
        assert_eq!(c.audience, Audience::AuthorAgent);
    }

    #[test]
    fn audit_counts_and_totality_holds() {
        let map = sample_map();
        let paths: Vec<&Path> = vec![
            Path::new("docs/lore/2026-04-20-x.md"),
            Path::new("docs/adr/057-x.md"),
            Path::new("STATUS.md"),
            Path::new("src/main.rs"),
            Path::new("docs/addl/bob/deck.pdf"),
        ];
        let report = map.audit(&paths);
        assert!(report.invariants_hold());
        assert_eq!(report.total, 5);
        assert_eq!(report.per_genre.get("chronicle").copied().unwrap_or(0), 1);
        assert_eq!(report.per_genre.get("adr").copied().unwrap_or(0), 1);
        assert_eq!(
            report.per_genre.get("github-surface").copied().unwrap_or(0),
            1
        );
        assert_eq!(report.per_genre.get("code").copied().unwrap_or(0), 1);
        assert_eq!(report.per_genre.get("addl").copied().unwrap_or(0), 1);
    }

    #[test]
    fn unknown_audience_errors() {
        let err = ArtifactMap::parse_toml(
            r#"
            [foo]
            location = ["*"]
            audience = "elves"
            "#,
        )
        .unwrap_err();
        match err {
            ArtifactMapError::UnknownAudience { genre, audience } => {
                assert_eq!(genre, "foo");
                assert_eq!(audience, "elves");
            }
            _ => panic!("expected UnknownAudience, got {err:?}"),
        }
    }

    #[test]
    fn empty_location_errors() {
        let err = ArtifactMap::parse_toml(
            r#"
            [foo]
            location = []
            audience = "public"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ArtifactMapError::NoLocations(_)));
    }

    #[test]
    fn audience_display_roundtrip() {
        assert_eq!(Audience::Public.as_display(), "public");
        assert_eq!(Audience::Team.as_display(), "team");
        assert_eq!(Audience::AuthorAgent.as_display(), "author+agent");
        assert_eq!(
            Audience::Partner("operator-b".to_owned()).as_display(),
            "partner:operator-b"
        );
        assert_eq!(Audience::Solo.as_display(), "solo");
    }
}
