// SPDX-License-Identifier: AGPL-3.0-only

//! Ordered model fallback chain for `claude` worker sessions.
//!
//! # Why this module exists
//!
//! A tenant worker used to be pinned to a *single* model id (the
//! `claude_model` key in `rpp.toml`, then defaulting to `claude-fable-5`). When the
//! instance's Claude account lost access to that model (a vendor-side model
//! retirement), the worker `claude` CLI did **not** exit on
//! `model_not_found` — it sat idle forever, a *false-active* worker that
//! the liveness probe could not distinguish from a slow-but-working one.
//! The transcript made the root cause visible: rows carrying
//! `model: "<synthetic>"`, `isApiErrorMessage`, and a *"model currently
//! unavailable"* refusal.
//!
//! The fix: never pin to one model. Probe an **ordered chain** —
//! preferred → fallbacks — and spawn the worker on the first model that
//! actually answers. If *none* answer, fail fast with a cause instead of
//! spawning a worker that would freeze.
//!
//! # Single source of model-id literals
//!
//! [`DEFAULT_MODEL_CHAIN`] is the **only** place model-id strings live.
//! [`crate::model_chain::PREFERRED_MODEL`] (the chain head) is what the
//! rpp-adapter uses as its default `claude_model` pin; the `cs tackle`
//! spawn path builds the probe order from this same chain. No second
//! copy of a default model id exists to drift out of sync at the next
//! model change — moving the fleet is a one-line edit of the slice
//! below (avoiding the classic bug: the copy nobody re-syncs).
//!
//! # Honouring the operator preference
//!
//! The chain is the *default*, not a straitjacket. An operator pin
//! (`claude_model` in `rpp.toml`, surfaced here as the `preferred`
//! argument to [`build_chain`]) is placed at the **head** of the probe
//! order, ahead of the built-in defaults. So the pinned model is
//! honoured first whenever the account has access to it; the fallbacks
//! only ever engage when the preferred model is genuinely unreachable.
//!
//! # Cost-aware fallback — silence never escalates to strong
//!
//! The original chain (`task-20260614-3116`) was **cost-inverted**: the
//! first fallback from the then cheap floor `claude-fable-5` was the
//! *strongest, most expensive* model `claude-opus-4-8`. A transient
//! outage of the floor — the exact false-active symptom the fallback exists for
//! — therefore silently escalated a worker to strong, expensive credits
//! with **no positive operator act**. That violates the unanimous
//! `delib-20260704-b476` invariant #3: *"strong is never inherited;
//! silence resolves to the weakest safe model."* (Diagnosed by
//! `task-20260705-1ad9`; fixed here by `task-20260705-ba98`.)
//!
//! Two changes close the leak, both preserving resilience for the
//! benign (cheap → cheap) case:
//!
//! 1. **Cost-ascending order.** [`DEFAULT_MODEL_CHAIN`] is now ordered
//!    cheap → mid → strong, so the *first* fallback is the next-cheapest
//!    model, never the strongest.
//! 2. **Strong-fallback exclusion.** [`build_chain`] never appends a
//!    *strong* model to the fallback tail unless the pin **itself** is
//!    strong (a positive per-molecule act). A cheap pin falls through
//!    only to cheaper-or-equal models; if none answer, the caller fails
//!    **closed** ([`NoModelAvailable`]) rather than silently spending on
//!    strong. cosmon knows the cost class of its *own* default chain
//!    intrinsically ([`DEFAULT_STRONG_MODELS`]); an operator's per-adapter
//!    `strong = [...]` set (b476 T1's cost-class annotation) is folded in
//!    on top via `extra_strong`.

use serde::Serialize;

/// The ordered fallback chain — the **single source** of model-id
/// literals, so no second copy can drift out of sync.
///
/// Ordered **cost-ascending**, tried head-to-tail: `claude-sonnet-5` (the
/// floor, and the preferred default) → `claude-opus-5-5` (strong). Each entry
/// is probed in order; the first that answers backs the worker — but a
/// *strong* entry is only ever reached as a fallback when the pin itself is
/// strong (see [`build_chain`] and [`DEFAULT_STRONG_MODELS`]). Moving the
/// fleet to a new model is a one-line diff of this slice and nothing else.
///
/// # How this chain was chosen (issue #81 point 2)
///
/// Until 2026-09-25 it read `claude-fable-5` → `claude-sonnet-4-6` →
/// `claude-opus-4-8`. Fable 5 then moved to usage credits bought separately
/// from the plan, so on an account without them the default stopped every
/// worker on a credits screen; the other two ids had been superseded. The
/// replacement follows the operator's model calibration rather than a
/// ranking of models:
///
/// - the everyday default is `claude-sonnet-5`, so it is the floor — the
///   model silence resolves to;
/// - the strong tier is `claude-opus-5-5`, which superseded `claude-opus-5`.
///   Strong is never inherited, so it sits last and is reachable only behind
///   a strong pin;
/// - no model that needs separately purchased credits is a default. The
///   calibration also names `claude-fable-5-1` as a strong-tier choice; it is
///   left out here because the Fable family is the one measured on credits,
///   and a default must run on the plan alone. An operator who has credits
///   pins it explicitly.
///
/// There is no cheaper-than-floor entry: a `claude-sonnet-5` pin that does
/// not answer fails closed rather than falling back to a model the
/// calibration does not name for everyday work.
pub const DEFAULT_MODEL_CHAIN: &[&str] = &["claude-sonnet-5", "claude-opus-5-5"];

/// The subset of [`DEFAULT_MODEL_CHAIN`] that is **strong** (expensive)
/// cost-class — cosmon's intrinsic knowledge of its *own* curated default
/// chain, so the fallback layer is safe out of the box with zero operator
/// config.
///
/// This is **not** an operator validity table and does **not** judge the
/// legality of arbitrary pinned ids (the backend does that — von-neumann's
/// verdict C, `delib-20260704-b476` T5). It is a cost-class annotation used
/// only to keep the fallback tail from silently escalating to a strong
/// model. An operator's per-adapter `[adapters.<name>].strong = [...]` set
/// is folded in on top (the `extra_strong` argument to [`build_chain`] /
/// [`decide_worker_model`]); an id in *neither* set is treated as
/// non-strong (cheap/safe), fail-open by construction.
pub const DEFAULT_STRONG_MODELS: &[&str] = &["claude-opus-5-5"];

/// Is `model` in the **strong** cost-class — either cosmon's intrinsic
/// [`DEFAULT_STRONG_MODELS`] or the operator-declared `extra_strong` set?
///
/// Fail-open: an empty/unknown id resolves to `false` (cheap/safe).
/// Matching is exact on the trimmed id (a model id is an opaque token).
fn is_strong_class(model: &str, extra_strong: &[String]) -> bool {
    let m = model.trim();
    !m.is_empty()
        && (DEFAULT_STRONG_MODELS.contains(&m)
            || crate::model_budget::is_strong_model(extra_strong, m))
}

/// The preferred default model — the head of [`DEFAULT_MODEL_CHAIN`].
///
/// Used by the rpp-adapter as the default `claude_model` pin when the
/// operator configures nothing, so the literal is not duplicated across
/// crates.
pub const PREFERRED_MODEL: &str = DEFAULT_MODEL_CHAIN[0];

/// Build the ordered chain of models to probe.
///
/// The operator preference (when `Some` and non-blank) is placed first;
/// the built-in [`DEFAULT_MODEL_CHAIN`] follows, with any duplicate of
/// the preference removed so a model is never probed twice.
///
/// **Cost-aware fallback (the load-bearing safety property).** A *strong*
/// default is appended to the fallback tail **only** when the pin itself
/// is strong. A cheap pin therefore falls through only to
/// cheaper-or-equal models — silence (a probe failure of the pin) can
/// never escalate to a strong, expensive model. The strong cost class is
/// cosmon's intrinsic [`DEFAULT_STRONG_MODELS`] union the operator's
/// `extra_strong` set (`delib-20260704-b476` T1). See
/// `is_strong_class`.
///
/// ```
/// use cosmon_core::model_chain::build_chain;
///
/// // The floor pin: the strong `claude-opus-5-5` is EXCLUDED from the
/// // fallback tail, so the chain is the floor alone.
/// assert_eq!(build_chain(Some("claude-sonnet-5"), &[]), vec!["claude-sonnet-5"]);
///
/// // A strong pin is a positive act: the whole chain is available
/// // (opus hoisted to the head, the floor behind it).
/// assert_eq!(
///     build_chain(Some("claude-opus-5-5"), &[]),
///     vec!["claude-opus-5-5", "claude-sonnet-5"],
/// );
///
/// // A novel cheap pin is prepended; its outage falls to the floor, and
/// // strong stays excluded from the tail.
/// assert_eq!(
///     build_chain(Some("claude-haiku-4-5"), &[]),
///     vec!["claude-haiku-4-5", "claude-sonnet-5"],
/// );
///
/// // An operator can widen the strong set: marking the floor strong drops
/// // it from a cheap pin's tail too, leaving only the pin itself.
/// assert_eq!(
///     build_chain(Some("claude-haiku-4-5"), &["claude-sonnet-5".to_owned()]),
///     vec!["claude-haiku-4-5"],
/// );
/// ```
#[must_use]
pub fn build_chain(preferred: Option<&str>, extra_strong: &[String]) -> Vec<String> {
    let mut chain: Vec<String> = Vec::with_capacity(DEFAULT_MODEL_CHAIN.len() + 1);
    let mut pin_is_strong = false;
    if let Some(p) = preferred {
        let p = p.trim();
        if !p.is_empty() {
            // The pin's cost class decides whether strong defaults may join
            // the tail. A strong pin is a positive per-molecule act already
            // honoured upstream (`strong_gate`), so its cost class is not a
            // surprise; a cheap pin must never fall through to strong.
            pin_is_strong = is_strong_class(p, extra_strong);
            chain.push(p.to_owned());
        }
    }
    for &m in DEFAULT_MODEL_CHAIN {
        if chain.iter().any(|c| c == m) {
            continue;
        }
        // Silence never escalates to strong: a strong default is a valid
        // fallback only when the pin itself is strong.
        if is_strong_class(m, extra_strong) && !pin_is_strong {
            continue;
        }
        chain.push(m.to_owned());
    }
    chain
}

/// The verdict of probing one model for availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The model answered — it can back a worker.
    Available,
    /// The model could not be used; the string carries the reason
    /// (e.g. `model_not_found`, `probe timed out`) for the audit trail.
    Unavailable(String),
}

/// One row of the probe audit trail — recorded for **every** model
/// tried, available or not, so the operator can see exactly why a given
/// model was chosen (or why the chain was exhausted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeRecord {
    /// The model id that was probed.
    pub model: String,
    /// `"available"` or `"unavailable"`.
    pub outcome: &'static str,
    /// Free-form detail (empty for the available case).
    pub detail: String,
}

/// The result of walking the chain: the chosen model (if any) plus the
/// full per-model audit trail. `chosen == None` means *no* model in the
/// chain answered — the caller must fail fast rather than spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelSelection {
    /// The first model that probed [`ProbeOutcome::Available`], or
    /// `None` when the whole chain was exhausted.
    pub chosen: Option<String>,
    /// The audit trail, one entry per model probed, in chain order.
    pub probes: Vec<ProbeRecord>,
}

/// Walk `chain` head-to-tail, returning the first model that probes
/// available together with the audit trail.
///
/// `probe` is injected so the selection logic is unit-testable without a
/// live `claude` binary; production callers pass a closure that actually
/// spawns the probe (see `cosmon-cli`'s `probe_claude_model`).
///
/// Probing **stops** at the first available model — later models are not
/// probed (their rows are absent from the trail), which is the whole
/// point: the preferred model, when reachable, costs exactly one probe.
pub fn select_available_model<P>(chain: &[String], mut probe: P) -> ModelSelection
where
    P: FnMut(&str) -> ProbeOutcome,
{
    let mut probes = Vec::with_capacity(chain.len());
    for model in chain {
        match probe(model) {
            ProbeOutcome::Available => {
                probes.push(ProbeRecord {
                    model: model.clone(),
                    outcome: "available",
                    detail: String::new(),
                });
                return ModelSelection {
                    chosen: Some(model.clone()),
                    probes,
                };
            }
            ProbeOutcome::Unavailable(reason) => {
                probes.push(ProbeRecord {
                    model: model.clone(),
                    outcome: "unavailable",
                    detail: reason,
                });
            }
        }
    }
    ModelSelection {
        chosen: None,
        probes,
    }
}

/// No model in the requested chain was reachable — the caller must fail
/// fast instead of spawning a worker that would freeze.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "no model in the fallback chain is available (probed {}): {}",
    .probed.len(),
    .probed.iter().map(|p| format!("{}={}", p.model, p.detail)).collect::<Vec<_>>().join(", ")
)]
pub struct NoModelAvailable {
    /// The audit trail of every model probed, all unavailable.
    pub probed: Vec<ProbeRecord>,
}

/// The decision taken for a worker's effective model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum DecidedModel {
    /// The operator opted out of pinning (`claude_model = ""`): the
    /// caller emits no `ANTHROPIC_MODEL`, the `claude` CLI uses its own
    /// default. No probing is performed — opting out is a deliberate
    /// escape hatch, honoured verbatim.
    OptOut,
    /// A model from the chain probed available and backs the worker.
    Selected {
        /// The chosen model id.
        model: String,
        /// The full selection audit trail.
        selection: ModelSelection,
    },
}

/// Decide a worker's effective model, honouring the operator opt-out and
/// failing fast when the whole chain is unreachable.
///
/// - `preferred == None` or blank → [`DecidedModel::OptOut`] (no
///   probing, no pin). This preserves the `claude_model = ""` opt-out
///   semantics exactly.
/// - `preferred == Some(pin)` → build the [cost-aware chain](build_chain)
///   (pin at the head, no strong fallback for a cheap pin), probe in
///   order, and return [`DecidedModel::Selected`] for the first available
///   model.
/// - a pin was requested but **no** model in the chain answered →
///   `Err(`[`NoModelAvailable`]`)` so the caller refuses to spawn. Because
///   a cheap pin's chain excludes strong models, exhaustion **fails
///   closed** rather than silently escalating to strong — the invariant.
///
/// `extra_strong` is the operator's per-adapter strong cost-class set
/// (`delib-20260704-b476` T1), folded into cosmon's intrinsic
/// [`DEFAULT_STRONG_MODELS`]; pass `&[]` when none is declared.
///
/// `probe` is injected for testability (see [`select_available_model`]).
///
/// # Errors
///
/// Returns [`NoModelAvailable`] when a pin was requested but every model
/// in the resulting chain probed [`ProbeOutcome::Unavailable`].
pub fn decide_worker_model<P>(
    preferred: Option<&str>,
    extra_strong: &[String],
    probe: P,
) -> Result<DecidedModel, NoModelAvailable>
where
    P: FnMut(&str) -> ProbeOutcome,
{
    // Opt-out: a blank or absent preference means the operator asked the
    // CLI to resolve its own model. We do not probe and do not pin.
    let pin = preferred.map(str::trim).filter(|p| !p.is_empty());
    let Some(pin) = pin else {
        return Ok(DecidedModel::OptOut);
    };

    let chain = build_chain(Some(pin), extra_strong);
    let selection = select_available_model(&chain, probe);
    match &selection.chosen {
        Some(model) => Ok(DecidedModel::Selected {
            model: model.clone(),
            selection,
        }),
        None => Err(NoModelAvailable {
            probed: selection.probes,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cheap pin that is not the floor — its outage is what exercises the
    /// cheap → floor fallback now that the default chain has no mid tier.
    const CHEAP_PIN: &str = "claude-haiku-4-5";
    const FLOOR: &str = "claude-sonnet-5";
    const STRONG: &str = "claude-opus-5-5";

    /// The chain head must equal the documented preferred model so the
    /// rpp-adapter default and the chain never drift apart.
    #[test]
    fn preferred_model_is_chain_head() {
        assert_eq!(PREFERRED_MODEL, DEFAULT_MODEL_CHAIN[0]);
        assert_eq!(PREFERRED_MODEL, FLOOR);
    }

    /// Issue #81 point 2: no default may be a model that runs on usage
    /// credits bought separately from the plan. Fable 5 moved to such
    /// credits; an account without them stops the worker on a credits
    /// screen with nobody attached. The floor is the everyday model.
    #[test]
    fn default_chain_floor_is_not_a_credits_only_model() {
        const CREDITS_ONLY: &[&str] = &["claude-fable-5"];
        assert_eq!(PREFERRED_MODEL, "claude-sonnet-5", "the floor");
        for m in DEFAULT_MODEL_CHAIN {
            assert!(
                !CREDITS_ONLY.contains(m),
                "`{m}` needs separately purchased credits and cannot be a default"
            );
        }
    }

    /// The default chain is ordered cost-ascending: floor → strong. The
    /// strong model is the *last* entry, never the first fallback (the cost
    /// inversion that `task-20260705-ba98` fixed).
    #[test]
    fn default_chain_is_cost_ascending() {
        assert_eq!(DEFAULT_MODEL_CHAIN, &[FLOOR, STRONG]);
        assert!(is_strong_class(STRONG, &[]), "opus is strong");
        assert!(!is_strong_class(FLOOR, &[]), "the floor is not strong");
    }

    /// Every strong default is in the chain — the strong set annotates the
    /// chain, it does not name models the chain never probes.
    #[test]
    fn strong_defaults_are_chain_members() {
        for m in DEFAULT_STRONG_MODELS {
            assert!(DEFAULT_MODEL_CHAIN.contains(m), "{m} not in the chain");
        }
    }

    /// A cheap pin's fallback tail EXCLUDES the strong model — silence
    /// never escalates to strong.
    #[test]
    fn build_chain_cheap_pin_excludes_strong_tail() {
        assert_eq!(build_chain(Some(CHEAP_PIN), &[]), vec![CHEAP_PIN, FLOOR]);
        assert_eq!(build_chain(Some(FLOOR), &[]), vec![FLOOR]);
    }

    /// A blank/absent preference yields the floor with the strong model
    /// excluded — never a silent strong default. (In practice
    /// [`decide_worker_model`] short-circuits blank to `OptOut` and never
    /// builds a chain; this pins the pure builder's own contract.)
    #[test]
    fn build_chain_blank_preference_excludes_strong() {
        assert_eq!(build_chain(Some("   "), &[]), vec![FLOOR]);
        assert_eq!(build_chain(None, &[]), vec![FLOOR]);
    }

    /// A strong pin is a positive act: the whole chain is available, opus
    /// hoisted to the head with the floor behind it.
    #[test]
    fn build_chain_strong_pin_keeps_full_chain() {
        assert_eq!(build_chain(Some(STRONG), &[]), vec![STRONG, FLOOR]);
    }

    /// An operator-declared strong id is dropped from a cheap pin's tail
    /// on top of the intrinsic set — here the floor is marked strong, so a
    /// cheap pin falls through to nothing but itself.
    #[test]
    fn build_chain_extra_strong_widens_exclusion() {
        assert_eq!(
            build_chain(Some(CHEAP_PIN), &[FLOOR.to_owned()]),
            vec![CHEAP_PIN],
        );
    }

    fn chain(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn select_first_available_costs_one_probe() {
        let mut calls = Vec::new();
        let sel = select_available_model(&chain(&["a", "b", "c"]), |m| {
            calls.push(m.to_owned());
            ProbeOutcome::Available
        });
        assert_eq!(sel.chosen.as_deref(), Some("a"));
        assert_eq!(calls, vec!["a"], "must stop at first available");
        assert_eq!(sel.probes.len(), 1);
        assert_eq!(sel.probes[0].outcome, "available");
    }

    /// The load-bearing gate: when the preferred model is unavailable,
    /// selection falls through to the next, and the trail records why.
    #[test]
    fn select_falls_through_to_next_available() {
        let sel = select_available_model(&chain(&["a", "b", "c"]), |m| {
            if m == "a" {
                ProbeOutcome::Unavailable("model_not_found".to_owned())
            } else {
                ProbeOutcome::Available
            }
        });
        assert_eq!(sel.chosen.as_deref(), Some("b"));
        // Two rows: the failed probe + the successful one.
        assert_eq!(sel.probes.len(), 2);
        assert_eq!(sel.probes[0].model, "a");
        assert_eq!(sel.probes[0].outcome, "unavailable");
        assert_eq!(sel.probes[0].detail, "model_not_found");
        assert_eq!(sel.probes[1].model, "b");
        assert_eq!(sel.probes[1].outcome, "available");
    }

    #[test]
    fn select_falls_through_two_levels() {
        let sel = select_available_model(&chain(&["a", "b", "c"]), |m| {
            if m == "c" {
                ProbeOutcome::Available
            } else {
                ProbeOutcome::Unavailable("unavailable".to_owned())
            }
        });
        assert_eq!(sel.chosen.as_deref(), Some("c"));
        assert_eq!(sel.probes.len(), 3);
    }

    /// The fail-fast signal: no model answers → `chosen == None`, full
    /// trail retained so the caller can name the cause.
    #[test]
    fn select_none_available_yields_no_choice_with_full_trail() {
        let sel = select_available_model(&chain(&["a", "b", "c"]), |_| {
            ProbeOutcome::Unavailable("down".to_owned())
        });
        assert!(sel.chosen.is_none());
        assert_eq!(sel.probes.len(), 3);
        assert!(sel.probes.iter().all(|p| p.outcome == "unavailable"));
    }

    #[test]
    fn decide_opt_out_when_preference_blank() {
        let d = decide_worker_model(Some(""), &[], |_| ProbeOutcome::Available).unwrap();
        assert_eq!(d, DecidedModel::OptOut);
        let d = decide_worker_model(None, &[], |_| ProbeOutcome::Available).unwrap();
        assert_eq!(d, DecidedModel::OptOut);
    }

    #[test]
    fn decide_opt_out_never_probes() {
        let mut probed = false;
        let _ = decide_worker_model(None, &[], |_| {
            probed = true;
            ProbeOutcome::Available
        });
        assert!(!probed, "opt-out must not spawn a probe");
    }

    #[test]
    fn decide_selects_preferred_when_available() {
        let d = decide_worker_model(Some(FLOOR), &[], |_| ProbeOutcome::Available).unwrap();
        match d {
            DecidedModel::Selected { model, .. } => assert_eq!(model, FLOOR),
            DecidedModel::OptOut => panic!("expected a selection"),
        }
    }

    /// Gate: a cheap pin forced unavailable → worker gets the floor, NEVER
    /// the strong model, and the choice is observable.
    #[test]
    fn decide_falls_back_to_floor_not_strong() {
        let d = decide_worker_model(Some(CHEAP_PIN), &[], |m| {
            if m == CHEAP_PIN {
                ProbeOutcome::Unavailable("model currently unavailable".to_owned())
            } else {
                ProbeOutcome::Available
            }
        })
        .unwrap();
        match d {
            DecidedModel::Selected { model, selection } => {
                assert_eq!(model, FLOOR, "the floor, not strong");
                // Observability: the pin's failure is recorded with cause.
                assert_eq!(selection.probes[0].model, CHEAP_PIN);
                assert_eq!(selection.probes[0].outcome, "unavailable");
                assert!(selection.probes[0].detail.contains("unavailable"));
                assert!(
                    !selection.probes.iter().any(|p| p.model == STRONG),
                    "the strong model is not in a cheap pin's probe trail"
                );
            }
            DecidedModel::OptOut => panic!("expected a fallback selection"),
        }
    }

    /// A *strong* pin is a positive act — opus is honoured when available.
    #[test]
    fn decide_honours_strong_pin_when_available() {
        let d = decide_worker_model(Some(STRONG), &[], |_| ProbeOutcome::Available).unwrap();
        match d {
            DecidedModel::Selected { model, .. } => assert_eq!(model, STRONG),
            DecidedModel::OptOut => panic!("expected a strong selection"),
        }
    }

    /// A strong pin that is down downgrades to the floor — a downgrade is
    /// always safe; only silent *escalation* is the bug.
    #[test]
    fn decide_strong_pin_down_downgrades_to_floor() {
        let d = decide_worker_model(Some(STRONG), &[], |m| {
            if m == STRONG {
                ProbeOutcome::Unavailable("opus down".to_owned())
            } else {
                ProbeOutcome::Available
            }
        })
        .unwrap();
        match d {
            DecidedModel::Selected { model, .. } => assert_eq!(model, FLOOR),
            DecidedModel::OptOut => panic!("expected a downgrade selection"),
        }
    }

    /// Gate: a cheap pin whose entire cheap tail is down → fail **closed**
    /// with cause, and the strong model is NEVER probed or selected. This
    /// is the teeth of invariant #3: silence resolves to refusal, never to
    /// strong.
    #[test]
    fn decide_cheap_pin_fails_closed_never_reaches_strong() {
        let mut probed = Vec::new();
        let err = decide_worker_model(Some(CHEAP_PIN), &[], |m| {
            probed.push(m.to_owned());
            ProbeOutcome::Unavailable("403 model_not_found".to_owned())
        })
        .unwrap_err();
        assert_eq!(probed, vec![CHEAP_PIN, FLOOR], "only the cheap tail");
        assert_eq!(err.probed.len(), 2);
        let msg = err.to_string();
        assert!(msg.contains("no model in the fallback chain is available"));
        assert!(msg.contains(CHEAP_PIN));
        assert!(msg.contains("model_not_found"));
    }

    /// The floor pin that does not answer fails closed at once: the floor
    /// has no cheaper fallback and strong is never reached from it.
    #[test]
    fn decide_floor_pin_down_fails_closed() {
        let err = decide_worker_model(Some(FLOOR), &[], |_| {
            ProbeOutcome::Unavailable("You don't have usage credits yet.".to_owned())
        })
        .unwrap_err();
        assert_eq!(err.probed.len(), 1);
        assert_eq!(err.probed[0].model, FLOOR);
    }

    /// GUARDS AGAINST — silent-fallback model-routing pathology
    /// (diagnosed by `task-20260705-1ad9`; fixed by `task-20260705-ba98`).
    ///
    /// The probe-fallback layer (shipped by `task-20260614-3116`) once
    /// silently escalated a **cheap** pin to the **strongest, most
    /// expensive** model in the fleet, violating `delib-20260704-b476`'s
    /// unanimous invariant #3, *"strong is never inherited; silence
    /// resolves to the weakest safe model."* The same scenario must resolve
    /// to the floor, never to strong:
    ///
    ///   * the operator pins a cheap model;
    ///   * it momentarily probes [`ProbeOutcome::Unavailable`] (account
    ///     outage / rate limit / `model_not_found` — the exact
    ///     false-active symptom the fallback was built for);
    ///   * [`decide_worker_model`] walks the cost-aware chain, whose tail
    ///     for a cheap pin is the floor alone (opus excluded).
    #[test]
    fn silent_fallback_guards_against_cheap_pin_escalating_to_strong() {
        assert_eq!(
            DEFAULT_MODEL_CHAIN.last(),
            Some(&STRONG),
            "the strong model is the last entry, only reachable behind a strong pin"
        );

        let decided = decide_worker_model(Some(CHEAP_PIN), &[], |m| {
            if m == CHEAP_PIN {
                ProbeOutcome::Unavailable("model currently unavailable".to_owned())
            } else {
                ProbeOutcome::Available
            }
        })
        .expect("the floor answers, so the chain does not fail fast");

        match decided {
            DecidedModel::Selected { model, selection } => {
                assert_eq!(model, FLOOR, "cheap pin falls through to the floor");
                assert!(
                    !selection.probes.iter().any(|p| p.model == STRONG),
                    "strong is not in a cheap pin's probe trail at all"
                );
            }
            DecidedModel::OptOut => panic!("expected a cost-aware fallback selection"),
        }
    }

    /// The selection trail must serialise (it is persisted to the
    /// molecule state dir for operator observability).
    #[test]
    fn selection_serialises_to_json() {
        let sel = ModelSelection {
            chosen: Some(FLOOR.to_owned()),
            probes: vec![ProbeRecord {
                model: CHEAP_PIN.to_owned(),
                outcome: "unavailable",
                detail: "model_not_found".to_owned(),
            }],
        };
        let json = serde_json::to_string(&sel).unwrap();
        assert!(json.contains("\"chosen\":\"claude-sonnet-5\""));
        assert!(json.contains("\"model_not_found\""));
    }
}
