// SPDX-License-Identifier: AGPL-3.0-only

//! Slugification for tmux session names.
//!
//! Converts a molecule topic into a short, human-readable kebab-case slug
//! that can be combined with a molecule ID suffix to form a tmux session
//! name like `needs-recompile-fix-1b86`. Slugs are constrained to ASCII
//! alphanumerics and hyphens so they are valid `WorkerId` values.

/// Maximum length of the slug portion of a session name.
pub const MAX_SLUG_LEN: usize = 30;

/// Maximum number of meaningful words retained in the slug.
pub const MAX_SLUG_WORDS: usize = 5;

/// Length of the short molecule ID suffix appended to the slug.
pub const SHORT_ID_LEN: usize = 4;

/// Low-content words stripped during slugification.
///
/// These words carry little identity signal, so dropping them lets the slug
/// fit more meaningful tokens within [`MAX_SLUG_LEN`].
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "of", "for", "to", "in", "on", "at", "with", "and", "or", "but", "is", "are",
    "be", "by", "from", "as", "into", "that", "this",
];

/// Slugify a free-form topic into a kebab-case identifier.
///
/// Keeps up to [`MAX_SLUG_WORDS`] meaningful words (stop words removed),
/// lowercases, replaces non-alphanumerics with hyphens, and truncates at
/// [`MAX_SLUG_LEN`] — always on a word boundary so the result never ends
/// with a partial token or a trailing hyphen. Returns an empty string when
/// no meaningful content remains.
#[must_use]
pub fn slugify(input: &str) -> String {
    let lowered = input.to_ascii_lowercase();
    let mut words: Vec<String> = lowered
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();

    // Drop stop words but only if doing so leaves at least one word.
    let meaningful: Vec<String> = words
        .iter()
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .cloned()
        .collect();
    if !meaningful.is_empty() {
        words = meaningful;
    }

    words.truncate(MAX_SLUG_WORDS);

    let mut out = String::new();
    for word in words {
        let candidate = if out.is_empty() {
            word.clone()
        } else {
            format!("{out}-{word}")
        };
        if candidate.len() > MAX_SLUG_LEN {
            break;
        }
        out = candidate;
    }

    // Defensive trim — should be unreachable given the construction above.
    out.trim_matches('-').to_owned()
}

/// Derive the last [`SHORT_ID_LEN`] ASCII alphanumeric characters of a
/// molecule ID — used as a collision-resistant suffix.
///
/// Returns the full ID when shorter than [`SHORT_ID_LEN`].
#[must_use]
pub fn short_id(mol_id: &str) -> String {
    let clean: String = mol_id.chars().filter(char::is_ascii_alphanumeric).collect();
    if clean.len() <= SHORT_ID_LEN {
        clean
    } else {
        clean[clean.len() - SHORT_ID_LEN..].to_owned()
    }
}

/// Build a tmux session name from a topic and molecule ID.
///
/// Produces `{slug}-{shortid}` when the topic yields a non-empty slug,
/// otherwise falls back to the raw molecule ID so every session remains
/// addressable. The result is guaranteed to be a valid [`crate::id::WorkerId`]
/// string (ASCII alphanumerics and hyphens, no leading/trailing hyphen).
#[must_use]
pub fn session_name_for(topic: Option<&str>, mol_id: &str) -> String {
    let slug = topic.map(slugify).unwrap_or_default();
    if slug.is_empty() {
        return mol_id.to_owned();
    }
    let short = short_id(mol_id);
    if short.is_empty() {
        slug
    } else {
        format!("{slug}-{short}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_basic() {
        assert_eq!(
            slugify("Fix the broken tmux session"),
            "fix-broken-tmux-session"
        );
    }

    #[test]
    fn test_slugify_drops_stop_words() {
        assert_eq!(
            slugify("Add a11y support to the cockpit panel"),
            "add-a11y-support-cockpit-panel"
        );
    }

    #[test]
    fn test_slugify_truncates_to_max_len() {
        let s = slugify("supercalifragilistic expialidocious wonderful magical elixir");
        assert!(s.len() <= MAX_SLUG_LEN, "slug too long: {s}");
        // Should end on a word boundary, never on a hyphen.
        assert!(!s.ends_with('-'));
    }

    #[test]
    fn test_slugify_limits_word_count() {
        let s = slugify("one two three four five six seven eight");
        assert_eq!(s.split('-').count(), MAX_SLUG_WORDS);
    }

    #[test]
    fn test_slugify_strips_special_chars() {
        assert_eq!(
            slugify("fix: send-keys escaping (v2)"),
            "fix-send-keys-escaping-v2"
        );
    }

    #[test]
    fn test_slugify_empty_input() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("   "), "");
        assert_eq!(slugify("!!! ??? ..."), "");
    }

    #[test]
    fn test_slugify_all_stopwords_preserved() {
        // If every word is a stop word, keep them rather than return empty.
        let s = slugify("the of to for");
        assert!(!s.is_empty());
    }

    #[test]
    fn test_short_id_normal() {
        assert_eq!(short_id("task-20260411-1b86"), "1b86");
    }

    #[test]
    fn test_short_id_short_input() {
        assert_eq!(short_id("ab"), "ab");
    }

    #[test]
    fn test_session_name_with_topic() {
        let name = session_name_for(
            Some("Needs recompile: fix the stale binary"),
            "task-20260411-1b86",
        );
        assert!(name.ends_with("-1b86"), "missing short id suffix: {name}");
        assert!(name.starts_with("needs-recompile"));
    }

    #[test]
    fn test_session_name_empty_topic_falls_back() {
        let name = session_name_for(None, "task-20260411-1b86");
        assert_eq!(name, "task-20260411-1b86");
        let name = session_name_for(Some("   "), "task-20260411-1b86");
        assert_eq!(name, "task-20260411-1b86");
    }

    #[test]
    fn test_session_name_is_valid_worker_id() {
        let name = session_name_for(
            Some("Cockpit a11y: keyboard navigation overhaul!"),
            "task-20260411-65b3",
        );
        // WorkerId allows ASCII alphanumeric and hyphens, no leading/trailing hyphen.
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert!(!name.starts_with('-'));
        assert!(!name.ends_with('-'));
        assert!(crate::id::WorkerId::new(name).is_ok());
    }
}
