// SPDX-License-Identifier: AGPL-3.0-only

//! Pure tick evaluator — produces one [`Decision`] per patrol.
//!
//! ## What evaluation computes (Step 2 — complete)
//!
//! Decisions are produced in this order; the first that matches wins:
//!
//! 1. Schema-level cadence validity (`interval_seconds` XOR `cron`) →
//!    [`Decision::Invalid`].
//! 2. `enabled = false` → `WouldSkip("disabled")`.
//! 3. Global kill-switch path exists → `WouldSkip("global kill-switch present")`.
//! 4. Per-patrol kill-switch path exists → `WouldSkip("kill-switch present")`.
//! 5. Missing `require_env` variable → `WouldSkip("required env var X unset")`.
//! 6. **Cadence gate** (new in Step 2):
//!    - For `interval_seconds = N`: fire iff `last_fired_at.is_none()` or
//!      `now - last_fired_at >= N`.
//!    - For `cron = "…"`: fire iff `CronExpr::matches(now)` **and**
//!      `floor_minute(last_fired_at) < floor_minute(now)` (de-duplicates
//!      a match window shorter than the tick interval).
//!
//! ## What Step 2 deliberately does **not** do
//!
//! - Catch-up on missed minutes for cron. A machine asleep through
//!   Sunday 9:00 does not fire last week's digest on Monday; that is a
//!   feature, not a gap (see `cron.rs` docstring).
//! - Jitter. A tick that evaluates "due since T+300s" always fires; we
//!   don't spread load across ticks. Patrols are minute-scale; this is
//!   fine.

use std::path::PathBuf;

use chrono::{DateTime, Timelike, Utc};

use crate::config::{Config, Patrol, Sunset, SunsetStrategy};
use crate::convergence::{
    operator_trigger_predicate, read_samples_tolerant, sample_count_predicate,
    variance_threshold_predicate,
};
use crate::cron::CronExpr;
use crate::decision::Decision;
use crate::environment::{shellexpand_home, Environment};
use crate::state::{PatrolState, SchedulerState};

/// Evaluate every patrol in `cfg` under `env` + `state` and return a
/// vector of `(patrol_name, decision)` pairs in declaration order. Pure
/// function: no I/O, no process spawning, no state mutation.
///
/// Step 2 upgraded this from the scaffold — it now consults `state` for
/// interval and cron cadence gates. The signature is stable; future
/// additions (e.g. a jitter policy) go on [`Environment`] or [`Patrol`],
/// not on `tick` itself.
pub fn tick<E: Environment + ?Sized>(
    cfg: &Config,
    env: &E,
    state: &SchedulerState,
) -> Vec<(String, Decision)> {
    let global_kill = env.path_exists(&cfg.scheduler.kill_switch);
    let now = env.now();

    cfg.patrols
        .iter()
        .map(|p| {
            let prior = state.patrols.get(&p.name);
            (
                p.name.clone(),
                evaluate_one(p, env, global_kill, now, prior),
            )
        })
        .collect()
}

/// Decide the outcome for a single patrol. Extracted for direct unit
/// testing. `prior` is `None` if the patrol has never fired (first boot
/// or newly added to TOML).
fn evaluate_one<E: Environment + ?Sized>(
    patrol: &Patrol,
    env: &E,
    global_kill: bool,
    now: DateTime<Utc>,
    prior: Option<&PatrolState>,
) -> Decision {
    if let Err(reason) = patrol.validate_cadence() {
        return Decision::invalid(reason.to_owned());
    }
    if !patrol.enabled {
        return Decision::skip("disabled");
    }
    // Idempotent short-circuit — once the sunset action has run,
    // never re-evaluate the patrol. This fence prevents a double-
    // unload window between the first sunset and state propagation.
    if prior.and_then(|p| p.sunset_decided_at).is_some() {
        return Decision::skip("already sunsetted");
    }
    if global_kill {
        return Decision::skip("global kill-switch present");
    }
    if let Some(ks) = &patrol.kill_switch {
        if env.path_exists(ks) {
            return Decision::skip("kill-switch present");
        }
    }
    for var in &patrol.require_env {
        if !env.env_var_set(var) {
            return Decision::skip(format!("required env var {var} unset"));
        }
    }
    // Convergence gate: when a `[patrol.sunset]` rule is attached and its
    // predicate fires, the dispatcher gets `WouldSunset` and runs the
    // sunset action instead of the normal command.
    if let Some(sunset) = &patrol.sunset {
        if let Some(decision) = sunset_gate(sunset, env) {
            return decision;
        }
    }
    cadence_gate(patrol, now, prior)
}

/// Evaluate the `[patrol.sunset]` rule attached to a patrol.
///
/// Returns `Some(Decision::WouldSunset { .. })` when the strategy's
/// predicate fires on the current sample (or trigger-file) state, and
/// `None` when the patrol should keep collecting. Never returns
/// `WouldFire` / `WouldSkip` — those remain the cadence gate's job.
fn sunset_gate<E: Environment + ?Sized>(sunset: &Sunset, env: &E) -> Option<Decision> {
    match sunset.strategy {
        SunsetStrategy::VarianceThreshold => {
            let sample_file = sunset.sample_file.as_deref()?;
            let path = PathBuf::from(shellexpand_home(sample_file).into_owned());
            let read = read_samples_tolerant(&path);
            // Validator guarantees `variance_threshold` is meaningful when
            // the strategy is variance-threshold; default to a conservative
            // 0.05 if unset (validated only at dispatch today).
            let threshold = sunset.variance_threshold?;
            let window = usize::try_from(sunset.window?).ok()?;
            let min_samples = sunset.min_samples.unwrap_or(0);
            if variance_threshold_predicate(window, min_samples, threshold, &read.values) {
                Some(Decision::sunset(format!(
                    "variance-threshold converged (σ² < {threshold}, window={window}, \
                     samples={have})",
                    have = read.values.len()
                )))
            } else {
                None
            }
        }
        SunsetStrategy::SampleCount => {
            let sample_file = sunset.sample_file.as_deref()?;
            let path = PathBuf::from(shellexpand_home(sample_file).into_owned());
            let read = read_samples_tolerant(&path);
            let target = sunset.min_samples?;
            #[allow(clippy::cast_possible_truncation)]
            let have = read.values.len() as u64;
            if sample_count_predicate(have, target) {
                Some(Decision::sunset(format!(
                    "sample-count reached {have} (target {target})"
                )))
            } else {
                None
            }
        }
        SunsetStrategy::OperatorTriggerOnly => {
            if operator_trigger_predicate(env, sunset.trigger_file.as_deref()) {
                Some(Decision::sunset(format!(
                    "operator-trigger file present: {}",
                    sunset.trigger_file.as_deref().unwrap_or("<none>")
                )))
            } else {
                None
            }
        }
    }
}

/// The time-based gate: interval or cron, depending on which field the
/// operator set. `Patrol::validate_cadence` guarantees exactly one is
/// `Some` by the time we get here.
fn cadence_gate(patrol: &Patrol, now: DateTime<Utc>, prior: Option<&PatrolState>) -> Decision {
    if let Some(secs) = patrol.interval_seconds {
        return interval_gate(secs, now, prior);
    }
    if let Some(expr) = patrol.cron.as_deref() {
        return cron_gate(expr, now, prior);
    }
    // Unreachable after validate_cadence; guard defensively.
    Decision::invalid("no cadence after validation".to_owned())
}

fn interval_gate(
    interval_seconds: u64,
    now: DateTime<Utc>,
    prior: Option<&PatrolState>,
) -> Decision {
    let Some(last) = prior.and_then(|p| p.last_fired_at) else {
        return Decision::WouldFire;
    };
    let elapsed_i64 = (now - last).num_seconds();
    // `elapsed_i64 < 0` means the clock moved backward (NTP step,
    // operator editing the state file by hand). Treat as "due" —
    // safer to re-fire a patrol than to stall forever on a negative
    // delta. `u64::try_from` narrows safely for the non-negative path.
    let Ok(elapsed) = u64::try_from(elapsed_i64) else {
        return Decision::WouldFire;
    };
    if elapsed >= interval_seconds {
        Decision::WouldFire
    } else {
        let remaining = interval_seconds - elapsed;
        Decision::skip(format!("not due yet; {remaining}s until next fire"))
    }
}

fn cron_gate(expr: &str, now: DateTime<Utc>, prior: Option<&PatrolState>) -> Decision {
    let parsed = match CronExpr::parse(expr) {
        Ok(p) => p,
        Err(e) => return Decision::invalid(format!("cron parse error: {e}")),
    };
    if !parsed.matches(now) {
        return Decision::skip("not due yet (cron does not match this minute)");
    }
    // Prevent re-firing inside the same minute. If last_fired_at floors
    // to the same minute as `now`, we already fired this match window.
    if let Some(last) = prior.and_then(|p| p.last_fired_at) {
        if floor_minute(last) >= floor_minute(now) {
            return Decision::skip("already fired this minute");
        }
    }
    Decision::WouldFire
}

fn floor_minute(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::StubEnv;
    use chrono::TimeZone;

    fn cfg_from(raw: &str) -> Config {
        Config::from_str_validated(raw).expect("valid toml")
    }

    /// Fixed now in tests: 2026-04-19 09:00:00 UTC. Chosen so the cron
    /// expression `0 9 * * 0` (Sunday 9am local) has a deterministic
    /// outcome only if the operator's local TZ is UTC. Tests that need
    /// stricter timezone control use explicit `Local` conversions.
    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 19, 9, 0, 0).unwrap()
    }

    fn state_with_last_fire(name: &str, last: DateTime<Utc>) -> SchedulerState {
        let mut s = SchedulerState::default();
        s.patrols.insert(
            name.to_owned(),
            PatrolState {
                last_fired_at: Some(last),
                last_exit_code: None,
                last_pid: None,
                fire_count: 1,
                sunset_decided_at: None,
            },
        );
        s
    }

    fn state_with_sunset_decided(name: &str, at: DateTime<Utc>) -> SchedulerState {
        let mut s = SchedulerState::default();
        s.patrols.insert(
            name.to_owned(),
            PatrolState {
                last_fired_at: None,
                last_exit_code: None,
                last_pid: None,
                fire_count: 0,
                sunset_decided_at: Some(at),
            },
        );
        s
    }

    #[test]
    fn disabled_patrol_skips() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/nothing"
            tick_interval_seconds = 60

            [[patrol]]
            name = "off"
            interval_seconds = 60
            command = ["echo"]
            enabled = false
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let state = SchedulerState::default();
        let decisions = tick(&cfg, &env, &state);
        assert_eq!(decisions.len(), 1);
        assert!(matches!(
            decisions[0].1,
            Decision::WouldSkip { ref reason } if reason == "disabled"
        ));
    }

    #[test]
    fn global_kill_switch_skips_everything() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/stand-down"
            tick_interval_seconds = 60

            [[patrol]]
            name = "a"
            interval_seconds = 60
            command = ["echo", "a"]

            [[patrol]]
            name = "b"
            cron = "0 * * * *"
            command = ["echo", "b"]
        "#,
        );
        let env = StubEnv::default()
            .with_path("/tmp/stand-down")
            .with_now(fixed_now());
        let state = SchedulerState::default();
        let decisions = tick(&cfg, &env, &state);
        assert_eq!(decisions.len(), 2);
        for (_, d) in &decisions {
            assert!(matches!(
                d,
                Decision::WouldSkip { reason } if reason == "global kill-switch present"
            ));
        }
    }

    #[test]
    fn interval_first_fire_without_prior_state() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "fresh"
            interval_seconds = 300
            command = ["echo"]
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn interval_skips_when_not_due() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "pulse"
            interval_seconds = 300
            command = ["echo"]
        "#,
        );
        let now = fixed_now();
        // Fired 60s ago — 240s remaining.
        let state = state_with_last_fire("pulse", now - chrono::Duration::seconds(60));
        let env = StubEnv::default().with_now(now);
        let decisions = tick(&cfg, &env, &state);
        assert!(matches!(
            decisions[0].1,
            Decision::WouldSkip { ref reason } if reason.contains("not due yet")
        ));
    }

    #[test]
    fn interval_fires_when_elapsed_exceeds_interval() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "pulse"
            interval_seconds = 300
            command = ["echo"]
        "#,
        );
        let now = fixed_now();
        let state = state_with_last_fire("pulse", now - chrono::Duration::seconds(301));
        let env = StubEnv::default().with_now(now);
        let decisions = tick(&cfg, &env, &state);
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn clock_skew_forces_fire_rather_than_stall() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "pulse"
            interval_seconds = 300
            command = ["echo"]
        "#,
        );
        let now = fixed_now();
        // last_fired_at in the future by 2h (NTP step back in time)
        let state = state_with_last_fire("pulse", now + chrono::Duration::hours(2));
        let env = StubEnv::default().with_now(now);
        let decisions = tick(&cfg, &env, &state);
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn cron_fires_at_matching_minute_without_prior_state() {
        // Use an every-minute cron so the test is timezone-independent.
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "every-minute"
            cron = "* * * * *"
            command = ["echo"]
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn cron_deduplicates_same_minute() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "every-minute"
            cron = "* * * * *"
            command = ["echo"]
        "#,
        );
        let now = fixed_now();
        let state = state_with_last_fire("every-minute", now); // same minute
        let env = StubEnv::default().with_now(now);
        let decisions = tick(&cfg, &env, &state);
        assert!(matches!(
            decisions[0].1,
            Decision::WouldSkip { ref reason } if reason == "already fired this minute"
        ));
    }

    #[test]
    fn cron_fires_next_minute_after_prior_fire() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "every-minute"
            cron = "* * * * *"
            command = ["echo"]
        "#,
        );
        let now = fixed_now();
        let last = now - chrono::Duration::seconds(61);
        let state = state_with_last_fire("every-minute", last);
        let env = StubEnv::default().with_now(now);
        let decisions = tick(&cfg, &env, &state);
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn cron_invalid_expression_marks_patrol_invalid() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "broken"
            cron = "not a cron"
            command = ["echo"]
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(matches!(decisions[0].1, Decision::Invalid { .. }));
    }

    #[test]
    fn order_is_preserved() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "first"
            interval_seconds = 60
            command = ["echo", "1"]

            [[patrol]]
            name = "second"
            interval_seconds = 60
            command = ["echo", "2"]

            [[patrol]]
            name = "third"
            interval_seconds = 60
            command = ["echo", "3"]
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        let names: Vec<&str> = decisions.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }

    #[test]
    fn missing_require_env_still_skips_with_new_api() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "gated"
            interval_seconds = 60
            command = ["echo"]
            require_env = ["MUST_BE_SET"]
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(matches!(
            decisions[0].1,
            Decision::WouldSkip { ref reason }
                if reason == "required env var MUST_BE_SET unset"
        ));
    }

    // ----- sunset gate ---------------------------------------------------

    #[test]
    fn sunset_variance_threshold_first_tick_converges() {
        // Stationary series in a real tempfile; sunset gate should fire.
        let tmp = tempfile::tempdir().unwrap();
        let sample_path = tmp.path().join("stationary.tsv");
        let body: String = (0..60).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "0.{}", 500 + (i % 3));
            acc
        });
        std::fs::write(&sample_path, body).unwrap();
        let sample_str = sample_path.to_string_lossy().into_owned();

        let cfg = cfg_from(&format!(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "u2-probe"
            interval_seconds = 300
            command = ["echo"]
            [patrol.sunset]
            strategy = "variance-threshold"
            sample_file = "{sample_str}"
            min_samples = 30
            variance_threshold = 0.01
            window = 10
        "#
        ));

        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(
            matches!(decisions[0].1, Decision::WouldSunset { ref reason }
                if reason.contains("variance-threshold")),
            "expected WouldSunset, got {:?}",
            decisions[0].1
        );
    }

    #[test]
    fn sunset_second_tick_after_persist_short_circuits_to_skip() {
        // Same config as the converged case; state now records that we
        // already sunsetted. Next tick must be `WouldSkip("already
        // sunsetted")` — no WouldSunset, no WouldFire — regardless of
        // whether the sample file would still converge.
        let tmp = tempfile::tempdir().unwrap();
        let sample_path = tmp.path().join("stationary.tsv");
        let body: String = (0..60).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "0.{}", 500 + (i % 3));
            acc
        });
        std::fs::write(&sample_path, body).unwrap();
        let sample_str = sample_path.to_string_lossy().into_owned();

        let cfg = cfg_from(&format!(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "u2-probe"
            interval_seconds = 300
            command = ["echo"]
            [patrol.sunset]
            strategy = "variance-threshold"
            sample_file = "{sample_str}"
            min_samples = 30
            variance_threshold = 0.01
            window = 10
        "#
        ));

        let env = StubEnv::default().with_now(fixed_now());
        let state = state_with_sunset_decided("u2-probe", fixed_now());
        let decisions = tick(&cfg, &env, &state);
        assert!(
            matches!(decisions[0].1, Decision::WouldSkip { ref reason }
                if reason == "already sunsetted"),
            "expected already-sunsetted skip, got {:?}",
            decisions[0].1
        );
    }

    #[test]
    fn sunset_not_enough_samples_does_not_converge() {
        let tmp = tempfile::tempdir().unwrap();
        let sample_path = tmp.path().join("few.tsv");
        std::fs::write(&sample_path, "0.5\n0.5\n0.5\n").unwrap();
        let sample_str = sample_path.to_string_lossy().into_owned();

        let cfg = cfg_from(&format!(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "probe"
            interval_seconds = 60
            command = ["echo"]
            [patrol.sunset]
            strategy = "variance-threshold"
            sample_file = "{sample_str}"
            min_samples = 30
            variance_threshold = 0.01
            window = 10
        "#
        ));

        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        // 3 < min_samples=30, so gate does not fire; cadence_gate runs.
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn sunset_sample_count_reached_fires_sunset() {
        let tmp = tempfile::tempdir().unwrap();
        let sample_path = tmp.path().join("count.tsv");
        let body: String = (0..100).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "{i}");
            acc
        });
        std::fs::write(&sample_path, body).unwrap();
        let sample_str = sample_path.to_string_lossy().into_owned();

        let cfg = cfg_from(&format!(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "countdown"
            interval_seconds = 60
            command = ["echo"]
            [patrol.sunset]
            strategy = "sample-count"
            sample_file = "{sample_str}"
            min_samples = 50
        "#
        ));

        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(
            matches!(decisions[0].1, Decision::WouldSunset { ref reason }
                if reason.contains("sample-count")),
            "expected sample-count sunset, got {:?}",
            decisions[0].1
        );
    }

    #[test]
    fn sunset_operator_trigger_fires_when_file_present() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "manual"
            interval_seconds = 60
            command = ["echo"]
            [patrol.sunset]
            strategy = "operator-trigger-only"
            trigger_file = "/tmp/stop-me"
        "#,
        );
        let env = StubEnv::default()
            .with_path("/tmp/stop-me")
            .with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(matches!(decisions[0].1, Decision::WouldSunset { .. }));
    }

    #[test]
    fn sunset_unload_failure_does_not_loop_second_tick() {
        // Scenario: first tick converges → caller runs sunset action,
        // advisory unload fails, but flag is still flipped. Second tick
        // must NOT re-fire the sunset even if the sample file still
        // converges and even though the unload never succeeded. This is
        // the idempotence contract of the task.
        //
        // We simulate the post-failure state by presenting the tick
        // with `sunset_decided_at = Some(ts)` plus a still-converging
        // sample file. The gate must short-circuit to "already sunsetted".
        let tmp = tempfile::tempdir().unwrap();
        let sample_path = tmp.path().join("still-stationary.tsv");
        let body: String = (0..60).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "0.{}", 500 + (i % 3));
            acc
        });
        std::fs::write(&sample_path, body).unwrap();
        let sample_str = sample_path.to_string_lossy().into_owned();

        let cfg = cfg_from(&format!(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "u2-probe"
            interval_seconds = 300
            command = ["echo"]
            [patrol.sunset]
            strategy = "variance-threshold"
            sample_file = "{sample_str}"
            min_samples = 30
            variance_threshold = 0.01
            window = 10
            launchctl_plist = "/tmp/nonexistent.plist"
        "#
        ));

        let env = StubEnv::default().with_now(fixed_now());
        // After the first tick succeeded (even if advisory unload failed),
        // state carries `sunset_decided_at = Some(…)`.
        let state = state_with_sunset_decided("u2-probe", fixed_now());
        let decisions = tick(&cfg, &env, &state);
        assert!(
            matches!(decisions[0].1, Decision::WouldSkip { ref reason }
                if reason == "already sunsetted"),
            "second tick must short-circuit regardless of unload failure; got {:?}",
            decisions[0].1
        );
    }

    #[test]
    fn sunset_gate_ignored_without_sunset_block() {
        // Patrol with no [patrol.sunset] should behave exactly as before.
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/none"
            tick_interval_seconds = 60

            [[patrol]]
            name = "vanilla"
            interval_seconds = 300
            command = ["echo"]
        "#,
        );
        let env = StubEnv::default().with_now(fixed_now());
        let decisions = tick(&cfg, &env, &SchedulerState::default());
        assert!(matches!(decisions[0].1, Decision::WouldFire));
    }

    #[test]
    fn per_patrol_kill_switch_skips_only_that_one() {
        let cfg = cfg_from(
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "/tmp/no-global"
            tick_interval_seconds = 60

            [[patrol]]
            name = "quiet"
            interval_seconds = 60
            command = ["echo"]
            kill_switch = "/tmp/quiet.lock"

            [[patrol]]
            name = "noisy"
            interval_seconds = 60
            command = ["echo"]
        "#,
        );
        let env = StubEnv::default()
            .with_path("/tmp/quiet.lock")
            .with_now(fixed_now());
        let state = SchedulerState::default();
        let decisions = tick(&cfg, &env, &state);
        assert_eq!(decisions.len(), 2);
        assert!(matches!(
            decisions[0].1,
            Decision::WouldSkip { ref reason } if reason == "kill-switch present"
        ));
        assert!(matches!(decisions[1].1, Decision::WouldFire));
    }
}
