// SPDX-License-Identifier: AGPL-3.0-only

//! Kill switches — the file-presence controls that stop autonomous activity.
//!
//! Issue #108: cosmon used to honour several independent switch files, each
//! read by a different component, and nothing said which file stopped what.
//! An operator who laid down `stand-down.lock` to stop everything still had
//! healing running; `autopilot.off` read like a stop control and stopped
//! nothing inside `cs`; `ask.off` was documented and never checked.
//!
//! This module is the single catalogue. [`KillSwitch::StandDown`] is the one
//! global switch: every [`Autonomous`] component halts when it is present.
//! The other switches are **scoped overrides** — each halts only the
//! components it names, so an operator can quiet healing without standing the
//! whole fleet down.
//!
//! Pure data and predicates only: which files exist is an I/O question the
//! caller answers (by convention under `~/.cosmon/`) and hands in as a set.
//!
//! ```
//! use cosmon_core::kill_switch::{halting, Autonomous, KillSwitch};
//!
//! // health.off quiets healing, and nothing else.
//! let active = [KillSwitch::Health];
//! assert_eq!(halting(Autonomous::Heal, &active), Some(KillSwitch::Health));
//! assert_eq!(halting(Autonomous::Patrol, &active), None);
//!
//! // stand-down.lock stops everything.
//! let active = [KillSwitch::StandDown];
//! for c in Autonomous::ALL {
//!     assert_eq!(halting(c, &active), Some(KillSwitch::StandDown));
//! }
//! ```

use serde::Serialize;

/// A kill-switch file under `~/.cosmon/`. Presence means *stop* — the
/// fail-safe direction ADR-050 §3 fixed for every cosmon lockfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KillSwitch {
    /// `stand-down.lock` — the global switch. Stops every autonomous
    /// component. Also the default `kill_switch` path of the scheduler
    /// (ADR-050) and the daemon supervisor (ADR-053).
    StandDown,
    /// `health.off` — scoped: the ADR-137 heal pass and the API-stall
    /// propulsion sweep.
    Health,
    /// `autopilot.off` — scoped: the autopilot patrols (the nightly curate
    /// sweep and the step-level checks of the autopilot formulas).
    Autopilot,
    /// `ask.off` — scoped: `cs ask --execute` dispatch (ADR-071).
    Ask,
}

impl KillSwitch {
    /// Every switch, global first. The order is the display order of
    /// `cs status` and the precedence of [`halting`].
    pub const ALL: [Self; 4] = [Self::StandDown, Self::Health, Self::Autopilot, Self::Ask];

    /// The file name under `~/.cosmon/` whose presence activates the switch.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::StandDown => "stand-down.lock",
            Self::Health => "health.off",
            Self::Autopilot => "autopilot.off",
            Self::Ask => "ask.off",
        }
    }

    /// Whether this switch halts `component`. The global switch halts every
    /// component; a scoped switch halts only the ones it names.
    #[must_use]
    pub const fn halts(self, component: Autonomous) -> bool {
        match self {
            Self::StandDown => true,
            Self::Health => matches!(component, Autonomous::Heal | Autonomous::ApiStallSweep),
            Self::Autopilot => matches!(component, Autonomous::AutopilotPatrol),
            Self::Ask => matches!(component, Autonomous::AskDispatch),
        }
    }

    /// The components this switch halts, in [`Autonomous::ALL`] order. Used
    /// to render "what does this file stop?" without restating the table.
    #[must_use]
    pub fn scope(self) -> Vec<Autonomous> {
        Autonomous::ALL
            .into_iter()
            .filter(|c| self.halts(*c))
            .collect()
    }
}

/// A component that acts without an operator at the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Autonomous {
    /// `cosmon-scheduler` ticks (ADR-050).
    Scheduler,
    /// `cosmon-daemon-supervisor` respawns (ADR-053).
    DaemonRespawn,
    /// `cs patrol` remediation: respawn, propel, nudge, expire, orphan
    /// freeze, harvest, dialogue auto-confirm.
    Patrol,
    /// `cs patrol --heal` (ADR-137).
    Heal,
    /// `cs patrol --propel-api-stall`.
    ApiStallSweep,
    /// The curate sweep and the autopilot formulas' step checks.
    AutopilotPatrol,
    /// `cs ask --execute`.
    AskDispatch,
}

impl Autonomous {
    /// Every autonomous component.
    pub const ALL: [Self; 7] = [
        Self::Scheduler,
        Self::DaemonRespawn,
        Self::Patrol,
        Self::Heal,
        Self::ApiStallSweep,
        Self::AutopilotPatrol,
        Self::AskDispatch,
    ];

    /// Short stable label, as printed by `cs status`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Scheduler => "scheduler",
            Self::DaemonRespawn => "daemon-respawn",
            Self::Patrol => "patrol",
            Self::Heal => "heal",
            Self::ApiStallSweep => "api-stall-sweep",
            Self::AutopilotPatrol => "autopilot-patrol",
            Self::AskDispatch => "ask-dispatch",
        }
    }
}

/// The switch that halts `component` given the `active` set, if any. The
/// global switch wins over a scoped one, so a report names the broadest
/// reason the component is stopped.
#[must_use]
pub fn halting(component: Autonomous, active: &[KillSwitch]) -> Option<KillSwitch> {
    KillSwitch::ALL
        .into_iter()
        .find(|s| active.contains(s) && s.halts(component))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_down_halts_every_component() {
        for c in Autonomous::ALL {
            assert_eq!(
                halting(c, &[KillSwitch::StandDown]),
                Some(KillSwitch::StandDown),
                "{c:?}"
            );
        }
    }

    #[test]
    fn no_switch_halts_nothing() {
        for c in Autonomous::ALL {
            assert_eq!(halting(c, &[]), None, "{c:?}");
        }
    }

    #[test]
    fn every_switch_halts_something() {
        // Issue #108: no switch may exist only as a label.
        for s in KillSwitch::ALL {
            assert!(!s.scope().is_empty(), "{s:?} halts no component");
        }
    }

    #[test]
    fn scoped_switches_halt_only_their_components() {
        assert_eq!(
            KillSwitch::Health.scope(),
            [Autonomous::Heal, Autonomous::ApiStallSweep]
        );
        assert_eq!(KillSwitch::Autopilot.scope(), [Autonomous::AutopilotPatrol]);
        assert_eq!(KillSwitch::Ask.scope(), [Autonomous::AskDispatch]);
    }

    #[test]
    fn global_switch_is_reported_over_a_scoped_one() {
        let active = [KillSwitch::Health, KillSwitch::StandDown];
        assert_eq!(
            halting(Autonomous::Heal, &active),
            Some(KillSwitch::StandDown)
        );
    }
}
