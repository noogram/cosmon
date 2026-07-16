// SPDX-License-Identifier: AGPL-3.0-only

//! `QualityBand` — a band-of-day stamped on every `cs` verb event so the
//! trace is no longer blind to the physiological state of the scribe.
//!
//! ## Why
//!
//! A bare event log is invariant to *when* — and in what shape — the
//! operator was when they emitted it: a decision made at 4 a.m. after a
//! sleepless night looks identical to one made fresh after a full sleep.
//! That invariance is exactly what makes a fatigue-driven pathology
//! undetectable from inside the log. `QualityBand` breaks it by
//! recording, alongside the UTC timestamp, a discrete band derived from
//! observable signals — *without asking anything of the operator*.
//!
//! The band is admissible to any downstream scheduler or analysis. It is
//! also falsifiable: a 30-day ratio of `tackle → defer` (or
//! `collapse → re-nucleate`) that differs between bands would corroborate
//! the hypothesis (prediction: `nocturne ≥ 1.4 × diurne`); if the ratio is
//! flat, the band captured nothing and should be retracted.
//!
//! ## A behavioural-observable layer, not a witness
//!
//! This is a layer that lives strictly *above* the existing trace. It does
//! not replace or gate a future operator-health daemon (one that might
//! supply a sleep-debt signal from `HealthKit`); it composes with it. Until
//! such a daemon exists, the hour-of-day signal plus the inter-emission
//! latency are enough to start measuring.
//!
//! ## Signals (today)
//!
//! - **local hour-of-day** — primary; cheap, requires no operator interaction.
//! - **latency since the last cs verb emission** — secondary; distinguishes
//!   a `06:30` verb at the end of an all-night session (`Nocturne`) from a
//!   `06:30` verb after a real sleep window (`PostVeille`).
//!
//! ## Signals (later)
//!
//! - distribution of verbs across 24h (the operator's emission rate).
//! - sleep-debt from a future operator-health daemon.
//!
//! The enum is closed at three values; both later signals refine the
//! discriminator that maps observations to a band, not the band space itself.

use serde::{Deserialize, Serialize};

/// Band-of-day attached to a cs verb emission so the trace becomes a
/// witness of the scribe's physiological state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QualityBand {
    /// Daytime hours (local), normal cognitive function expected.
    Diurne,
    /// Late night / very early morning (local), fatigue likely.
    Nocturne,
    /// Recovery window after a sleep gap — a verb landed in an early
    /// morning hour with sufficient quiescence behind it that the
    /// operator most plausibly slept rather than worked through.
    PostVeille,
}

impl QualityBand {
    /// Stable string form — the same wire shape used in JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Diurne => "diurne",
            Self::Nocturne => "nocturne",
            Self::PostVeille => "post-veille",
        }
    }
}

impl std::fmt::Display for QualityBand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Quiescence threshold (seconds) above which an early-morning verb
/// counts as `PostVeille` rather than continuation of a `Nocturne` run.
///
/// Tuned to **4 hours**: shorter than the shortest plausible sleep window
/// for the operator, long enough to exclude trivial pauses (toilet break,
/// short walk, brief meeting).
pub const POST_VEILLE_QUIESCENCE_S: u64 = 4 * 3600;

/// Hours `[start, end)` (local 24h clock) of the early-morning recovery
/// window. A verb in this window with sufficient quiescence behind it
/// is `PostVeille`; without quiescence it is `Nocturne` (operator did
/// not sleep).
pub const POST_VEILLE_HOURS: std::ops::Range<u32> = 6..10;

/// Hours `[start, end)` (local 24h clock) of the diurnal band.
/// Wraps from the `PostVeille` window through normal evening.
pub const DIURNE_HOURS: std::ops::Range<u32> = 6..23;

/// Compute the band from observable signals available *at the moment of
/// emission*. Pure function — same inputs always produce the same band.
///
/// `local_hour` is the operator's wall-clock hour in `[0, 24)`. Caller is
/// responsible for the UTC → local conversion (which depends on the host
/// timezone and is intentionally out of scope here so this function stays
/// pure for testing).
///
/// `latency_since_last_verb_s` is the gap (in seconds) from the most
/// recent prior cs verb emission. `None` means "no prior verb on record"
/// (cold log) — treated as long quiescence.
///
/// # Panics
///
/// Does not panic. `local_hour` outside `[0, 24)` is treated as `Diurne`.
#[must_use]
pub fn compute(local_hour: u32, latency_since_last_verb_s: Option<u64>) -> QualityBand {
    // Defensive: hours outside `[0, 24)` are bogus inputs; default to
    // Diurne rather than smearing the falsifier with Nocturne noise.
    if local_hour >= 24 {
        return QualityBand::Diurne;
    }

    // Late evening (23:00–24:00) and small hours (00:00–06:00) are
    // Nocturne regardless of latency.
    if !(POST_VEILLE_HOURS.start..23).contains(&local_hour) {
        return QualityBand::Nocturne;
    }

    // Early morning (06:00–10:00) is PostVeille if there was a real
    // quiescent gap; otherwise the operator worked through the night
    // and the band is still Nocturne.
    if POST_VEILLE_HOURS.contains(&local_hour) {
        let quiescent = match latency_since_last_verb_s {
            None => true, // cold log — first verb of the day after silence
            Some(s) => s >= POST_VEILLE_QUIESCENCE_S,
        };
        return if quiescent {
            QualityBand::PostVeille
        } else {
            QualityBand::Nocturne
        };
    }

    // Daytime — between PostVeille close (10:00) and Nocturne open (23:00).
    QualityBand::Diurne
}

/// Resolve the local hour of `now` against the host timezone.
///
/// Pulled out so callers can stub it in tests; the writer uses
/// [`local_hour_now`] for the production path.
#[must_use]
pub fn local_hour_of(now: chrono::DateTime<chrono::Utc>) -> u32 {
    use chrono::Timelike;
    now.with_timezone(&chrono::Local).hour()
}

/// Resolve the operator's local hour right now. Convenience around
/// [`local_hour_of`] for the writer's hot path.
#[must_use]
pub fn local_hour_now() -> u32 {
    local_hour_of(chrono::Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nocturne_at_small_hours() {
        for h in 0..6 {
            assert_eq!(compute(h, Some(60)), QualityBand::Nocturne, "hour {h}");
        }
        assert_eq!(compute(23, Some(60)), QualityBand::Nocturne);
    }

    #[test]
    fn diurne_at_normal_hours() {
        for h in 10..23 {
            assert_eq!(compute(h, Some(60)), QualityBand::Diurne, "hour {h}");
        }
    }

    #[test]
    fn post_veille_after_quiescent_gap() {
        for h in 6..10 {
            assert_eq!(
                compute(h, Some(POST_VEILLE_QUIESCENCE_S)),
                QualityBand::PostVeille,
                "hour {h}"
            );
            assert_eq!(
                compute(h, Some(POST_VEILLE_QUIESCENCE_S * 2)),
                QualityBand::PostVeille,
                "hour {h}"
            );
        }
    }

    #[test]
    fn nocturne_in_morning_window_without_quiescence() {
        // Operator worked through the night — the 07:00 verb is Nocturne,
        // not PostVeille, because the latency is short.
        for h in 6..10 {
            assert_eq!(
                compute(h, Some(60)),
                QualityBand::Nocturne,
                "hour {h} short-gap"
            );
            assert_eq!(
                compute(h, Some(POST_VEILLE_QUIESCENCE_S - 1)),
                QualityBand::Nocturne,
                "hour {h} sub-threshold"
            );
        }
    }

    #[test]
    fn cold_log_in_morning_is_post_veille() {
        // No prior verb — treat as long quiescence (operator just woke up
        // and the log is empty).
        assert_eq!(compute(7, None), QualityBand::PostVeille);
    }

    #[test]
    fn out_of_range_hour_defaults_to_diurne() {
        // Defensive: a bogus hour value doesn't break recording.
        assert_eq!(compute(99, Some(60)), QualityBand::Diurne);
    }

    #[test]
    fn json_round_trip_kebab_case() {
        let cases = [
            (QualityBand::Diurne, "\"diurne\""),
            (QualityBand::Nocturne, "\"nocturne\""),
            (QualityBand::PostVeille, "\"post-veille\""),
        ];
        for (band, expected) in cases {
            let s = serde_json::to_string(&band).unwrap();
            assert_eq!(s, expected);
            let back: QualityBand = serde_json::from_str(&s).unwrap();
            assert_eq!(back, band);
        }
    }

    #[test]
    fn as_str_matches_serialized_form() {
        assert_eq!(QualityBand::Diurne.as_str(), "diurne");
        assert_eq!(QualityBand::Nocturne.as_str(), "nocturne");
        assert_eq!(QualityBand::PostVeille.as_str(), "post-veille");
    }
}
