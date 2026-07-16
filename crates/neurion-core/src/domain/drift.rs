// SPDX-License-Identifier: Apache-2.0

//! Drift detection — thresholds for the five broken-promise signals.
//!
//! Wheeler: a family is a direction of bit-flux across the boundary.
//! godin: every promise has an unsubscribe event.
//! carnot: a drift is a boundary-condition violation.
//!
//! Concretely, each family has an irreversibility that earns it its
//! identity; when the metric a family is promising falls below the
//! threshold, the boundary is broken and the galaxy has drifted into
//! the wrong family. This module exposes the five threshold tests as
//! pure functions over synthetic-data inputs so formulas can be
//! exercised in unit tests without touching neurion or the filesystem.
//!
//! The five drifts:
//!
//! 1. [`hub_to_project_drift`] — hub whose code commits outnumber its
//!    conversational artefacts over a trailing 14-day window.
//! 2. [`editorial_to_introspection_drift`] — editorial galaxy with ≥ 30
//!    consecutive days of zero artefact crossing to a named external
//!    audience.
//! 3. [`infra_to_imposition_drift`] — infra galaxy with sister-filed
//!    `infra-coupling` issues (1/qtr warning, 2+ verdict).
//! 4. [`vanity_family_drift`] — galaxy whose success metric reads 0 for
//!    three consecutive reporting periods while a neighbor family's
//!    metric would be non-zero on the same repo.
//! 5. [`project_to_frozen_drift`] — project with exergy → 0 AND flux →
//!    0 AND artefacts still consulted (tag `status:frozen`, do not
//!    delete).
//!
//! None of these helpers *act* — they return a [`DriftVerdict`] that
//! the formula turns into an `issue` molecule via `cs nucleate`. This
//! keeps the Rust core side-effect free and the patrol a pure
//! projection over metrics.

use serde::{Deserialize, Serialize};

use super::galaxy_kind::GalaxyKind;

/// Outcome of a drift-threshold test.
///
/// `NoFire` means the galaxy is within the boundary the family
/// promises to keep. `Warning` and `Fire` both warrant an `issue`
/// molecule, but `Warning` names a softer signal the operator can
/// watch without action; `Fire` is a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "severity")]
pub enum DriftVerdict {
    /// Metric is within bounds — no issue molecule needed.
    NoFire,
    /// Metric crossed the soft threshold — tag `temp:warm`, surface
    /// to the operator, do not re-type the galaxy yet.
    Warning,
    /// Metric crossed the hard threshold — nucleate an `issue`
    /// molecule tagged `drift-detected`. Operator decides re-typing.
    Fire,
}

impl DriftVerdict {
    /// True when the verdict is anything stronger than `NoFire`.
    #[must_use]
    pub fn fired(&self) -> bool {
        !matches!(self, Self::NoFire)
    }
}

/// Evidence attached to every drift report — the numbers that caused
/// the threshold to fire, for the `issue` molecule's body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriftReport {
    /// Galaxy name (`repos.name`).
    pub galaxy: String,
    /// `galaxy_kind` at the moment of detection — `None` for nascent
    /// galaxies, which can still drift (vanity family especially).
    pub expected_kind: Option<GalaxyKind>,
    /// Short identifier of the drift (`hub-to-project`, …).
    pub drift: &'static str,
    /// Severity verdict.
    pub verdict: DriftVerdict,
    /// Human-readable reason — goes straight into the issue topic.
    pub reason: String,
    /// Raw numbers that fired the threshold, as `(label, value)` tuples
    /// rendered by formulas into the issue molecule body.
    pub numbers: Vec<(String, i64)>,
}

impl DriftReport {
    /// Shape the topic string a formula should pass to `cs nucleate
    /// --var topic=...` when the verdict fires.
    #[must_use]
    pub fn issue_topic(&self) -> String {
        format!(
            "drift-detected: {drift} — {galaxy} ({reason})",
            drift = self.drift,
            galaxy = self.galaxy,
            reason = self.reason,
        )
    }
}

// ---------------------------------------------------------------------------
// Drift 1 — hub → project (delib-5168 §4 Q8a)
// ---------------------------------------------------------------------------

/// Trailing 14-day window observation for a `SocialHub` galaxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubWindow {
    /// Commits on non-bot source code in the last 14 days.
    pub code_commits: u32,
    /// Conversational artefacts (messages, human-co-signed molecules)
    /// in the last 14 days.
    pub conversational_artefacts: u32,
}

/// Drift 1 — hub → project: fires when code commits outnumber
/// conversational artefacts over a trailing 14-day window.
///
/// ```
/// # use neurion_core::domain::drift::{hub_to_project_drift, DriftVerdict, HubWindow};
/// let w = HubWindow { code_commits: 40, conversational_artefacts: 12 };
/// assert_eq!(
///     hub_to_project_drift("showroom", w).verdict,
///     DriftVerdict::Fire
/// );
/// ```
#[must_use]
pub fn hub_to_project_drift(galaxy: &str, window: HubWindow) -> DriftReport {
    let verdict = if window.code_commits > window.conversational_artefacts {
        DriftVerdict::Fire
    } else {
        DriftVerdict::NoFire
    };
    DriftReport {
        galaxy: galaxy.to_owned(),
        expected_kind: Some(GalaxyKind::SocialHub),
        drift: "hub-to-project",
        verdict,
        reason: format!(
            "code_commits={code} > conversational_artefacts={conv} (14d)",
            code = window.code_commits,
            conv = window.conversational_artefacts,
        ),
        numbers: vec![
            (
                "code_commits_14d".to_owned(),
                i64::from(window.code_commits),
            ),
            (
                "conversational_artefacts_14d".to_owned(),
                i64::from(window.conversational_artefacts),
            ),
        ],
    }
}

// ---------------------------------------------------------------------------
// Drift 2 — editorial → introspection (delib-5168 §4 Q8b)
// ---------------------------------------------------------------------------

/// Drift 2 — editorial → introspection: fires after ≥ 30 consecutive
/// days with zero artefact crossing to a named external audience.
///
/// ```
/// # use neurion_core::domain::drift::{editorial_to_introspection_drift, DriftVerdict};
/// let verdict = editorial_to_introspection_drift("chancery", 45).verdict;
/// assert_eq!(verdict, DriftVerdict::Fire);
/// let soft = editorial_to_introspection_drift("chancery", 22).verdict;
/// assert_eq!(soft, DriftVerdict::NoFire);
/// ```
#[must_use]
pub fn editorial_to_introspection_drift(galaxy: &str, silence_days: u32) -> DriftReport {
    let verdict = if silence_days >= 30 {
        DriftVerdict::Fire
    } else {
        DriftVerdict::NoFire
    };
    DriftReport {
        galaxy: galaxy.to_owned(),
        expected_kind: Some(GalaxyKind::Editorial),
        drift: "editorial-to-introspection",
        verdict,
        reason: format!("silence_days={silence_days} ≥ 30 (no external publication)",),
        numbers: vec![("silence_days".to_owned(), i64::from(silence_days))],
    }
}

// ---------------------------------------------------------------------------
// Drift 3 — infra → imposition (delib-5168 §4 Q8c)
// ---------------------------------------------------------------------------

/// Drift 3 — infra → imposition: 1 sister-filed `infra-coupling` issue
/// in the trailing quarter is a *warning*; 2+ is a *verdict*.
///
/// ```
/// # use neurion_core::domain::drift::{infra_to_imposition_drift, DriftVerdict};
/// assert_eq!(infra_to_imposition_drift("cosmon", 0).verdict, DriftVerdict::NoFire);
/// assert_eq!(infra_to_imposition_drift("cosmon", 1).verdict, DriftVerdict::Warning);
/// assert_eq!(infra_to_imposition_drift("cosmon", 3).verdict, DriftVerdict::Fire);
/// ```
#[must_use]
pub fn infra_to_imposition_drift(galaxy: &str, coupling_issues_90d: u32) -> DriftReport {
    let verdict = match coupling_issues_90d {
        0 => DriftVerdict::NoFire,
        1 => DriftVerdict::Warning,
        _ => DriftVerdict::Fire,
    };
    DriftReport {
        galaxy: galaxy.to_owned(),
        expected_kind: Some(GalaxyKind::Infra),
        drift: "infra-to-imposition",
        verdict,
        reason: format!("infra_coupling_issues_90d={coupling_issues_90d}",),
        numbers: vec![(
            "infra_coupling_issues_90d".to_owned(),
            i64::from(coupling_issues_90d),
        )],
    }
}

// ---------------------------------------------------------------------------
// Drift 4 — vanity family (delib-5168 §4 Q8d)
// ---------------------------------------------------------------------------

/// Input for the vanity-family test: three consecutive reporting periods
/// of the galaxy's *own* family metric, and what a *neighbor* family
/// would read on the same repo for the same span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VanityObservation {
    /// The claimed family's North Star over the last three periods.
    pub own_metric_periods: [i64; 3],
    /// The neighbor family whose metric would be non-zero on this repo.
    pub neighbor_kind: GalaxyKind,
    /// The neighbor family's metric over the same three periods.
    pub neighbor_metric_periods: [i64; 3],
}

/// Drift 4 — vanity family: success metric reads 0 for three
/// consecutive reporting periods AND a neighbor family's metric would
/// be non-zero for the same repo. The suggestion is to re-type, not
/// prop up.
///
/// ```
/// # use neurion_core::domain::drift::{vanity_family_drift, DriftVerdict, VanityObservation};
/// # use neurion_core::domain::galaxy_kind::GalaxyKind;
/// let obs = VanityObservation {
///     own_metric_periods: [0, 0, 0],
///     neighbor_kind: GalaxyKind::Project,
///     neighbor_metric_periods: [5, 7, 4],
/// };
/// assert_eq!(vanity_family_drift("demo", GalaxyKind::Editorial, obs).verdict, DriftVerdict::Fire);
/// ```
#[must_use]
pub fn vanity_family_drift(
    galaxy: &str,
    claimed_kind: GalaxyKind,
    obs: VanityObservation,
) -> DriftReport {
    let all_own_zero = obs.own_metric_periods.iter().all(|n| *n == 0);
    let any_neighbor_nonzero = obs.neighbor_metric_periods.iter().any(|n| *n != 0);
    let verdict = if all_own_zero && any_neighbor_nonzero {
        DriftVerdict::Fire
    } else {
        DriftVerdict::NoFire
    };
    DriftReport {
        galaxy: galaxy.to_owned(),
        expected_kind: Some(claimed_kind),
        drift: "vanity-family",
        verdict,
        reason: format!(
            "own({claimed})=0×3 ; neighbor({neighbor})={a},{b},{c}",
            claimed = claimed_kind,
            neighbor = obs.neighbor_kind,
            a = obs.neighbor_metric_periods[0],
            b = obs.neighbor_metric_periods[1],
            c = obs.neighbor_metric_periods[2],
        ),
        numbers: vec![
            ("own_period_1".to_owned(), obs.own_metric_periods[0]),
            ("own_period_2".to_owned(), obs.own_metric_periods[1]),
            ("own_period_3".to_owned(), obs.own_metric_periods[2]),
            (
                "neighbor_period_1".to_owned(),
                obs.neighbor_metric_periods[0],
            ),
            (
                "neighbor_period_2".to_owned(),
                obs.neighbor_metric_periods[1],
            ),
            (
                "neighbor_period_3".to_owned(),
                obs.neighbor_metric_periods[2],
            ),
        ],
    }
}

// ---------------------------------------------------------------------------
// Drift 5 — project → frozen (delib-5168 §4 Q8e)
// ---------------------------------------------------------------------------

/// Trailing-window observation for a project galaxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrozenObservation {
    /// Exergy over the last reporting period (principles·deliverables).
    /// Zero means nothing crystallised, nothing shipped.
    pub exergy: i64,
    /// Flux over the last reporting period (commits + chronicles).
    /// Zero means the project has stopped moving entirely.
    pub flux: i64,
    /// Artefact-consultation signal (git-reads, pdf opens, …). Non-zero
    /// distinguishes a frozen-yet-useful artefact from dead code.
    pub artefacts_consulted: i64,
}

/// Drift 5 — project → frozen: exergy → 0 AND flux → 0 AND artefacts
/// still consulted. The verdict instructs tagging `status:frozen`, not
/// deletion.
///
/// ```
/// # use neurion_core::domain::drift::{project_to_frozen_drift, DriftVerdict, FrozenObservation};
/// let obs = FrozenObservation { exergy: 0, flux: 0, artefacts_consulted: 12 };
/// assert_eq!(project_to_frozen_drift("experiment", obs).verdict, DriftVerdict::Fire);
/// let still_flowing = FrozenObservation { exergy: 0, flux: 3, artefacts_consulted: 0 };
/// assert_eq!(project_to_frozen_drift("experiment", still_flowing).verdict, DriftVerdict::NoFire);
/// ```
#[must_use]
pub fn project_to_frozen_drift(galaxy: &str, obs: FrozenObservation) -> DriftReport {
    let verdict = if obs.exergy == 0 && obs.flux == 0 && obs.artefacts_consulted > 0 {
        DriftVerdict::Fire
    } else {
        DriftVerdict::NoFire
    };
    DriftReport {
        galaxy: galaxy.to_owned(),
        expected_kind: Some(GalaxyKind::Project),
        drift: "project-to-frozen",
        verdict,
        reason: format!(
            "exergy={exergy} flux={flux} artefacts_consulted={reads}",
            exergy = obs.exergy,
            flux = obs.flux,
            reads = obs.artefacts_consulted,
        ),
        numbers: vec![
            ("exergy".to_owned(), obs.exergy),
            ("flux".to_owned(), obs.flux),
            ("artefacts_consulted".to_owned(), obs.artefacts_consulted),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_to_project_fires_when_code_outnumbers_talk() {
        let r = hub_to_project_drift(
            "showroom",
            HubWindow {
                code_commits: 40,
                conversational_artefacts: 12,
            },
        );
        assert_eq!(r.verdict, DriftVerdict::Fire);
        assert!(r.reason.contains("code_commits=40"));
        assert_eq!(r.drift, "hub-to-project");
        assert_eq!(r.expected_kind, Some(GalaxyKind::SocialHub));
        assert!(r
            .issue_topic()
            .starts_with("drift-detected: hub-to-project"));
    }

    #[test]
    fn hub_to_project_no_fire_at_boundary_equality() {
        // The threshold is strict inequality: equal counts is NOT a drift.
        let r = hub_to_project_drift(
            "showroom",
            HubWindow {
                code_commits: 20,
                conversational_artefacts: 20,
            },
        );
        assert_eq!(r.verdict, DriftVerdict::NoFire);
    }

    #[test]
    fn hub_to_project_no_fire_when_conversation_leads() {
        let r = hub_to_project_drift(
            "demo-squad",
            HubWindow {
                code_commits: 1,
                conversational_artefacts: 9,
            },
        );
        assert_eq!(r.verdict, DriftVerdict::NoFire);
    }

    #[test]
    fn editorial_introspection_fires_at_30_days() {
        assert_eq!(
            editorial_to_introspection_drift("chancery", 30).verdict,
            DriftVerdict::Fire
        );
        assert_eq!(
            editorial_to_introspection_drift("chancery", 29).verdict,
            DriftVerdict::NoFire
        );
        assert_eq!(
            editorial_to_introspection_drift("chancery", 90).verdict,
            DriftVerdict::Fire
        );
    }

    #[test]
    fn infra_to_imposition_has_three_bands() {
        assert_eq!(
            infra_to_imposition_drift("cosmon", 0).verdict,
            DriftVerdict::NoFire
        );
        assert_eq!(
            infra_to_imposition_drift("cosmon", 1).verdict,
            DriftVerdict::Warning
        );
        assert_eq!(
            infra_to_imposition_drift("cosmon", 2).verdict,
            DriftVerdict::Fire
        );
        assert_eq!(
            infra_to_imposition_drift("cosmon", 9).verdict,
            DriftVerdict::Fire
        );
    }

    #[test]
    fn vanity_family_fires_only_when_all_three_own_are_zero_and_neighbor_has_signal() {
        let fire = vanity_family_drift(
            "demo",
            GalaxyKind::Editorial,
            VanityObservation {
                own_metric_periods: [0, 0, 0],
                neighbor_kind: GalaxyKind::Project,
                neighbor_metric_periods: [0, 1, 0],
            },
        );
        assert_eq!(fire.verdict, DriftVerdict::Fire);

        let own_has_one = vanity_family_drift(
            "demo",
            GalaxyKind::Editorial,
            VanityObservation {
                own_metric_periods: [0, 1, 0],
                neighbor_kind: GalaxyKind::Project,
                neighbor_metric_periods: [3, 3, 3],
            },
        );
        assert_eq!(own_has_one.verdict, DriftVerdict::NoFire);

        let neighbor_silent = vanity_family_drift(
            "demo",
            GalaxyKind::Editorial,
            VanityObservation {
                own_metric_periods: [0, 0, 0],
                neighbor_kind: GalaxyKind::Project,
                neighbor_metric_periods: [0, 0, 0],
            },
        );
        assert_eq!(neighbor_silent.verdict, DriftVerdict::NoFire);
    }

    #[test]
    fn project_to_frozen_requires_reads_to_distinguish_from_dead() {
        let useful = project_to_frozen_drift(
            "crunch-audio",
            FrozenObservation {
                exergy: 0,
                flux: 0,
                artefacts_consulted: 12,
            },
        );
        assert_eq!(useful.verdict, DriftVerdict::Fire);

        let dead = project_to_frozen_drift(
            "crunch-audio",
            FrozenObservation {
                exergy: 0,
                flux: 0,
                artefacts_consulted: 0,
            },
        );
        // Zero consultation → this is a candidate for archival, not a
        // frozen-project drift. The project-to-frozen drift only fires
        // when artefacts *are* still consulted.
        assert_eq!(dead.verdict, DriftVerdict::NoFire);

        let alive = project_to_frozen_drift(
            "crunch-audio",
            FrozenObservation {
                exergy: 2,
                flux: 5,
                artefacts_consulted: 10,
            },
        );
        assert_eq!(alive.verdict, DriftVerdict::NoFire);
    }

    #[test]
    fn fired_is_true_for_warning_and_fire_only() {
        assert!(!DriftVerdict::NoFire.fired());
        assert!(DriftVerdict::Warning.fired());
        assert!(DriftVerdict::Fire.fired());
    }

    #[test]
    fn drift_report_issue_topic_has_galaxy_and_drift() {
        let r = infra_to_imposition_drift("cosmon", 2);
        let topic = r.issue_topic();
        assert!(topic.contains("infra-to-imposition"));
        assert!(topic.contains("cosmon"));
        assert!(topic.contains("infra_coupling_issues_90d=2"));
    }

    #[test]
    fn numbers_preserve_the_raw_metric_values() {
        let r = hub_to_project_drift(
            "showroom",
            HubWindow {
                code_commits: 7,
                conversational_artefacts: 3,
            },
        );
        let pairs: std::collections::HashMap<_, _> =
            r.numbers.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(pairs["code_commits_14d"], 7);
        assert_eq!(pairs["conversational_artefacts_14d"], 3);
    }

    #[test]
    fn drift_report_serializes_with_kebab_case_verdict() {
        let r = editorial_to_introspection_drift("chancery", 45);
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"severity\":\"fire\""));
        assert!(j.contains("\"drift\":\"editorial-to-introspection\""));
    }
}
