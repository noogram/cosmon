// SPDX-License-Identifier: AGPL-3.0-only

//! `Atlas`: Materialized Query Interface for external sources.
//!
//! Implements the **external-reference flavor** of the Content-Identity Principle
//! (ADR-011). External sources — Google Docs, PDFs, web pages — are identified
//! by opaque identifiers from authoritative registries (Drive file IDs, DOIs,
//! URLs) rather than by content hash. `Atlas` materializes these sources into
//! local companion notes with `type: materialization` and `editable: false`
//! frontmatter, enabling read-only local mirrors that stay in sync.
//!
//! # Architecture
//!
//! The pipeline has three stages, each behind a trait (zero I/O in core):
//!
//! 1. **Fetch** — retrieve raw bytes from the external source (e.g., rclone)
//! 2. **Convert** — transform raw bytes to the target format (e.g., pandoc)
//! 3. **Companion note** — wrap the converted content with materialization metadata
//!
//! ```text
//! ExternalRef ──[Fetcher]──> RawContent ──[Converter]──> ConvertedContent
//!                                                              │
//!                                                    CompanionNote::assemble()
//!                                                              │
//!                                                              ▼
//!                                                     Markdown + frontmatter
//! ```
//!
//! # Companion-note pattern
//!
//! Each materialized source produces a companion note with YAML frontmatter:
//!
//! ```yaml
//! ---
//! type: materialization
//! editable: false
//! source_id: "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms"
//! source_type: google_doc
//! fetched_at: "2026-04-04T22:50:00Z"
//! content_hash: "e3b0c44..."
//! ---
//! ```

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cas::ContentHash;
use crate::error::CosmonError;

// ---------------------------------------------------------------------------
// ExternalRef — identity for external sources
// ---------------------------------------------------------------------------

/// Identifies an external source by its authoritative identifier.
///
/// This is the "external reference" flavor of content-identity from ADR-011:
/// identity is delegated to an external registry (Google Drive, DOI, URL)
/// because byte-reproducibility is not guaranteed.
///
/// # Examples
///
/// ```
/// use cosmon_core::atlas::{ExternalRef, SourceType};
///
/// let doc = ExternalRef::new(
///     "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms",
///     SourceType::GoogleDoc,
/// ).unwrap();
/// assert_eq!(doc.source_type(), &SourceType::GoogleDoc);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalRef {
    /// The opaque identifier from the authoritative source.
    source_id: String,
    /// What kind of external source this references.
    source_type: SourceType,
}

impl ExternalRef {
    /// Create a new external reference.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::Runtime`] if `source_id` is empty.
    pub fn new(source_id: impl Into<String>, source_type: SourceType) -> Result<Self, CosmonError> {
        let source_id = source_id.into();
        if source_id.is_empty() {
            return Err(CosmonError::Runtime {
                reason: "external ref source_id cannot be empty".to_owned(),
            });
        }
        Ok(Self {
            source_id,
            source_type,
        })
    }

    /// The opaque source identifier (e.g., Google Drive file ID, DOI).
    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// The type of external source.
    #[must_use]
    pub fn source_type(&self) -> &SourceType {
        &self.source_type
    }
}

impl fmt::Display for ExternalRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source_type, self.source_id)
    }
}

// ---------------------------------------------------------------------------
// SourceType — what kind of external source
// ---------------------------------------------------------------------------

/// The type of external source being referenced.
///
/// Each variant implies a specific fetch mechanism (e.g., rclone backend
/// for Google Docs, HTTP for URLs) and potentially a default converter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// Google Doc (fetched via rclone `drive` backend, `copyid`).
    GoogleDoc,
    /// Google Sheet.
    GoogleSheet,
    /// Generic Google Drive file (binary, PDF, etc.).
    GoogleDrive,
    /// A web page identified by URL.
    WebPage,
    /// An academic paper identified by DOI.
    Doi,
}

impl fmt::Display for SourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GoogleDoc => write!(f, "google_doc"),
            Self::GoogleSheet => write!(f, "google_sheet"),
            Self::GoogleDrive => write!(f, "google_drive"),
            Self::WebPage => write!(f, "web_page"),
            Self::Doi => write!(f, "doi"),
        }
    }
}

impl FromStr for SourceType {
    type Err = CosmonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "google_doc" => Ok(Self::GoogleDoc),
            "google_sheet" => Ok(Self::GoogleSheet),
            "google_drive" => Ok(Self::GoogleDrive),
            "web_page" => Ok(Self::WebPage),
            "doi" => Ok(Self::Doi),
            other => Err(CosmonError::Runtime {
                reason: format!("unknown source type: {other}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// ConvertFormat — target format for the converter stage
// ---------------------------------------------------------------------------

/// Target format for the conversion stage.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvertFormat {
    /// GitHub-flavored Markdown (default for companion notes).
    Markdown,
    /// Plain text.
    PlainText,
    /// HTML.
    Html,
}

#[allow(clippy::derivable_impls)]
impl Default for ConvertFormat {
    fn default() -> Self {
        Self::Markdown
    }
}

impl fmt::Display for ConvertFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Markdown => write!(f, "markdown"),
            Self::PlainText => write!(f, "plain_text"),
            Self::Html => write!(f, "html"),
        }
    }
}

// ---------------------------------------------------------------------------
// MaterializationSpec — how to materialize an external ref
// ---------------------------------------------------------------------------

/// Specification for how to materialize an external reference.
///
/// Describes the full pipeline: which fetch backend to use, what conversion
/// to apply, and where to write the companion note.
///
/// # Examples
///
/// ```
/// use cosmon_core::atlas::{ExternalRef, MaterializationSpec, SourceType, ConvertFormat};
///
/// let ext_ref = ExternalRef::new("1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms", SourceType::GoogleDoc).unwrap();
/// let spec = MaterializationSpec::new(ext_ref, ConvertFormat::Markdown);
/// assert_eq!(spec.target_format(), &ConvertFormat::Markdown);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationSpec {
    /// The external source to materialize.
    external_ref: ExternalRef,
    /// Target format for conversion.
    target_format: ConvertFormat,
    /// Optional output path hint (relative to vault root).
    output_path: Option<String>,
}

impl MaterializationSpec {
    /// Create a new materialization spec.
    #[must_use]
    pub fn new(external_ref: ExternalRef, target_format: ConvertFormat) -> Self {
        Self {
            external_ref,
            target_format,
            output_path: None,
        }
    }

    /// Set the output path hint.
    #[must_use]
    pub fn with_output_path(mut self, path: impl Into<String>) -> Self {
        self.output_path = Some(path.into());
        self
    }

    /// The external source to materialize.
    #[must_use]
    pub fn external_ref(&self) -> &ExternalRef {
        &self.external_ref
    }

    /// The target conversion format.
    #[must_use]
    pub fn target_format(&self) -> &ConvertFormat {
        &self.target_format
    }

    /// Optional output path hint.
    #[must_use]
    pub fn output_path(&self) -> Option<&str> {
        self.output_path.as_deref()
    }
}

// ---------------------------------------------------------------------------
// CompanionNote — materialized output with metadata
// ---------------------------------------------------------------------------

/// A materialized companion note with YAML frontmatter metadata.
///
/// Companion notes are read-only local mirrors of external sources. They carry
/// `type: materialization` and `editable: false` in their frontmatter, signaling
/// to editors and agents that the content is managed by the `Atlas` pipeline and
/// should not be hand-edited.
///
/// # Examples
///
/// ```
/// use cosmon_core::atlas::{CompanionNote, ExternalRef, SourceType};
/// use cosmon_core::cas::ContentHash;
/// use chrono::Utc;
///
/// let ext_ref = ExternalRef::new("abc123", SourceType::GoogleDoc).unwrap();
/// let hash = ContentHash::new(
///     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
/// ).unwrap();
/// let note = CompanionNote::new(ext_ref, "Hello world".to_owned(), hash, Utc::now());
/// assert!(!CompanionNote::editable());
/// assert!(note.render().starts_with("---\n"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionNote {
    /// The external source this note was materialized from.
    source: ExternalRef,
    /// The converted content body (e.g., Markdown text).
    body: String,
    /// SHA-256 hash of the raw fetched content (before conversion).
    content_hash: ContentHash,
    /// When the content was last fetched from the external source.
    fetched_at: DateTime<Utc>,
}

impl CompanionNote {
    /// Assemble a companion note from fetched and converted content.
    #[must_use]
    pub fn new(
        source: ExternalRef,
        body: String,
        content_hash: ContentHash,
        fetched_at: DateTime<Utc>,
    ) -> Self {
        Self {
            source,
            body,
            content_hash,
            fetched_at,
        }
    }

    /// Companion notes are always read-only.
    #[must_use]
    pub fn editable() -> bool {
        false
    }

    /// The external source reference.
    #[must_use]
    pub fn source(&self) -> &ExternalRef {
        &self.source
    }

    /// The converted content body.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// SHA-256 hash of the raw fetched bytes.
    #[must_use]
    pub fn content_hash(&self) -> &ContentHash {
        &self.content_hash
    }

    /// When the content was last fetched.
    #[must_use]
    pub fn fetched_at(&self) -> &DateTime<Utc> {
        &self.fetched_at
    }

    /// Render the companion note as Markdown with YAML frontmatter.
    ///
    /// The output is suitable for writing directly to a `.md` file.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "---\ntype: materialization\neditable: false\nsource_id: \"{}\"\nsource_type: {}\nfetched_at: \"{}\"\ncontent_hash: \"{}\"\n---\n\n{}",
            self.source.source_id(),
            self.source.source_type(),
            self.fetched_at.to_rfc3339(),
            self.content_hash,
            self.body,
        )
    }

    /// Check whether the content has changed by comparing content hashes.
    ///
    /// Returns `true` if `new_hash` differs from the stored hash, meaning
    /// the external source has been updated since last fetch.
    #[must_use]
    pub fn is_stale(&self, new_hash: &ContentHash) -> bool {
        &self.content_hash != new_hash
    }
}

// ---------------------------------------------------------------------------
// Fetcher trait — retrieve raw bytes from an external source
// ---------------------------------------------------------------------------

/// Trait for fetching raw bytes from an external source.
///
/// Implementations handle the transport layer: rclone for Google Drive,
/// HTTP for web pages, etc. This trait is I/O-free in the core crate;
/// concrete implementations live in separate crates.
pub trait Fetcher {
    /// Fetch raw bytes from the given external reference.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on network, auth, or not-found failures.
    fn fetch(&self, ext_ref: &ExternalRef) -> Result<Vec<u8>, CosmonError>;
}

// ---------------------------------------------------------------------------
// Converter trait — transform raw bytes to target format
// ---------------------------------------------------------------------------

/// Trait for converting raw fetched content to a target format.
///
/// The canonical implementation shells out to pandoc, but this trait
/// allows testing with in-memory converters.
pub trait Converter {
    /// Convert raw bytes to the target format.
    ///
    /// Returns the converted content as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on conversion failures (unsupported format,
    /// malformed input, etc.).
    fn convert(
        &self,
        raw: &[u8],
        source_type: &SourceType,
        target: &ConvertFormat,
    ) -> Result<String, CosmonError>;
}

// ---------------------------------------------------------------------------
// Materializer — the full pipeline
// ---------------------------------------------------------------------------

/// Orchestrates the full materialization pipeline: fetch → convert → companion note.
///
/// This is a convenience that composes a [`Fetcher`] and [`Converter`] into
/// the complete `Atlas` pipeline.
pub struct Materializer<F, C> {
    fetcher: F,
    converter: C,
}

impl<F: Fetcher, C: Converter> Materializer<F, C> {
    /// Create a new materializer with the given fetcher and converter.
    pub fn new(fetcher: F, converter: C) -> Self {
        Self { fetcher, converter }
    }

    /// Materialize an external source into a companion note.
    ///
    /// # Pipeline
    ///
    /// 1. Fetch raw bytes via the [`Fetcher`]
    /// 2. Compute SHA-256 content hash of the raw bytes
    /// 3. Convert to the target format via the [`Converter`]
    /// 4. Assemble a [`CompanionNote`] with metadata
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] if any pipeline stage fails.
    pub fn materialize(
        &self,
        spec: &MaterializationSpec,
        hash_fn: impl FnOnce(&[u8]) -> Result<ContentHash, CosmonError>,
    ) -> Result<CompanionNote, CosmonError> {
        let raw = self.fetcher.fetch(spec.external_ref())?;
        let content_hash = hash_fn(&raw)?;
        let body = self.converter.convert(
            &raw,
            spec.external_ref().source_type(),
            spec.target_format(),
        )?;
        Ok(CompanionNote::new(
            spec.external_ref().clone(),
            body,
            content_hash,
            Utc::now(),
        ))
    }

    /// Check if a companion note is stale without re-materializing.
    ///
    /// Fetches new content, computes the hash, and compares against
    /// the existing note's hash. Returns `true` if content changed.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] if the fetch or hash computation fails.
    pub fn is_stale(
        &self,
        existing: &CompanionNote,
        hash_fn: impl FnOnce(&[u8]) -> Result<ContentHash, CosmonError>,
    ) -> Result<bool, CosmonError> {
        let raw = self.fetcher.fetch(existing.source())?;
        let new_hash = hash_fn(&raw)?;
        Ok(existing.is_stale(&new_hash))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn test_external_ref_creation() {
        let r = ExternalRef::new("abc123", SourceType::GoogleDoc).unwrap();
        assert_eq!(r.source_id(), "abc123");
        assert_eq!(r.source_type(), &SourceType::GoogleDoc);
        assert_eq!(r.to_string(), "google_doc:abc123");
    }

    #[test]
    fn test_external_ref_rejects_empty_id() {
        let err = ExternalRef::new("", SourceType::GoogleDoc).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_source_type_roundtrip() {
        for (s, expected) in [
            ("google_doc", SourceType::GoogleDoc),
            ("google_sheet", SourceType::GoogleSheet),
            ("google_drive", SourceType::GoogleDrive),
            ("web_page", SourceType::WebPage),
            ("doi", SourceType::Doi),
        ] {
            let parsed: SourceType = s.parse().unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn test_source_type_rejects_unknown() {
        let err = "floppy_disk".parse::<SourceType>().unwrap_err();
        assert!(err.to_string().contains("unknown source type"));
    }

    #[test]
    fn test_materialization_spec() {
        let ext_ref = ExternalRef::new("doc123", SourceType::GoogleDoc).unwrap();
        let spec = MaterializationSpec::new(ext_ref.clone(), ConvertFormat::Markdown)
            .with_output_path("research/synced/my-doc.md");
        assert_eq!(spec.external_ref(), &ext_ref);
        assert_eq!(spec.target_format(), &ConvertFormat::Markdown);
        assert_eq!(spec.output_path(), Some("research/synced/my-doc.md"));
    }

    #[test]
    fn test_companion_note_render() {
        let ext_ref = ExternalRef::new("abc123", SourceType::GoogleDoc).unwrap();
        let hash = ContentHash::new(TEST_HASH).unwrap();
        let fetched = chrono::DateTime::parse_from_rfc3339("2026-04-04T22:50:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let note = CompanionNote::new(ext_ref, "# Hello\n\nWorld".to_owned(), hash, fetched);

        assert!(!CompanionNote::editable());
        let rendered = note.render();
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.contains("type: materialization"));
        assert!(rendered.contains("editable: false"));
        assert!(rendered.contains("source_id: \"abc123\""));
        assert!(rendered.contains("source_type: google_doc"));
        assert!(rendered.contains("content_hash:"));
        assert!(rendered.contains("# Hello\n\nWorld"));
    }

    #[test]
    fn test_companion_note_staleness() {
        let ext_ref = ExternalRef::new("abc123", SourceType::GoogleDoc).unwrap();
        let hash = ContentHash::new(TEST_HASH).unwrap();
        let note = CompanionNote::new(ext_ref, "body".to_owned(), hash.clone(), Utc::now());

        // Same hash → not stale
        assert!(!note.is_stale(&hash));

        // Different hash → stale
        let new_hash =
            ContentHash::new("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")
                .unwrap();
        assert!(note.is_stale(&new_hash));
    }

    #[test]
    fn test_companion_note_serde_roundtrip() {
        let ext_ref = ExternalRef::new("doc123", SourceType::GoogleDoc).unwrap();
        let hash = ContentHash::new(TEST_HASH).unwrap();
        let note = CompanionNote::new(ext_ref, "body".to_owned(), hash, Utc::now());

        let json = serde_json::to_string(&note).unwrap();
        let back: CompanionNote = serde_json::from_str(&json).unwrap();
        assert_eq!(note, back);
    }

    #[test]
    fn test_external_ref_serde_roundtrip() {
        let ext_ref = ExternalRef::new("1BxiMVs0XRA5", SourceType::GoogleDoc).unwrap();
        let json = serde_json::to_string(&ext_ref).unwrap();
        let back: ExternalRef = serde_json::from_str(&json).unwrap();
        assert_eq!(ext_ref, back);
    }

    // --- Materializer with test doubles ---

    struct TestFetcher {
        content: Vec<u8>,
    }

    impl Fetcher for TestFetcher {
        fn fetch(&self, _ext_ref: &ExternalRef) -> Result<Vec<u8>, CosmonError> {
            Ok(self.content.clone())
        }
    }

    struct TestConverter;

    impl Converter for TestConverter {
        fn convert(
            &self,
            raw: &[u8],
            _source_type: &SourceType,
            _target: &ConvertFormat,
        ) -> Result<String, CosmonError> {
            String::from_utf8(raw.to_vec()).map_err(|e| CosmonError::Runtime {
                reason: e.to_string(),
            })
        }
    }

    fn test_hash_fn(data: &[u8]) -> Result<ContentHash, CosmonError> {
        // Deterministic test hash based on length
        let hex = format!("{:0>64x}", data.len());
        ContentHash::new(hex)
    }

    #[test]
    fn test_materializer_pipeline() {
        let fetcher = TestFetcher {
            content: b"# Hello from Google Docs".to_vec(),
        };
        let converter = TestConverter;
        let materializer = Materializer::new(fetcher, converter);

        let ext_ref = ExternalRef::new("doc123", SourceType::GoogleDoc).unwrap();
        let spec = MaterializationSpec::new(ext_ref, ConvertFormat::Markdown);

        let note = materializer.materialize(&spec, test_hash_fn).unwrap();
        assert_eq!(note.body(), "# Hello from Google Docs");
        assert!(!CompanionNote::editable());
    }

    #[test]
    fn test_materializer_staleness_check() {
        let fetcher = TestFetcher {
            content: b"updated content".to_vec(),
        };
        let converter = TestConverter;
        let materializer = Materializer::new(fetcher, converter);

        let ext_ref = ExternalRef::new("doc123", SourceType::GoogleDoc).unwrap();
        // Old note had different content (length 5 → different hash)
        let old_hash = test_hash_fn(b"hello").unwrap();
        let old_note = CompanionNote::new(ext_ref, "hello".to_owned(), old_hash, Utc::now());

        let stale = materializer.is_stale(&old_note, test_hash_fn).unwrap();
        assert!(stale, "content changed so note should be stale");
    }

    #[test]
    fn test_convert_format_default() {
        assert_eq!(ConvertFormat::default(), ConvertFormat::Markdown);
    }
}
