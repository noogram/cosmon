// SPDX-License-Identifier: AGPL-3.0-only

//! Truncation that cannot panic on text the operator typed.
//!
//! Rust's `&s[..n]` slices **bytes**, and it panics when `n` lands inside a
//! multi-byte character. Every place in the CLI that clipped a title, a slug,
//! a shell command or an event detail with `&s[..n]` was therefore one
//! accented character away from taking down a first-line command: `cs peek`
//! panicked on 2026-08-11 with *end byte index 27 is not a char boundary; it
//! is inside 'é'*. The helpers here are the single place that knows how to cut
//! a string, so there is one implementation to get right rather than one per
//! call site.
//!
//! Two units, because two different questions are being asked:
//!
//! * [`truncate_display`] — *how much of the terminal does this occupy?* The
//!   answer is the display column, not the byte and not the `char`. A CJK
//!   ideogram and most emoji are painted two columns wide (`cs ensemble`
//!   already renders 👻 and ♥), so counting `chars()` overflows a table cell
//!   and shifts every column to its right; counting bytes under-fills it for
//!   any accented text. Use this for anything laid out in columns.
//! * [`truncate_bytes`] — *does this fit in the budget I promised?* Use this
//!   for a stored or transmitted field with a byte cap (an event payload, a
//!   log line), where the cap is genuinely about size and not about width.
//!
//! Both cut on a whole character, so the result is valid UTF-8 for every
//! possible budget — which is the property the tests assert exhaustively.

/// Take the longest prefix of `s` whose rendered width is at most `budget`
/// terminal columns.
///
/// Exposed separately from [`truncate_display`] because a caller that appends
/// its own marker — `cs peek`'s slug truncation keeps the trailing short hash
/// and glues the two halves with `…` — needs to spend the budget itself.
pub fn take_width(s: &str, budget: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// Truncate `s` to at most `max` display columns, appending `…` when trimmed.
///
/// The returned string never exceeds `max` columns: the ellipsis is paid for
/// out of the budget, not added on top of it. A `max` of 0 yields the empty
/// string — there is no room even for the marker.
pub fn truncate_display(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if s.width() <= max {
        s.to_owned()
    } else if max == 0 {
        String::new()
    } else if max == 1 {
        "…".to_owned()
    } else {
        format!("{}…", take_width(s, max - 1))
    }
}

/// Truncate `s` to at most `max_bytes` bytes of *content*, backing up to the
/// nearest preceding character boundary, and append `…` when trimmed.
///
/// The marker itself is 3 bytes on top of `max_bytes`, matching the previous
/// `format!("{}…", &s[..n])` call sites this replaced: the cap is a rough
/// bound on a stored field, not a hard frame width.
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s[..cut].to_owned();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// The strings that used to panic, plus the wide characters the width
    /// unit exists for. `é` is 2 bytes / 1 column, `序` is 3 bytes / 2
    /// columns, `👻` is 4 bytes / 2 columns — so every disagreement between
    /// the three units is represented.
    const SPECIMENS: &[&str] = &[
        "évolution différée",
        "task-20260810-af1e — préparé",
        "序章 molecule",
        "👻 ghost ♥ heart",
        "aéb👻c序d",
        "",
        "ascii-only",
    ];

    /// The bug itself: no budget, on any specimen, may panic. A single
    /// example case would exercise one cut position and miss the rest — the
    /// panic only happens when the cut lands *inside* a character, which is
    /// half the byte offsets of an accented string.
    #[test]
    fn truncating_at_every_budget_never_panics() {
        for s in SPECIMENS {
            for n in 0..=s.len() {
                let _ = truncate_display(s, n);
                let _ = truncate_bytes(s, n);
                let _ = take_width(s, n);
            }
        }
    }

    /// A truncated cell must fit the frame it was cut for, ellipsis included,
    /// or the table columns to its right shift. This is what the byte unit
    /// silently got wrong before the panic ever fired: `"évolution".len()`
    /// is 10, so a 9-column cell clipped text that would have fit.
    #[test]
    fn display_truncation_respects_the_column_budget() {
        for s in SPECIMENS {
            for n in 0..=s.width() + 2 {
                let out = truncate_display(s, n);
                assert!(
                    out.width() <= n,
                    "{s:?} truncated to {n} columns rendered {} wide: {out:?}",
                    out.width()
                );
            }
        }
    }

    /// Everything that fits is kept: truncation must not clip early the way
    /// the byte-counting version did on accented text.
    #[test]
    fn display_truncation_keeps_what_fits() {
        assert_eq!(truncate_display("évolution", 9), "évolution");
        assert_eq!(truncate_display("序章", 4), "序章");
        assert_eq!(truncate_display("évolution", 5), "évol…");
        // A wide character that would straddle the last column is dropped
        // whole rather than half-painted.
        assert_eq!(truncate_display("a序b", 3), "a…");
    }

    /// The byte unit cuts on a boundary and stays under its content cap.
    #[test]
    fn byte_truncation_cuts_on_a_boundary() {
        // "é" spans bytes 0..2, so a cap of 1 backs up to 0.
        assert_eq!(truncate_bytes("éa", 1), "…");
        assert_eq!(truncate_bytes("éa", 2), "é…");
        assert_eq!(truncate_bytes("éa", 3), "éa");
    }
}
