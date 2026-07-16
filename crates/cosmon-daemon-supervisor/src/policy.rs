// SPDX-License-Identifier: AGPL-3.0-only

//! Pure decision helpers — throttle windows, kill-switch precedence.
//!
//! The supervisor's event loop is a thin orchestrator around three port
//! calls. Every interesting decision — "should I respawn now?", "is this
//! daemon muted by a kill-switch?", "when does the throttle elapse?" — is
//! a **pure** function in this module. That keeps the loop easy to read and
//! the logic easy to proptest without a runtime.

use chrono::{DateTime, Duration, Utc};

use crate::config::DaemonSpec;

// ---------------------------------------------------------------------------
// Kill-switch
// ---------------------------------------------------------------------------

/// Kill-switch verdict for one daemon.
///
/// Precedence (highest wins):
/// 1. Global `supervisor.kill_switch` present → [`Self::GlobalMute`].
/// 2. Per-daemon `kill_switch` present → [`Self::DaemonMute`].
/// 3. Daemon's own `enabled = false` → [`Self::Disabled`].
/// 4. Otherwise [`Self::Run`].
///
/// The ordering is the one `cosmon-scheduler` uses for patrols: global gets
/// to silence everything, per-daemon is a scalpel, `enabled` is the spec-
/// level opt-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSwitchDecision {
    /// No mute active — spawn/keep running as normal.
    Run,
    /// Global stand-down lock present; every child quiesces.
    GlobalMute,
    /// Per-daemon lock present; this child alone quiesces.
    DaemonMute,
    /// Spec has `enabled = false`; child is not started.
    Disabled,
}

impl KillSwitchDecision {
    /// `true` iff the decision disallows running the child.
    #[must_use]
    pub const fn is_muted(self) -> bool {
        !matches!(self, Self::Run)
    }
}

/// Compute the kill-switch decision for a single daemon, given whether the
/// global and per-daemon lockfiles exist *right now*. The filesystem probe
/// itself lives in the real adapter (Task 2); this function takes booleans
/// so it can be fuzzed without touching disk.
#[must_use]
pub fn kill_switch_decision(
    spec: &DaemonSpec,
    global_lock_present: bool,
    daemon_lock_present: bool,
) -> KillSwitchDecision {
    if global_lock_present {
        return KillSwitchDecision::GlobalMute;
    }
    // Per-daemon lock only meaningful if the spec declared one.
    if spec.kill_switch.is_some() && daemon_lock_present {
        return KillSwitchDecision::DaemonMute;
    }
    if !spec.enabled {
        return KillSwitchDecision::Disabled;
    }
    KillSwitchDecision::Run
}

// ---------------------------------------------------------------------------
// Respawn policy
// ---------------------------------------------------------------------------

/// What the supervisor should do with an exited child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnDecision {
    /// Respawn immediately (`throttle_seconds == 0` or throttle already
    /// elapsed when inspected).
    SpawnNow,
    /// Park the child until `until` has elapsed.
    ThrottleUntil(DateTime<Utc>),
    /// Do nothing — the child is muted by kill-switch or `enabled=false`.
    Quiesce,
}

/// Decide what to do when a child just exited (or is being evaluated
/// mid-throttle).
///
/// This is the single decision point for "when do we call the process port
/// again?". Feeding it a pure `now` + pure `exited_at` keeps it fuzzable and
/// independent of the wall clock.
#[must_use]
pub fn respawn_decision(
    spec: &DaemonSpec,
    exited_at: DateTime<Utc>,
    now: DateTime<Utc>,
    kill_switch: KillSwitchDecision,
) -> RespawnDecision {
    if kill_switch.is_muted() {
        return RespawnDecision::Quiesce;
    }
    if spec.throttle_seconds == 0 {
        return RespawnDecision::SpawnNow;
    }
    let until = throttle_deadline(spec, exited_at);
    if now >= until {
        RespawnDecision::SpawnNow
    } else {
        RespawnDecision::ThrottleUntil(until)
    }
}

/// Compute the wall-clock time at which a throttled child may respawn.
///
/// Saturating arithmetic: if `exited_at + throttle` overflows (hypothetical
/// astronomic timestamps), we return `DateTime<Utc>::MAX` so the supervisor
/// never spins.
#[must_use]
pub fn throttle_deadline(spec: &DaemonSpec, exited_at: DateTime<Utc>) -> DateTime<Utc> {
    // `chrono::Duration::try_seconds` returns None on overflow of its internal
    // `i64` seconds bound — which only happens for `> ~292 × 10⁹ years`. We
    // fall back to `DateTime::MAX` so the supervisor would simply stay
    // throttled rather than respawn aggressively.
    let Some(delta) =
        Duration::try_seconds(i64::try_from(spec.throttle_seconds).unwrap_or(i64::MAX))
    else {
        return DateTime::<Utc>::MAX_UTC;
    };
    exited_at
        .checked_add_signed(delta)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

// ---------------------------------------------------------------------------
// Crash-loop escape valve (task-20260608-1c59)
// ---------------------------------------------------------------------------

/// Decide whether a child's recent crash history warrants a `PropulsionDown`
/// alert on the operator-visible notify channel.
///
/// # Why this exists
///
/// The supervisor's `Exited → throttle → SpawnNow` policy is *correct* — it
/// keeps re-spawning a crashed child forever. But "forever re-spawning a
/// child that crashes on every boot" is **silent give-up dressed as
/// diligence**: nothing the operator watches ever fires. The failure
/// (ADR-053 ~:220) is subtle: a config
/// that parses but is semantically wrong hash-matches `keep`, the child
/// crash-loops, and the propulsion is dead all night with no signal.
///
/// The escape valve: after `threshold` (K) crashes land inside a rolling
/// `window` (W), the supervisor emits one operator-visible alert instead of
/// crash-looping in silence. The bug being fixed is **a missing event
/// nothing watches for** — so the fix is to *make the event exist*.
///
/// # Contract
///
/// `crash_times` is the list of recent crash timestamps for one child. The
/// function counts how many fall inside `[now - window, now]` and returns
/// `true` iff that count is at least `threshold`. It is pure — no clock, no
/// I/O — so it fuzzes without a runtime, matching every other decision in
/// this module. `threshold == 0` is treated as "disabled" (never alerts),
/// so an operator can switch the valve off with one config value.
#[must_use]
pub fn crash_loop_alert(
    crash_times: &[DateTime<Utc>],
    now: DateTime<Utc>,
    threshold: u32,
    window: Duration,
) -> bool {
    if threshold == 0 {
        return false;
    }
    let cutoff = now - window;
    let recent = crash_times.iter().filter(|t| **t >= cutoff).count();
    recent >= threshold as usize
}

/// Prune crash timestamps that have aged out of the rolling `window`.
///
/// Kept next to [`crash_loop_alert`] because the two are used together: the
/// event loop prunes on every recorded crash so the in-memory history stays
/// bounded and the "re-arm after recovery" semantics fall out for free —
/// once the old crashes age out and the count drops back below `threshold`,
/// the alert latch is cleared and a *fresh* crash loop will alert again.
#[must_use]
pub fn prune_crash_times(
    crash_times: &[DateTime<Utc>],
    now: DateTime<Utc>,
    window: Duration,
) -> Vec<DateTime<Utc>> {
    let cutoff = now - window;
    crash_times
        .iter()
        .copied()
        .filter(|t| *t >= cutoff)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn t(n: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(n, 0).unwrap()
    }

    fn base_spec() -> DaemonSpec {
        DaemonSpec {
            name: "x".into(),
            binary: "/bin/x".into(),
            args: vec![],
            throttle_seconds: 30,
            env: BTreeMap::new(),
            log_stdout: None,
            log_stderr: None,
            kill_switch: None,
            enabled: true,
        }
    }

    #[test]
    fn global_lock_beats_everything() {
        let mut spec = base_spec();
        spec.enabled = false;
        spec.kill_switch = Some("/tmp/local.lock".into());
        let d = kill_switch_decision(&spec, true, true);
        assert_eq!(d, KillSwitchDecision::GlobalMute);
    }

    #[test]
    fn daemon_lock_requires_declared_path() {
        let spec = base_spec();
        // Spec didn't declare a per-daemon kill-switch, so even if a lock
        // file exists at some phantom path it must not affect us.
        let d = kill_switch_decision(&spec, false, true);
        assert_eq!(d, KillSwitchDecision::Run);
    }

    #[test]
    fn daemon_lock_mutes_when_declared_and_present() {
        let mut spec = base_spec();
        spec.kill_switch = Some("/tmp/local.lock".into());
        let d = kill_switch_decision(&spec, false, true);
        assert_eq!(d, KillSwitchDecision::DaemonMute);
    }

    #[test]
    fn disabled_flag_prevents_spawn() {
        let mut spec = base_spec();
        spec.enabled = false;
        assert_eq!(
            kill_switch_decision(&spec, false, false),
            KillSwitchDecision::Disabled
        );
    }

    #[test]
    fn respawn_waits_until_throttle_elapses() {
        let spec = base_spec(); // throttle_seconds = 30
        let d = respawn_decision(&spec, t(0), t(10), KillSwitchDecision::Run);
        assert_eq!(d, RespawnDecision::ThrottleUntil(t(30)));
    }

    #[test]
    fn respawn_fires_when_throttle_has_elapsed() {
        let spec = base_spec();
        let d = respawn_decision(&spec, t(0), t(31), KillSwitchDecision::Run);
        assert_eq!(d, RespawnDecision::SpawnNow);
    }

    #[test]
    fn respawn_zero_throttle_is_spawn_now() {
        let mut spec = base_spec();
        spec.throttle_seconds = 0;
        let d = respawn_decision(&spec, t(0), t(0), KillSwitchDecision::Run);
        assert_eq!(d, RespawnDecision::SpawnNow);
    }

    #[test]
    fn muted_child_is_always_quiesced() {
        let spec = base_spec();
        for muted in [
            KillSwitchDecision::GlobalMute,
            KillSwitchDecision::DaemonMute,
            KillSwitchDecision::Disabled,
        ] {
            let d = respawn_decision(&spec, t(0), t(1_000_000), muted);
            assert_eq!(d, RespawnDecision::Quiesce, "muted state: {muted:?}");
        }
    }

    #[test]
    fn throttle_deadline_uses_spec_seconds() {
        let mut spec = base_spec();
        spec.throttle_seconds = 45;
        assert_eq!(throttle_deadline(&spec, t(100)), t(145));
    }

    #[test]
    fn throttle_deadline_saturates_on_overflow() {
        let mut spec = base_spec();
        spec.throttle_seconds = u64::MAX;
        // Just verify it doesn't panic and stays at MAX.
        let d = throttle_deadline(&spec, t(0));
        assert_eq!(d, DateTime::<Utc>::MAX_UTC);
    }

    // --- crash-loop escape valve (task-20260608-1c59) ----------------------

    #[test]
    fn crash_loop_below_threshold_does_not_alert() {
        // 2 crashes in the window, K = 3 → no alert yet.
        let crashes = [t(100), t(110)];
        assert!(!crash_loop_alert(
            &crashes,
            t(120),
            3,
            Duration::seconds(300)
        ));
    }

    #[test]
    fn crash_loop_at_threshold_alerts() {
        // 3 crashes inside a 300s window, K = 3 → alert.
        let crashes = [t(100), t(150), t(200)];
        assert!(crash_loop_alert(
            &crashes,
            t(250),
            3,
            Duration::seconds(300)
        ));
    }

    #[test]
    fn crash_loop_ignores_crashes_outside_window() {
        // Two ancient crashes + one recent: only the recent one counts, so
        // K = 2 must NOT alert. This is the "re-arm after recovery" path —
        // a child that was healthy for the window then crashes once is not
        // a crash loop.
        let crashes = [t(0), t(10), t(1000)];
        assert!(!crash_loop_alert(
            &crashes,
            t(1010),
            2,
            Duration::seconds(300)
        ));
    }

    #[test]
    fn crash_loop_threshold_zero_disables_the_valve() {
        let crashes = [t(1), t(2), t(3), t(4), t(5)];
        assert!(!crash_loop_alert(&crashes, t(6), 0, Duration::seconds(300)));
    }

    #[test]
    fn prune_drops_only_aged_out_timestamps() {
        let crashes = [t(0), t(100), t(250)];
        // Window = 200s, now = 300 → cutoff = 100. t(0) drops, t(100) and
        // t(250) survive (cutoff is inclusive).
        let pruned = prune_crash_times(&crashes, t(300), Duration::seconds(200));
        assert_eq!(pruned, vec![t(100), t(250)]);
    }

    proptest::proptest! {
        /// Pruning never invents a timestamp, never keeps an aged-out one,
        /// and is idempotent — pruning twice equals pruning once. These are
        /// the invariants the event loop relies on to keep `crash_times`
        /// bounded without losing an in-window crash.
        #[test]
        fn prune_is_sound_and_idempotent(
            secs in proptest::collection::vec(0i64..10_000, 0..50),
            now in 0i64..10_000,
            window_s in 1i64..5_000,
        ) {
            let times: Vec<DateTime<Utc>> = secs.iter().map(|s| t(*s)).collect();
            let now = t(now);
            let window = Duration::seconds(window_s);
            let pruned = prune_crash_times(&times, now, window);
            // Every survivor was in the input and inside the window.
            for p in &pruned {
                proptest::prop_assert!(times.contains(p));
                proptest::prop_assert!(*p >= now - window);
            }
            // No in-window timestamp was dropped.
            let kept_count = times.iter().filter(|x| **x >= now - window).count();
            proptest::prop_assert_eq!(pruned.len(), kept_count);
            // Idempotent.
            let twice = prune_crash_times(&pruned, now, window);
            proptest::prop_assert_eq!(pruned, twice);
        }

        /// The alert decision is monotone in the crash count: pruning (which
        /// only ever removes timestamps) can never turn a non-alert into an
        /// alert. A fired valve is always justified by enough in-window
        /// crashes.
        #[test]
        fn alert_is_justified_by_in_window_count(
            secs in proptest::collection::vec(0i64..10_000, 0..50),
            now in 0i64..10_000,
            window_s in 1i64..5_000,
            threshold in 1u32..20,
        ) {
            let times: Vec<DateTime<Utc>> = secs.iter().map(|s| t(*s)).collect();
            let now = t(now);
            let window = Duration::seconds(window_s);
            let fired = crash_loop_alert(&times, now, threshold, window);
            let in_window = times.iter().filter(|x| **x >= now - window).count();
            proptest::prop_assert_eq!(fired, in_window >= threshold as usize);
            // Pruning cannot manufacture an alert.
            let pruned = prune_crash_times(&times, now, window);
            let fired_after = crash_loop_alert(&pruned, now, threshold, window);
            proptest::prop_assert_eq!(fired, fired_after);
        }
    }

    #[test]
    fn prune_then_alert_models_the_event_loop_cycle() {
        // Mirror the loop: prune, push the new crash, then test the alert.
        let window = Duration::seconds(60);
        let mut history: Vec<DateTime<Utc>> = vec![t(0), t(5)]; // old
                                                                // A fresh crash at t(100): the old ones age out.
        let now = t(100);
        history = prune_crash_times(&history, now, window);
        history.push(now);
        // Only one crash in-window → K=3 does not alert.
        assert!(!crash_loop_alert(&history, now, 3, window));

        // Two more rapid crashes inside the window.
        history.push(t(110));
        history.push(t(120));
        assert!(
            crash_loop_alert(&history, t(120), 3, window),
            "three crashes within 60s must alert"
        );
    }
}
