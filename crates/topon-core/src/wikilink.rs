// SPDX-License-Identifier: Apache-2.0

//! Wikilink parser for Obsidian vault files.
//!
//! Parses `[[target]]` and `[[target|alias]]` wikilinks from markdown text.
//! Wikilinks are the primary navigation structure in an Obsidian vault —
//! they form a directed graph analogous to the symbol reference graph in code.
//! Building a PageRank-weighted wikilink graph surfaces the "hub" notes that
//! connect the most concepts.

use serde::{Deserialize, Serialize};

/// A parsed wikilink from markdown text.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Wikilink {
    /// The link target (note name or path).
    pub target: String,
    /// Optional display alias (the text after `|`).
    pub alias: Option<String>,
    /// Byte offset of the opening `[[` in the source text.
    pub start: usize,
    /// Byte offset past the closing `]]` in the source text.
    pub end: usize,
}

/// Parse all wikilinks from markdown text.
///
/// Handles:
/// - `[[target]]` — simple link
/// - `[[target|alias]]` — aliased link
/// - `[[target#heading]]` — heading links (target includes the `#heading`)
/// - Nested brackets are not supported (first `]]` closes the link)
///
/// Links inside code blocks (`` ` `` or `` ``` ``) are intentionally NOT
/// filtered — the parser is purely syntactic. Callers who need to exclude
/// code blocks should pre-process the text.
#[must_use]
pub fn parse_wikilinks(text: &str) -> Vec<Wikilink> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        // Look for `[[`.
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let start = i;
            i += 2;

            // Scan for `]]`, collecting content.
            let content_start = i;
            let mut found_close = false;

            while i + 1 < len {
                if bytes[i] == b']' && bytes[i + 1] == b']' {
                    let content = &text[content_start..i];
                    let end = i + 2;

                    // Skip empty links.
                    if !content.is_empty() {
                        let (target, alias) = if let Some(pipe_pos) = content.find('|') {
                            (
                                content[..pipe_pos].to_owned(),
                                Some(content[pipe_pos + 1..].to_owned()),
                            )
                        } else {
                            (content.to_owned(), None)
                        };

                        links.push(Wikilink {
                            target,
                            alias,
                            start,
                            end,
                        });
                    }

                    i = end;
                    found_close = true;
                    break;
                }
                // Wikilinks don't span newlines in Obsidian.
                if bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }

            if !found_close {
                // No closing `]]` found — skip the opening `[[`.
                i = start + 2;
            }
        } else {
            i += 1;
        }
    }

    links
}

/// Extract unique link targets from parsed wikilinks.
#[must_use]
pub fn unique_targets(links: &[Wikilink]) -> Vec<&str> {
    let mut targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
    targets.sort_unstable();
    targets.dedup();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_wikilink() {
        let links = parse_wikilinks("See [[note name]] for details.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "note name");
        assert_eq!(links[0].alias, None);
    }

    #[test]
    fn test_aliased_wikilink() {
        let links = parse_wikilinks("See [[actual note|display text]] here.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "actual note");
        assert_eq!(links[0].alias.as_deref(), Some("display text"));
    }

    #[test]
    fn test_heading_wikilink() {
        let links = parse_wikilinks("Jump to [[note#section]].");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "note#section");
    }

    #[test]
    fn test_multiple_wikilinks() {
        let links = parse_wikilinks("Link [[a]] and [[b|B]] and [[c]].");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].target, "a");
        assert_eq!(links[1].target, "b");
        assert_eq!(links[1].alias.as_deref(), Some("B"));
        assert_eq!(links[2].target, "c");
    }

    #[test]
    fn test_empty_wikilink_skipped() {
        let links = parse_wikilinks("Empty [[]] link.");
        assert!(links.is_empty());
    }

    #[test]
    fn test_unclosed_wikilink() {
        let links = parse_wikilinks("Unclosed [[ no end.");
        assert!(links.is_empty());
    }

    #[test]
    fn test_newline_breaks_wikilink() {
        let links = parse_wikilinks("Broken [[across\nlines]].");
        assert!(links.is_empty());
    }

    #[test]
    fn test_wikilink_spans() {
        let text = "Start [[target]] end.";
        let links = parse_wikilinks(text);
        assert_eq!(links[0].start, 6);
        assert_eq!(links[0].end, 16);
        assert_eq!(&text[links[0].start..links[0].end], "[[target]]");
    }

    #[test]
    fn test_unique_targets() {
        let links = parse_wikilinks("See [[a]] and [[b]] and [[a]] again.");
        let targets = unique_targets(&links);
        assert_eq!(targets, vec!["a", "b"]);
    }

    #[test]
    fn test_wikilink_serde_roundtrip() {
        let link = Wikilink {
            target: "some note".into(),
            alias: Some("display".into()),
            start: 0,
            end: 25,
        };
        let json = serde_json::to_string(&link).unwrap();
        let back: Wikilink = serde_json::from_str(&json).unwrap();
        assert_eq!(link, back);
    }

    #[test]
    fn test_adjacent_wikilinks() {
        let links = parse_wikilinks("[[a]][[b]]");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "a");
        assert_eq!(links[1].target, "b");
    }

    #[test]
    fn test_wikilink_with_special_chars() {
        let links = parse_wikilinks("[[note (2024)]] and [[café]]");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "note (2024)");
        assert_eq!(links[1].target, "café");
    }
}
