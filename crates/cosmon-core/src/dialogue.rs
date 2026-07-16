// SPDX-License-Identifier: AGPL-3.0-only

//! Blocking-dialogue classifier — read a worker pane, decide the *stake*.
//!
//! # Why this module exists
//!
//! A `cs tackle`d worker runs `claude` inside a tmux pane. Some of the things
//! that pane asks for are *cheap keystrokes with no consequence* — a tool
//! permission prompt (`Do you want to proceed? › 1. Yes`) — and some are
//! *decisions with money on them* — the Claude Code spend-limit / usage-limit
//! dialog. The incident that motivated this module (showroom, 2026-07-03/04):
//! **ten workers sat blocked ~30 hours on the spend-limit dialog** because
//! there was no human at the keyboard to press Enter, and the pilot had to
//! propel every one by hand.
//!
//! The operator's ask has two halves that pull in opposite directions:
//!
//! - **auto-confirm the cheap prompts** so a worker never rots 30h waiting for
//!   a keystroke that carries no stake; but
//! - **never, ever auto-confirm a money choice** — a spend-limit or a plan
//!   upgrade must page a human, because pressing Enter there spends the fund's
//!   money without consent.
//!
//! This module is the *decision core* that separates those two worlds. It is
//! pure text-in / verdict-out — no tmux, no I/O, no side effects — so the
//! classification is an executable spec, unit-testable without a live pane.
//!
//! # The be1e discipline (ADR-137 §2) — why the verdict is not the action
//!
//! The adversarial audit `delib-20260625-be1e` reached a SEV-1 conclusion from
//! four directions: **pane text is an adversarial channel.** A worker controls
//! what its pane renders and can print the exact glyphs of any rule meant to
//! police it — *"a guard that recognizes its target by its name arrests
//! everyone who says the name."* The escape is stratification: read pane text
//! only to *surface a finding to a human*, and key any *autonomous mutation*
//! off control-plane state instead.
//!
//! This classifier honors that by construction:
//!
//! 1. The **only** verdict that permits an autonomous keystroke is
//!    [`DialogueClass::Permission`], and even then the caller must have
//!    *opted in* (`--auto-confirm-safe`) — the default is surface-to-human.
//! 2. A money marker **dominates** every other signal
//!    ([`DialogueClass::MoneyStake`] can never be auto-confirmed, regardless
//!    of caller flags — the refusal is encoded here in pure code, not in
//!    config a tired operator can mis-set).
//! 3. Anything blocking but unrecognised is [`DialogueClass::Unknown`], which
//!    **fails safe toward the alert path** — surface to a human, never act.
//! 4. A destructive/irreversible marker inside an otherwise permission-shaped
//!    prompt downgrades it to `Unknown` — we do not auto-accept a `rm -rf`,
//!    a `git push`, or a `--force` just because the surrounding shape looked
//!    like a routine tool prompt.
//!
//! So: the classifier may *read* the pane; whether the caller may *act* on a
//! given class is a separate, deliberately narrow decision expressed by
//! [`DialogueClass::auto_confirmable`] and [`DialogueClass::requires_alert`].

use serde::{Deserialize, Serialize};

/// The stake a blocking pane dialogue carries — the load-bearing output of
/// this module.
///
/// Ordering of severity (for a human reader): `None` < `Permission` <
/// `Unknown` < `MoneyStake`. The classifier picks the *most severe* class
/// whose markers are present, so a permission prompt that also mentions a
/// spend limit resolves to `MoneyStake` (fail-safe toward the alert path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueClass {
    /// No blocking prompt is visible in the captured pane. The worker is
    /// either thinking or genuinely idle; this module says nothing about
    /// which — that is [`crate::patrol`]'s progress/heartbeat job.
    None,
    /// A tool/permission prompt with **no money stake and no destructive
    /// action** — e.g. `claude` asking to read a file, run a read-only
    /// command, or edit within the worktree. This is the *only* class a
    /// caller may auto-confirm, and only when explicitly opted in.
    Permission,
    /// A prompt that touches **billing, spend limits, usage credits, or a
    /// plan upgrade**. This class is **never** auto-confirmable — pressing
    /// Enter here spends money. The caller's sole sanctioned response is to
    /// alert the operator.
    MoneyStake,
    /// A blocking prompt was detected but does not match a known *safe*
    /// permission shape — an unrecognised confirmation, or a
    /// permission-shaped prompt carrying a destructive/irreversible action
    /// (`rm -rf`, `git push`, `--force`, publish, …). Fails safe: surface to
    /// a human, never act.
    Unknown,
}

impl DialogueClass {
    /// May a caller that has opted into auto-confirm safely fire the
    /// default-accept keystroke for this class?
    ///
    /// `true` **only** for [`DialogueClass::Permission`]. Money stakes and
    /// unknown blocks always return `false` — this is the pure-code encoding
    /// of *"jamais auto-confirmer un choix d'argent"* and *"fail safe on the
    /// unrecognised"*.
    #[must_use]
    pub const fn auto_confirmable(self) -> bool {
        matches!(self, Self::Permission)
    }

    /// Does this class warrant paging the operator?
    ///
    /// `true` for [`DialogueClass::MoneyStake`] and [`DialogueClass::Unknown`]
    /// — the two classes a human must resolve. `Permission` does not alert by
    /// itself (it is either auto-confirmed or reported quietly), and `None`
    /// never alerts.
    #[must_use]
    pub const fn requires_alert(self) -> bool {
        matches!(self, Self::MoneyStake | Self::Unknown)
    }

    /// Stable lowercase token for JSON output and `cs notify --level` routing.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Permission => "permission",
            Self::MoneyStake => "money_stake",
            Self::Unknown => "unknown",
        }
    }
}

/// The verdict of [`classify_pane`]: the [`DialogueClass`] plus the pane line
/// that triggered it, kept as evidence for the audit event and the operator
/// alert. `evidence` is `None` exactly when `class == DialogueClass::None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogueScan {
    /// The classified stake of the blocking dialogue.
    pub class: DialogueClass,
    /// The trimmed pane line whose marker fired the classification. Carried
    /// verbatim (truncated) into the alert so the operator sees *why* without
    /// re-capturing the pane.
    pub evidence: Option<String>,
}

/// Money markers — case-insensitive substrings that, if present *anywhere* in
/// the captured pane, force [`DialogueClass::MoneyStake`]. This table is the
/// single most important safety surface in the module: a false negative here
/// means an autonomous Enter on a spend decision. When in doubt, add the
/// marker — the cost of a false positive is one operator page, the cost of a
/// false negative is spending money without consent.
const MONEY_MARKERS: &[&str] = &[
    "spend limit",
    "spending limit",
    "usage limit",
    "usage credit",
    "credit balance",
    "out of credit",
    "insufficient credit",
    "billing",
    "payment",
    "upgrade your plan",
    "upgrade to claude",
    "upgrade plan",
    "monthly limit",
    "5-hour limit",
    "weekly limit",
    "purchase",
    "add funds",
    "buy more",
    "cost limit",
    "budget",
];

/// Destructive / irreversible markers — if present, a prompt that would
/// otherwise look like a routine permission is downgraded to
/// [`DialogueClass::Unknown`] (surface, never auto-confirm). These are the
/// actions a human must consciously approve even though no money is at stake.
const RISKY_MARKERS: &[&str] = &[
    "rm -rf",
    "rm -r",
    "git push",
    "force push",
    "--force",
    "-force",
    "git reset --hard",
    "publish",
    "npm publish",
    "cargo publish",
    "delete",
    "drop table",
    "sudo ",
    "overwrite",
    "deploy",
    "release",
];

/// Permission-prompt markers — the *safe*, auto-confirmable tool-use shape a
/// `claude` worker renders when it wants to run a read-only command, read a
/// file, or edit within its worktree. Presence of one of these (and the
/// absence of any money or risky marker) yields [`DialogueClass::Permission`].
const PERMISSION_MARKERS: &[&str] = &[
    "do you want to proceed",
    "wants to use",
    "wants to run",
    "wants to read",
    "wants to edit",
    "wants to create",
    "allow this tool",
    "allow this command",
    "grant permission",
    "yes, and don't ask again",
    "yes, allow",
];

/// Generic blocking markers — the pane is *waiting on input* but matches no
/// safe permission shape. Yields [`DialogueClass::Unknown`] (alert, never
/// act) when nothing more specific matched. Kept deliberately broad: a
/// missed block costs a stalled worker slot, and the response is only ever a
/// human page.
const BLOCKING_MARKERS: &[&str] = &[
    "press enter to",
    "enter to confirm",
    "enter to continue",
    "[y/n]",
    "(y/n)",
    "continue? ",
    "are you sure",
    "confirm",
    "❯ 1.",
    "› 1.",
    "1. yes",
    "waiting for",
];

/// Return the first marker from `markers` found (case-insensitively) in the
/// already-lowercased `haystack`, together with the trimmed line it occurred
/// on for evidence.
fn first_match<'a>(lower: &str, lines: &'a [&'a str], markers: &[&str]) -> Option<String> {
    for marker in markers {
        if let Some(pos) = lower.find(marker) {
            // Recover the human-readable line the match fell on so the alert
            // shows real pane text, not the lowercased haystack.
            let line = line_containing(lines, lower, pos);
            return Some(truncate_evidence(line.unwrap_or(marker)));
        }
    }
    None
}

/// Given the byte offset `pos` into the lowercased single-string `lower`
/// (built by joining `lines` with `\n`), return the original line that
/// contains it. Robust to the empty-lines case; returns `None` if the offset
/// cannot be mapped (should not happen for a real match).
fn line_containing<'a>(lines: &'a [&'a str], lower: &str, pos: usize) -> Option<&'a str> {
    let mut cursor = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        // +1 for the '\n' join separator, matching how `lower` was built.
        let sep = usize::from(idx + 1 < lines.len());
        let end = cursor + line.len();
        if pos <= end {
            return Some(line);
        }
        cursor = end + sep;
        // Guard against pathological drift.
        let _ = lower;
    }
    lines.last().copied()
}

/// Trim and cap an evidence line so a runaway pane line cannot bloat the event
/// log or the notification body.
fn truncate_evidence(line: &str) -> String {
    const MAX: usize = 160;
    let t = line.trim();
    if t.chars().count() <= MAX {
        return t.to_owned();
    }
    let cut: String = t.chars().take(MAX).collect();
    format!("{cut}…")
}

/// Classify the captured text of a worker pane into a [`DialogueScan`].
///
/// The decision order encodes the safety invariants (most-severe wins):
///
/// 1. **Money dominates.** If any [`MONEY_MARKERS`] entry is present anywhere,
///    the verdict is [`DialogueClass::MoneyStake`] — full stop. A permission
///    prompt that also mentions a spend limit is a money decision.
/// 2. **Permission, but only if clean.** A [`PERMISSION_MARKERS`] hit with no
///    money marker *and* no [`RISKY_MARKERS`] hit yields
///    [`DialogueClass::Permission`] (the auto-confirmable class).
/// 3. **Risky-but-permission-shaped ⇒ Unknown.** A permission marker sitting
///    next to a destructive action is *not* safe to auto-accept.
/// 4. **Generic block ⇒ Unknown.** Any [`BLOCKING_MARKERS`] hit with nothing
///    safer resolves to [`DialogueClass::Unknown`] (alert, never act).
/// 5. **Otherwise `None`.**
///
/// Only the tail of a pane is meaningful (the live prompt sits at the bottom),
/// but callers typically pass the last N captured lines already; this function
/// classifies whatever it is given.
#[must_use]
pub fn classify_pane(text: &str) -> DialogueScan {
    let lines: Vec<&str> = text.lines().collect();
    let lower = text.to_lowercase();

    // 1. Money dominates unconditionally.
    if let Some(ev) = first_match(&lower, &lines, MONEY_MARKERS) {
        return DialogueScan {
            class: DialogueClass::MoneyStake,
            evidence: Some(ev),
        };
    }

    let permission_hit = first_match(&lower, &lines, PERMISSION_MARKERS);
    let risky_hit = first_match(&lower, &lines, RISKY_MARKERS);

    // 2 & 3. A clean permission prompt is auto-confirmable; a risky one is not.
    if let Some(perm_ev) = permission_hit {
        if let Some(risk_ev) = risky_hit {
            return DialogueScan {
                class: DialogueClass::Unknown,
                evidence: Some(risk_ev),
            };
        }
        return DialogueScan {
            class: DialogueClass::Permission,
            evidence: Some(perm_ev),
        };
    }

    // 4. A generic block we could not classify safely — alert, never act.
    if let Some(ev) = first_match(&lower, &lines, BLOCKING_MARKERS).or(risky_hit) {
        return DialogueScan {
            class: DialogueClass::Unknown,
            evidence: Some(ev),
        };
    }

    // 5. Nothing blocking.
    DialogueScan {
        class: DialogueClass::None,
        evidence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pane_is_none() {
        assert_eq!(classify_pane("").class, DialogueClass::None);
        assert_eq!(classify_pane("   \n\n  ").class, DialogueClass::None);
    }

    #[test]
    fn ordinary_work_output_is_none() {
        let pane = "Running cargo test --workspace\n   Compiling cosmon-core v0.1.0\n\
                    test result: ok. 412 passed";
        assert_eq!(classify_pane(pane).class, DialogueClass::None);
    }

    #[test]
    fn tool_permission_prompt_is_permission() {
        let pane = "cosmon wants to run `ls -la`\n\nDo you want to proceed?\n \
                    ❯ 1. Yes\n   2. Yes, and don't ask again\n   3. No";
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::Permission);
        assert!(scan.class.auto_confirmable());
        assert!(!scan.class.requires_alert());
        assert!(scan.evidence.is_some());
    }

    #[test]
    fn spend_limit_dialog_is_money_never_confirmable() {
        // The exact incident: the Claude Code spend-limit dialog.
        let pane = "You've approached your usage limit for Claude.\n\
                    Approaching spend limit — increase your spending limit?\n \
                    Press Enter to continue";
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::MoneyStake);
        assert!(!scan.class.auto_confirmable());
        assert!(scan.class.requires_alert());
    }

    #[test]
    fn money_marker_dominates_permission_shape() {
        // A prompt that looks like a routine permission but mentions money
        // MUST resolve to MoneyStake — the load-bearing safety invariant.
        let pane = "Do you want to proceed? This will use your usage credit.\n \
                    ❯ 1. Yes";
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::MoneyStake);
        assert!(!scan.class.auto_confirmable());
    }

    #[test]
    fn risky_action_downgrades_permission_to_unknown() {
        let pane = "cosmon wants to run `git push --force origin main`\n\
                    Do you want to proceed?\n ❯ 1. Yes";
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::Unknown);
        assert!(!scan.class.auto_confirmable());
        assert!(scan.class.requires_alert());
    }

    #[test]
    fn rm_rf_is_never_auto_confirmed() {
        let pane = "Allow this command? `rm -rf /tmp/scratch`\n ❯ 1. Yes";
        assert!(!classify_pane(pane).class.auto_confirmable());
    }

    #[test]
    fn generic_yes_no_block_is_unknown() {
        let pane = "Overwrite existing config? [y/n]";
        let scan = classify_pane(pane);
        // "overwrite" is a risky marker -> Unknown, alert path.
        assert_eq!(scan.class, DialogueClass::Unknown);
        assert!(scan.class.requires_alert());
    }

    #[test]
    fn bare_enter_to_confirm_is_unknown_not_permission() {
        // No recognised safe shape, no money — a block we cannot safely act on.
        let pane = "Enter to confirm the selection above";
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::Unknown);
        assert!(!scan.class.auto_confirmable());
    }

    #[test]
    fn evidence_is_the_matching_line_trimmed() {
        let pane = "line one\n   Approaching spend limit now   \nline three";
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::MoneyStake);
        assert_eq!(
            scan.evidence.as_deref(),
            Some("Approaching spend limit now")
        );
    }

    #[test]
    fn evidence_is_truncated_when_huge() {
        let long = "spend limit ".to_owned() + &"x".repeat(500);
        let scan = classify_pane(&long);
        assert_eq!(scan.class, DialogueClass::MoneyStake);
        let ev = scan.evidence.unwrap();
        assert!(
            ev.chars().count() <= 161,
            "evidence not truncated: {}",
            ev.len()
        );
        assert!(ev.ends_with('…'));
    }

    #[test]
    fn class_str_roundtrips_are_stable() {
        assert_eq!(DialogueClass::None.as_str(), "none");
        assert_eq!(DialogueClass::Permission.as_str(), "permission");
        assert_eq!(DialogueClass::MoneyStake.as_str(), "money_stake");
        assert_eq!(DialogueClass::Unknown.as_str(), "unknown");
    }

    #[test]
    fn only_permission_is_auto_confirmable() {
        assert!(DialogueClass::Permission.auto_confirmable());
        assert!(!DialogueClass::None.auto_confirmable());
        assert!(!DialogueClass::MoneyStake.auto_confirmable());
        assert!(!DialogueClass::Unknown.auto_confirmable());
    }

    #[test]
    fn case_insensitive_matching() {
        let pane = "APPROACHING SPEND LIMIT";
        assert_eq!(classify_pane(pane).class, DialogueClass::MoneyStake);
    }
}
