// SPDX-License-Identifier: AGPL-3.0-only

//! Fleet specification — declarative agent fleet deployment.
//!
//! A fleet spec is a TOML-defined topology of agents with communication
//! channels. It declares WHO exists and WHO can talk to WHOM. Workflow
//! logic (what each agent does) belongs in formulas, not here.
//!
//! # Two forms of `fleet.toml`
//!
//! **Legacy (monolithic)** — a single fleet with a flat name:
//!
//! ```toml
//! fleet = "example"
//! version = 1
//!
//! [[agents]]
//! name = "writer"
//! role = "implementation"
//! clearance = "write"
//! ```
//!
//! **New (composable, ADR-038/ADR-039)** — `[fleet]` block with identity
//! and optional composition declarations:
//!
//! ```toml
//! [fleet]
//! schema_version = 1
//! id = "master"
//!
//! [[fleet.include]]
//! source = "file:./fleets/wiki.toml"
//! as = "wiki"
//!
//! [[agents]]
//! name = "writer"
//! role = "implementation"
//! clearance = "write"
//! ```
//!
//! Both forms parse through [`FleetSpec::parse`]. Legacy form is preserved
//! byte-for-byte for backward compatibility; it cannot carry `[[fleet.include]]`
//! declarations.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde::Deserialize;

use crate::agent::AgentRole;
use crate::clearance::Clearance;
use crate::id::AgentId;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from parsing or validating a fleet spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set will grow; external callers must keep a `_ =>` arm
pub enum FleetSpecError {
    /// The TOML source is syntactically invalid.
    #[error("TOML parse error: {0}")]
    Toml(String),

    /// No agents declared.
    #[error("fleet must have at least one agent")]
    NoAgents,

    /// Two agents share the same name.
    #[error("duplicate agent name: {0}")]
    DuplicateAgent(String),

    /// A channel references an agent that doesn't exist.
    #[error("channel references unknown agent: {0}")]
    UnknownAgent(String),

    /// An agent name failed ID validation.
    #[error("invalid agent name: {0}")]
    InvalidName(String),

    /// A self-loop channel (agent talking to itself).
    #[error("self-loop channel: {0} -> {0}")]
    SelfLoop(String),

    /// Missing top-level fleet declaration (neither `fleet = "..."` nor `[fleet]`).
    #[error("missing fleet declaration: expected either `fleet = \"...\"` or `[fleet]` block")]
    MissingFleet,

    /// The `[fleet].id` field was missing or malformed.
    #[error("missing or invalid `fleet.id`: {0}")]
    InvalidFleetId(String),

    /// Unsupported fleet schema version (v0 resolver expects `schema_version = 1`).
    #[error("unsupported fleet schema_version: {0} (v0 supports only 1)")]
    UnsupportedSchemaVersion(u32),

    /// An `[[fleet.include]]` entry used a URI scheme not supported in v0.
    ///
    /// v0 accepts only `file:` scheme. `git+https:` and `cas:sha256-...` are
    /// reserved for future versions (ADR-035) and parse-but-error for now.
    #[error("include uri scheme not implemented in v0: {scheme} (uri={uri})")]
    IncludeSchemeUnsupported {
        /// The scheme portion of the URI (e.g. `git+https`, `cas`).
        scheme: String,
        /// The full `source` field as written in the TOML.
        uri: String,
    },

    /// An `[[fleet.include]]` entry had no URI scheme or a malformed one.
    #[error("include source is not a valid URI (expected `file:...` or `scheme:...`): {0}")]
    IncludeSourceInvalid(String),

    /// Composition produced duplicate agent ids across fleets — v0 rejects these
    /// loudly with the collision site so the operator can rename via `as =`.
    ///
    /// Boxed to keep `FleetSpecError` itself small (clippy
    /// `result_large_err`).
    #[error(transparent)]
    DuplicateAgentAcrossFleets(Box<DuplicateAgentAcrossFleetsDetails>),

    /// A `fleet.id` was malformed (must match `[a-z0-9][a-z0-9-]*`).
    #[error("invalid fleet.id `{0}`: must match [a-z0-9][a-z0-9-]*")]
    MalformedFleetId(String),

    /// A child fleet carried a field forbidden in included fleets (tolnay
    /// — reserve operational knobs to the master).
    #[error("field `{field}` is not allowed in an included child fleet ({fleet_id})")]
    ForbiddenChildField {
        /// The offending field name.
        field: String,
        /// Child fleet id.
        fleet_id: String,
    },
}

/// Payload for [`FleetSpecError::DuplicateAgentAcrossFleets`].
///
/// Carried behind a `Box` to keep the parent enum compact (clippy
/// `result_large_err`) while preserving a rich, user-facing error message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "duplicate agent id `{agent}` in composed fleet\n  \
     — defined in `{fleet_a}` (from {source_a}{line_a})\n  \
     — redefined in `{fleet_b}` (from {source_b}{line_b})\n  \
     hint: rename with `as = \"...\"` on one of the `[[fleet.include]]` entries"
)]
pub struct DuplicateAgentAcrossFleetsDetails {
    /// The duplicated agent name (namespaced if `as` was applied).
    pub agent: String,
    /// `FleetId` that contributed the first definition.
    pub fleet_a: String,
    /// Human-readable source path of `fleet_a` (e.g. the include URI).
    pub source_a: String,
    /// Line tag for `fleet_a` definition site (empty string if unknown).
    pub line_a: String,
    /// `FleetId` that contributed the redefinition.
    pub fleet_b: String,
    /// Human-readable source path of `fleet_b`.
    pub source_b: String,
    /// Line tag for `fleet_b` definition site.
    pub line_b: String,
}

// ---------------------------------------------------------------------------
// Raw TOML shapes
// ---------------------------------------------------------------------------

/// Raw deserialization of the root `fleet` field.
///
/// Accepts either the legacy string form (`fleet = "name"`) or the new
/// `[fleet]` table. This is the only load-time branch between the two
/// formats; everything below is shared.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawFleetField {
    /// Legacy: `fleet = "name"`.
    Legacy(String),
    /// New composable form: `[fleet]` table with `id`, `schema_version`,
    /// and optional `[[fleet.include]]`.
    Block(RawFleetBlock),
}

#[derive(Deserialize)]
struct RawFleetBlock {
    id: String,
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    include: Vec<RawInclude>,
}

fn default_schema_version() -> u32 {
    1
}

#[derive(Deserialize)]
struct RawInclude {
    source: String,
    #[serde(default, rename = "as")]
    as_prefix: Option<String>,
}

#[derive(Deserialize)]
struct RawFleetSpec {
    fleet: RawFleetField,
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    description: String,
    #[serde(default)]
    workdir: Option<String>,
    /// Free-form, advisory operator self-classification.
    /// Carried as `Option<String>` — never matched on, never enumerated.
    #[serde(default)]
    organization_type: Option<String>,
    /// Optional, exogenous review policy for every molecule in this fleet.
    #[serde(default)]
    review: CrossProviderReview,
    #[serde(default)]
    agents: Vec<RawAgentSpec>,
    #[serde(default)]
    channels: Vec<RawChannel>,
    #[serde(default)]
    constitution: Option<RawConstitution>,
    #[serde(default)]
    grades: Vec<RawGrade>,
}

fn default_version() -> u32 {
    1
}

#[derive(Deserialize)]
struct RawAgentSpec {
    name: String,
    role: String,
    clearance: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawConstitution {
    #[serde(default)]
    pillars: Vec<String>,
    #[serde(default)]
    conflict_resolution: Option<String>,
}

#[derive(Deserialize)]
struct RawGrade {
    id: String,
    #[serde(default)]
    description: String,
    order: u32,
    #[serde(default)]
    promote_requires: Option<RawPromoteRequires>,
}

#[derive(Deserialize)]
struct RawPromoteRequires {
    role: String,
    #[serde(default = "default_min_reviews")]
    min_reviews: u32,
}

fn default_min_reviews() -> u32 {
    1
}

#[derive(Deserialize)]
struct RawChannel {
    from: String,
    to: String,
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// A declared `[[fleet.include]]` entry — NOT yet resolved.
///
/// Used by the resolver (outside this crate, which keeps cosmon-core I/O-free)
/// to walk referenced files. The parse step only validates the URI shape
/// and the `as` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetInclude {
    /// Raw URI as written in TOML (e.g. `"file:./fleets/wiki.toml"`).
    pub source: String,
    /// The scheme portion (`file`, `git+https`, `cas`).
    pub scheme: String,
    /// The scheme-less remainder (e.g. `./fleets/wiki.toml`).
    pub path: String,
    /// Optional namespace prefix applied to agent names from the included fleet.
    ///
    /// When `Some("wiki")`, an agent named `editor` in the included fleet
    /// appears as `wiki:editor` in the composed fleet.
    pub as_prefix: Option<String>,
}

/// A parsed, validated fleet specification.
#[derive(Debug, Clone)]
pub struct FleetSpec {
    /// Fleet name (used as deployment identifier and as `FleetId`).
    pub name: String,
    /// Schema version of the composable fleet format (ADR-039).
    ///
    /// Always `1` for v0. Legacy monolithic fleets (without `[fleet]` block)
    /// default to `1`.
    pub schema_version: u32,
    /// Spec version (legacy field, orthogonal to `schema_version`).
    pub version: u32,
    /// Human-readable description.
    pub description: String,
    /// Default working directory for all agents.
    pub workdir: Option<String>,
    /// Free-form, advisory operator self-classification.
    ///
    /// This is **purely advisory IFBDD instrumentation**: cosmon emits a
    /// [`FleetTyped`](crate::event_v2::EventV2::FleetTyped) event whenever a
    /// fleet with this field set is loaded, but no code path branches on
    /// the value. There is no enum, no canonical list, no validation
    /// beyond "is a string". The field exists so that later — when
    /// either ≥3 fleets converge on the same value with the same
    /// operational meaning, or N≥2 distinct human operators exist with
    /// observable preference divergences, or a concrete user need names
    /// exactly which code path needs to branch on it — we can revisit
    /// the decision with empirical evidence rather than speculation.
    /// Until then: write whatever phrase fits, change it freely, no
    /// behavioural difference.
    pub organization_type: Option<String>,
    /// Opt-in cross-provider review policy projected onto newly nucleated work.
    ///
    /// It is deliberately off by default: routine work does not acquire a
    /// committee merely because of its criticality label.
    pub review: CrossProviderReview,
    /// Agent definitions in this fleet.
    pub agents: Vec<FleetAgentSpec>,
    /// Directed communication channels (adjacency list).
    channels: HashMap<String, HashSet<String>>,
    /// Fleet constitution (pillars, conflict resolution rules).
    pub constitution: Option<Constitution>,
    /// Quality grade state machine.
    pub grades: Vec<Grade>,
    /// Declared includes (unresolved — the caller walks them).
    pub includes: Vec<FleetInclude>,
}

/// Opt-in cross-provider review policy for a fleet or spore.
///
/// In TOML this is expressed as `[review] cross_provider = true`, with an
/// optional `reviewer_adapter` pin.  When enabled, callers project both the
/// `needs-review` reservation and the more specific
/// `needs-review-cross-provider` marker onto the work they create.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CrossProviderReview {
    /// Whether this declaration opts work into cross-provider review.
    #[serde(default)]
    pub cross_provider: bool,
    /// Optional adapter selected for the independent refuter.
    #[serde(default)]
    pub reviewer_adapter: Option<String>,
}

/// An agent declared in the fleet spec.
#[derive(Debug, Clone)]
pub struct FleetAgentSpec {
    /// Agent name (becomes the `WorkerId` when spawned).
    pub name: AgentId,
    /// Role within the fleet.
    pub role: AgentRole,
    /// Permission level.
    pub clearance: Clearance,
    /// Optional initial prompt to send after spawn.
    pub prompt: Option<String>,
    /// Optional model override (e.g. "haiku", "sonnet", "opus").
    pub model: Option<String>,
    /// Fleet identity of the fleet file that originally declared this agent.
    ///
    /// Set by [`FleetSpec::compose`] during load-time flattening so the
    /// operator frame preserves provenance even after the runtime sees one
    /// flat fleet (einstein — frame-equivalence with observability bit).
    ///
    /// `None` on agents from a freshly-parsed un-flattened `FleetSpec`.
    pub origin_fleet_id: Option<String>,
}

/// Fleet constitution — declarative rules auto-injected into agent prompts.
#[derive(Debug, Clone)]
pub struct Constitution {
    /// Foundational principles (e.g. Wikipedia's Five Pillars).
    pub pillars: Vec<String>,
    /// Conflict resolution method.
    pub conflict_resolution: Option<String>,
}

/// A quality grade in the article state machine.
#[derive(Debug, Clone)]
pub struct Grade {
    /// Grade identifier (e.g. "stub", "good-article", "featured").
    pub id: String,
    /// Description.
    pub description: String,
    /// Ordering (lower = lower quality).
    pub order: u32,
    /// Role required to promote to this grade.
    pub promote_role: Option<String>,
    /// Minimum independent reviews required for promotion.
    pub min_reviews: u32,
}

impl FleetSpec {
    /// Create a minimal single-agent fleet spec without any I/O.
    ///
    /// Returns a fleet with one agent named `"default"` with role
    /// `Implementation` and clearance `Write`. No channels, no
    /// constitution, no grades, no includes.
    ///
    /// This is the fleet-of-one fallback used by `cs tackle` when no
    /// `fleet.toml` is present — it ensures the dispatch path always
    /// has a `FleetSpec` in hand (ADR-040).
    #[inline]
    #[must_use]
    #[allow(clippy::missing_panics_doc)] // "default" is a valid static AgentId — cannot panic.
    pub fn default_singleton() -> Self {
        let agent_id = AgentId::new("default").expect("static agent id \"default\" is valid");
        Self {
            name: String::from("default"),
            schema_version: 1,
            version: 1,
            description: String::new(),
            workdir: None,
            organization_type: None,
            review: CrossProviderReview::default(),
            agents: vec![FleetAgentSpec {
                name: agent_id,
                role: AgentRole::Implementation,
                clearance: Clearance::Write,
                prompt: None,
                model: None,
                origin_fleet_id: None,
            }],
            channels: HashMap::new(),
            constitution: None,
            grades: Vec::new(),
            includes: Vec::new(),
        }
    }

    /// Parse a fleet spec from TOML text.
    ///
    /// Accepts both legacy (`fleet = "name"`) and new (`[fleet]` block)
    /// top-level forms. The `includes` list is populated only when the
    /// new form is used; the resolver that actually follows the includes
    /// lives outside this crate to keep cosmon-core I/O-free.
    ///
    /// # Errors
    ///
    /// Returns [`FleetSpecError`] if the TOML is invalid, agents are
    /// missing or duplicated, channels reference unknown agents, or the
    /// `[fleet]` block / include URIs are malformed.
    pub fn parse(toml_text: &str) -> Result<Self, FleetSpecError> {
        let raw: RawFleetSpec =
            toml::from_str(toml_text).map_err(|e| FleetSpecError::Toml(e.to_string()))?;

        // Branch on the top-level `fleet` form.
        let (name, schema_version, includes) = match raw.fleet {
            RawFleetField::Legacy(s) => (s, 1u32, Vec::new()),
            RawFleetField::Block(block) => {
                validate_fleet_id(&block.id)?;
                if block.schema_version != 1 {
                    return Err(FleetSpecError::UnsupportedSchemaVersion(
                        block.schema_version,
                    ));
                }
                let mut includes = Vec::with_capacity(block.include.len());
                for ri in block.include {
                    includes.push(parse_include(&ri)?);
                }
                (block.id, block.schema_version, includes)
            }
        };

        if raw.agents.is_empty() && includes.is_empty() {
            return Err(FleetSpecError::NoAgents);
        }

        // Parse and validate agents.
        let mut agents = Vec::with_capacity(raw.agents.len());
        let mut agent_names: HashSet<String> = HashSet::new();

        for ra in &raw.agents {
            if !agent_names.insert(ra.name.clone()) {
                return Err(FleetSpecError::DuplicateAgent(ra.name.clone()));
            }

            let name =
                AgentId::new(&ra.name).map_err(|e| FleetSpecError::InvalidName(e.to_string()))?;
            let role: AgentRole = ra
                .role
                .parse()
                .map_err(|_| FleetSpecError::InvalidName(format!("unknown role: {}", ra.role)))?;
            let clearance: Clearance = ra.clearance.parse().map_err(|_| {
                FleetSpecError::InvalidName(format!("unknown clearance: {}", ra.clearance))
            })?;

            agents.push(FleetAgentSpec {
                name,
                role,
                clearance,
                prompt: ra.prompt.clone(),
                model: ra.model.clone(),
                origin_fleet_id: None,
            });
        }

        // Parse and validate channels.
        let mut channels: HashMap<String, HashSet<String>> = HashMap::new();

        for rc in &raw.channels {
            if !agent_names.contains(&rc.from) {
                return Err(FleetSpecError::UnknownAgent(rc.from.clone()));
            }
            if !agent_names.contains(&rc.to) {
                return Err(FleetSpecError::UnknownAgent(rc.to.clone()));
            }
            if rc.from == rc.to {
                return Err(FleetSpecError::SelfLoop(rc.from.clone()));
            }

            channels
                .entry(rc.from.clone())
                .or_default()
                .insert(rc.to.clone());
        }

        // Parse constitution.
        let constitution = raw.constitution.map(|c| Constitution {
            pillars: c.pillars,
            conflict_resolution: c.conflict_resolution,
        });

        // Parse grades.
        let grades: Vec<Grade> = raw
            .grades
            .into_iter()
            .map(|g| Grade {
                id: g.id,
                description: g.description,
                order: g.order,
                promote_role: g.promote_requires.as_ref().map(|r| r.role.clone()),
                min_reviews: g.promote_requires.as_ref().map_or(1, |r| r.min_reviews),
            })
            .collect();

        Ok(Self {
            name,
            schema_version,
            version: raw.version,
            description: raw.description,
            workdir: raw.workdir,
            organization_type: raw.organization_type,
            review: raw.review,
            agents,
            channels,
            constitution,
            grades,
            includes,
        })
    }

    /// Compose a parent fleet with already-parsed child fleets into ONE
    /// flat fleet (ADR-038).
    ///
    /// This is a pure, total, deterministic function: `compose: [Fleet] → Fleet`,
    /// run at load time. The runtime only ever sees the flattened result
    /// (einstein — frame-equivalence).
    ///
    /// Each child is provided alongside the resolved [`FleetInclude`] that
    /// caused it to be loaded (so `as` prefixes and provenance metadata
    /// can be applied correctly).
    ///
    /// # Behavior
    ///
    /// - Every agent from every child is added to the composed fleet, prefixed
    ///   with `"<as>:<agent>"` if `include.as_prefix` is `Some`.
    /// - Every agent carries `origin_fleet_id` pointing at its source fleet's id.
    /// - Channels from children are preserved (renamed if `as_prefix` applies).
    /// - Duplicate agent ids across fleets trigger [`FleetSpecError::DuplicateAgentAcrossFleets`]
    ///   naming both fleets and suggesting `as =`.
    /// - Constitution and grades from children are currently dropped in v0
    ///   (no user-facing merge policy — the parent's constitution wins).
    ///
    /// # Errors
    ///
    /// Returns [`FleetSpecError::DuplicateAgentAcrossFleets`] on any
    /// cross-fleet agent collision.
    pub fn compose(
        parent: FleetSpec,
        children: Vec<(FleetInclude, FleetSpec, String)>,
    ) -> Result<FleetSpec, FleetSpecError> {
        // children: (include-decl, parsed-fleet, source-string-for-error-messages)
        // Parent source string — we only know its id; no file provenance at compose time.
        let parent_source = format!("<master:{}>", parent.name);

        let mut out_agents: Vec<FleetAgentSpec> = Vec::new();
        let mut out_channels: HashMap<String, HashSet<String>> = HashMap::new();
        let mut provenance: HashMap<String, (String, String)> = HashMap::new();
        // provenance: agent_name → (fleet_id, source_string)

        // 1. Parent agents first — they take provenance "master" ids.
        for mut a in parent.agents {
            let name = a.name.as_str().to_owned();
            a.origin_fleet_id = Some(parent.name.clone());
            provenance.insert(name.clone(), (parent.name.clone(), parent_source.clone()));
            out_agents.push(a);
        }
        for (from, tos) in parent.channels {
            out_channels.entry(from).or_default().extend(tos);
        }

        // 2. Walk children in declared order.
        for (include, child, child_source) in children {
            let prefix = include.as_prefix.as_deref();

            // Agents — rename with prefix, carry provenance.
            for mut a in child.agents {
                let effective_name = match prefix {
                    Some(p) => format!("{p}:{}", a.name.as_str()),
                    None => a.name.as_str().to_owned(),
                };
                if let Some((other_fleet, other_src)) = provenance.get(&effective_name) {
                    return Err(FleetSpecError::DuplicateAgentAcrossFleets(Box::new(
                        DuplicateAgentAcrossFleetsDetails {
                            agent: effective_name,
                            fleet_a: other_fleet.clone(),
                            source_a: other_src.clone(),
                            line_a: String::new(),
                            fleet_b: child.name.clone(),
                            source_b: child_source.clone(),
                            line_b: String::new(),
                        },
                    )));
                }
                provenance.insert(
                    effective_name.clone(),
                    (child.name.clone(), child_source.clone()),
                );
                let new_id = AgentId::new(&effective_name)
                    .map_err(|e| FleetSpecError::InvalidName(e.to_string()))?;
                a.name = new_id;
                a.origin_fleet_id = Some(child.name.clone());
                out_agents.push(a);
            }

            // Channels — rename endpoints with prefix.
            for (from, tos) in child.channels {
                let from_eff = prefix.map_or(from.clone(), |p| format!("{p}:{from}"));
                let renamed_tos: HashSet<String> = tos
                    .into_iter()
                    .map(|t| prefix.map_or(t.clone(), |p| format!("{p}:{t}")))
                    .collect();
                out_channels
                    .entry(from_eff)
                    .or_default()
                    .extend(renamed_tos);
            }
        }

        Ok(FleetSpec {
            name: parent.name,
            schema_version: parent.schema_version,
            version: parent.version,
            description: parent.description,
            workdir: parent.workdir,
            // Parent's `organization_type` wins for the composed fleet —
            // children's values are dropped (same policy as constitution
            // and grades). The composed fleet IS the master's view; child
            // metadata only travels via per-agent `origin_fleet_id`.
            organization_type: parent.organization_type,
            review: parent.review,
            agents: out_agents,
            channels: out_channels,
            constitution: parent.constitution,
            grades: parent.grades,
            // After flattening, the composed fleet no longer carries the
            // include list — it IS the result of resolving them.
            includes: Vec::new(),
        })
    }

    /// Build a constitution preamble to prepend to agent prompts.
    ///
    /// Returns `None` if no constitution is defined.
    #[must_use]
    pub fn constitution_preamble(&self) -> Option<String> {
        let c = self.constitution.as_ref()?;
        if c.pillars.is_empty() {
            return None;
        }
        let mut text = String::from("CONSTITUTION (fleet-level rules — always apply):\n");
        for (i, pillar) in c.pillars.iter().enumerate() {
            let _ = writeln!(text, "{}. {pillar}", i + 1);
        }
        if let Some(ref cr) = c.conflict_resolution {
            let _ = writeln!(text, "\nConflict resolution: {cr}");
        }
        Some(text)
    }

    /// Build a grades summary for agent context.
    #[must_use]
    pub fn grades_summary(&self) -> Option<String> {
        if self.grades.is_empty() {
            return None;
        }
        let mut text = String::from("QUALITY GRADES (article quality ladder):\n");
        let mut sorted = self.grades.clone();
        sorted.sort_by_key(|g| g.order);
        for g in &sorted {
            let req = g.promote_role.as_deref().unwrap_or("any");
            let _ = writeln!(
                text,
                "  {} (order {}): {} [requires: {}, min reviews: {}]",
                g.id, g.order, g.description, req, g.min_reviews
            );
        }
        Some(text)
    }

    /// Check whether agent `from` can send messages to agent `to`.
    #[must_use]
    pub fn can_send(&self, from: &str, to: &str) -> bool {
        self.channels
            .get(from)
            .is_some_and(|targets| targets.contains(to))
    }

    /// Whether to inject the transport/cognition principle into agent prompts.
    /// Default: true. Override with `inject_transport = false` in fleet.toml.
    #[must_use]
    pub fn inject_transport(&self) -> bool {
        // Reads from constitution — if constitution exists and has a "no_transport_preamble"
        // pillar, disable. Otherwise default true.
        !self.constitution.as_ref().is_some_and(|c| {
            c.pillars
                .iter()
                .any(|p| p.contains("no_transport_preamble"))
        })
    }

    /// Whether to inject the energy ethics principle into agent prompts.
    /// Default: true. Override with `inject_energy = false` in fleet.toml.
    #[must_use]
    pub fn inject_energy(&self) -> bool {
        !self
            .constitution
            .as_ref()
            .is_some_and(|c| c.pillars.iter().any(|p| p.contains("no_energy_preamble")))
    }

    /// List all agents that `from` can communicate with.
    #[must_use]
    pub fn targets(&self, from: &str) -> Vec<&str> {
        self.channels
            .get(from)
            .map(|set| set.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_fleet_id(id: &str) -> Result<(), FleetSpecError> {
    if id.is_empty() {
        return Err(FleetSpecError::InvalidFleetId("empty fleet id".to_string()));
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(FleetSpecError::MalformedFleetId(id.to_string()));
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(FleetSpecError::MalformedFleetId(id.to_string()));
        }
    }
    Ok(())
}

fn parse_include(ri: &RawInclude) -> Result<FleetInclude, FleetSpecError> {
    let Some(idx) = ri.source.find(':') else {
        return Err(FleetSpecError::IncludeSourceInvalid(ri.source.clone()));
    };
    let scheme = ri.source[..idx].to_string();
    let path = ri.source[idx + 1..].to_string();
    if scheme.is_empty() {
        return Err(FleetSpecError::IncludeSourceInvalid(ri.source.clone()));
    }
    // v0: accept `file:` unconditionally; other schemes parse-but-error on use.
    // We still build the FleetInclude so resolvers can report the scheme
    // name — validation happens when the resolver actually tries to load.
    if let Some(ref p) = ri.as_prefix {
        validate_fleet_id(p).map_err(|_| FleetSpecError::MalformedFleetId(p.clone()))?;
    }
    Ok(FleetInclude {
        source: ri.source.clone(),
        scheme,
        path,
        as_prefix: ri.as_prefix.clone(),
    })
}

/// Best-effort line number of the `name = "<agent>"` line in a fleet TOML text.
///
/// Used by resolvers to enrich duplicate-agent error messages. Returns a
/// human-friendly suffix like `" line 42"` or an empty string on miss.
/// Exposed so the CLI resolver (which does the I/O) can pass strings back
/// into [`FleetSpec::compose`] via [`FleetSpecError::DuplicateAgentAcrossFleets`].
#[must_use]
pub fn find_agent_line_tag(toml_text: &str, agent_name: &str) -> String {
    for (idx, line) in toml_text.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("name") {
            continue;
        }
        // Match `name = "agent_name"` or `name="agent_name"` with various whitespace.
        let after_eq = trimmed.split_once('=').map(|(_, v)| v.trim());
        if let Some(v) = after_eq {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if v == agent_name {
                return format!(" line {}", idx + 1);
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_FLEET: &str = r#"
fleet = "test"
version = 1

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"

[[agents]]
name = "beta"
role = "advisory"
clearance = "read"

[[channels]]
from = "alpha"
to = "beta"
"#;

    #[test]
    fn test_parse_minimal_fleet() {
        let spec = FleetSpec::parse(MINIMAL_FLEET).unwrap();
        assert_eq!(spec.name, "test");
        assert_eq!(spec.version, 1);
        assert_eq!(spec.schema_version, 1);
        assert!(spec.includes.is_empty());
        assert_eq!(spec.agents.len(), 2);
        assert!(spec.can_send("alpha", "beta"));
        assert!(!spec.can_send("beta", "alpha"));
        assert!(!spec.review.cross_provider);
    }

    #[test]
    fn cross_provider_review_is_an_explicit_fleet_opt_in() {
        let spec = FleetSpec::parse(
            r#"
fleet = "reviewed"

[review]
cross_provider = true
reviewer_adapter = "openai"

[[agents]]
name = "worker"
role = "implementation"
clearance = "write"
"#,
        )
        .unwrap();
        assert!(spec.review.cross_provider);
        assert_eq!(spec.review.reviewer_adapter.as_deref(), Some("openai"));
    }

    #[test]
    fn test_parse_new_form_block() {
        let toml = r#"
[fleet]
schema_version = 1
id = "cosmon"

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert_eq!(spec.name, "cosmon");
        assert_eq!(spec.schema_version, 1);
        assert!(spec.includes.is_empty());
        assert_eq!(spec.agents.len(), 1);
    }

    #[test]
    fn test_parse_new_form_with_includes() {
        let toml = r#"
[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"
as = "wiki"

[[fleet.include]]
source = "file:./fleets/dev.toml"
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert_eq!(spec.name, "master");
        assert_eq!(spec.includes.len(), 2);
        assert_eq!(spec.includes[0].scheme, "file");
        assert_eq!(spec.includes[0].path, "./fleets/wiki.toml");
        assert_eq!(spec.includes[0].as_prefix.as_deref(), Some("wiki"));
        assert_eq!(spec.includes[1].as_prefix, None);
    }

    #[test]
    fn test_parse_with_prompts_and_workdir() {
        let toml = r#"
fleet = "prompted"
version = 1
workdir = "~/work"

[[agents]]
name = "worker"
role = "implementation"
clearance = "write"
prompt = "You are a worker."
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert_eq!(spec.workdir.as_deref(), Some("~/work"));
        assert_eq!(spec.agents[0].prompt.as_deref(), Some("You are a worker."));
    }

    #[test]
    fn test_parse_no_agents_fails() {
        let toml = r#"fleet = "empty""#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::NoAgents));
    }

    #[test]
    fn test_new_form_with_only_includes_is_ok() {
        // A master fleet that has no direct agents but composes children.
        let toml = r#"
[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/child.toml"
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert_eq!(spec.agents.len(), 0);
        assert_eq!(spec.includes.len(), 1);
    }

    #[test]
    fn test_parse_duplicate_agent_fails() {
        let toml = r#"
fleet = "dup"

[[agents]]
name = "same"
role = "implementation"
clearance = "write"

[[agents]]
name = "same"
role = "advisory"
clearance = "read"
"#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::DuplicateAgent(_)));
    }

    #[test]
    fn test_parse_unknown_channel_agent_fails() {
        let toml = r#"
fleet = "bad-channel"

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"

[[channels]]
from = "alpha"
to = "ghost"
"#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::UnknownAgent(_)));
    }

    #[test]
    fn test_parse_self_loop_fails() {
        let toml = r#"
fleet = "loop"

[[agents]]
name = "narcissist"
role = "implementation"
clearance = "write"

[[channels]]
from = "narcissist"
to = "narcissist"
"#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::SelfLoop(_)));
    }

    #[test]
    fn test_targets_lists_reachable_agents() {
        let spec = FleetSpec::parse(MINIMAL_FLEET).unwrap();
        let targets = spec.targets("alpha");
        assert_eq!(targets, vec!["beta"]);
        assert!(spec.targets("beta").is_empty());
    }

    #[test]
    fn test_doc_example_roundtrip() {
        let toml = r#"
fleet = "example"
version = 1

[[agents]]
name = "writer"
role = "implementation"
clearance = "write"

[[agents]]
name = "reviewer"
role = "advisory"
clearance = "read"

[[channels]]
from = "writer"
to = "reviewer"
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert_eq!(spec.name, "example");
        assert_eq!(spec.agents.len(), 2);
        assert!(spec.can_send("writer", "reviewer"));
        assert!(!spec.can_send("reviewer", "writer"));
    }

    #[test]
    fn test_malformed_fleet_id_rejected() {
        let toml = r#"
[fleet]
schema_version = 1
id = "Bad_ID"

[[agents]]
name = "x"
role = "implementation"
clearance = "write"
"#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::MalformedFleetId(_)));
    }

    #[test]
    fn test_unsupported_schema_version_rejected() {
        let toml = r#"
[fleet]
schema_version = 2
id = "x"

[[agents]]
name = "x"
role = "implementation"
clearance = "write"
"#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::UnsupportedSchemaVersion(2)));
    }

    #[test]
    fn test_include_source_without_scheme_rejected() {
        let toml = r#"
[fleet]
schema_version = 1
id = "m"

[[fleet.include]]
source = "./fleets/wiki.toml"

[[agents]]
name = "x"
role = "implementation"
clearance = "write"
"#;
        let err = FleetSpec::parse(toml).unwrap_err();
        assert!(matches!(err, FleetSpecError::IncludeSourceInvalid(_)));
    }

    #[test]
    fn test_compose_two_fleets_no_collision() {
        let parent = FleetSpec::parse(
            r#"
[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"

[[agents]]
name = "blob"
role = "advisory"
clearance = "read"
"#,
        )
        .unwrap();
        let wiki = FleetSpec::parse(
            r#"
[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
        )
        .unwrap();
        let include = parent.includes[0].clone();
        let composed = FleetSpec::compose(
            parent,
            vec![(include, wiki, "file:./fleets/wiki.toml".into())],
        )
        .unwrap();
        assert_eq!(composed.agents.len(), 2);
        let names: Vec<&str> = composed.agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"blob"));
        assert!(names.contains(&"editor"));
        // Provenance preserved
        for a in &composed.agents {
            assert!(a.origin_fleet_id.is_some());
        }
    }

    #[test]
    fn test_compose_with_as_prefix() {
        let parent = FleetSpec::parse(
            r#"
[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"
as = "wiki"

[[agents]]
name = "blob"
role = "advisory"
clearance = "read"
"#,
        )
        .unwrap();
        let wiki = FleetSpec::parse(
            r#"
[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
        )
        .unwrap();
        let include = parent.includes[0].clone();
        let composed = FleetSpec::compose(
            parent,
            vec![(include, wiki, "file:./fleets/wiki.toml".into())],
        )
        .unwrap();
        let names: Vec<&str> = composed.agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"wiki:editor"));
    }

    #[test]
    fn test_compose_hard_fails_on_duplicate() {
        let parent = FleetSpec::parse(
            r#"
[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"

[[agents]]
name = "editor"
role = "advisory"
clearance = "read"
"#,
        )
        .unwrap();
        let wiki = FleetSpec::parse(
            r#"
[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
        )
        .unwrap();
        let include = parent.includes[0].clone();
        let err = FleetSpec::compose(
            parent,
            vec![(include, wiki, "file:./fleets/wiki.toml".into())],
        )
        .unwrap_err();
        match err {
            FleetSpecError::DuplicateAgentAcrossFleets(details) => {
                assert_eq!(details.agent, "editor");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn test_find_agent_line_tag() {
        let text = r#"
[fleet]
id = "x"

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"

[[agents]]
name = "beta"
role = "advisory"
clearance = "read"
"#;
        assert_eq!(find_agent_line_tag(text, "beta"), " line 11");
        assert_eq!(find_agent_line_tag(text, "nonexistent"), "");
    }

    #[test]
    fn test_default_singleton_has_one_agent() {
        let spec = FleetSpec::default_singleton();
        assert_eq!(spec.agents.len(), 1);
        assert_eq!(spec.agents[0].name.as_str(), "default");
        assert_eq!(spec.agents[0].role, AgentRole::Implementation);
        assert_eq!(spec.agents[0].clearance, Clearance::Write);
        assert!(spec.agents[0].prompt.is_none());
        assert!(spec.agents[0].model.is_none());
        assert!(spec.agents[0].origin_fleet_id.is_none());
    }

    #[test]
    fn test_default_singleton_has_no_channels() {
        let spec = FleetSpec::default_singleton();
        assert!(spec.channels.is_empty());
        assert!(spec.constitution.is_none());
        assert!(spec.grades.is_empty());
        assert!(spec.includes.is_empty());
    }

    #[test]
    fn test_default_singleton_metadata() {
        let spec = FleetSpec::default_singleton();
        assert_eq!(spec.name, "default");
        assert_eq!(spec.schema_version, 1);
        assert_eq!(spec.version, 1);
        assert!(spec.description.is_empty());
        assert!(spec.workdir.is_none());
        assert!(spec.organization_type.is_none());
    }

    // -----------------------------------------------------------------
    // organization_type — advisory IFBDD field (delib-20260509-18df §D-C).
    //
    // The structural promise of this field is *negative*: setting it must
    // produce a `FleetSpec` that is observably identical to one without
    // it across every operational dimension (agents, channels, grades,
    // constitution preamble, can-send checks, targets, transport
    // injection). The two tests below pin that promise — they are the
    // safety net that detects the day someone adds a `match
    // organization_type` somewhere and silently breaks the deferral.
    // -----------------------------------------------------------------

    #[test]
    fn test_organization_type_parses_when_present() {
        let toml = r#"
fleet = "typed"
version = 1
organization_type = "editorial-board"

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert_eq!(spec.organization_type.as_deref(), Some("editorial-board"));
    }

    #[test]
    fn test_organization_type_optional_default_none() {
        let toml = r#"
fleet = "untyped"
version = 1

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"
"#;
        let spec = FleetSpec::parse(toml).unwrap();
        assert!(spec.organization_type.is_none());
    }

    #[test]
    fn test_organization_type_does_not_change_observable_behaviour() {
        // Two fleets identical in every operational way except for
        // `organization_type` on one of them. They MUST produce
        // identical agents, channels, can_send, targets, constitution
        // preamble, grades summary, and transport/energy flags.
        let untyped_toml = r#"
fleet = "twins"
version = 1
description = "negative-test fleet"

[constitution]
pillars = ["Be excellent."]

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"

[[agents]]
name = "beta"
role = "advisory"
clearance = "read"

[[channels]]
from = "alpha"
to = "beta"
"#;
        let typed_toml = r#"
fleet = "twins"
version = 1
description = "negative-test fleet"
organization_type = "editorial-board"

[constitution]
pillars = ["Be excellent."]

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"

[[agents]]
name = "beta"
role = "advisory"
clearance = "read"

[[channels]]
from = "alpha"
to = "beta"
"#;
        let untyped = FleetSpec::parse(untyped_toml).unwrap();
        let typed = FleetSpec::parse(typed_toml).unwrap();

        // The advisory field IS the only difference.
        assert!(untyped.organization_type.is_none());
        assert_eq!(typed.organization_type.as_deref(), Some("editorial-board"));

        // Every other observable property is identical.
        assert_eq!(untyped.name, typed.name);
        assert_eq!(untyped.version, typed.version);
        assert_eq!(untyped.schema_version, typed.schema_version);
        assert_eq!(untyped.description, typed.description);
        assert_eq!(untyped.workdir, typed.workdir);
        assert_eq!(untyped.agents.len(), typed.agents.len());
        for (a, b) in untyped.agents.iter().zip(typed.agents.iter()) {
            assert_eq!(a.name.as_str(), b.name.as_str());
            assert_eq!(a.role, b.role);
            assert_eq!(a.clearance, b.clearance);
            assert_eq!(a.prompt, b.prompt);
            assert_eq!(a.model, b.model);
            assert_eq!(a.origin_fleet_id, b.origin_fleet_id);
        }
        assert_eq!(untyped.channels, typed.channels);
        assert_eq!(
            untyped.can_send("alpha", "beta"),
            typed.can_send("alpha", "beta")
        );
        assert_eq!(
            untyped.can_send("beta", "alpha"),
            typed.can_send("beta", "alpha")
        );
        assert_eq!(untyped.targets("alpha"), typed.targets("alpha"));
        assert_eq!(
            untyped.constitution_preamble(),
            typed.constitution_preamble()
        );
        assert_eq!(untyped.grades_summary(), typed.grades_summary());
        assert_eq!(untyped.inject_transport(), typed.inject_transport());
        assert_eq!(untyped.inject_energy(), typed.inject_energy());
    }

    #[test]
    fn test_organization_type_compose_parent_wins() {
        // When composing a master fleet with included children, the
        // master's `organization_type` is the value that surfaces on
        // the composed fleet — children cannot impose a typology onto
        // their parent. This mirrors the existing constitution/grades
        // policy.
        // `organization_type` is a top-level field (sibling of `version`,
        // `description`, `workdir`); it must appear *before* the `[fleet]`
        // table to land at the document root rather than inside `[fleet]`.
        let parent = FleetSpec::parse(
            r#"
organization_type = "research-lab"

[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"

[[agents]]
name = "blob"
role = "advisory"
clearance = "read"
"#,
        )
        .unwrap();
        let child = FleetSpec::parse(
            r#"
organization_type = "editorial-board"

[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
        )
        .unwrap();
        let include = parent.includes[0].clone();
        let composed = FleetSpec::compose(
            parent,
            vec![(include, child, "file:./fleets/wiki.toml".into())],
        )
        .unwrap();
        assert_eq!(composed.organization_type.as_deref(), Some("research-lab"));
    }
}
