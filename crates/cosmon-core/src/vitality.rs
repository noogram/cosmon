// SPDX-License-Identifier: AGPL-3.0-only

//! Runtime-vitality — a pure, zero-I/O projection of fleet liveness.
//!
//! The central insight from `delib-20260626-9825` (C2): the event log is
//! the *pre-integrated derivative* of fleet progress. Counting completion
//! events in a trailing window gives `P = dΦ/dt` without any stored previous
//! Φ value and without any new state store. This module is the pure fold;
//! the I/O shell in `cosmon-cli::cmd::pulse` reads disk and hands the
//! numbers in.
//!
//! # Anti-silence — three redundant layers (C3)
//!
//! 1. **Ordered total predicate** — `subsystem_dead` is the *first* RED
//!    clause; it is a positive condition (`H_sched > τ`), never an absence.
//! 2. **Max-severity aggregation** — `state = max(regime_sev, voyant_sev)`;
//!    the dot cannot be green when any voyant is red.
//! 3. **Six-named-field struct** — `Voyants` is a struct, never a `HashMap`.
//!    `serde` emits every field unconditionally; a dead subsystem serializes
//!    `"off"` (red-class) and *cannot vanish* from the wire.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::vitality::{vitality, VitalityInputs, VoyantState, PulseState};
//! use chrono::Utc;
//!
//! // Quiescent fleet — no live work, nothing burning.
//! let inputs = VitalityInputs {
//!     now: Utc::now(),
//!     progress_count: 0,
//!     fuel_debit: 0,
//!     live_work: 0,
//!     live_workers: 0,
//!     sched_age_secs: 10.0,
//!     starved_count: 0,
//!     window_secs: 300.0,
//!     sched_tau_secs: 600.0,
//!     b_min_tokens: 100,
//!     fuel_pct: 0.5,
//!     scanned: 0,
//!     subsystem_scheduler: VoyantState::Green,
//!     subsystem_drainage: VoyantState::Green,
//!     subsystem_propel: VoyantState::Green,
//!     subsystem_heal: VoyantState::Green,
//!     subsystem_fuel: VoyantState::Green,
//!     subsystem_workers: VoyantState::Green,
//! };
//! let pulse = vitality(&inputs);
//! assert_eq!(pulse.state, PulseState::Green);
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// VoyantState — per-subsystem traffic light
// ---------------------------------------------------------------------------

/// Per-subsystem status, the atomic unit of the vitality strip.
///
/// The ordering is intentional: `Red > Amber > Green > Off`. `Off`
/// is distinct from `Red` — it means the subsystem is not configured,
/// not that it has failed. Absent data (*staleness* detected by the
/// caller) maps to `Red` in the aggregation layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoyantState {
    /// Subsystem is not configured / not applicable.
    Off,
    /// Subsystem is healthy.
    Green,
    /// Subsystem shows a non-critical anomaly.
    Amber,
    /// Subsystem is dead or critically anomalous.
    Red,
}

impl VoyantState {
    /// The CSS/hex color corresponding to this state (for renderers).
    #[must_use]
    pub fn color_hex(self) -> &'static str {
        match self {
            Self::Off => "#888888",
            Self::Green => "#19C37D",
            Self::Amber => "#F0A202",
            Self::Red => "#FF1744",
        }
    }

    /// Single-character terminal glyph.
    #[must_use]
    pub fn glyph(self) -> char {
        match self {
            Self::Off => '○',
            Self::Green => '●',
            Self::Amber => '◐',
            Self::Red => '■',
        }
    }

    /// Returns `true` when this state is red-class (red or off).
    ///
    /// `off` is red-class because an absent subsystem report is the
    /// silence that caused the origin wound.
    #[must_use]
    pub fn is_red_class(self) -> bool {
        matches!(self, Self::Red | Self::Off)
    }
}

impl std::fmt::Display for VoyantState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Green => f.write_str("green"),
            Self::Amber => f.write_str("amber"),
            Self::Red => f.write_str("red"),
        }
    }
}

// ---------------------------------------------------------------------------
// Voyants — the six-field struct (C3 layer 3)
// ---------------------------------------------------------------------------

/// The six subsystem voyants.
///
/// This is a **named-field struct**, never a `HashMap`. `serde` emits all
/// six keys on every JSON line by construction. A dead subsystem serializes
/// `"off"` or `"red"` — it *cannot vanish* from the wire. Absence-of-key is
/// unrepresentable (the C3 anti-silence invariant at the serialization layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voyants {
    /// Scheduler heartbeat voyant: red when `H_sched > τ`.
    pub scheduler: VoyantState,
    /// Drainage voyant: red when no patrol/evolve tick in τ while `L > 0`.
    pub drainage: VoyantState,
    /// Propel voyant: reflects `cs patrol --propel` liveness.
    pub propel: VoyantState,
    /// Heal voyant: reflects `cs patrol --heal` liveness.
    pub heal: VoyantState,
    /// Fuel voyant: reflects budget exhaustion risk.
    pub fuel: VoyantState,
    /// Workers voyant: reflects active worker health.
    pub workers: VoyantState,
}

impl Voyants {
    /// Worst voyant across all six fields (max severity).
    ///
    /// The dot state is `max(regime_sev, voyants.worst())` so the dot
    /// *cannot* be green when any voyant is red (C3 layer 2).
    #[must_use]
    pub fn worst(self) -> VoyantState {
        [
            self.scheduler,
            self.drainage,
            self.propel,
            self.heal,
            self.fuel,
            self.workers,
        ]
        .into_iter()
        .max()
        .unwrap_or(VoyantState::Off)
    }
}

// ---------------------------------------------------------------------------
// PulseState — the top-level traffic light
// ---------------------------------------------------------------------------

/// The three-way runtime-vitality classification.
///
/// Computed by the ordered total predicate in [`vitality`] (first-match
/// wins — every input lands in exactly one bucket).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PulseState {
    /// Fleet is dead or critically anomalous.
    Red,
    /// Fleet is alive but stalled or starved.
    Amber,
    /// Fleet is working or quiescent.
    Green,
}

impl PulseState {
    /// Severity number for `max(regime, voyants)` comparison.
    #[must_use]
    pub fn severity(self) -> u8 {
        match self {
            Self::Green => 0,
            Self::Amber => 1,
            Self::Red => 2,
        }
    }

    /// Single-character terminal glyph.
    #[must_use]
    pub fn glyph(self) -> char {
        match self {
            Self::Green => '●',
            Self::Amber => '◐',
            Self::Red => '■',
        }
    }
}

impl std::fmt::Display for PulseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Red => f.write_str("red"),
            Self::Amber => f.write_str("amber"),
            Self::Green => f.write_str("green"),
        }
    }
}

// ---------------------------------------------------------------------------
// Headline — the "word seizes the number-slot" rule (C4)
// ---------------------------------------------------------------------------

/// The headline emitted by `cs pulse`.
///
/// Jobs's ruling (C4): emit a **word** when a number would lie (subsystem
/// dead, spinning), else the RPM number. One slot, one token — the number
/// and the word are mutually exclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Headline {
    /// A number (RPM) is the story — fleet is doing measurable work.
    Rpm(f64),
    /// A word is the story — a number would be misleading.
    Word(String),
}

impl std::fmt::Display for Headline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpm(r) => write!(f, "{r:.1}rpm"),
            Self::Word(w) => f.write_str(w),
        }
    }
}

// ---------------------------------------------------------------------------
// VitalityInputs — all borrows satisfied by the caller's I/O shell
// ---------------------------------------------------------------------------

/// All signals needed to compute a [`Pulse`], aggregated by the I/O shell.
///
/// The caller (the `pulse.rs` shell) does the three disk reads and passes
/// the numbers here. This struct is all owned values so it is `'static`
/// and can be constructed in tests without a filesystem.
#[derive(Debug, Clone)]
pub struct VitalityInputs {
    /// Wall clock at computation time (injected so tests control it).
    pub now: DateTime<Utc>,

    /// Count of `StepCompleted`/`MoleculeCompleted` events in window W.
    /// This is `P = dΦ/dt` — the pre-integrated derivative from the
    /// event log (C2).
    pub progress_count: u64,

    /// Cumulative token debit in window W (delta of `EnergyTick` totals).
    /// The spinning detector fires when `B > b_min` and `P == 0`.
    pub fuel_debit: u64,

    /// Current live work = count of `Running` + `Pending` molecules.
    /// `L = 0` → quiescent (GREEN); `L > 0, P == 0` → stalled.
    ///
    /// This is the *DAG-frontier* signal — how much work is in flight on the
    /// control plane. It is **not** the count of live worker processes: a
    /// fleet can carry dozens of `Running`/`Pending` molecules that were
    /// completed-but-never-harvested, with no worker actually attached. The
    /// stalled/quiescent regime distinction wants this molecule-count; the
    /// workers voyant wants [`Self::live_workers`] instead.
    pub live_work: u64,

    /// Count of **live worker processes** — verified tmux sessions on the
    /// fleet socket(s), each owning a non-dead pane that parses as a
    /// `WorkerId`. This is the number an operator means by "how many workers
    /// are running"; it drives both `workers_count` and the workers voyant.
    ///
    /// Distinct from [`Self::live_work`]: that counts active/pending
    /// molecules (which inflate with stale, never-harvested work); this
    /// counts actually-alive worker sessions. The two diverge exactly when
    /// completed molecules are left in `Running`/`Pending` status.
    pub live_workers: u64,

    /// Age of the last scheduler tick in seconds.
    /// `H_sched > sched_tau_secs` → `subsystem_dead` → RED.
    pub sched_age_secs: f64,

    /// Count of `Starved` molecules.
    /// `starved_count > 0` → AMBER (starved ≠ quiescent, D4 finding).
    pub starved_count: u64,

    /// Observation window in seconds. Used to compute RPM.
    pub window_secs: f64,

    /// Scheduler staleness threshold τ in seconds.
    /// When `sched_age_secs > sched_tau_secs`, the scheduler is declared dead.
    pub sched_tau_secs: f64,

    /// Minimum fuel debit (tokens) to declare spinning.
    /// A single stray retry token should not trigger spinning.
    pub b_min_tokens: u64,

    /// Fraction of fuel budget consumed (0.0–1.0).
    pub fuel_pct: f64,

    /// Total events scanned (informational, surfaced in the JSON schema).
    pub scanned: u64,

    // -- Per-subsystem states (six named fields, C3 layer 3) --
    /// Scheduler subsystem state.
    pub subsystem_scheduler: VoyantState,
    /// Drainage subsystem state.
    pub subsystem_drainage: VoyantState,
    /// Propel subsystem state.
    pub subsystem_propel: VoyantState,
    /// Heal subsystem state.
    pub subsystem_heal: VoyantState,
    /// Fuel subsystem state.
    pub subsystem_fuel: VoyantState,
    /// Workers subsystem state.
    pub subsystem_workers: VoyantState,
}

// ---------------------------------------------------------------------------
// Pulse — the computed output value
// ---------------------------------------------------------------------------

/// The output of [`vitality`].
///
/// Serializes to the `cosmon.pulse/v1` NDJSON schema:
/// ```json
/// {"state":"green","headline_rpm":4.2,"voyants":{...},"fuel_pct":0.42,"rpm":4.2,"workers_count":7,"scanned":300,"ts":"..."}
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pulse {
    /// Top-level traffic-light state.
    pub state: PulseState,

    /// RPM in the observation window (`dΦ/dt`). Always present; use
    /// `headline_word` to decide whether the number or the word is shown.
    pub rpm: f64,

    /// The headline RPM when the number is the story.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline_rpm: Option<f64>,

    /// The headline word when a number would lie (SPINNING, DRAINAGE OFF, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline_word: Option<String>,

    /// The six-field voyant strip (C3 layer 3).
    pub voyants: Voyants,

    /// Fraction of fuel budget consumed (0.0–1.0).
    pub fuel_pct: f64,

    /// Count of live worker processes (verified tmux sessions on the fleet
    /// socket(s)), NOT the count of active/pending molecules.
    ///
    /// Same source as the workers voyant color — one source of truth. The
    /// shell counts non-dead tmux panes that parse as a `WorkerId`; a
    /// completed-but-never-harvested molecule left in `Running` status has
    /// no live session and is therefore *not* counted here (the misleading
    /// "88 workers / 2 live sessions" inflation this field exists to fix).
    /// Surfaced on the workers voyant line in all three output formats.
    pub workers_count: u64,

    /// Total events scanned from the event log.
    pub scanned: u64,

    /// UTC timestamp of this reading.
    pub ts: DateTime<Utc>,
}

impl Pulse {
    /// The headline as a display token — either the RPM or the word.
    ///
    /// Renderers call this to decide what to put in the single number/word
    /// slot (C4). Returns `SPINNING`, `DRAINAGE OFF`, etc. when a number
    /// would lie, otherwise the RPM with one decimal place.
    #[must_use]
    pub fn headline(&self) -> Headline {
        if let Some(ref w) = self.headline_word {
            return Headline::Word(w.clone());
        }
        Headline::Rpm(self.rpm)
    }
}

// ---------------------------------------------------------------------------
// vitality() — the pure function
// ---------------------------------------------------------------------------

/// Compute a [`Pulse`] from in-memory signals.
///
/// This is a pure function: no filesystem access, no clock reads (clock is
/// injected via `inputs.now`), no mutable state. The caller is the I/O
/// shell in `cosmon-cli::cmd::pulse`.
///
/// ## Ordered total predicate (first-match wins)
///
/// - **RED** ⟺ `subsystem_dead(H_sched > τ)` ∨ `fuel_exhausted` ∨
///   `spinning(B > b_min ∧ P == 0)`
/// - **AMBER** ⟺ ¬RED ∧ `((P==0 ∧ L>0 ∧ H_sched≤τ) ∨ starved>0)`
/// - **GREEN** ⟺ ¬RED ∧ ¬AMBER ∧ `((P>0 ∧ ¬fuel_exhausted) ∨ L==0)`
///
/// The predicate is total: every input combination lands in exactly one
/// bucket (no gap, no overlap).
#[must_use]
pub fn vitality(inputs: &VitalityInputs) -> Pulse {
    let voyants = Voyants {
        scheduler: inputs.subsystem_scheduler,
        drainage: inputs.subsystem_drainage,
        propel: inputs.subsystem_propel,
        heal: inputs.subsystem_heal,
        fuel: inputs.subsystem_fuel,
        workers: inputs.subsystem_workers,
    };

    // RPM = completions per minute in window W.
    #[allow(clippy::cast_precision_loss)]
    let rpm = if inputs.window_secs > 0.0 {
        inputs.progress_count as f64 / (inputs.window_secs / 60.0)
    } else {
        0.0
    };

    // --- Predicate clauses ---

    let subsystem_dead = inputs.sched_age_secs > inputs.sched_tau_secs
        || voyants.scheduler.is_red_class()
        || voyants.drainage.is_red_class();

    let fuel_exhausted = inputs.fuel_pct >= 1.0;

    // spinning: tokens burned (B > b_min) but zero progress.
    let spinning = inputs.fuel_debit > inputs.b_min_tokens && inputs.progress_count == 0;

    // --- Regime (from the ordered predicate) ---
    let regime = if subsystem_dead || fuel_exhausted || spinning {
        PulseState::Red
    } else if (inputs.progress_count == 0 && inputs.live_work > 0 && !subsystem_dead)
        || inputs.starved_count > 0
    {
        PulseState::Amber
    } else {
        // GREEN: P>0, or L==0 (quiescent)
        PulseState::Green
    };

    // --- Final state = max(regime_sev, worst_voyant_sev) ---
    let worst_voyant = voyants.worst();
    let state = if worst_voyant.is_red_class() && regime != PulseState::Red {
        // Any red-class voyant lifts the state to Red (C3 layer 2).
        // Only amber voyants don't automatically override green/amber.
        PulseState::Red
    } else if worst_voyant == VoyantState::Amber && regime == PulseState::Green {
        PulseState::Amber
    } else {
        regime
    };

    // --- Headline (C4): word seizes the slot when a number would lie ---
    let (headline_rpm, headline_word) = if subsystem_dead {
        let word = if voyants.drainage.is_red_class() {
            "DRAINAGE OFF".to_owned()
        } else if voyants.scheduler.is_red_class() {
            "SCHEDULER DEAD".to_owned()
        } else {
            "SUBSYSTEM DEAD".to_owned()
        };
        (None, Some(word))
    } else if spinning {
        (None, Some("SPINNING".to_owned()))
    } else {
        (Some(rpm), None)
    };

    Pulse {
        state,
        rpm,
        headline_rpm,
        headline_word,
        voyants,
        fuel_pct: inputs.fuel_pct,
        workers_count: inputs.live_workers,
        scanned: inputs.scanned,
        ts: inputs.now,
    }
}

// ---------------------------------------------------------------------------
// Tests — exhaustive predicate coverage (DoD requirement)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn base_inputs() -> VitalityInputs {
        VitalityInputs {
            now: Utc::now(),
            progress_count: 0,
            fuel_debit: 0,
            live_work: 0,
            live_workers: 0,
            sched_age_secs: 10.0,
            starved_count: 0,
            window_secs: 300.0,
            sched_tau_secs: 600.0,
            b_min_tokens: 100,
            fuel_pct: 0.5,
            scanned: 42,
            subsystem_scheduler: VoyantState::Green,
            subsystem_drainage: VoyantState::Green,
            subsystem_propel: VoyantState::Green,
            subsystem_heal: VoyantState::Green,
            subsystem_fuel: VoyantState::Green,
            subsystem_workers: VoyantState::Green,
        }
    }

    // --- GREEN branch ---

    #[test]
    fn test_green_quiescent_no_work() {
        // L == 0, P == 0 → quiescent GREEN
        let inputs = base_inputs(); // live_work=0, progress_count=0
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Green);
        assert!(pulse.headline_word.is_none());
        assert!(pulse.headline_rpm.is_some());
    }

    #[test]
    fn test_green_doing_work() {
        // P > 0, not spinning, not starved → GREEN
        let mut inputs = base_inputs();
        inputs.progress_count = 5;
        inputs.live_work = 2;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Green);
        assert!(pulse.headline_word.is_none());
    }

    #[test]
    fn test_rpm_calculation() {
        // 6 completions in 300s window = 6/(300/60) = 1.2 rpm
        let mut inputs = base_inputs();
        inputs.progress_count = 6;
        inputs.window_secs = 300.0;
        let pulse = vitality(&inputs);
        assert!((pulse.rpm - 1.2).abs() < 1e-9);
    }

    // --- AMBER branch ---

    #[test]
    fn test_amber_stalled_turning_over() {
        // P == 0, L > 0, H_sched <= τ → AMBER (turning over)
        let mut inputs = base_inputs();
        inputs.progress_count = 0;
        inputs.live_work = 3;
        // sched_age_secs (10) < sched_tau_secs (600) → not subsystem_dead
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Amber);
        // Not spinning (fuel_debit=0 <= b_min=100), so no SPINNING word
        assert!(pulse.headline_word.is_none());
    }

    #[test]
    fn test_amber_starved_molecules() {
        // starved_count > 0 → AMBER regardless of P
        let mut inputs = base_inputs();
        inputs.starved_count = 2;
        inputs.progress_count = 5; // would normally be GREEN
        inputs.live_work = 3;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Amber);
    }

    // --- RED branch ---

    #[test]
    fn test_red_subsystem_dead_scheduler_age() {
        // sched_age_secs > sched_tau_secs → subsystem_dead → RED
        let mut inputs = base_inputs();
        inputs.sched_age_secs = 700.0; // > 600 τ
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Red);
        assert!(pulse.headline_word.is_some());
        let word = pulse.headline_word.as_deref().unwrap();
        assert!(word.contains("DEAD") || word.contains("DRAINAGE") || word.contains("SCHEDULER"));
    }

    #[test]
    fn test_red_fuel_exhausted() {
        // fuel_pct >= 1.0 → fuel_exhausted → RED
        let mut inputs = base_inputs();
        inputs.fuel_pct = 1.0;
        inputs.live_work = 2;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Red);
    }

    #[test]
    fn test_red_spinning() {
        // B > b_min AND P == 0 → spinning → RED
        let mut inputs = base_inputs();
        inputs.fuel_debit = 200; // > b_min (100)
        inputs.progress_count = 0;
        inputs.live_work = 2;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Red);
        assert_eq!(pulse.headline_word.as_deref(), Some("SPINNING"));
    }

    #[test]
    fn test_spinning_requires_both_conditions() {
        // B > b_min but P > 0 → NOT spinning → not RED from this clause
        let mut inputs = base_inputs();
        inputs.fuel_debit = 200;
        inputs.progress_count = 1;
        inputs.live_work = 1;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Green);

        // B <= b_min and P == 0 → NOT spinning
        let mut inputs2 = base_inputs();
        inputs2.fuel_debit = 50; // <= b_min (100)
        inputs2.progress_count = 0;
        inputs2.live_work = 1;
        let pulse2 = vitality(&inputs2);
        assert_ne!(pulse2.state, PulseState::Red); // AMBER (stalled), not RED
    }

    #[test]
    fn test_red_from_voyant_override() {
        // regime=GREEN but a red-class voyant overrides to RED
        let mut inputs = base_inputs();
        inputs.progress_count = 5;
        inputs.live_work = 2;
        inputs.subsystem_drainage = VoyantState::Red;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Red);
    }

    #[test]
    fn test_drainage_off_word() {
        // drainage voyant is red-class → headline word is DRAINAGE OFF
        let mut inputs = base_inputs();
        inputs.subsystem_drainage = VoyantState::Off;
        inputs.sched_age_secs = 700.0; // trigger subsystem_dead
        let pulse = vitality(&inputs);
        assert_eq!(pulse.headline_word.as_deref(), Some("DRAINAGE OFF"));
    }

    #[test]
    fn test_scheduler_dead_word() {
        // scheduler voyant is red, drainage is green → SCHEDULER DEAD
        let mut inputs = base_inputs();
        inputs.subsystem_scheduler = VoyantState::Red;
        inputs.sched_age_secs = 700.0; // subsystem_dead from age
        let pulse = vitality(&inputs);
        // drainage is green, scheduler is red → SCHEDULER DEAD
        assert_eq!(pulse.headline_word.as_deref(), Some("SCHEDULER DEAD"));
    }

    // --- Voyant struct invariants ---

    #[test]
    fn test_voyants_worst_is_max() {
        let v = Voyants {
            scheduler: VoyantState::Green,
            drainage: VoyantState::Amber,
            propel: VoyantState::Green,
            heal: VoyantState::Off,
            fuel: VoyantState::Green,
            workers: VoyantState::Green,
        };
        // Amber > Green > Off in our ordering? Wait, Off < Green < Amber < Red.
        // So worst = max(Green, Amber, Green, Off, Green, Green) = Amber.
        assert_eq!(v.worst(), VoyantState::Amber);
    }

    #[test]
    fn test_voyants_worst_all_green() {
        let v = Voyants {
            scheduler: VoyantState::Green,
            drainage: VoyantState::Green,
            propel: VoyantState::Green,
            heal: VoyantState::Green,
            fuel: VoyantState::Green,
            workers: VoyantState::Green,
        };
        assert_eq!(v.worst(), VoyantState::Green);
    }

    #[test]
    fn test_voyants_serde_all_six_fields_present() {
        let v = Voyants {
            scheduler: VoyantState::Green,
            drainage: VoyantState::Off,
            propel: VoyantState::Amber,
            heal: VoyantState::Red,
            fuel: VoyantState::Green,
            workers: VoyantState::Green,
        };
        let json = serde_json::to_string(&v).unwrap();
        // All six keys must be present — the anti-silence invariant.
        assert!(json.contains("\"scheduler\""));
        assert!(json.contains("\"drainage\""));
        assert!(json.contains("\"propel\""));
        assert!(json.contains("\"heal\""));
        assert!(json.contains("\"fuel\""));
        assert!(json.contains("\"workers\""));
        // "off" subsystem serializes as "off", not absent.
        assert!(json.contains("\"off\""));
    }

    #[test]
    fn test_pulse_serde_roundtrip() {
        let inputs = base_inputs();
        let pulse = vitality(&inputs);
        let json = serde_json::to_string(&pulse).unwrap();
        let back: Pulse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.state, pulse.state);
        assert!((back.rpm - pulse.rpm).abs() < 1e-9);
    }

    #[test]
    fn test_headline_word_or_rpm_mutually_exclusive() {
        // RED spinning → word only, no rpm in headline
        let mut inputs = base_inputs();
        inputs.fuel_debit = 200;
        inputs.progress_count = 0;
        let pulse = vitality(&inputs);
        assert!(pulse.headline_word.is_some());
        assert!(pulse.headline_rpm.is_none());

        // GREEN → rpm only, no word
        let mut inputs2 = base_inputs();
        inputs2.progress_count = 3;
        inputs2.live_work = 1;
        let pulse2 = vitality(&inputs2);
        assert!(pulse2.headline_word.is_none());
        assert!(pulse2.headline_rpm.is_some());
    }

    #[test]
    fn test_voyant_state_ordering() {
        // Off < Green < Amber < Red (for max-aggregation to work)
        assert!(VoyantState::Off < VoyantState::Green);
        assert!(VoyantState::Green < VoyantState::Amber);
        assert!(VoyantState::Amber < VoyantState::Red);
    }

    #[test]
    fn test_workers_count_propagated() {
        // workers_count tracks live_workers (live sessions), not live_work.
        let mut inputs = base_inputs();
        inputs.live_workers = 7;
        let pulse = vitality(&inputs);
        assert_eq!(pulse.workers_count, 7);
    }

    #[test]
    fn test_workers_count_zero_when_quiescent() {
        let inputs = base_inputs(); // live_workers = 0
        let pulse = vitality(&inputs);
        assert_eq!(pulse.workers_count, 0);
    }

    #[test]
    fn test_workers_count_decoupled_from_live_work() {
        // The origin bug: many active/pending molecules (live_work) but few
        // live worker sessions (live_workers). workers_count must follow the
        // live-session count, never the inflated molecule count.
        let mut inputs = base_inputs();
        inputs.live_work = 88; // 88 active/pending molecules, mostly stale
        inputs.live_workers = 2; // only 2 real tmux worker sessions
        let pulse = vitality(&inputs);
        assert_eq!(
            pulse.workers_count, 2,
            "workers_count must report live sessions (2), not stale molecules (88)"
        );
    }

    #[test]
    fn test_green_quiescent_when_all_zero() {
        // Zero everything: quiescent fleet → GREEN
        let inputs = VitalityInputs {
            now: Utc::now(),
            progress_count: 0,
            fuel_debit: 0,
            live_work: 0,
            live_workers: 0,
            sched_age_secs: 0.0,
            starved_count: 0,
            window_secs: 300.0,
            sched_tau_secs: 600.0,
            b_min_tokens: 100,
            fuel_pct: 0.0,
            scanned: 0,
            subsystem_scheduler: VoyantState::Green,
            subsystem_drainage: VoyantState::Green,
            subsystem_propel: VoyantState::Green,
            subsystem_heal: VoyantState::Green,
            subsystem_fuel: VoyantState::Green,
            subsystem_workers: VoyantState::Green,
        };
        let pulse = vitality(&inputs);
        assert_eq!(pulse.state, PulseState::Green);
    }
}
