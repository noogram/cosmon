// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest brake: which molecules an automatic path must leave for an
//! explicit landing (ADR-192 I9, one landing path per molecule).
//!
//! The predicate lives here, a pure function of a molecule's tags, so that
//! every automatic caller of `cs done` (the resident sweep, the legacy
//! runtime, the pane-died hook, `cs patrol --harvest`) asks the same question
//! of the same persisted fact. A restart or a stale process re-reads the tag
//! and cannot bypass the hold. An explicit `cs done <id>` and `cs land-train`'s
//! `cs done --expect-head` closures do not consult it: they are the gestures
//! the brake reserves the landing for.

/// The tag that reserves a molecule's merge as an operator (or train) gesture.
pub const NO_AUTO_HARVEST_TAG: &str = "no-auto-harvest";

/// Prefix of the routing intent naming a non-trunk merge target.
pub const HARVEST_TO_PREFIX: &str = "harvest_to:";

/// Whether an automatic path must leave this molecule's merge alone.
///
/// Two tags reserve it. `no-auto-harvest` is the explicit brake: the landing
/// is an operator gesture, typically `cs land-train`. `harvest_to:<branch>` is
/// a routing intent: automatic paths can only merge to the trunk, so any
/// `harvest_to:` tag, whatever branch it names, means the merge is not theirs
/// to make.
///
/// # Examples
///
/// ```
/// use cosmon_core::harvest_hold::harvest_reserved;
///
/// assert!(harvest_reserved(["kind:task", "no-auto-harvest"]));
/// assert!(harvest_reserved(["harvest_to:spore/math"]));
/// assert!(!harvest_reserved(["kind:task", "needs-review"]));
/// ```
#[must_use]
pub fn harvest_reserved(tags: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    tags.into_iter().any(|tag| {
        let tag = tag.as_ref();
        tag == NO_AUTO_HARVEST_TAG || tag.starts_with(HARVEST_TO_PREFIX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_auto_harvest_reserves_the_landing() {
        assert!(harvest_reserved(["no-auto-harvest"]));
    }

    #[test]
    fn any_harvest_to_branch_reserves_the_landing() {
        assert!(harvest_reserved(["harvest_to:spore/x"]));
        assert!(harvest_reserved(["harvest_to:main"]));
    }

    #[test]
    fn other_tags_and_near_misses_do_not() {
        assert!(!harvest_reserved(Vec::<String>::new()));
        assert!(!harvest_reserved([
            "hold:human",
            "needs-review",
            "security"
        ]));
        assert!(!harvest_reserved(["no-auto-harvest-not", "harvest_to"]));
    }
}
