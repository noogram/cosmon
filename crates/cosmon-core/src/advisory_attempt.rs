// SPDX-License-Identifier: AGPL-3.0-only

//! Pure contract for declared advisory attempts (ADR-182).
//!
//! An attempt is evidence inside an owning molecule, never a second lifecycle.
//! This module therefore owns no process, filesystem or clock.  It validates
//! declarations and folds observations supplied by an adapter.  Filesystem
//! custody lives in `cosmon-state`; provider handles remain private to the
//! adapter and deliberately have no field in these public records.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::{DateTime, Utc};
use cosmon_hash::Hash;
use serde::{Deserialize, Serialize};

/// Current schema for canonical advisory-attempt records.
pub const ADVISORY_ATTEMPT_SCHEMA_VERSION: u16 = 1;

macro_rules! advisory_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validate and construct the identifier.
            ///
            /// # Errors
            /// Returns [`AdvisoryAttemptError::InvalidIdentifier`] for an empty,
            /// overlong or filesystem-unsafe token.
            pub fn new(value: impl Into<String>) -> Result<Self, AdvisoryAttemptError> {
                let value = value.into();
                let valid = !value.is_empty()
                    && value.len() <= 80
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
                if !valid {
                    return Err(AdvisoryAttemptError::InvalidIdentifier {
                        kind: $kind,
                        value,
                    });
                }
                Ok(Self(value))
            }

            /// Return the validated token.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = AdvisoryAttemptError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

advisory_id!(
    /// Stable seat name in one declared advisory work scope.
    AdvisorySeatId,
    "advisory seat"
);
advisory_id!(
    /// Cosmon-owned idempotency key for one provider execution attempt.
    AdvisoryAttemptId,
    "advisory attempt"
);

/// Relative path of a raw response in canonical molecule custody.
///
/// Paths are restricted to `advisory/responses/` so an attempt can never
/// overwrite molecule lifecycle state or a tracked worktree file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AdvisoryArtifactPath(String);

impl AdvisoryArtifactPath {
    /// Validate a canonical response destination.
    ///
    /// # Errors
    /// Returns [`AdvisoryAttemptError::InvalidArtifactPath`] for absolute
    /// paths, traversal, empty segments, private dot-files or paths outside
    /// `advisory/responses/`.
    pub fn new(value: impl Into<String>) -> Result<Self, AdvisoryAttemptError> {
        let value = value.into();
        let segments: Vec<&str> = value.split('/').collect();
        let valid_segment = |segment: &&str| {
            !segment.is_empty()
                && *segment != "."
                && *segment != ".."
                && !segment.starts_with('.')
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        let valid = value.len() <= 240
            && segments.len() >= 3
            && segments.first() == Some(&"advisory")
            && segments.get(1) == Some(&"responses")
            && segments.iter().all(valid_segment);
        if !valid {
            return Err(AdvisoryAttemptError::InvalidArtifactPath(value));
        }
        Ok(Self(value))
    }

    /// Return the portable slash-separated path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AdvisoryArtifactPath {
    type Error = AdvisoryAttemptError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AdvisoryArtifactPath> for String {
    fn from(value: AdvisoryArtifactPath) -> Self {
        value.0
    }
}

/// Why an adapter could not supply an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryUnavailableReason {
    /// The adapter did not inspect this value.
    NotObserved,
    /// The provider or installed harness does not expose this value.
    Unsupported,
    /// The inspected source could not be parsed.
    MalformedSource,
    /// Available observations contradicted one another.
    ConflictingEvidence,
}

/// A sourced value or an explicit absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AdvisoryObservation<T> {
    /// A value observed through the named stable source or schema.
    Observed {
        /// Observed value.
        value: T,
        /// Stable observation source, never a native thread or machine path.
        source: String,
    },
    /// The value was unavailable for a recorded reason.
    Unavailable {
        /// Evidence-preserving absence reason.
        reason: AdvisoryUnavailableReason,
    },
}

impl<T> AdvisoryObservation<T> {
    /// Borrow the observed value, when present.
    #[must_use]
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Observed { value, .. } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Native collaboration protocol exposed to a root model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAdvisoryProtocol {
    /// Codex's legacy spawn/send-input/resume/close family.
    V1,
    /// Codex's named-path message/follow-up/interrupt/list family.
    V2,
}

/// One native collaboration faculty relevant to a declared attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAdvisoryTool {
    /// Start an advisory execution.
    Spawn,
    /// Send an ordinary message without changing lifecycle authority.
    Message,
    /// Send input through the legacy V1 collaboration surface.
    SendInput,
    /// Trigger a new native turn for an existing execution.
    FollowUp,
    /// Wait for activity.
    Wait,
    /// Interrupt a running native execution.
    Interrupt,
    /// List registered native executions.
    List,
    /// Resume a legacy execution.
    Resume,
    /// Close a legacy execution.
    Close,
    /// Use the optional provider-local message board.
    MessageBoard,
}

/// Explicit fallback selected when the requested native surface is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryFallback {
    /// Refuse dispatch and leave the required seat missing.
    Refuse,
    /// Use a separately supervised molecule already admitted by the work scope.
    SeparateMolecule,
    /// Run a labelled sequential-persona comparison baseline.
    SequentialPersonaBaseline,
}

/// Native surface requested for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCapabilityRequest {
    /// Required protocol family.
    pub protocol: NativeAdvisoryProtocol,
    /// Required tools; extra observed tools confer no authority.
    pub required_tools: BTreeSet<NativeAdvisoryTool>,
    /// Preselected action if the requested surface is unsupported.
    pub fallback: AdvisoryFallback,
}

/// Declared and observed native surface for an installed worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCapabilityObservation {
    /// Binary version observed from the executable actually selected for dispatch.
    pub binary_version: AdvisoryObservation<String>,
    /// Realized root model, distinct from a requested model pin.
    pub realized_model: AdvisoryObservation<String>,
    /// Effective feature values exposed by the running configuration.
    pub feature_flags: AdvisoryObservation<BTreeMap<String, bool>>,
    /// Tools actually exposed to the root model.
    pub tools: AdvisoryObservation<BTreeSet<NativeAdvisoryTool>>,
    /// Protocol inferred from the effective root tool surface.
    pub protocol: AdvisoryObservation<NativeAdvisoryProtocol>,
}

/// Result of matching a request against an installed worker observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NativeCapabilityDecision {
    /// The requested protocol and every required tool were observed.
    Supported,
    /// Dispatch is unsupported and must take the explicitly recorded fallback.
    Unsupported {
        /// Human-readable, deterministic gaps.
        gaps: Vec<String>,
        /// Fallback declared before dispatch.
        selected_fallback: AdvisoryFallback,
    },
}

impl NativeCapabilityDecision {
    /// Whether native dispatch may proceed.
    #[must_use]
    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Validate a native request and compare it with observed capabilities.
///
/// # Errors
/// Returns [`AdvisoryAttemptError::InvalidToolCombination`] when tools from
/// the other protocol family are requested or a board is requested without V2.
pub fn assess_native_capabilities(
    request: &NativeCapabilityRequest,
    observed: &NativeCapabilityObservation,
) -> Result<NativeCapabilityDecision, AdvisoryAttemptError> {
    let invalid = match request.protocol {
        NativeAdvisoryProtocol::V1 => request.required_tools.iter().find(|tool| {
            matches!(
                tool,
                NativeAdvisoryTool::Message
                    | NativeAdvisoryTool::FollowUp
                    | NativeAdvisoryTool::Interrupt
                    | NativeAdvisoryTool::List
                    | NativeAdvisoryTool::MessageBoard
            )
        }),
        NativeAdvisoryProtocol::V2 => request.required_tools.iter().find(|tool| {
            matches!(
                tool,
                NativeAdvisoryTool::SendInput
                    | NativeAdvisoryTool::Resume
                    | NativeAdvisoryTool::Close
            )
        }),
    };
    if let Some(tool) = invalid {
        return Err(AdvisoryAttemptError::InvalidToolCombination {
            protocol: request.protocol,
            tool: *tool,
        });
    }

    let mut gaps = Vec::new();
    match observed.protocol.value() {
        Some(protocol) if *protocol == request.protocol => {}
        Some(protocol) => gaps.push(format!(
            "requested {:?}, observed {:?}",
            request.protocol, protocol
        )),
        None => gaps.push("effective protocol unavailable".to_owned()),
    }
    match observed.tools.value() {
        Some(tools) => {
            for missing in request.required_tools.difference(tools) {
                gaps.push(format!("required tool {missing:?} unavailable"));
            }
        }
        None => gaps.push("root tool exposure unavailable".to_owned()),
    }
    if gaps.is_empty() {
        Ok(NativeCapabilityDecision::Supported)
    } else {
        Ok(NativeCapabilityDecision::Unsupported {
            gaps,
            selected_fallback: request.fallback,
        })
    }
}

/// Strength of permission evidence for an effective execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEnforcement {
    /// The adapter demonstrated denial at the effect boundary.
    EffectBoundary,
    /// The only restriction is role or prompt text.
    ConventionOnly,
    /// No enforcement observation was available.
    Unknown,
}

/// Requested access envelope for an advisory seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedPermissionEnvelope {
    /// Named faculties the attempt may use.
    pub allowed: BTreeSet<String>,
    /// Named faculties that must be denied at an effect boundary.
    pub denied: BTreeSet<String>,
}

/// Effective access observed for a dispatched attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePermissionEnvelope {
    /// Faculties observed reachable by the attempt.
    pub allowed: BTreeSet<String>,
    /// Faculties observed denied.
    pub denied: BTreeSet<String>,
    /// Evidence strength for the denial claim.
    pub enforcement: PermissionEnforcement,
}

/// Role, model and permissions frozen before native dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedExecution {
    /// Declared native role name.
    pub role: String,
    /// Declared model name.
    pub model: String,
    /// Declared permission envelope.
    pub permissions: RequestedPermissionEnvelope,
}

/// Effective role, model and permissions observed for an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveExecution {
    /// Effective native role after inheritance and role configuration.
    pub role: AdvisoryObservation<String>,
    /// Effective model after inheritance and role configuration.
    pub model: AdvisoryObservation<String>,
    /// Effective permission envelope.
    pub permissions: AdvisoryObservation<EffectivePermissionEnvelope>,
}

/// Check that inheritance did not silently change model, role or permissions.
///
/// # Errors
/// Returns a typed mismatch or unavailable-evidence error. A persona description
/// never satisfies a required permission denial.
pub fn validate_effective_execution(
    requested: &RequestedExecution,
    effective: &EffectiveExecution,
) -> Result<(), AdvisoryAttemptError> {
    let role = effective
        .role
        .value()
        .ok_or(AdvisoryAttemptError::EffectiveSettingUnavailable("role"))?;
    if role != &requested.role {
        return Err(AdvisoryAttemptError::EffectiveSettingMismatch {
            field: "role",
            requested: requested.role.clone(),
            observed: role.clone(),
        });
    }
    let model = effective
        .model
        .value()
        .ok_or(AdvisoryAttemptError::EffectiveSettingUnavailable("model"))?;
    if model != &requested.model {
        return Err(AdvisoryAttemptError::EffectiveSettingMismatch {
            field: "model",
            requested: requested.model.clone(),
            observed: model.clone(),
        });
    }
    let permissions =
        effective
            .permissions
            .value()
            .ok_or(AdvisoryAttemptError::EffectiveSettingUnavailable(
                "permissions",
            ))?;
    let undeclared: Vec<String> = permissions
        .allowed
        .difference(&requested.permissions.allowed)
        .cloned()
        .collect();
    if !undeclared.is_empty() {
        return Err(AdvisoryAttemptError::PermissionWidened(undeclared));
    }
    let missing_denials: Vec<String> = requested
        .permissions
        .denied
        .difference(&permissions.denied)
        .cloned()
        .collect();
    if !missing_denials.is_empty() {
        return Err(AdvisoryAttemptError::MissingPermissionDenial(
            missing_denials,
        ));
    }
    if !requested.permissions.denied.is_empty()
        && permissions.enforcement != PermissionEnforcement::EffectBoundary
    {
        return Err(AdvisoryAttemptError::PermissionNotEnforced(
            permissions.enforcement,
        ));
    }
    Ok(())
}

/// Frozen provenance for the assignment and its inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptProvenance {
    /// Public source revision or immutable source label.
    pub source_revision: String,
    /// Digest of the complete input given to the attempt.
    pub input_digest: Hash,
    /// Digest of the owning work-scope revision.
    pub scope_revision: Hash,
}

/// Hard limits registered before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryAttemptLimits {
    /// Total provider executions authorized for this seat, including retries.
    pub max_attempts: u16,
    /// Maximum simultaneously active executions for this seat.
    pub max_concurrency: u16,
    /// Latest time at which a new execution may start.
    pub deadline: DateTime<Utc>,
    /// Maximum raw response bytes accepted into custody.
    pub max_output_bytes: u64,
    /// Maximum messages the seat may send and receive.
    pub max_messages: u32,
    /// Aggregate message byte ceiling.
    pub max_message_bytes: u64,
    /// Maximum bounded discussion rounds.
    pub max_discussion_rounds: u16,
}

impl AdvisoryAttemptLimits {
    /// Validate that every dispatch-relevant bound is finite and non-zero.
    ///
    /// # Errors
    /// Returns [`AdvisoryAttemptError::InvalidLimits`] when a required bound
    /// is zero.
    pub fn validate(&self) -> Result<(), AdvisoryAttemptError> {
        if self.max_attempts == 0
            || self.max_concurrency == 0
            || self.max_output_bytes == 0
            || self.max_messages == 0
            || self.max_message_bytes == 0
        {
            return Err(AdvisoryAttemptError::InvalidLimits);
        }
        Ok(())
    }
}

/// Immutable intent persisted before a provider call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryAttemptDeclaration {
    /// Schema used to decode the record.
    pub schema_version: u16,
    /// Cosmon-owned provider-call idempotency key.
    pub attempt_id: AdvisoryAttemptId,
    /// Declared reviewer seat.
    pub seat_id: AdvisorySeatId,
    /// Whether the owning work requires this seat before acceptance.
    pub required: bool,
    /// Digest of the frozen assignment text and rubric.
    pub assignment_digest: Hash,
    /// Source, input and scope provenance.
    pub provenance: AttemptProvenance,
    /// Canonical raw-response destination.
    pub output_path: AdvisoryArtifactPath,
    /// Requested role, model and permission envelope.
    pub requested_execution: RequestedExecution,
    /// Native capability required by this intent.
    pub capability_request: NativeCapabilityRequest,
    /// Registered resource and discussion bounds.
    pub limits: AdvisoryAttemptLimits,
    /// Caller-supplied intent-record time.
    pub recorded_at: DateTime<Utc>,
}

impl AdvisoryAttemptDeclaration {
    /// Validate schema and finite limits before persistence.
    ///
    /// # Errors
    /// Returns a schema, limit or tool-combination error.
    pub fn validate(&self) -> Result<(), AdvisoryAttemptError> {
        if self.schema_version != ADVISORY_ATTEMPT_SCHEMA_VERSION {
            return Err(AdvisoryAttemptError::UnsupportedSchema(self.schema_version));
        }
        self.limits.validate()?;
        let placeholder = NativeCapabilityObservation {
            binary_version: AdvisoryObservation::Unavailable {
                reason: AdvisoryUnavailableReason::NotObserved,
            },
            realized_model: AdvisoryObservation::Unavailable {
                reason: AdvisoryUnavailableReason::NotObserved,
            },
            feature_flags: AdvisoryObservation::Unavailable {
                reason: AdvisoryUnavailableReason::NotObserved,
            },
            tools: AdvisoryObservation::Unavailable {
                reason: AdvisoryUnavailableReason::NotObserved,
            },
            protocol: AdvisoryObservation::Unavailable {
                reason: AdvisoryUnavailableReason::NotObserved,
            },
        };
        assess_native_capabilities(&self.capability_request, &placeholder)?;
        Ok(())
    }
}

/// Strongest recorded observation about the provider spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SpawnObservation {
    /// No provider call has been observed.
    NotAttempted,
    /// The adapter acknowledged a provider execution.
    Acknowledged {
        /// Observation time supplied by the adapter.
        at: DateTime<Utc>,
    },
    /// The call may or may not have created an execution.
    Uncertain {
        /// Observation time supplied by the adapter.
        at: DateTime<Utc>,
        /// Stable explanation without a private provider handle.
        reason: String,
    },
    /// The adapter refused before creating an execution.
    Refused {
        /// Observation time supplied by the adapter.
        at: DateTime<Utc>,
        /// Stable refusal explanation.
        reason: String,
    },
}

/// Raw response bytes that reached canonical custody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedAttemptOutput {
    /// Digest of the exact durable bytes.
    pub digest: Hash,
    /// Byte length used for limit validation and recovery.
    pub bytes: u64,
    /// Atomic-publication completion time supplied by the adapter.
    pub persisted_at: DateTime<Utc>,
}

/// Evidence recorded when an owner accepts a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptAcceptance {
    /// Digest of the accepted durable bytes.
    pub artifact_digest: Hash,
    /// Work-scope identity of the acceptance authority.
    pub accepted_by: String,
    /// Structural validations performed before acceptance.
    pub checks: Vec<String>,
    /// Limitations that remain explicit after acceptance.
    pub limitations: Vec<String>,
    /// Caller-supplied acceptance time.
    pub accepted_at: DateTime<Utc>,
}

/// Owner disposition for one attempt; it is evidence, not molecule status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AttemptDisposition {
    /// No terminal owner disposition has been recorded.
    Pending,
    /// The durable response passed the declared acceptance boundary.
    Accepted(AttemptAcceptance),
    /// The owner rejected the response.
    Rejected {
        /// Stable reason for rejection.
        reason: String,
        /// Caller-supplied decision time.
        at: DateTime<Utc>,
    },
    /// The attempt ended without an acceptable response.
    Missing {
        /// Stable explanation of the missing evidence.
        reason: String,
        /// Caller-supplied decision time.
        at: DateTime<Utc>,
    },
}

/// Canonical fold of one declared advisory attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryAttemptRecord {
    /// Immutable intent.
    pub declaration: AdvisoryAttemptDeclaration,
    /// Installed-worker capability observation made before dispatch.
    pub capabilities: NativeCapabilityObservation,
    /// Deterministic support/fallback decision.
    pub capability_decision: NativeCapabilityDecision,
    /// Effective child settings, recorded before or at spawn acknowledgment.
    pub effective_execution: Option<EffectiveExecution>,
    /// Strongest spawn observation.
    pub spawn: SpawnObservation,
    /// Raw response publication observation.
    pub output: Option<PersistedAttemptOutput>,
    /// Acceptance, rejection or missing disposition.
    pub disposition: AttemptDisposition,
    /// Monotonic record revision used by persistence adapters.
    pub revision: u64,
}

/// Result of applying an observation to an attempt record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptMutation {
    /// The observation advanced the record.
    Applied,
    /// A late observation was preserved without changing acceptance.
    AppliedLate,
    /// The same observation was already present.
    Duplicate,
    /// A weaker late observation was ignored.
    IgnoredLate,
}

impl AdvisoryAttemptRecord {
    /// Construct the initial fold after capability inspection.
    ///
    /// # Errors
    /// Returns validation or capability-combination errors.
    pub fn new(
        declaration: AdvisoryAttemptDeclaration,
        capabilities: NativeCapabilityObservation,
    ) -> Result<Self, AdvisoryAttemptError> {
        declaration.validate()?;
        let capability_decision =
            assess_native_capabilities(&declaration.capability_request, &capabilities)?;
        Ok(Self {
            declaration,
            capabilities,
            capability_decision,
            effective_execution: None,
            spawn: SpawnObservation::NotAttempted,
            output: None,
            disposition: AttemptDisposition::Pending,
            revision: 0,
        })
    }

    fn advance(&mut self, late: bool) -> AttemptMutation {
        self.revision = self.revision.saturating_add(1);
        if late {
            AttemptMutation::AppliedLate
        } else {
            AttemptMutation::Applied
        }
    }

    /// Record effective settings after validating inheritance and permissions.
    ///
    /// # Errors
    /// Returns a setting or permission mismatch, or a conflict with a prior
    /// effective-settings observation.
    pub fn record_effective_execution(
        &mut self,
        effective: EffectiveExecution,
    ) -> Result<AttemptMutation, AdvisoryAttemptError> {
        validate_effective_execution(&self.declaration.requested_execution, &effective)?;
        match &self.effective_execution {
            Some(existing) if existing == &effective => Ok(AttemptMutation::Duplicate),
            Some(_) => Err(AdvisoryAttemptError::ConflictingObservation(
                "effective execution",
            )),
            None => {
                let late = !matches!(self.disposition, AttemptDisposition::Pending);
                self.effective_execution = Some(effective);
                Ok(self.advance(late))
            }
        }
    }

    /// Record the strongest known spawn result without regressing on late data.
    ///
    /// # Errors
    /// Returns [`AdvisoryAttemptError::ConflictingObservation`] when two
    /// definitive observations disagree.
    pub fn record_spawn(
        &mut self,
        observation: SpawnObservation,
    ) -> Result<AttemptMutation, AdvisoryAttemptError> {
        use SpawnObservation::{Acknowledged, NotAttempted, Refused, Uncertain};
        if self.spawn == observation {
            return Ok(AttemptMutation::Duplicate);
        }
        let replace = match (&self.spawn, &observation) {
            (NotAttempted, NotAttempted) => return Ok(AttemptMutation::Duplicate),
            (NotAttempted, _)
            | (Uncertain { .. }, Acknowledged { .. } | Refused { .. })
            | (Uncertain { .. }, Uncertain { .. })
            | (Acknowledged { .. }, Acknowledged { .. })
            | (Refused { .. }, Refused { .. }) => true,
            (Acknowledged { .. } | Refused { .. }, NotAttempted | Uncertain { .. }) => {
                return Ok(AttemptMutation::IgnoredLate)
            }
            (Acknowledged { .. }, Refused { .. }) | (Refused { .. }, Acknowledged { .. }) => {
                return Err(AdvisoryAttemptError::ConflictingObservation("spawn"))
            }
            (_, NotAttempted) => return Ok(AttemptMutation::IgnoredLate),
        };
        if replace {
            let late = !matches!(self.disposition, AttemptDisposition::Pending);
            self.spawn = observation;
            return Ok(self.advance(late));
        }
        Ok(AttemptMutation::IgnoredLate)
    }

    /// Record durable raw-response bytes after the storage adapter publishes them.
    ///
    /// # Errors
    /// Returns a size error or a conflicting-output error. A revision with new
    /// bytes must use a new attempt id rather than overwrite this record.
    pub fn record_output(
        &mut self,
        output: PersistedAttemptOutput,
    ) -> Result<AttemptMutation, AdvisoryAttemptError> {
        if output.bytes > self.declaration.limits.max_output_bytes {
            return Err(AdvisoryAttemptError::OutputTooLarge {
                bytes: output.bytes,
                limit: self.declaration.limits.max_output_bytes,
            });
        }
        match &self.output {
            Some(existing)
                if existing.digest == output.digest && existing.bytes == output.bytes =>
            {
                Ok(AttemptMutation::Duplicate)
            }
            Some(_) => Err(AdvisoryAttemptError::ConflictingObservation("output")),
            None => {
                let late = !matches!(self.disposition, AttemptDisposition::Pending);
                self.output = Some(output);
                Ok(self.advance(late))
            }
        }
    }

    /// Accept the digest already published to canonical custody.
    ///
    /// # Errors
    /// Refuses unsupported native dispatch, missing settings, absent bytes,
    /// digest mismatch, empty validation or conflict with a prior disposition.
    pub fn accept(
        &mut self,
        acceptance: AttemptAcceptance,
    ) -> Result<AttemptMutation, AdvisoryAttemptError> {
        if !self.capability_decision.is_supported() {
            return Err(AdvisoryAttemptError::UnsupportedNativeCapability);
        }
        let effective = self.effective_execution.as_ref().ok_or(
            AdvisoryAttemptError::EffectiveSettingUnavailable("effective execution"),
        )?;
        validate_effective_execution(&self.declaration.requested_execution, effective)?;
        let output = self
            .output
            .as_ref()
            .ok_or(AdvisoryAttemptError::OutputNotPersisted)?;
        if output.digest != acceptance.artifact_digest {
            return Err(AdvisoryAttemptError::ArtifactDigestMismatch {
                expected: output.digest,
                observed: acceptance.artifact_digest,
            });
        }
        if acceptance.accepted_by.trim().is_empty() || acceptance.checks.is_empty() {
            return Err(AdvisoryAttemptError::EmptyAcceptanceEvidence);
        }
        match &self.disposition {
            AttemptDisposition::Accepted(existing) if existing == &acceptance => {
                Ok(AttemptMutation::Duplicate)
            }
            AttemptDisposition::Pending => {
                self.disposition = AttemptDisposition::Accepted(acceptance);
                Ok(self.advance(false))
            }
            _ => Err(AdvisoryAttemptError::ConflictingObservation(
                "attempt disposition",
            )),
        }
    }

    /// Mark the attempt missing without converting silence into assent.
    ///
    /// # Errors
    /// Returns a conflict when the owner already recorded a different terminal
    /// disposition.
    pub fn mark_missing(
        &mut self,
        reason: String,
        at: DateTime<Utc>,
    ) -> Result<AttemptMutation, AdvisoryAttemptError> {
        let next = AttemptDisposition::Missing { reason, at };
        match &self.disposition {
            AttemptDisposition::Pending => {
                self.disposition = next;
                Ok(self.advance(false))
            }
            existing if existing == &next => Ok(AttemptMutation::Duplicate),
            AttemptDisposition::Accepted(_) => Ok(AttemptMutation::IgnoredLate),
            _ => Err(AdvisoryAttemptError::ConflictingObservation(
                "attempt disposition",
            )),
        }
    }
}

/// Reconstructed seat obligations from canonical attempt records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisoryReconstruction {
    /// Accepted artifact digest per seat.
    pub accepted: BTreeMap<AdvisorySeatId, Hash>,
    /// Required seats for which no accepted record exists.
    pub missing_required: BTreeSet<AdvisorySeatId>,
    /// Seats with a spawn whose creation remains uncertain.
    pub uncertain_spawn: BTreeSet<AdvisorySeatId>,
}

impl AdvisoryReconstruction {
    /// Report whether every required seat has an accepted artifact.
    ///
    /// Missing and pending dispositions remain unsatisfied because
    /// reconstruction places both in [`Self::missing_required`].
    #[must_use]
    pub fn all_required_accepted(&self) -> bool {
        self.missing_required.is_empty()
    }
}

/// Rebuild accepted and missing seats without provider history.
///
/// # Errors
/// Returns [`AdvisoryAttemptError::ConflictingAcceptedArtifacts`] if one seat
/// carries different accepted digests. Storage adapters must additionally
/// verify that the referenced bytes still exist before calling this function.
pub fn reconstruct_attempts(
    records: &[AdvisoryAttemptRecord],
) -> Result<AdvisoryReconstruction, AdvisoryAttemptError> {
    let mut required = BTreeSet::new();
    let mut accepted = BTreeMap::new();
    let mut uncertain_spawn = BTreeSet::new();
    for record in records {
        let seat = record.declaration.seat_id.clone();
        if record.declaration.required {
            required.insert(seat.clone());
        }
        if matches!(record.spawn, SpawnObservation::Uncertain { .. }) {
            uncertain_spawn.insert(seat.clone());
        }
        if let AttemptDisposition::Accepted(disposition) = &record.disposition {
            if let Some(existing) = accepted.insert(seat.clone(), disposition.artifact_digest) {
                if existing != disposition.artifact_digest {
                    return Err(AdvisoryAttemptError::ConflictingAcceptedArtifacts(seat));
                }
            }
        }
    }
    let missing_required = required
        .difference(&accepted.keys().cloned().collect())
        .cloned()
        .collect();
    Ok(AdvisoryReconstruction {
        accepted,
        missing_required,
        uncertain_spawn,
    })
}

/// Validation and fold errors for declared advisory attempts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdvisoryAttemptError {
    /// Identifier was not a portable token.
    #[error("invalid {kind} identifier: {value:?}")]
    InvalidIdentifier {
        /// Identifier class.
        kind: &'static str,
        /// Rejected value.
        value: String,
    },
    /// Response path escaped the canonical advisory response area.
    #[error("invalid advisory artifact path: {0:?}")]
    InvalidArtifactPath(String),
    /// Record schema is newer or older than this implementation.
    #[error("unsupported advisory attempt schema {0}")]
    UnsupportedSchema(u16),
    /// One or more finite bounds was zero.
    #[error("advisory attempt limits must be finite and non-zero")]
    InvalidLimits,
    /// Requested tool belongs to a different protocol family.
    #[error("tool {tool:?} is not valid for protocol {protocol:?}")]
    InvalidToolCombination {
        /// Requested protocol.
        protocol: NativeAdvisoryProtocol,
        /// Incompatible tool.
        tool: NativeAdvisoryTool,
    },
    /// Required effective setting was not observed.
    #[error("effective {0} is unavailable")]
    EffectiveSettingUnavailable(&'static str),
    /// Effective role or model differed from the declaration.
    #[error("effective {field} changed: requested {requested:?}, observed {observed:?}")]
    EffectiveSettingMismatch {
        /// Setting name.
        field: &'static str,
        /// Frozen request.
        requested: String,
        /// Effective value.
        observed: String,
    },
    /// Effective access included undeclared faculties.
    #[error("effective permissions widened: {0:?}")]
    PermissionWidened(Vec<String>),
    /// Required denials were not observed.
    #[error("required permission denials missing: {0:?}")]
    MissingPermissionDenial(Vec<String>),
    /// Denial existed only as convention or was unavailable.
    #[error("permission denial is not effect-boundary enforced: {0:?}")]
    PermissionNotEnforced(PermissionEnforcement),
    /// Two observations for an immutable boundary disagreed.
    #[error("conflicting advisory observation: {0}")]
    ConflictingObservation(&'static str),
    /// Raw response exceeded its registered byte ceiling.
    #[error("advisory output has {bytes} bytes, limit is {limit}")]
    OutputTooLarge {
        /// Observed byte length.
        bytes: u64,
        /// Registered ceiling.
        limit: u64,
    },
    /// Acceptance was attempted before canonical output publication.
    #[error("advisory output is not persisted")]
    OutputNotPersisted,
    /// Acceptance referenced bytes other than the published response.
    #[error("artifact digest mismatch: expected {expected}, observed {observed}")]
    ArtifactDigestMismatch {
        /// Published digest.
        expected: Hash,
        /// Acceptance digest.
        observed: Hash,
    },
    /// Acceptance carried no owner or validation checks.
    #[error("acceptance requires an owner and at least one validation check")]
    EmptyAcceptanceEvidence,
    /// Unsupported native surface cannot yield an accepted native artifact.
    #[error("native capability decision is unsupported")]
    UnsupportedNativeCapability,
    /// One seat carried more than one accepted digest.
    #[error("seat {0} has conflicting accepted artifact digests")]
    ConflictingAcceptedArtifacts(AdvisorySeatId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed<T>(value: T) -> AdvisoryObservation<T> {
        AdvisoryObservation::Observed {
            value,
            source: "test.v1".to_owned(),
        }
    }

    fn declaration(required: bool) -> AdvisoryAttemptDeclaration {
        AdvisoryAttemptDeclaration {
            schema_version: ADVISORY_ATTEMPT_SCHEMA_VERSION,
            attempt_id: AdvisoryAttemptId::new("architect-1").unwrap(),
            seat_id: AdvisorySeatId::new("architect").unwrap(),
            required,
            assignment_digest: Hash::of_bytes(b"assignment"),
            provenance: AttemptProvenance {
                source_revision: "source-revision".to_owned(),
                input_digest: Hash::of_bytes(b"input"),
                scope_revision: Hash::of_bytes(b"scope"),
            },
            output_path: AdvisoryArtifactPath::new("advisory/responses/architect-1.md").unwrap(),
            requested_execution: RequestedExecution {
                role: "reviewer".to_owned(),
                model: "model-a".to_owned(),
                permissions: RequestedPermissionEnvelope {
                    allowed: BTreeSet::from(["read_sources".to_owned()]),
                    denied: BTreeSet::from(["lifecycle".to_owned()]),
                },
            },
            capability_request: NativeCapabilityRequest {
                protocol: NativeAdvisoryProtocol::V2,
                required_tools: BTreeSet::from([
                    NativeAdvisoryTool::Spawn,
                    NativeAdvisoryTool::Message,
                ]),
                fallback: AdvisoryFallback::SeparateMolecule,
            },
            limits: AdvisoryAttemptLimits {
                max_attempts: 1,
                max_concurrency: 1,
                deadline: DateTime::from_timestamp(2_000_000_000, 0).unwrap(),
                max_output_bytes: 4096,
                max_messages: 4,
                max_message_bytes: 4096,
                max_discussion_rounds: 1,
            },
            recorded_at: DateTime::from_timestamp(1_900_000_000, 0).unwrap(),
        }
    }

    fn capabilities() -> NativeCapabilityObservation {
        NativeCapabilityObservation {
            binary_version: observed("0.157.1".to_owned()),
            realized_model: observed("model-a".to_owned()),
            feature_flags: observed(BTreeMap::from([
                ("multi_agent".to_owned(), true),
                ("multi_agent_v2".to_owned(), true),
            ])),
            tools: observed(BTreeSet::from([
                NativeAdvisoryTool::Spawn,
                NativeAdvisoryTool::Message,
                NativeAdvisoryTool::Wait,
            ])),
            protocol: observed(NativeAdvisoryProtocol::V2),
        }
    }

    fn effective() -> EffectiveExecution {
        EffectiveExecution {
            role: observed("reviewer".to_owned()),
            model: observed("model-a".to_owned()),
            permissions: observed(EffectivePermissionEnvelope {
                allowed: BTreeSet::from(["read_sources".to_owned()]),
                denied: BTreeSet::from(["lifecycle".to_owned()]),
                enforcement: PermissionEnforcement::EffectBoundary,
            }),
        }
    }

    fn ready_record() -> (AdvisoryAttemptRecord, Hash) {
        let mut record = AdvisoryAttemptRecord::new(declaration(true), capabilities()).unwrap();
        record.record_effective_execution(effective()).unwrap();
        let digest = Hash::of_bytes(b"response");
        record
            .record_output(PersistedAttemptOutput {
                digest,
                bytes: 8,
                persisted_at: Utc::now(),
            })
            .unwrap();
        (record, digest)
    }

    #[test]
    fn invalid_protocol_tool_combination_is_clear() {
        let mut request = declaration(true).capability_request;
        request.protocol = NativeAdvisoryProtocol::V1;
        let error = assess_native_capabilities(&request, &capabilities()).unwrap_err();
        assert!(matches!(
            error,
            AdvisoryAttemptError::InvalidToolCombination {
                tool: NativeAdvisoryTool::Message,
                ..
            }
        ));
    }

    #[test]
    fn unsupported_surface_selects_declared_fallback() {
        let request = declaration(true).capability_request;
        let mut observation = capabilities();
        observation.tools = observed(BTreeSet::from([NativeAdvisoryTool::Spawn]));
        let decision = assess_native_capabilities(&request, &observation).unwrap();
        assert!(matches!(
            decision,
            NativeCapabilityDecision::Unsupported {
                selected_fallback: AdvisoryFallback::SeparateMolecule,
                ..
            }
        ));
    }

    #[test]
    fn prompt_only_permission_denial_is_refused() {
        let mut settings = effective();
        let AdvisoryObservation::Observed { value, .. } = &mut settings.permissions else {
            panic!("fixture is observed")
        };
        value.enforcement = PermissionEnforcement::ConventionOnly;
        let error = validate_effective_execution(&declaration(true).requested_execution, &settings)
            .unwrap_err();
        assert_eq!(
            error,
            AdvisoryAttemptError::PermissionNotEnforced(PermissionEnforcement::ConventionOnly)
        );
    }

    #[test]
    fn acceptance_requires_persisted_matching_bytes() {
        let mut record = AdvisoryAttemptRecord::new(declaration(true), capabilities()).unwrap();
        record.record_effective_execution(effective()).unwrap();
        let digest = Hash::of_bytes(b"response");
        let acceptance = AttemptAcceptance {
            artifact_digest: digest,
            accepted_by: "owner".to_owned(),
            checks: vec!["complete response".to_owned()],
            limitations: Vec::new(),
            accepted_at: Utc::now(),
        };
        assert_eq!(
            record.accept(acceptance.clone()).unwrap_err(),
            AdvisoryAttemptError::OutputNotPersisted
        );
        record
            .record_output(PersistedAttemptOutput {
                digest,
                bytes: 8,
                persisted_at: Utc::now(),
            })
            .unwrap();
        assert_eq!(record.accept(acceptance).unwrap(), AttemptMutation::Applied);
    }

    #[test]
    fn acceptance_refuses_digest_that_differs_from_persisted_output() {
        let (mut record, persisted_digest) = ready_record();
        let acceptance_digest = Hash::of_bytes(b"different response");

        assert_eq!(
            record
                .accept(AttemptAcceptance {
                    artifact_digest: acceptance_digest,
                    accepted_by: "owner".to_owned(),
                    checks: vec!["digest and rubric".to_owned()],
                    limitations: Vec::new(),
                    accepted_at: Utc::now(),
                })
                .unwrap_err(),
            AdvisoryAttemptError::ArtifactDigestMismatch {
                expected: persisted_digest,
                observed: acceptance_digest,
            }
        );
        assert_eq!(record.disposition, AttemptDisposition::Pending);
    }

    #[test]
    fn acceptance_requires_owner_and_validation_check() {
        let (mut no_owner, digest) = ready_record();
        assert_eq!(
            no_owner
                .accept(AttemptAcceptance {
                    artifact_digest: digest,
                    accepted_by: "  ".to_owned(),
                    checks: vec!["digest and rubric".to_owned()],
                    limitations: Vec::new(),
                    accepted_at: Utc::now(),
                })
                .unwrap_err(),
            AdvisoryAttemptError::EmptyAcceptanceEvidence
        );
        assert_eq!(no_owner.disposition, AttemptDisposition::Pending);

        let (mut no_checks, digest) = ready_record();
        assert_eq!(
            no_checks
                .accept(AttemptAcceptance {
                    artifact_digest: digest,
                    accepted_by: "owner".to_owned(),
                    checks: Vec::new(),
                    limitations: Vec::new(),
                    accepted_at: Utc::now(),
                })
                .unwrap_err(),
            AdvisoryAttemptError::EmptyAcceptanceEvidence
        );
        assert_eq!(no_checks.disposition, AttemptDisposition::Pending);
    }

    #[test]
    fn duplicate_and_late_observations_do_not_regress_acceptance() {
        let mut record = AdvisoryAttemptRecord::new(declaration(true), capabilities()).unwrap();
        record.record_effective_execution(effective()).unwrap();
        let digest = Hash::of_bytes(b"response");
        let output = PersistedAttemptOutput {
            digest,
            bytes: 8,
            persisted_at: Utc::now(),
        };
        assert_eq!(
            record.record_output(output.clone()).unwrap(),
            AttemptMutation::Applied
        );
        assert_eq!(
            record.record_output(output).unwrap(),
            AttemptMutation::Duplicate
        );
        record
            .accept(AttemptAcceptance {
                artifact_digest: digest,
                accepted_by: "owner".to_owned(),
                checks: vec!["digest and rubric".to_owned()],
                limitations: Vec::new(),
                accepted_at: Utc::now(),
            })
            .unwrap();
        assert_eq!(
            record
                .mark_missing("late completion notice loss".to_owned(), Utc::now())
                .unwrap(),
            AttemptMutation::IgnoredLate
        );
        assert!(matches!(
            record.disposition,
            AttemptDisposition::Accepted(_)
        ));
    }

    #[test]
    fn reconstruction_uses_accepted_digests_not_spawn_notices() {
        let mut accepted = AdvisoryAttemptRecord::new(declaration(true), capabilities()).unwrap();
        accepted.record_effective_execution(effective()).unwrap();
        let digest = Hash::of_bytes(b"response");
        accepted
            .record_output(PersistedAttemptOutput {
                digest,
                bytes: 8,
                persisted_at: Utc::now(),
            })
            .unwrap();
        accepted
            .accept(AttemptAcceptance {
                artifact_digest: digest,
                accepted_by: "owner".to_owned(),
                checks: vec!["rubric".to_owned()],
                limitations: Vec::new(),
                accepted_at: Utc::now(),
            })
            .unwrap();

        let mut missing_decl = declaration(true);
        missing_decl.attempt_id = AdvisoryAttemptId::new("turing-1").unwrap();
        missing_decl.seat_id = AdvisorySeatId::new("turing").unwrap();
        missing_decl.output_path =
            AdvisoryArtifactPath::new("advisory/responses/turing-1.md").unwrap();
        let mut missing = AdvisoryAttemptRecord::new(missing_decl, capabilities()).unwrap();
        missing
            .record_spawn(SpawnObservation::Acknowledged { at: Utc::now() })
            .unwrap();

        let reconstruction = reconstruct_attempts(&[accepted, missing]).unwrap();
        assert_eq!(
            reconstruction
                .accepted
                .get(&AdvisorySeatId::new("architect").unwrap()),
            Some(&digest)
        );
        assert!(reconstruction
            .missing_required
            .contains(&AdvisorySeatId::new("turing").unwrap()));
        assert!(!reconstruction.all_required_accepted());
    }

    #[test]
    fn missing_or_pending_required_seat_never_counts_as_fully_accepted() {
        let mut missing = AdvisoryAttemptRecord::new(declaration(true), capabilities()).unwrap();
        missing
            .mark_missing("provider returned no artifact".to_owned(), Utc::now())
            .unwrap();
        let missing_reconstruction = reconstruct_attempts(&[missing]).unwrap();
        assert!(!missing_reconstruction.all_required_accepted());

        let pending = AdvisoryAttemptRecord::new(declaration(true), capabilities()).unwrap();
        let pending_reconstruction = reconstruct_attempts(&[pending]).unwrap();
        assert!(!pending_reconstruction.all_required_accepted());
    }

    #[test]
    fn artifact_paths_cannot_reach_lifecycle_state() {
        assert!(AdvisoryArtifactPath::new("state.json").is_err());
        assert!(AdvisoryArtifactPath::new("advisory/responses/../../state.json").is_err());
        assert!(AdvisoryArtifactPath::new("advisory/responses/review.md").is_ok());
    }
}
