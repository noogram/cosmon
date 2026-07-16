// SPDX-License-Identifier: AGPL-3.0-only

//! Hook event system — events to configurable hooks.
//!
//! Cosmon emits domain [`Event`] values during operation.
//! The hook system lets operators configure actions that fire in response:
//! shell commands, HTTP webhooks, or custom handlers via the [`HookRunner`]
//! trait.
//!
//! This module is pure data + trait definitions (zero I/O). Concrete runners
//! (process spawning, HTTP clients) live in adapter crates.
//!
//! # Design
//!
//! Inspired by Claude Code's hook architecture (25 event types × 5 hook types),
//! adapted to Cosmon's physics vocabulary:
//!
//! | Concept | Claude Code | Cosmon |
//! |---------|------------|--------|
//! | Events | `PreToolUse`, `PostToolUse`, … | `WorkerSpawned`, `MoleculeTransitioned`, … |
//! | Actions | Shell command | `ShellCommand`, `HttpWebhook` |
//! | Config | `settings.json` hooks array | `HookConfig` with TOML/JSON support |
//! | Execution | Built-in runner | `HookRunner` trait (hexagonal port) |
//!
//! # Examples
//!
//! ```
//! use cosmon_core::hook::{
//!     FailurePolicy, HookAction, HookBinding, HookConfig, HookEventFilter, HttpMethod,
//! };
//!
//! // Notify a webhook when any worker spawns:
//! let binding = HookBinding {
//!     event: HookEventFilter::WorkerSpawned,
//!     action: HookAction::HttpWebhook {
//!         url: "https://hooks.example.com/cosmon".to_owned(),
//!         method: HttpMethod::Post,
//!         headers: Default::default(),
//!     },
//!     timeout_secs: 10,
//!     on_failure: FailurePolicy::Log,
//! };
//!
//! let config = HookConfig {
//!     bindings: vec![binding],
//! };
//!
//! assert_eq!(config.bindings.len(), 1);
//! ```

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::event::{Envelope, Event};

// ---------------------------------------------------------------------------
// HookEventFilter — which events trigger a hook
// ---------------------------------------------------------------------------

/// Selects which domain events trigger a hook.
///
/// Each variant matches the corresponding [`Event`] variant. `Any` matches all
/// events (useful for logging/auditing hooks).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventFilter {
    /// Fires on any event.
    Any,
    /// Fires when a worker is spawned.
    WorkerSpawned,
    /// Fires when a worker terminates.
    WorkerTerminated,
    /// Fires when a molecule is dispatched to a worker.
    MoleculeDispatched,
    /// Fires when a molecule transitions between lifecycle states.
    MoleculeTransitioned,
    /// Fires when a molecule step completes.
    StepCompleted,
    /// Fires when a task is dispatched via the communication fabric.
    TaskDispatched,
    /// Fires when a molecule advances to the next step.
    MoleculeEvolved,
    /// Fires when a molecule is completed.
    MoleculeCompleted,
    /// Fires when a molecule collapses.
    MoleculeCollapsed,
    /// Fires when a molecule is frozen.
    MoleculeFrozen,
    /// Fires when a molecule is thawed.
    MoleculeThawed,
    /// Fires when a molecule decays into children.
    MoleculeDecayed,
    /// Fires when molecules are merged.
    MoleculeMerged,
    /// Fires when a molecule's kind is transformed.
    MoleculeTransformed,
    /// Fires when an error occurs.
    ErrorOccurred,
}

impl HookEventFilter {
    /// Returns `true` if this filter matches the given event.
    #[must_use]
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Self::Any => true,
            Self::WorkerSpawned => matches!(event, Event::WorkerSpawned { .. }),
            Self::WorkerTerminated => matches!(event, Event::WorkerTerminated { .. }),
            Self::MoleculeDispatched => matches!(event, Event::MoleculeDispatched { .. }),
            Self::MoleculeTransitioned => matches!(event, Event::MoleculeTransitioned { .. }),
            Self::StepCompleted => matches!(event, Event::StepCompleted { .. }),
            Self::TaskDispatched => matches!(event, Event::TaskDispatched { .. }),
            Self::MoleculeEvolved => matches!(event, Event::MoleculeEvolved { .. }),
            Self::MoleculeCompleted => matches!(event, Event::MoleculeCompleted { .. }),
            Self::MoleculeCollapsed => matches!(event, Event::MoleculeCollapsed { .. }),
            Self::MoleculeFrozen => matches!(event, Event::MoleculeFrozen { .. }),
            Self::MoleculeThawed => matches!(event, Event::MoleculeThawed { .. }),
            Self::MoleculeDecayed => matches!(event, Event::MoleculeDecayed { .. }),
            Self::MoleculeMerged => matches!(event, Event::MoleculeMerged { .. }),
            Self::MoleculeTransformed => matches!(event, Event::MoleculeTransformed { .. }),
            Self::ErrorOccurred => matches!(event, Event::ErrorOccurred { .. }),
        }
    }
}

impl fmt::Display for HookEventFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("any"),
            Self::WorkerSpawned => f.write_str("worker_spawned"),
            Self::WorkerTerminated => f.write_str("worker_terminated"),
            Self::MoleculeDispatched => f.write_str("molecule_dispatched"),
            Self::MoleculeTransitioned => f.write_str("molecule_transitioned"),
            Self::StepCompleted => f.write_str("step_completed"),
            Self::TaskDispatched => f.write_str("task_dispatched"),
            Self::MoleculeEvolved => f.write_str("molecule_evolved"),
            Self::MoleculeCompleted => f.write_str("molecule_completed"),
            Self::MoleculeCollapsed => f.write_str("molecule_collapsed"),
            Self::MoleculeFrozen => f.write_str("molecule_frozen"),
            Self::MoleculeThawed => f.write_str("molecule_thawed"),
            Self::MoleculeDecayed => f.write_str("molecule_decayed"),
            Self::MoleculeMerged => f.write_str("molecule_merged"),
            Self::MoleculeTransformed => f.write_str("molecule_transformed"),
            Self::ErrorOccurred => f.write_str("error_occurred"),
        }
    }
}

// ---------------------------------------------------------------------------
// HookAction — what a hook does
// ---------------------------------------------------------------------------

/// HTTP method for webhook hooks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// HTTP POST (default for webhooks).
    Post,
    /// HTTP PUT.
    Put,
}

#[allow(clippy::derivable_impls)]
impl Default for HttpMethod {
    fn default() -> Self {
        Self::Post
    }
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Post => f.write_str("POST"),
            Self::Put => f.write_str("PUT"),
        }
    }
}

/// The action a hook performs when its event filter matches.
///
/// Pure data — no I/O. The [`HookRunner`] trait interprets these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookAction {
    /// Execute a shell command. The event envelope is passed as JSON via
    /// the `COSMON_EVENT` environment variable.
    ShellCommand {
        /// The command to execute (passed to the system shell).
        command: String,
    },

    /// Send the event envelope as an HTTP request body.
    HttpWebhook {
        /// The webhook URL.
        url: String,
        /// HTTP method (defaults to POST).
        #[serde(default)]
        method: HttpMethod,
        /// Additional HTTP headers.
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

impl fmt::Display for HookAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShellCommand { command } => write!(f, "shell: {command}"),
            Self::HttpWebhook { url, method, .. } => write!(f, "{method} {url}"),
        }
    }
}

// ---------------------------------------------------------------------------
// FailurePolicy — what happens when a hook fails
// ---------------------------------------------------------------------------

/// Policy for handling hook execution failures.
///
/// Hooks are side-effects and should not block the main event pipeline.
/// This policy controls how failures are surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Log the failure and continue (default).
    Log,
    /// Silently ignore the failure.
    Ignore,
    /// Emit an [`Event::ErrorOccurred`] for the failure.
    Emit,
}

#[allow(clippy::derivable_impls)]
impl Default for FailurePolicy {
    fn default() -> Self {
        Self::Log
    }
}

impl fmt::Display for FailurePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Log => f.write_str("log"),
            Self::Ignore => f.write_str("ignore"),
            Self::Emit => f.write_str("emit"),
        }
    }
}

// ---------------------------------------------------------------------------
// HookBinding — one event-to-action mapping
// ---------------------------------------------------------------------------

/// A single hook binding: when event X occurs, perform action Y.
///
/// Multiple bindings can match the same event — all matching hooks fire.
///
/// # Serialization
///
/// ```
/// use cosmon_core::hook::{FailurePolicy, HookAction, HookBinding, HookEventFilter};
///
/// let binding = HookBinding {
///     event: HookEventFilter::WorkerSpawned,
///     action: HookAction::ShellCommand {
///         command: "echo $COSMON_EVENT >> /tmp/cosmon.log".to_owned(),
///     },
///     timeout_secs: 5,
///     on_failure: FailurePolicy::Log,
/// };
///
/// let json = serde_json::to_string(&binding).unwrap();
/// let back: HookBinding = serde_json::from_str(&json).unwrap();
/// assert_eq!(back.event, HookEventFilter::WorkerSpawned);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookBinding {
    /// Which events trigger this hook.
    pub event: HookEventFilter,
    /// What to do when the event fires.
    pub action: HookAction,
    /// Maximum seconds to wait for hook execution. 0 means fire-and-forget.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
    /// What to do if the hook fails.
    #[serde(default)]
    pub on_failure: FailurePolicy,
}

/// Default timeout for hook execution (5 seconds).
const fn default_timeout() -> u32 {
    5
}

// ---------------------------------------------------------------------------
// HookConfig — the full hook configuration
// ---------------------------------------------------------------------------

/// Complete hook configuration for a Cosmon instance.
///
/// Loaded from configuration files (TOML or JSON). Contains zero or more
/// [`HookBinding`]s that map events to actions.
///
/// # Examples
///
/// ```
/// use cosmon_core::hook::HookConfig;
///
/// // Empty config — no hooks fire:
/// let config = HookConfig::default();
/// assert!(config.bindings.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HookConfig {
    /// The list of event-to-action bindings.
    #[serde(default)]
    pub bindings: Vec<HookBinding>,
}

impl HookConfig {
    /// Returns all bindings whose event filter matches the given event.
    #[must_use]
    pub fn matching_bindings(&self, event: &Event) -> Vec<&HookBinding> {
        self.bindings
            .iter()
            .filter(|b| b.event.matches(event))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// HookResult — outcome of executing a hook
// ---------------------------------------------------------------------------

/// Outcome of a single hook execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookResult {
    /// Hook executed successfully.
    Ok,
    /// Hook failed with the given error message.
    Failed(String),
    /// Hook execution timed out.
    TimedOut,
    /// Hook was skipped (e.g. disabled, filtered out).
    Skipped,
}

impl fmt::Display for HookResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::Failed(msg) => write!(f, "failed: {msg}"),
            Self::TimedOut => f.write_str("timed out"),
            Self::Skipped => f.write_str("skipped"),
        }
    }
}

// ---------------------------------------------------------------------------
// HookRunner — hexagonal port for executing hooks
// ---------------------------------------------------------------------------

/// Hexagonal port for hook execution.
///
/// The core crate defines this trait; adapter crates provide implementations
/// that actually spawn processes or make HTTP calls. This keeps cosmon-core
/// free of I/O.
///
/// # Contract
///
/// - Implementations MUST respect `binding.timeout_secs` (0 = fire-and-forget).
/// - Implementations MUST NOT panic on invalid commands or unreachable URLs.
/// - The event envelope is passed as JSON. For shell hooks, set the
///   `COSMON_EVENT` environment variable. For HTTP hooks, send as the
///   request body with `Content-Type: application/json`.
pub trait HookRunner {
    /// Execute a single hook binding for the given event envelope.
    ///
    /// Returns the outcome. Implementations should handle errors internally
    /// and return [`HookResult::Failed`] rather than propagating.
    fn run(&self, binding: &HookBinding, envelope: &Envelope) -> HookResult;
}

/// Execute all matching hooks for an event.
///
/// Finds all bindings in `config` that match the event in `envelope`, runs
/// them through `runner`, and returns a `(binding, result)` pair for each.
///
/// # Examples
///
/// ```
/// use cosmon_core::event::{Envelope, Event};
/// use cosmon_core::hook::{
///     dispatch_hooks, FailurePolicy, HookAction, HookBinding, HookConfig,
///     HookEventFilter, HookResult, HookRunner,
/// };
/// use cosmon_core::id::WorkerId;
///
/// // A test runner that always succeeds:
/// struct NoopRunner;
/// impl HookRunner for NoopRunner {
///     fn run(&self, _binding: &HookBinding, _envelope: &Envelope) -> HookResult {
///         HookResult::Ok
///     }
/// }
///
/// let config = HookConfig {
///     bindings: vec![HookBinding {
///         event: HookEventFilter::WorkerSpawned,
///         action: HookAction::ShellCommand {
///             command: "echo hello".to_owned(),
///         },
///         timeout_secs: 5,
///         on_failure: FailurePolicy::Log,
///     }],
/// };
///
/// let envelope = Envelope::now(Event::WorkerSpawned {
///     worker_id: WorkerId::new("quartz").unwrap(),
///     agent: "polecat".to_owned(),
/// });
///
/// let results = dispatch_hooks(&config, &envelope, &NoopRunner);
/// assert_eq!(results.len(), 1);
/// assert_eq!(results[0].1, HookResult::Ok);
/// ```
pub fn dispatch_hooks<'a, R: HookRunner>(
    config: &'a HookConfig,
    envelope: &Envelope,
    runner: &R,
) -> Vec<(&'a HookBinding, HookResult)> {
    config
        .matching_bindings(&envelope.event)
        .into_iter()
        .map(|binding| {
            let result = runner.run(binding, envelope);
            (binding, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Envelope, Event};
    use crate::id::{MoleculeId, WorkerId};
    use crate::molecule::MoleculeStatus;

    // -- helpers --

    struct AlwaysOk;
    impl HookRunner for AlwaysOk {
        fn run(&self, _: &HookBinding, _: &Envelope) -> HookResult {
            HookResult::Ok
        }
    }

    struct AlwaysFail;
    impl HookRunner for AlwaysFail {
        fn run(&self, _: &HookBinding, _: &Envelope) -> HookResult {
            HookResult::Failed("boom".to_owned())
        }
    }

    fn shell_binding(event: HookEventFilter) -> HookBinding {
        HookBinding {
            event,
            action: HookAction::ShellCommand {
                command: "echo test".to_owned(),
            },
            timeout_secs: 5,
            on_failure: FailurePolicy::Log,
        }
    }

    fn worker_spawned_envelope() -> Envelope {
        Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("quartz").unwrap(),
            agent: "polecat".to_owned(),
        })
    }

    // -- HookEventFilter tests --

    #[test]
    fn test_filter_any_matches_all() {
        let events = vec![
            Event::WorkerSpawned {
                worker_id: WorkerId::new("a").unwrap(),
                agent: "x".to_owned(),
            },
            Event::WorkerTerminated {
                worker_id: WorkerId::new("a").unwrap(),
                reason: "done".to_owned(),
            },
            Event::ErrorOccurred {
                context: "ctx".to_owned(),
                message: "msg".to_owned(),
            },
        ];
        for event in &events {
            assert!(
                HookEventFilter::Any.matches(event),
                "Any should match {event:?}"
            );
        }
    }

    #[test]
    fn test_filter_specific_matches_correct_variant() {
        let spawned = Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        };
        let terminated = Event::WorkerTerminated {
            worker_id: WorkerId::new("a").unwrap(),
            reason: "done".to_owned(),
        };

        assert!(HookEventFilter::WorkerSpawned.matches(&spawned));
        assert!(!HookEventFilter::WorkerSpawned.matches(&terminated));
        assert!(HookEventFilter::WorkerTerminated.matches(&terminated));
        assert!(!HookEventFilter::WorkerTerminated.matches(&spawned));
    }

    #[test]
    fn test_filter_molecule_events() {
        let transitioned = Event::MoleculeTransitioned {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            from: MoleculeStatus::Running,
            to: MoleculeStatus::Completed,
        };
        assert!(HookEventFilter::MoleculeTransitioned.matches(&transitioned));
        assert!(!HookEventFilter::StepCompleted.matches(&transitioned));
    }

    // -- HookAction serde tests --

    #[test]
    fn test_shell_command_serde_roundtrip() {
        let action = HookAction::ShellCommand {
            command: "notify-send 'Worker spawned'".to_owned(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: HookAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, action);
        assert!(json.contains("\"type\":\"shell_command\""));
    }

    #[test]
    fn test_http_webhook_serde_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_owned(), "Bearer tok".to_owned());
        let action = HookAction::HttpWebhook {
            url: "https://hooks.example.com/cosmon".to_owned(),
            method: HttpMethod::Post,
            headers,
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: HookAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, action);
        assert!(json.contains("\"type\":\"http_webhook\""));
    }

    // -- HookBinding serde tests --

    #[test]
    fn test_binding_serde_roundtrip() {
        let binding = shell_binding(HookEventFilter::WorkerSpawned);
        let json = serde_json::to_string(&binding).unwrap();
        let back: HookBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, binding);
    }

    #[test]
    fn test_binding_defaults() {
        // Deserialize with only required fields — defaults fill in:
        let json = r#"{
            "event": "any",
            "action": {"type": "shell_command", "command": "echo hi"}
        }"#;
        let binding: HookBinding = serde_json::from_str(json).unwrap();
        assert_eq!(binding.timeout_secs, 5);
        assert_eq!(binding.on_failure, FailurePolicy::Log);
    }

    // -- HookConfig tests --

    #[test]
    fn test_config_matching_bindings_empty() {
        let config = HookConfig::default();
        let event = Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        };
        assert!(config.matching_bindings(&event).is_empty());
    }

    #[test]
    fn test_config_matching_bindings_selective() {
        let config = HookConfig {
            bindings: vec![
                shell_binding(HookEventFilter::WorkerSpawned),
                shell_binding(HookEventFilter::ErrorOccurred),
            ],
        };

        let spawned = Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        };
        assert_eq!(config.matching_bindings(&spawned).len(), 1);

        let error = Event::ErrorOccurred {
            context: "ctx".to_owned(),
            message: "msg".to_owned(),
        };
        assert_eq!(config.matching_bindings(&error).len(), 1);

        let terminated = Event::WorkerTerminated {
            worker_id: WorkerId::new("a").unwrap(),
            reason: "done".to_owned(),
        };
        assert!(config.matching_bindings(&terminated).is_empty());
    }

    #[test]
    fn test_config_any_plus_specific_both_match() {
        let config = HookConfig {
            bindings: vec![
                shell_binding(HookEventFilter::Any),
                shell_binding(HookEventFilter::WorkerSpawned),
            ],
        };

        let spawned = Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        };
        // Both "any" and "worker_spawned" should match:
        assert_eq!(config.matching_bindings(&spawned).len(), 2);
    }

    // -- dispatch_hooks tests --

    #[test]
    fn test_dispatch_hooks_fires_matching() {
        let config = HookConfig {
            bindings: vec![shell_binding(HookEventFilter::WorkerSpawned)],
        };
        let envelope = worker_spawned_envelope();
        let results = dispatch_hooks(&config, &envelope, &AlwaysOk);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, HookResult::Ok);
    }

    #[test]
    fn test_dispatch_hooks_skips_non_matching() {
        let config = HookConfig {
            bindings: vec![shell_binding(HookEventFilter::ErrorOccurred)],
        };
        let envelope = worker_spawned_envelope();
        let results = dispatch_hooks(&config, &envelope, &AlwaysOk);
        assert!(results.is_empty());
    }

    #[test]
    fn test_dispatch_hooks_reports_failures() {
        let config = HookConfig {
            bindings: vec![shell_binding(HookEventFilter::WorkerSpawned)],
        };
        let envelope = worker_spawned_envelope();
        let results = dispatch_hooks(&config, &envelope, &AlwaysFail);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].1, HookResult::Failed(_)));
    }

    #[test]
    fn test_dispatch_hooks_multiple_bindings() {
        let config = HookConfig {
            bindings: vec![
                shell_binding(HookEventFilter::Any),
                shell_binding(HookEventFilter::WorkerSpawned),
                shell_binding(HookEventFilter::ErrorOccurred),
            ],
        };
        let envelope = worker_spawned_envelope();
        let results = dispatch_hooks(&config, &envelope, &AlwaysOk);
        // "any" and "worker_spawned" match; "error_occurred" does not:
        assert_eq!(results.len(), 2);
    }

    // -- Display tests --

    #[test]
    fn test_filter_display() {
        assert_eq!(HookEventFilter::Any.to_string(), "any");
        assert_eq!(HookEventFilter::WorkerSpawned.to_string(), "worker_spawned");
        assert_eq!(
            HookEventFilter::MoleculeTransitioned.to_string(),
            "molecule_transitioned"
        );
    }

    #[test]
    fn test_action_display() {
        let shell = HookAction::ShellCommand {
            command: "echo hi".to_owned(),
        };
        assert_eq!(shell.to_string(), "shell: echo hi");

        let webhook = HookAction::HttpWebhook {
            url: "https://example.com".to_owned(),
            method: HttpMethod::Post,
            headers: HashMap::new(),
        };
        assert_eq!(webhook.to_string(), "POST https://example.com");
    }

    #[test]
    fn test_hook_result_display() {
        assert_eq!(HookResult::Ok.to_string(), "ok");
        assert_eq!(HookResult::TimedOut.to_string(), "timed out");
        assert_eq!(HookResult::Skipped.to_string(), "skipped");
        assert_eq!(
            HookResult::Failed("oops".to_owned()).to_string(),
            "failed: oops"
        );
    }

    #[test]
    fn test_failure_policy_display() {
        assert_eq!(FailurePolicy::Log.to_string(), "log");
        assert_eq!(FailurePolicy::Ignore.to_string(), "ignore");
        assert_eq!(FailurePolicy::Emit.to_string(), "emit");
    }

    // -- HookConfig serde --

    #[test]
    fn test_config_serde_roundtrip() {
        let config = HookConfig {
            bindings: vec![
                shell_binding(HookEventFilter::WorkerSpawned),
                HookBinding {
                    event: HookEventFilter::ErrorOccurred,
                    action: HookAction::HttpWebhook {
                        url: "https://example.com/hook".to_owned(),
                        method: HttpMethod::Post,
                        headers: HashMap::new(),
                    },
                    timeout_secs: 10,
                    on_failure: FailurePolicy::Emit,
                },
            ],
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let back: HookConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, config);
    }

    #[test]
    fn test_config_toml_roundtrip() {
        let config = HookConfig {
            bindings: vec![shell_binding(HookEventFilter::Any)],
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let back: HookConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(back, config);
    }

    // -- HttpMethod tests --

    #[test]
    fn test_http_method_default_is_post() {
        assert_eq!(HttpMethod::default(), HttpMethod::Post);
    }

    #[test]
    fn test_http_method_display() {
        assert_eq!(HttpMethod::Post.to_string(), "POST");
        assert_eq!(HttpMethod::Put.to_string(), "PUT");
    }

    #[test]
    fn test_http_method_serde() {
        let json = serde_json::to_string(&HttpMethod::Post).unwrap();
        assert_eq!(json, "\"POST\"");
        let back: HttpMethod = serde_json::from_str(&json).unwrap();
        assert_eq!(back, HttpMethod::Post);
    }
}
