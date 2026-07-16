// SPDX-License-Identifier: AGPL-3.0-only

//! Strong-model cost-class + fail-closed per-galaxy dispatch ceiling
//! (delib-20260704-b476 / C4).
//!
//! # Why this module exists
//!
//! C1 gave `cs tackle` a per-molecule model pin; C2 promoted the choice to a
//! typed `ModelSelected` event on the wire. This module is the **safety
//! property** the panel converged on (carnot Q3, kahneman Q5): bound the
//! credit burn and make "a strong model leaks silently" structurally
//! impossible.
//!
//! Two mechanisms, both pure and I/O-free (the seam is the `cs tackle` shell
//! that reads the config + folds `events.jsonl`):
//!
//! 1. **Strong cost-class annotation** ([`is_strong_model`]). An
//!    operator-declared, **fail-open** set of model ids per adapter
//!    (`[adapters.<name>].strong = ["<id>", …]`). This is a *cost-class*
//!    annotation, **not** a validity table — an unlisted id is treated as
//!    non-strong (cheap/safe) by default, and the id itself is still carried
//!    opaquely for legality (the backend judges legality, von-neumann's
//!    verdict C). This threads tension T1 (opaque ids vs the strong-guard):
//!    cosmon knows nothing about model *legality* but carries a thin
//!    cost-class used only for the ceiling, the `⚡strong` glyph, and the
//!    config-default guard.
//!
//! 2. **Fail-closed ceiling** ([`strong_gate`]). A per-galaxy
//!    count-of-strong-dispatches cap `K` per rolling window. On the (K+1)th
//!    strong pin, `cs tackle` **refuses to spawn strong** — downgrade to the
//!    safe floor (`None`) or abort, per [`OverflowPolicy`]. A soft warning is
//!    NOT a ceiling. The running total is re-derived as a **fold over
//!    `events.jsonl`** ([`count_strong_in_window`]) — the `cs reconcile`
//!    idiom, crash-safe and idempotent; never a mutable counter file.
//!
//! # Carnot's bound (why the ceiling is the load-bearing property)
//!
//! Let `k = C_strong / C_economical` (strong drains ~k× faster). Under the
//! `/model` leak, worst-case burn over `N` dispatches is `N·k` — unbounded in
//! `N` and invisible. Under manual-pin **with a ceiling `K`**, worst-case is
//! `K·k` — **constant, independent of `N` and of routing-decision quality.**
//! Only the ceiling moves the worst case; auto-vs-manual only moves the
//! average. So the ceiling — not the router — is the safety property.
//!
//! # Kahneman's safe-default invariant
//!
//! Silence resolves to the weakest safe model; **strong requires a positive
//! per-molecule act** ([`source_is_positive_act`] — `--model` flag or a
//! formula-step pin). A *strong* model reached from a config/env *default* is a
//! safe-default violation ([`StrongGate::Downgrade`] with
//! [`DowngradeReason::NonPositiveSource`], and — for config `default_model` —
//! [`config_default_is_strong`] fails `cs reconcile --check`, Ghost A).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::event_v2::ModelSelectionSource;

/// Is `model` in the operator-declared **strong** cost-class set?
///
/// **Fail-open by construction:** an empty set, or an id absent from the set,
/// resolves to `false` (cheap/safe). This is deliberate — the annotation is a
/// cost-class hint, never a validity table, so an unlisted id must never be
/// treated as an error (it is simply "not known to be expensive"). Matching is
/// exact on the trimmed id (a model id is an opaque token; cosmon does not
/// normalise vendor spellings).
#[must_use]
pub fn is_strong_model(strong_set: &[String], model: &str) -> bool {
    let m = model.trim();
    !m.is_empty() && strong_set.iter().any(|s| s.trim() == m)
}

/// Is this resolution source a **positive per-molecule act** — the only kind
/// of source allowed to reach a *strong* model?
///
/// [`ModelSelectionSource::Flag`] (`cs tackle --model <id>`) and
/// [`ModelSelectionSource::FormulaPin`] (a formula step's `model = "<id>"`)
/// are positive acts: the operator/driver chose *this* model for *this*
/// molecule. Every other arm — env var, per-galaxy config, global config, the
/// floor — is a *default*, and a default may never resolve to strong
/// (kahneman's Ghost A/B). Strong reached from a default is downgraded to the
/// safe floor by [`strong_gate`].
#[must_use]
pub fn source_is_positive_act(source: &ModelSelectionSource) -> bool {
    matches!(
        source,
        ModelSelectionSource::Flag { .. } | ModelSelectionSource::FormulaPin { .. }
    )
}

/// Ghost A (`cs reconcile --check`): does a config `default_model` resolve to a
/// **strong** id?
///
/// A config that defaults the base model to a strong id *is* the original
/// sticky-`/model` bug wearing a config costume — it would silently dispatch
/// strong with zero per-molecule intent. Config may only *downgrade* (pin a
/// non-strong model); strong is reachable only from a positive per-molecule
/// act. `cs reconcile --check` folds this predicate over every
/// `[adapters.<name>]` entry and fails when any returns `true`.
///
/// `None` (no `default_model` declared) is never a violation — the floor is
/// `None`, and silence never resolves to strong.
#[must_use]
pub fn config_default_is_strong(default_model: Option<&str>, strong_set: &[String]) -> bool {
    default_model.is_some_and(|m| is_strong_model(strong_set, m))
}

/// What `cs tackle` does when the per-galaxy strong-dispatch ceiling is
/// reached (the (K+1)th strong pin in the window).
///
/// The default is [`OverflowPolicy::Downgrade`] — keep the fleet moving on the
/// economical model rather than block a dispatch outright. An operator who
/// would rather a hard stop declares `on_overflow = "abort"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverflowPolicy {
    /// Drop the strong pin to the safe floor (`None`) and keep spawning on the
    /// adapter's own (economical) default. The burst continues; the credit
    /// burn is bounded.
    #[default]
    Downgrade,
    /// Refuse the spawn entirely — `cs tackle` returns an error. Use when a
    /// strong dispatch over budget should stop and wait for an operator
    /// decision rather than silently downgrade.
    Abort,
}

impl OverflowPolicy {
    /// The stable wire/label token (`"downgrade"` / `"abort"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Downgrade => "downgrade",
            Self::Abort => "abort",
        }
    }
}

/// Why a strong model was dropped to the safe floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DowngradeReason {
    /// A *strong* model was reached from a config/env *default*, not a positive
    /// per-molecule act (Ghost A/B). Strong is reachable only from `--model` or
    /// a formula-step pin, so the default is dropped to `None`.
    NonPositiveSource,
    /// The per-galaxy strong-dispatch ceiling was already at `cap` in the
    /// window, so this (positive-act) strong pin is dropped to `None`.
    CeilingReached {
        /// Strong dispatches already counted in the window (`>= cap`).
        strong_count: u32,
        /// The configured cap `K`.
        cap: u32,
    },
    /// A ceiling is configured but the local strong-dispatch history was
    /// **unreadable**, so the in-window count is unknown. A budget ceiling must
    /// **fail closed** on unknown history — it drops to the floor rather than
    /// assume zero used (the `unreadable → empty` bug this closes, C3 of
    /// `delib-20260711-c6c8`).
    HistoryUnavailable {
        /// The configured cap `K` (the count is unknown, hence not carried).
        cap: u32,
    },
}

/// The result of folding the local `events.jsonl` for the in-window
/// strong-dispatch count — kept distinct so the ceiling can distinguish a
/// **trustworthy zero** from an **unknown** count.
///
/// A fresh galaxy whose log is genuinely absent folds to `Counted(0)`: a real
/// zero the ceiling may trust. A log that *exists but cannot be read/parsed*
/// folds to [`Self::Unavailable`]: the count is unknown, and a budget gate must
/// fail closed rather than treat it as `Counted(0)` (which would *open* the gate
/// exactly when the evidence is missing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalHistory {
    /// The log was read (or genuinely absent); this is the in-window count.
    Counted(u32),
    /// The log exists but could not be read/parsed — the count is unknown.
    Unavailable,
}

/// The verdict of the whole strong-dispatch gate for one `cs tackle`.
///
/// Produced by [`strong_gate`] from four inputs: whether the resolved model is
/// strong, where it came from, the in-window strong count, and the (optional)
/// cap + policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrongGate {
    /// The model is not strong (or nothing was pinned) — spawn verbatim. No
    /// counting, no guard: the cheap, byte-identical-to-today common path.
    NotStrong,
    /// Strong, reached from a positive act, and under the ceiling — spawn
    /// strong.
    AllowStrong,
    /// Strong must be dropped to the safe floor (`None`); the model pin is
    /// discarded and the adapter's own default applies.
    Downgrade {
        /// Whether the downgrade was the safe-default guard or the ceiling.
        reason: DowngradeReason,
    },
    /// Strong overflowed the ceiling and [`OverflowPolicy::Abort`] is in force
    /// — refuse to spawn.
    Abort {
        /// Strong dispatches already counted in the window (`>= cap`).
        strong_count: u32,
        /// The configured cap `K`.
        cap: u32,
    },
    /// A ceiling is configured, [`OverflowPolicy::Abort`] is in force, and the
    /// local strong-dispatch history was **unreadable** — refuse to spawn rather
    /// than route on an unknown count (fail closed).
    AbortHistoryUnavailable {
        /// The configured cap `K`.
        cap: u32,
    },
}

/// Decide the fate of a resolved model pin against the safe-default guard and
/// the fail-closed ceiling.
///
/// Pure and total — the caller supplies:
/// - `is_strong`: whether the resolved model is in its adapter's strong set
///   ([`is_strong_model`]); `false` when nothing was pinned (the floor);
/// - `source`: where the pin came from ([`ModelSelectionSource`]);
/// - `strong_count_in_window`: the current in-window strong-dispatch count
///   ([`count_strong_in_window`], a fold over `events.jsonl`);
/// - `cap`: the per-galaxy cap `K`, or `None` when no `[model_budget]` is
///   configured (the ceiling is opt-in per galaxy — absent config is unbounded
///   and byte-identical to today);
/// - `policy`: what to do on overflow ([`OverflowPolicy`]).
///
/// Decision order (each gate is stricter than the last):
/// 1. not strong → [`StrongGate::NotStrong`] (no counting, no guard);
/// 2. strong from a non-positive source → [`StrongGate::Downgrade`] with
///    [`DowngradeReason::NonPositiveSource`] (Ghost A/B safe-default guard);
/// 3. strong from a positive act, no cap → [`StrongGate::AllowStrong`];
/// 4. strong from a positive act, under cap → [`StrongGate::AllowStrong`];
/// 5. strong from a positive act, at/over cap → downgrade or abort per policy.
#[must_use]
pub fn strong_gate(
    is_strong: bool,
    source: &ModelSelectionSource,
    strong_count_in_window: u32,
    cap: Option<u32>,
    policy: OverflowPolicy,
) -> StrongGate {
    if !is_strong {
        return StrongGate::NotStrong;
    }
    // Ghost A/B: a strong model reached from anything but a positive
    // per-molecule act is a safe-default violation — drop to the floor.
    if !source_is_positive_act(source) {
        return StrongGate::Downgrade {
            reason: DowngradeReason::NonPositiveSource,
        };
    }
    // The fail-closed ceiling. No cap configured → unbounded (opt-in per
    // galaxy); a strong pin from a positive act is honoured.
    match cap {
        None => StrongGate::AllowStrong,
        Some(k) if strong_count_in_window < k => StrongGate::AllowStrong,
        Some(k) => match policy {
            OverflowPolicy::Downgrade => StrongGate::Downgrade {
                reason: DowngradeReason::CeilingReached {
                    strong_count: strong_count_in_window,
                    cap: k,
                },
            },
            OverflowPolicy::Abort => StrongGate::Abort {
                strong_count: strong_count_in_window,
                cap: k,
            },
        },
    }
}

/// The fail-closed sibling of [`strong_gate`]: decide a strong pin against a
/// [`LocalHistory`] fold that may be **unavailable**.
///
/// Identical to [`strong_gate`] on every path where the count is known
/// ([`LocalHistory::Counted`]). The one behavioural difference is
/// [`LocalHistory::Unavailable`] *with a ceiling configured*: the count is
/// unknown, so the gate fails closed — downgrade to the floor
/// ([`DowngradeReason::HistoryUnavailable`]) or abort
/// ([`StrongGate::AbortHistoryUnavailable`]) per policy, **never** treat unknown
/// history as `Counted(0)`. With **no** ceiling configured (`cap == None`) the
/// history is irrelevant (the burst is unbounded either way) and a positive-act
/// strong pin is honoured, byte-identical to today.
#[must_use]
pub fn strong_gate_with_history(
    is_strong: bool,
    source: &ModelSelectionSource,
    history: LocalHistory,
    cap: Option<u32>,
    policy: OverflowPolicy,
) -> StrongGate {
    if !is_strong {
        return StrongGate::NotStrong;
    }
    if !source_is_positive_act(source) {
        return StrongGate::Downgrade {
            reason: DowngradeReason::NonPositiveSource,
        };
    }
    match (cap, history) {
        // No ceiling → unbounded; unknown history is irrelevant.
        (None, _) => StrongGate::AllowStrong,
        // Known count → identical to the classic gate.
        (Some(_), LocalHistory::Counted(n)) => strong_gate(is_strong, source, n, cap, policy),
        // Ceiling configured but count unknown → FAIL CLOSED.
        (Some(k), LocalHistory::Unavailable) => match policy {
            OverflowPolicy::Downgrade => StrongGate::Downgrade {
                reason: DowngradeReason::HistoryUnavailable { cap: k },
            },
            OverflowPolicy::Abort => StrongGate::AbortHistoryUnavailable { cap: k },
        },
    }
}

/// One past strong-eligible dispatch, distilled from a `ModelSelected` event
/// for the ceiling fold. Only events that pinned a concrete model (`model:
/// Some`) become a record — the `None` floor is never strong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRecord {
    /// Adapter the model id is scoped to (a model id is only strong *within*
    /// its adapter's `strong` set).
    pub adapter_name: String,
    /// The pinned model id.
    pub model: String,
    /// When the selection happened (the event's `selected_at`).
    pub selected_at: DateTime<Utc>,
}

/// Count strong dispatches inside the rolling window `[now - window, now]`.
///
/// The `cs reconcile` idiom: a **pure fold over the event log**, never a
/// mutable counter file (crash-safe, idempotent, race-free across parallel
/// workers). `is_strong` classifies each record's `(adapter, model)` against
/// that adapter's strong set — records span adapters, so the classifier is
/// passed the adapter name, not a single flat set.
///
/// The window is half-open at neither end here (inclusive both sides): a
/// dispatch exactly `window` ago still counts, which is the conservative
/// choice for a *ceiling* (over-count rather than under-count near the edge).
#[must_use]
pub fn count_strong_in_window<F>(
    records: &[DispatchRecord],
    now: DateTime<Utc>,
    window: Duration,
    is_strong: F,
) -> u32
where
    F: Fn(&str, &str) -> bool,
{
    let floor = now - window;
    records
        .iter()
        .filter(|r| r.selected_at >= floor && r.selected_at <= now)
        .filter(|r| is_strong(&r.adapter_name, &r.model))
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn strong_set_is_fail_open() {
        // Empty set → nothing is strong.
        assert!(!is_strong_model(&[], "claude-opus-4-8"));
        // Unlisted id → not strong (fail-open, not an error).
        assert!(!is_strong_model(
            &set(&["claude-fable-5"]),
            "claude-sonnet-4-6"
        ));
        // Listed id → strong.
        assert!(is_strong_model(&set(&["claude-fable-5"]), "claude-fable-5"));
        // Whitespace is trimmed on both sides.
        assert!(is_strong_model(
            &set(&[" claude-fable-5 "]),
            "claude-fable-5"
        ));
        // Blank id is never strong.
        assert!(!is_strong_model(&set(&["claude-fable-5"]), "   "));
    }

    #[test]
    fn only_flag_and_formula_pin_are_positive_acts() {
        assert!(source_is_positive_act(&ModelSelectionSource::Flag {
            flag: "x".to_owned()
        }));
        assert!(source_is_positive_act(&ModelSelectionSource::FormulaPin {
            formula: "f".to_owned(),
            step_id: "s".to_owned(),
        }));
        // Every default source is NOT a positive act.
        assert!(!source_is_positive_act(&ModelSelectionSource::EnvVar {
            var: "COSMON_DEFAULT_MODEL".to_owned()
        }));
        assert!(!source_is_positive_act(&ModelSelectionSource::Config {
            path: "p".to_owned(),
            key: "k".to_owned(),
        }));
        assert!(!source_is_positive_act(
            &ModelSelectionSource::GlobalConfig {
                path: "p".to_owned()
            }
        ));
        assert!(!source_is_positive_act(&ModelSelectionSource::Default {
            fallback_reason: "floor".to_owned()
        }));
    }

    #[test]
    fn config_default_strong_is_ghost_a_violation() {
        let strong = set(&["claude-fable-5"]);
        assert!(config_default_is_strong(Some("claude-fable-5"), &strong));
        // A non-strong config default is fine (config may downgrade).
        assert!(!config_default_is_strong(
            Some("claude-sonnet-4-6"),
            &strong
        ));
        // No default declared → never a violation (the floor is None).
        assert!(!config_default_is_strong(None, &strong));
        // Fail-open: empty strong set means no config default is strong.
        assert!(!config_default_is_strong(Some("claude-fable-5"), &[]));
    }

    #[test]
    fn gate_non_strong_is_the_cheap_path() {
        let g = strong_gate(
            false,
            &ModelSelectionSource::Config {
                path: "p".to_owned(),
                key: "k".to_owned(),
            },
            999,
            Some(1),
            OverflowPolicy::Abort,
        );
        assert_eq!(g, StrongGate::NotStrong, "non-strong is never gated");
    }

    #[test]
    fn gate_downgrades_strong_from_a_default_source() {
        // Ghost A/B: strong reached from a config default → dropped to floor,
        // regardless of the ceiling.
        let g = strong_gate(
            true,
            &ModelSelectionSource::Config {
                path: "p".to_owned(),
                key: "k".to_owned(),
            },
            0,
            None,
            OverflowPolicy::Abort,
        );
        assert_eq!(
            g,
            StrongGate::Downgrade {
                reason: DowngradeReason::NonPositiveSource
            }
        );
    }

    #[test]
    fn gate_allows_strong_from_a_positive_act_under_the_cap() {
        let flag = ModelSelectionSource::Flag {
            flag: "claude-fable-5".to_owned(),
        };
        // No cap → unbounded, allowed.
        assert_eq!(
            strong_gate(true, &flag, 100, None, OverflowPolicy::Downgrade),
            StrongGate::AllowStrong
        );
        // Under the cap → allowed.
        assert_eq!(
            strong_gate(true, &flag, 2, Some(3), OverflowPolicy::Downgrade),
            StrongGate::AllowStrong
        );
    }

    #[test]
    fn gate_fails_closed_at_the_cap() {
        let flag = ModelSelectionSource::Flag {
            flag: "claude-fable-5".to_owned(),
        };
        // At the cap, downgrade policy → drop to floor with the ceiling reason.
        assert_eq!(
            strong_gate(true, &flag, 3, Some(3), OverflowPolicy::Downgrade),
            StrongGate::Downgrade {
                reason: DowngradeReason::CeilingReached {
                    strong_count: 3,
                    cap: 3
                }
            }
        );
        // At the cap, abort policy → refuse.
        assert_eq!(
            strong_gate(true, &flag, 3, Some(3), OverflowPolicy::Abort),
            StrongGate::Abort {
                strong_count: 3,
                cap: 3
            }
        );
        // Over the cap fails closed too.
        assert_eq!(
            strong_gate(true, &flag, 9, Some(3), OverflowPolicy::Abort),
            StrongGate::Abort {
                strong_count: 9,
                cap: 3
            }
        );
    }

    #[test]
    fn cap_zero_refuses_every_strong_dispatch() {
        // K=0 is a legitimate "no strong at all" policy: the first strong pin
        // is already at the cap.
        let flag = ModelSelectionSource::Flag {
            flag: "s".to_owned(),
        };
        assert_eq!(
            strong_gate(true, &flag, 0, Some(0), OverflowPolicy::Abort),
            StrongGate::Abort {
                strong_count: 0,
                cap: 0
            }
        );
    }

    fn rec(adapter: &str, model: &str, ago_hours: i64, now: DateTime<Utc>) -> DispatchRecord {
        DispatchRecord {
            adapter_name: adapter.to_owned(),
            model: model.to_owned(),
            selected_at: now - Duration::hours(ago_hours),
        }
    }

    #[test]
    fn window_fold_counts_only_strong_and_only_in_window() {
        let now = DateTime::parse_from_rfc3339("2026-07-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let strong = set(&["claude-fable-5"]);
        let is_strong = |_adapter: &str, model: &str| is_strong_model(&strong, model);
        let records = vec![
            rec("claude", "claude-fable-5", 1, now),  // strong, in window
            rec("claude", "claude-fable-5", 23, now), // strong, in window
            rec("claude", "claude-fable-5", 30, now), // strong, OUT of window
            rec("claude", "claude-sonnet-4-6", 2, now), // in window but not strong
        ];
        assert_eq!(
            count_strong_in_window(&records, now, Duration::hours(24), is_strong),
            2
        );
    }

    #[test]
    fn history_gate_matches_classic_gate_when_count_known() {
        let flag = ModelSelectionSource::Flag {
            flag: "s".to_owned(),
        };
        // Under the cap: allowed either way.
        assert_eq!(
            strong_gate_with_history(
                true,
                &flag,
                LocalHistory::Counted(2),
                Some(3),
                OverflowPolicy::Downgrade
            ),
            strong_gate(true, &flag, 2, Some(3), OverflowPolicy::Downgrade),
        );
        // At the cap: downgrades either way.
        assert_eq!(
            strong_gate_with_history(
                true,
                &flag,
                LocalHistory::Counted(3),
                Some(3),
                OverflowPolicy::Downgrade
            ),
            strong_gate(true, &flag, 3, Some(3), OverflowPolicy::Downgrade),
        );
    }

    #[test]
    fn history_gate_fails_closed_when_unreadable_under_a_ceiling() {
        let flag = ModelSelectionSource::Flag {
            flag: "s".to_owned(),
        };
        // Downgrade policy → drop to floor with the HistoryUnavailable reason
        // (NOT treated as Counted(0), which would allow the spawn).
        assert_eq!(
            strong_gate_with_history(
                true,
                &flag,
                LocalHistory::Unavailable,
                Some(3),
                OverflowPolicy::Downgrade
            ),
            StrongGate::Downgrade {
                reason: DowngradeReason::HistoryUnavailable { cap: 3 }
            }
        );
        // Abort policy → refuse.
        assert_eq!(
            strong_gate_with_history(
                true,
                &flag,
                LocalHistory::Unavailable,
                Some(3),
                OverflowPolicy::Abort
            ),
            StrongGate::AbortHistoryUnavailable { cap: 3 }
        );
    }

    #[test]
    fn history_gate_ignores_unreadable_when_no_ceiling() {
        // With no cap the ceiling is opt-out; unknown history must not block a
        // positive-act strong pin (byte-identical to today).
        let flag = ModelSelectionSource::Flag {
            flag: "s".to_owned(),
        };
        assert_eq!(
            strong_gate_with_history(
                true,
                &flag,
                LocalHistory::Unavailable,
                None,
                OverflowPolicy::Abort
            ),
            StrongGate::AllowStrong
        );
    }

    #[test]
    fn history_gate_still_guards_non_positive_source() {
        // A strong default is dropped before history is even consulted.
        let cfg = ModelSelectionSource::Config {
            path: "p".to_owned(),
            key: "k".to_owned(),
        };
        assert_eq!(
            strong_gate_with_history(
                true,
                &cfg,
                LocalHistory::Unavailable,
                Some(3),
                OverflowPolicy::Abort
            ),
            StrongGate::Downgrade {
                reason: DowngradeReason::NonPositiveSource
            }
        );
    }

    #[test]
    fn window_fold_classifies_per_adapter() {
        let now = DateTime::parse_from_rfc3339("2026-07-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // "gpt-strong" is strong for openai but not for claude.
        let is_strong = |adapter: &str, model: &str| match adapter {
            "openai" => model == "gpt-strong",
            "claude" => model == "claude-fable-5",
            _ => false,
        };
        let records = vec![
            rec("openai", "gpt-strong", 1, now),     // strong (openai)
            rec("claude", "gpt-strong", 1, now),     // NOT strong for claude
            rec("claude", "claude-fable-5", 1, now), // strong (claude)
        ];
        assert_eq!(
            count_strong_in_window(&records, now, Duration::hours(24), is_strong),
            2
        );
    }
}
