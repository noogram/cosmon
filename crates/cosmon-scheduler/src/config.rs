// SPDX-License-Identifier: AGPL-3.0-only

//! TOML schema for the patrol scheduler.
//!
//! The schema mirrors [idea-20260417-b52d's plan.md §"TOML schema (reference,
//! v1)"](../../../.cosmon/state/fleets/default/molecules/idea-20260417-b52d/plan.md).
//! Two discipline choices are worth calling out:
//!
//! 1. **Root is tolerant, patrols are strict.** We accept future root-level
//!    additions silently so old binaries keep working after a schema bump,
//!    but a typo inside `[[patrol]]` (e.g. `intervall_seconds`) must fail
//!    loudly because one drifted field is the difference between "fires
//!    every 5 minutes" and "never fires".
//!
//! 2. **`interval_seconds` XOR `cron`.** Exactly one cadence per patrol.
//!    Both unset or both set is a schema error. The validator lives on
//!    [`Patrol::validate_cadence`] so `Config::validate` can surface *all*
//!    invalid patrols in one pass rather than stopping on the first.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Root TOML document. Tolerant of unknown top-level keys so forward-compatible
/// field additions do not brick older binaries in the field.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Global scheduler settings.
    pub scheduler: SchedulerConfig,

    /// Declared patrols. Order is preserved for deterministic dry-run output.
    #[serde(default, rename = "patrol")]
    pub patrols: Vec<Patrol>,
}

/// Scheduler-wide settings applied to every tick.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SchedulerConfig {
    /// Path to the atomic state file (last-fire timestamps, counters).
    pub state_file: String,

    /// Path to the aggregate log file written on every tick.
    pub log_file: String,

    /// Path to the global kill-switch. If present, the scheduler treats the
    /// fleet as stood down and skips every patrol with reason
    /// `"global kill-switch present"`.
    pub kill_switch: String,

    /// Expected invocation cadence from the launchd trigger (informational;
    /// used by interval accounting in Step 2).
    pub tick_interval_seconds: u64,
}

/// A single patrol entry. Strict: unknown fields are a hard error.
///
/// Cadence invariant: exactly one of [`Self::interval_seconds`] or
/// [`Self::cron`] must be set. Validated by [`Self::validate_cadence`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Patrol {
    /// Unique name across the file. Used for state-file keying and log prefix.
    pub name: String,

    /// Fire every N seconds (simple cadence). XOR with [`Self::cron`].
    #[serde(default)]
    pub interval_seconds: Option<u64>,

    /// POSIX 5-field cron expression. XOR with [`Self::interval_seconds`].
    #[serde(default)]
    pub cron: Option<String>,

    /// Argv to spawn. Element 0 is resolved against `PATH` unless absolute.
    pub command: Vec<String>,

    /// Optional working directory (shell-expandable, e.g. `~/dev/foo`).
    #[serde(default)]
    pub working_dir: Option<String>,

    /// Environment variables merged onto the inherited process environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Per-patrol kill-switch path. If present, this patrol alone is skipped.
    #[serde(default)]
    pub kill_switch: Option<String>,

    /// Per-patrol log file. Falls back to scheduler-wide `log_file` when unset.
    #[serde(default)]
    pub log_file: Option<String>,

    /// Dispatch mode. Reserved for Step 2; today only `"detached"` is planned.
    #[serde(default = "default_dispatch")]
    pub dispatch: String,

    /// Environment variables that MUST be set in the invoking shell, otherwise
    /// the patrol is skipped with reason `"required env var X unset"`.
    #[serde(default)]
    pub require_env: Vec<String>,

    /// Upper bound on runtime (metadata only for now — enforcement in Step 2).
    #[serde(default)]
    pub timeout_seconds: Option<u64>,

    /// When `false`, the patrol is always skipped with reason `"disabled"`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Optional auto-sunset rule. When set, the scheduler evaluates a
    /// convergence metric every tick and, once the criterion fires, marks
    /// the patrol sunsetted (single-write, idempotent) and stops dispatching.
    /// See [`Sunset`] for the strategies and their required fields.
    #[serde(default)]
    pub sunset: Option<Sunset>,
}

/// Auto-sunset rule attached to a patrol.
///
/// Cosmon patrols are long-running by design; some (probes, campaigns) want
/// a *convergence-based* stop condition rather than an arbitrary calendar
/// timer. `Sunset` declares the rule to evaluate, the data source to read,
/// and the optional hooks to fire when the rule fires.
///
/// The struct is flat (rather than an enum-per-strategy) because TOML does
/// not nest internally-tagged enums cleanly with `deny_unknown_fields`. The
/// per-strategy field requirements are enforced by [`Self::validate`],
/// which collects *all* violations and returns them together so the operator
/// sees the whole picture in one `cosmon-scheduler tick --dry-run` run.
///
/// Invariants (checked by [`Self::validate`]):
///
/// - `min_samples` (when set) must be `>= 1`. Zero is meaningless and almost
///   always a typo; it would sunset on the very first empty tick.
/// - `strategy = "variance-threshold"` requires `sample_file`. Without a
///   data source there is no series to compute variance over.
/// - `strategy = "sample-count"` requires both `sample_file` and `min_samples`.
/// - `strategy = "operator-trigger-only"` has no required data fields; the
///   gate is flipped by the operator via `cs patrol sunset <name>` (future)
///   or a trigger file path (informational today).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // `on_sunset` is the stable TOML key
pub struct Sunset {
    /// Which convergence rule to evaluate.
    pub strategy: SunsetStrategy,

    /// TSV file the evaluator reads to compute the convergence metric.
    /// Required for `variance-threshold` and `sample-count`; ignored by
    /// `operator-trigger-only`.
    #[serde(default)]
    pub sample_file: Option<String>,

    /// Minimum sample count the data source must hold before the gate can
    /// fire. Guards against premature sunset on a sparsely-populated file.
    /// Required for `sample-count`; advisory for `variance-threshold`.
    /// Must be `>= 1` when set.
    #[serde(default)]
    pub min_samples: Option<u64>,

    /// Variance ceiling below which the series is declared stationary.
    /// Only meaningful for `variance-threshold`; ignored otherwise.
    #[serde(default)]
    pub variance_threshold: Option<f64>,

    /// Window size (in samples) for the rolling variance estimator.
    /// Only meaningful for `variance-threshold`; ignored otherwise.
    #[serde(default)]
    pub window: Option<u64>,

    /// Optional path to an operator-controlled trigger file. Only meaningful
    /// for `operator-trigger-only`; the scheduler sunsets the patrol once
    /// the file exists. Advisory metadata today.
    #[serde(default)]
    pub trigger_file: Option<String>,

    /// Optional launchd plist path (or label) to unload when the sunset
    /// action runs. Advisory — the scheduler runs `launchctl unload` as a
    /// best-effort side-effect; a failure emits a
    /// `patrol.sunset_unload_failed` event but does **not** block the
    /// sunset: `sunset_decided_at` is still set so subsequent ticks
    /// short-circuit.
    #[serde(default)]
    pub launchctl_plist: Option<String>,

    /// Hooks to fire on sunset, in order. Known hooks are defined by
    /// downstream integration (e.g. `"notify_telegram"`). Unknown hook
    /// names are accepted at parse time and only validated at dispatch.
    #[serde(default)]
    pub on_sunset: Vec<String>,
}

/// Convergence rule discriminant. Serialized in kebab-case to match the
/// rest of the TOML vocabulary. Unknown variants are rejected by serde at
/// parse time, so a typo like `"varience-threshold"` fails loudly before
/// the scheduler ever ticks.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SunsetStrategy {
    /// Sunset once the rolling variance of `sample_file` falls below
    /// `variance_threshold` (after at least `min_samples`, if set).
    VarianceThreshold,

    /// Sunset once the sample count in `sample_file` reaches `min_samples`.
    SampleCount,

    /// Never sunset automatically; wait for an explicit operator trigger.
    OperatorTriggerOnly,
}

fn default_dispatch() -> String {
    "detached".to_owned()
}

fn default_enabled() -> bool {
    true
}

/// Errors surfaced while loading or validating a [`Config`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read the TOML file from disk.
    #[error("failed to read config file: {0}")]
    Io(#[from] io::Error),

    /// The file was read but did not parse as TOML matching the schema.
    #[error("failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),

    /// Semantic validation failure (e.g. XOR violation, duplicate names).
    /// Contains one line per offending patrol so the operator sees the whole
    /// picture in a single diagnostic pass.
    #[error("config validation failed:\n{0}")]
    Invalid(String),
}

impl Config {
    /// Load and validate a config file. A convenience wrapper around
    /// [`Self::from_str_validated`] that reads from disk.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] if the file cannot be read, [`ConfigError::Parse`]
    /// if the TOML is malformed, or [`ConfigError::Invalid`] if semantic
    /// validation fails.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)?;
        Self::from_str_validated(&raw)
    }

    /// Parse a TOML string and then run semantic validation.
    ///
    /// Named `from_str_validated` (not `FromStr`) because the trait version
    /// would swallow validation errors behind an associated error type. Here
    /// we want the full `ConfigError` enum.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Parse`] if the input is not valid TOML,
    /// or [`ConfigError::Invalid`] if the parsed schema fails semantic
    /// validation (duplicate names, XOR cadence violations, empty commands).
    pub fn from_str_validated(raw: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Semantic validation: every patrol has exactly one cadence, names are
    /// unique. Collects *all* errors before returning so the operator sees
    /// the complete picture in one `cosmon-scheduler tick --dry-run` run.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Invalid`] with a multi-line diagnostic if any
    /// patrol violates: cadence XOR, unique-name, or non-empty-command.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();

        for (idx, p) in self.patrols.iter().enumerate() {
            if !seen.insert(p.name.as_str()) {
                errors.push(format!(
                    "patrol[{idx}] '{name}': duplicate name",
                    idx = idx,
                    name = p.name
                ));
            }
            if let Err(e) = p.validate_cadence() {
                errors.push(format!("patrol[{idx}] '{name}': {e}", name = p.name));
            }
            if p.command.is_empty() {
                errors.push(format!(
                    "patrol[{idx}] '{name}': command must have at least one element",
                    name = p.name
                ));
            }
            if let Some(sunset) = p.sunset.as_ref() {
                for e in sunset.validate() {
                    errors.push(format!(
                        "patrol[{idx}] '{name}': sunset: {e}",
                        name = p.name
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errors.join("\n")))
        }
    }
}

impl Patrol {
    /// Check the `interval_seconds` XOR `cron` invariant.
    ///
    /// # Errors
    ///
    /// Returns a static string describing the violation if both or neither
    /// cadence field is set.
    pub fn validate_cadence(&self) -> Result<(), &'static str> {
        match (self.interval_seconds.is_some(), self.cron.is_some()) {
            (true, true) => Err("both `interval_seconds` and `cron` set (XOR)"),
            (false, false) => Err("neither `interval_seconds` nor `cron` set"),
            _ => Ok(()),
        }
    }
}

impl Sunset {
    /// Collect every violation of the per-strategy field-requirement rules.
    ///
    /// Returns an empty vector if the block is internally consistent. Each
    /// entry is a stable human-readable fragment (no patrol context) that
    /// the caller prefixes with `patrol[idx] 'name': sunset: …` so the
    /// diagnostic lines up with the rest of `Config::validate`.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if let Some(n) = self.min_samples {
            if n < 1 {
                errors.push(format!("min_samples must be >= 1 (got {n})"));
            }
        }

        match self.strategy {
            SunsetStrategy::VarianceThreshold => {
                if self.sample_file.is_none() {
                    errors.push("strategy 'variance-threshold' requires `sample_file`".to_owned());
                }
            }
            SunsetStrategy::SampleCount => {
                if self.sample_file.is_none() {
                    errors.push("strategy 'sample-count' requires `sample_file`".to_owned());
                }
                if self.min_samples.is_none() {
                    errors.push("strategy 'sample-count' requires `min_samples`".to_owned());
                }
            }
            SunsetStrategy::OperatorTriggerOnly => {
                // No required data fields: the gate is flipped externally.
            }
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_toml() -> &'static str {
        r#"
            [scheduler]
            state_file = "~/.cosmon/scheduler.state.json"
            log_file = "~/.cosmon/scheduler.log"
            kill_switch = "~/.cosmon/stand-down.lock"
            tick_interval_seconds = 60

            [[patrol]]
            name = "chronicle-lint"
            cron = "0 9 * * 0"
            command = ["cs", "nucleate", "chronicle-lint"]
            enabled = true

            [[patrol]]
            name = "hello"
            interval_seconds = 300
            command = ["echo", "hello"]
        "#
    }

    #[test]
    fn parses_valid_toml() {
        let cfg = Config::from_str_validated(valid_toml()).expect("valid config");
        assert_eq!(cfg.scheduler.tick_interval_seconds, 60);
        assert_eq!(cfg.patrols.len(), 2);
        assert_eq!(cfg.patrols[0].name, "chronicle-lint");
        assert_eq!(cfg.patrols[1].interval_seconds, Some(300));
        assert!(cfg.patrols[1].enabled);
    }

    /// The intra-container watchdog config
    /// shipped as an adapter asset must parse + validate against this
    /// schema. A regression here means the published config contract is
    /// broken before any scheduler ever reads it.
    #[test]
    fn ships_valid_tenant_liveness_patrol_asset() {
        let raw =
            include_str!("../../cosmon-rpp-adapter/assets/patrols.toml").replace("NOYAU", "demo");
        let cfg = Config::from_str_validated(&raw).expect("shipped patrols.toml must validate");
        let propel = cfg
            .patrols
            .iter()
            .find(|p| p.name == "tenant-liveness-propel")
            .expect("tenant-liveness-propel patrol present");
        assert_eq!(propel.interval_seconds, Some(180), "cadence is 180s");
        assert_eq!(
            propel.command,
            vec!["cs", "patrol", "--propel", "--stale-after", "300"],
            "propel command carries the 300s stale threshold"
        );
        assert!(propel.enabled);
    }

    #[test]
    fn rejects_unknown_patrol_field() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "typo"
            intervall_seconds = 300
            command = ["echo", "hi"]
        "#;
        let err = Config::from_str_validated(raw).expect_err("unknown field rejected");
        assert!(
            matches!(err, ConfigError::Parse(_)),
            "expected parse error, got {err:?}"
        );
    }

    #[test]
    fn rejects_both_cadences_set() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "xor-fail"
            interval_seconds = 60
            cron = "0 * * * *"
            command = ["echo"]
        "#;
        let err = Config::from_str_validated(raw).expect_err("XOR violation");
        let msg = err.to_string();
        assert!(msg.contains("XOR"), "got: {msg}");
    }

    #[test]
    fn rejects_no_cadence() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "no-cadence"
            command = ["echo"]
        "#;
        let err = Config::from_str_validated(raw).expect_err("missing cadence");
        let msg = err.to_string();
        assert!(msg.contains("neither"), "got: {msg}");
    }

    #[test]
    fn rejects_duplicate_patrol_names() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "dup"
            interval_seconds = 60
            command = ["a"]

            [[patrol]]
            name = "dup"
            interval_seconds = 60
            command = ["b"]
        "#;
        let err = Config::from_str_validated(raw).expect_err("duplicate names");
        let msg = err.to_string();
        assert!(msg.contains("duplicate"), "got: {msg}");
    }

    #[test]
    fn rejects_empty_command() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "empty"
            interval_seconds = 60
            command = []
        "#;
        let err = Config::from_str_validated(raw).expect_err("empty command");
        let msg = err.to_string();
        assert!(msg.contains("command"), "got: {msg}");
    }

    #[test]
    fn tolerates_unknown_root_field() {
        // Root is tolerant — a future `[audit]` section does not break us.
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [audit]
            anything = "goes"
        "#;
        let cfg = Config::from_str_validated(raw).expect("tolerates unknown root section");
        assert_eq!(cfg.patrols.len(), 0);
    }

    #[test]
    fn load_reads_file_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("patrols.toml");
        fs::write(&path, valid_toml()).unwrap();
        let cfg = Config::load(&path).expect("loads from disk");
        assert_eq!(cfg.patrols.len(), 2);
    }

    #[test]
    fn sunset_valid_block_parses_all_strategies() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "variance"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "variance-threshold"
            sample_file = "/tmp/samples.tsv"
            min_samples = 30
            variance_threshold = 0.05
            window = 10
            on_sunset = ["notify_telegram"]

            [[patrol]]
            name = "count"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "sample-count"
            sample_file = "/tmp/samples.tsv"
            min_samples = 100

            [[patrol]]
            name = "manual"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "operator-trigger-only"
            trigger_file = "/tmp/stop"
        "#;
        let cfg = Config::from_str_validated(raw).expect("valid sunset blocks");
        assert_eq!(cfg.patrols.len(), 3);

        let variance = cfg.patrols[0].sunset.as_ref().expect("sunset present");
        assert_eq!(variance.strategy, SunsetStrategy::VarianceThreshold);
        assert_eq!(variance.sample_file.as_deref(), Some("/tmp/samples.tsv"));
        assert_eq!(variance.min_samples, Some(30));
        assert_eq!(variance.variance_threshold, Some(0.05));
        assert_eq!(variance.window, Some(10));
        assert_eq!(variance.on_sunset, vec!["notify_telegram".to_owned()]);

        let count = cfg.patrols[1].sunset.as_ref().expect("sunset present");
        assert_eq!(count.strategy, SunsetStrategy::SampleCount);
        assert_eq!(count.min_samples, Some(100));

        let manual = cfg.patrols[2].sunset.as_ref().expect("sunset present");
        assert_eq!(manual.strategy, SunsetStrategy::OperatorTriggerOnly);
        assert_eq!(manual.trigger_file.as_deref(), Some("/tmp/stop"));
    }

    #[test]
    fn sunset_unknown_strategy_rejected() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "bad-strategy"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "kolmogorov-smirnov"
            sample_file = "/tmp/samples.tsv"
        "#;
        let err = Config::from_str_validated(raw).expect_err("unknown strategy rejected");
        assert!(
            matches!(err, ConfigError::Parse(_)),
            "expected parse error, got {err:?}"
        );
    }

    #[test]
    fn sunset_min_samples_below_one_rejected() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "zero-samples"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "sample-count"
            sample_file = "/tmp/samples.tsv"
            min_samples = 0
        "#;
        let err = Config::from_str_validated(raw).expect_err("min_samples<1 rejected");
        let msg = err.to_string();
        assert!(msg.contains("min_samples must be >= 1"), "got: {msg}");
    }

    #[test]
    fn sunset_variance_threshold_without_sample_file_rejected() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "missing-source"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "variance-threshold"
            variance_threshold = 0.05
            window = 10
        "#;
        let err =
            Config::from_str_validated(raw).expect_err("variance-threshold without sample_file");
        let msg = err.to_string();
        assert!(
            msg.contains("variance-threshold") && msg.contains("sample_file"),
            "got: {msg}"
        );
    }

    #[test]
    fn sunset_unknown_field_in_sunset_rejected() {
        // Defense in depth: deny_unknown_fields on the Sunset struct itself.
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "typo"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "sample-count"
            sample_file = "/tmp/samples.tsv"
            min_samples = 10
            min_sampl3s = 99
        "#;
        let err =
            Config::from_str_validated(raw).expect_err("unknown field inside sunset rejected");
        assert!(
            matches!(err, ConfigError::Parse(_)),
            "expected parse error, got {err:?}"
        );
    }

    #[test]
    fn sunset_launchctl_plist_parses() {
        let raw = r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "u2-probe"
            interval_seconds = 86400
            command = ["probe"]
            [patrol.sunset]
            strategy = "sample-count"
            sample_file = "/tmp/samples.tsv"
            min_samples = 30
            launchctl_plist = "~/Library/LaunchAgents/com.example.u2.plist"
            on_sunset = ["unload_launchd"]
        "#;
        let cfg = Config::from_str_validated(raw).expect("valid launchctl_plist");
        let sunset = cfg.patrols[0].sunset.as_ref().unwrap();
        assert_eq!(
            sunset.launchctl_plist.as_deref(),
            Some("~/Library/LaunchAgents/com.example.u2.plist")
        );
    }

    #[test]
    fn sunset_absent_is_fine() {
        let cfg = Config::from_str_validated(valid_toml()).expect("valid");
        assert!(cfg.patrols.iter().all(|p| p.sunset.is_none()));
    }
}
