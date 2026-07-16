// SPDX-License-Identifier: AGPL-3.0-only

//! [`PilotDirective`] — in-REPL meta-commands that never reach the model.
//!
//! ## Pilot directives, not slash commands (ADR-096)
//!
//! claw-code has a `commands` crate with a static `SLASH_COMMAND_SPECS`
//! table and a `SlashCommand` enum. cosmon borrows the *pattern* — a small
//! in-REPL command registry, parsed before the line is handed to the model
//! — under ADR-096 (claw treated as bibliography) but renames it: these are
//! **pilot directives**, never "slash commands", and the type is
//! [`PilotDirective`], never `SlashCommand`. The `/` prefix is retained
//! because it is the universally-recognised REPL meta-command sigil, not
//! because of any claw lineage.
//!
//! A directive is parsed *before* the model loop and short-circuits it: it
//! mutates the session or prints to the operator, then returns straight to
//! the `❯` prompt without spending a model round-trip. That is the whole
//! point — `/observe <id>` lets the operator inspect a molecule for free,
//! where letting the *model* call the `observe` tool would cost a turn.

/// A meta-command the operator typed at the `❯` prompt, recognised by its
/// leading `/`. Parsed by [`PilotDirective::parse`]; dispatched by the
/// REPL loop in [`crate::repl`].
///
/// v0 carries exactly the four directives the brief names (`/help`,
/// `/quit`, `/compact`, `/observe`) plus an [`Unknown`](Self::Unknown)
/// catch-all so a mistyped `/qiut` is reported, not silently sent to the
/// model. `#[non_exhaustive]` because the directive set grows (delib §4
/// names `/peek`, `/model`, … for later increments).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PilotDirective {
    /// `/help` — print the directive list and one-line usage.
    Help,
    /// `/quit` — leave the REPL (the only path to the FSM terminal
    /// `stopped` state; `step()` itself can never terminate the session,
    /// the `InteractiveStopYields` invariant of ADR-115).
    Quit,
    /// `/compact` — force a compaction of the conversation log toward the
    /// session target *now*, between turns, instead of waiting for the
    /// automatic threshold trigger inside `step()`.
    Compact,
    /// `/observe <molecule-id>` — inspect one molecule's lifecycle state
    /// directly (reusing the read-only `observe` ops-tool), without
    /// spending a model turn. `molecule_id` is `None` when the operator
    /// typed a bare `/observe` with no argument — the REPL prints usage.
    Observe {
        /// The molecule id argument, or `None` if omitted.
        molecule_id: Option<String>,
    },
    /// A `/`-prefixed line whose verb matched no known directive. Carries
    /// the raw verb (without the leading `/`) so the REPL can echo it back
    /// in the "unknown directive" hint rather than silently forwarding a
    /// typo to the model.
    Unknown {
        /// The unrecognised verb, leading `/` stripped.
        verb: String,
    },
}

impl PilotDirective {
    /// Parse a raw operator line into a directive, or [`None`] if the line
    /// is not a directive (i.e. does not begin with `/`) and should be
    /// sent to the model as a normal turn.
    ///
    /// The leading/trailing whitespace is trimmed first; an empty line or
    /// a line whose first non-whitespace character is not `/` returns
    /// `None`. A `/`-prefixed line always returns `Some` — an unknown verb
    /// becomes [`PilotDirective::Unknown`] rather than `None`, so the REPL
    /// can tell "operator meant a directive but mistyped it" apart from
    /// "operator typed a message that happens to mention a slash".
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix('/')?;
        // Split the verb from its (optional) single argument.
        let mut parts = rest.splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or("");
        let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());

        Some(match verb {
            "help" | "h" | "?" => Self::Help,
            "quit" | "q" | "exit" => Self::Quit,
            "compact" => Self::Compact,
            "observe" | "o" => Self::Observe {
                molecule_id: arg.map(str::to_owned),
            },
            other => Self::Unknown {
                verb: other.to_owned(),
            },
        })
    }

    /// The multi-line help text printed by the `/help` directive — one
    /// line per directive, matching the parse table above.
    #[must_use]
    pub fn help_text() -> &'static str {
        "pilot directives (never sent to the model):\n  \
         /help              show this help\n  \
         /observe <id>      inspect one molecule's state (read-only, no model turn)\n  \
         /compact           compact the conversation log now\n  \
         /quit              leave the pilot\n\
         anything else is sent to the model as a turn."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_not_a_directive() {
        assert_eq!(PilotDirective::parse("observe the fleet please"), None);
        assert_eq!(PilotDirective::parse(""), None);
        assert_eq!(PilotDirective::parse("   "), None);
    }

    #[test]
    fn known_verbs_parse() {
        assert_eq!(PilotDirective::parse("/help"), Some(PilotDirective::Help));
        assert_eq!(PilotDirective::parse("/quit"), Some(PilotDirective::Quit));
        assert_eq!(
            PilotDirective::parse("  /compact  "),
            Some(PilotDirective::Compact)
        );
    }

    #[test]
    fn observe_captures_its_argument() {
        assert_eq!(
            PilotDirective::parse("/observe task-20260531-67f5"),
            Some(PilotDirective::Observe {
                molecule_id: Some("task-20260531-67f5".to_owned())
            })
        );
        assert_eq!(
            PilotDirective::parse("/observe"),
            Some(PilotDirective::Observe { molecule_id: None })
        );
    }

    #[test]
    fn unknown_verb_is_reported_not_forwarded() {
        assert_eq!(
            PilotDirective::parse("/qiut"),
            Some(PilotDirective::Unknown {
                verb: "qiut".to_owned()
            })
        );
    }

    #[test]
    fn help_text_lists_every_v0_directive() {
        let help = PilotDirective::help_text();
        for verb in ["/help", "/observe", "/compact", "/quit"] {
            assert!(help.contains(verb), "help must mention {verb}");
        }
    }
}
