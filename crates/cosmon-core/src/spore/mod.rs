// SPDX-License-Identifier: AGPL-3.0-only

//! The `spore.toml` schema parser (ADR-140 N2).
//!
//! A **spore** is a shareable, parameterizable template of a whole polymer:
//!
//! ```text
//! Spore = Fleet (crew) + [Formula] (per-node recipes) + ParamSchema
//!       + DAG-of-typed-edges + an optional .tla seal
//! ```
//!
//! This module parses the ADR-140 `spore.toml` schema into the [`Spore`]
//! domain type. The companion [`expand`](mod@expand) submodule turns a parsed spore plus
//! a parameter binding into the ordered list of `cs nucleate` calls (N3, ADR-140
//! D3); the parser itself is the pure, I/O-free front end that turns manifest
//! bytes into a validated value, or refuses with a precise [`SporeError`].
//!
//! # What the parser rejects
//!
//! Parsing is fail-closed. The four load-bearing rejections from ADR-140:
//!
//! - an **emergent** node without a `[spore.node.bounds]` block
//!   ([`SporeError::EmergentWithoutBounds`]) — an emergent zone whose
//!   topology is decided at run time is unprovable without a declared
//!   ceiling, which is exactly the unbounded foaming ADR-139 forbids;
//! - an **edge cycle** ([`SporeError::EdgeCycle`]) — the DAG must be acyclic
//!   so it replays as an ordered `--blocked-by` chain;
//! - an **unknown node kind** ([`SporeError::UnknownNodeKind`]) — `kind` is
//!   explicit, not inferred (ADR-140 D1), so a typo is a hard error;
//! - a **param type mismatch** ([`SporeError::ParamTypeMismatch`]) — a
//!   param's declared `default` must satisfy its declared `type`;
//! - a **node id that is not a safe path slug**
//!   ([`SporeError::InvalidNodeId`]) — the id becomes the germination alias,
//!   and the alias becomes a directory name under the ADR-161 run home, so
//!   `../../tracked-output` or `/tmp/x` would point a worker outside its own
//!   run home. The grammar is the first containment boundary; see
//!   [`validate_node_id`].
//!
//! A handful of structural checks ride along (duplicate node ids, dangling
//! edge endpoints, unknown formula aliases, unknown edge types); they
//! strengthen the contract without contradicting it.
//!
//! # Example
//!
//! ```
//! use cosmon_core::spore::{Spore, NodeKind};
//!
//! let toml = r#"
//! [spore]
//! name = "demo"
//! version = 1
//!
//! [spore.formulas.work]
//! path = "formulas/work.formula.toml"
//!
//! [[spore.node]]
//! id = "frame"
//! kind = "fixed"
//! formula = "work"
//!
//! [[spore.node]]
//! id = "synth"
//! kind = "fixed"
//! formula = "work"
//!
//! [[spore.edge]]
//! from = "frame"
//! to = "synth"
//! type = "feeds"
//! "#;
//!
//! let spore = Spore::parse(toml).unwrap();
//! assert_eq!(spore.name, "demo");
//! assert_eq!(spore.nodes.len(), 2);
//! assert_eq!(spore.nodes[0].kind, NodeKind::Fixed);
//! ```

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use crate::fleet::CrossProviderReview;

pub mod expand;
pub mod output;
pub mod seal;

pub use expand::{expand, ExpandError, NucleateCall};
pub use output::{
    forbidden_gate_output, inject_run_outputs, node_output_dir, run_dir, EscapedOutputHome,
    ForbiddenOutput, OUTPUT_DIR_VAR, RUN_DIR_VAR, SPORE_RUNS_DIR,
};
pub use seal::{
    gate, proof_hash, verify_seal, FakeTlcRunner, InMemorySealVerdictCache, ResolvedSeal, SealGate,
    SealStatus, SealVerdictCache, TlcOutcome, TlcRunner,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors raised while parsing or validating a `spore.toml` manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set will grow; external callers must keep a `_ =>` arm
pub enum SporeError {
    /// The TOML source text is syntactically invalid.
    #[error("TOML parse error: {0}")]
    Toml(String),

    /// A param declares a `type` the schema does not recognize.
    #[error("param \"{param}\" has unknown type \"{ty}\" (expected string, int, bool, enum, or list<string>)")]
    UnknownParamType {
        /// The offending param name.
        param: String,
        /// The unrecognized type string.
        ty: String,
    },

    /// A param's `default` value does not match its declared `type`, or an
    /// `enum` param's default is not a member of its `values` list.
    #[error("param \"{param}\": {detail}")]
    ParamTypeMismatch {
        /// The offending param name.
        param: String,
        /// Human-readable description of the mismatch.
        detail: String,
    },

    /// An `enum` param declares no `values` list (or an empty one).
    #[error("enum param \"{0}\" must declare a non-empty `values` list")]
    EnumWithoutValues(String),

    /// Two or more nodes share the same `id`.
    #[error("duplicate node id: {0}")]
    DuplicateNodeId(String),

    /// A node declares a `kind` the taxonomy does not recognize.
    #[error("node \"{node}\" has unknown kind \"{kind}\" (expected fixed, fanout, or emergent)")]
    UnknownNodeKind {
        /// The offending node id.
        node: String,
        /// The unrecognized kind string.
        kind: String,
    },

    /// An emergent node is missing its mandatory `[spore.node.bounds]` block.
    #[error("emergent node \"{0}\" must declare a [spore.node.bounds] block (ADR-140 D2)")]
    EmergentWithoutBounds(String),

    /// A node whose count is data-driven (`fanout` / `emergent`) is missing
    /// its `for_each` directive.
    #[error("{kind} node \"{node}\" must declare a `for_each` directive")]
    MissingForEach {
        /// The offending node id.
        node: String,
        /// The node kind that requires `for_each`.
        kind: &'static str,
    },

    /// A node references a formula alias that no `[spore.formulas.*]` table
    /// declares.
    #[error("node \"{node}\" references unknown formula alias \"{formula}\"")]
    UnknownFormula {
        /// The offending node id.
        node: String,
        /// The dangling formula alias.
        formula: String,
    },

    /// An edge declares a `type` outside `{feeds, produces, verifies}`.
    #[error(
        "edge {from} -> {to} has unknown type \"{ty}\" (expected feeds, produces, or verifies)"
    )]
    UnknownEdgeType {
        /// The edge source node id.
        from: String,
        /// The edge target node id.
        to: String,
        /// The unrecognized edge-type string.
        ty: String,
    },

    /// An edge endpoint names a node id that no `[[spore.node]]` declares.
    #[error("edge {from} -> {to} references unknown node \"{node}\"")]
    EdgeReferencesUnknownNode {
        /// The edge source node id.
        from: String,
        /// The edge target node id.
        to: String,
        /// The endpoint that does not resolve to a declared node.
        node: String,
    },

    /// The typed edges form a cycle; the DAG is not acyclic.
    #[error("edge cycle detected involving node \"{0}\"")]
    EdgeCycle(String),

    /// A node `id` is not a safe path slug. The id becomes the node's
    /// germination alias, and the alias becomes a **directory name** under the
    /// run-scoped output home (ADR-161). An id carrying a path separator, a
    /// `..`, a drive prefix, or a leading `/` would let the composed
    /// `output_dir` escape `<state>/spore-runs/<germination-id>/` and point a
    /// worker at the tracked tree. Refused at parse time, before any path is
    /// composed.
    #[error("node id \"{node}\" is not a safe path slug: {reason} (allowed: ASCII letters, digits, `_`, `-`; must start with a letter or digit; max {max} chars)", max = MAX_NODE_ID_LEN)]
    InvalidNodeId {
        /// The offending node id, verbatim.
        node: String,
        /// Which rule it broke.
        reason: &'static str,
    },
}

/// The longest node id the grammar accepts. A node id becomes a directory name
/// under the run home, so it stays well inside every filesystem's component
/// limit (255 bytes) even after the `__<index>` fan-out suffix.
pub const MAX_NODE_ID_LEN: usize = 64;

/// Validate that a node id is a safe path slug.
///
/// The id is not merely a label: [`expand`](mod@expand) turns it into the node's
/// germination **alias**, and [`node_output_dir`] turns the alias into a
/// directory under the run home. So the id grammar *is* the first containment
/// boundary of ADR-161 — an id like `../../tracked-output` or `/tmp/x` would
/// otherwise compose an `output_dir` outside the run home.
///
/// Accepts ASCII letters, digits, `_` and `-`, starting with a letter or digit,
/// at most [`MAX_NODE_ID_LEN`] characters. Everything else — path separators,
/// `..`, `.`, `:`, NUL, non-ASCII, leading `-` — is refused.
///
/// # Errors
/// Returns [`SporeError::InvalidNodeId`] naming the rule that was broken.
pub fn validate_node_id(id: &str) -> Result<(), SporeError> {
    let reject = |reason: &'static str| {
        Err(SporeError::InvalidNodeId {
            node: id.to_string(),
            reason,
        })
    };

    if id.is_empty() {
        return reject("it is empty");
    }
    if id.len() > MAX_NODE_ID_LEN {
        return reject("it is too long");
    }
    if !id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return reject("it does not start with an ASCII letter or digit");
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
    {
        return match bad {
            '/' | '\\' => reject("it contains a path separator"),
            '.' => reject("it contains a `.` (path traversal risk)"),
            _ => reject("it contains a character outside the safe slug alphabet"),
        };
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Param schema
// ---------------------------------------------------------------------------

/// The declared type of a spore parameter.
///
/// Drives default-value validation at parse time and (in N3) param-binding
/// validation at expansion time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParamType {
    /// A free-form UTF-8 string.
    String,
    /// A 64-bit signed integer.
    Int,
    /// A boolean.
    Bool,
    /// A closed enumeration; the member set lives in [`ParamSpec::values`].
    Enum,
    /// A list of strings (TOML `list<string>`).
    ListString,
}

impl ParamType {
    /// Parse a param `type` string into a [`ParamType`].
    ///
    /// Accepts `string`, `int`/`integer`, `bool`/`boolean`, `enum`, and
    /// `list<string>`. Returns `None` for anything else so the caller can
    /// raise a precise [`SporeError::UnknownParamType`].
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "string" => Some(Self::String),
            "int" | "integer" => Some(Self::Int),
            "bool" | "boolean" => Some(Self::Bool),
            "enum" => Some(Self::Enum),
            "list<string>" => Some(Self::ListString),
            _ => None,
        }
    }
}

/// One entry of the spore's `ParamSchema` (`[spore.params.<name>]`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    /// The declared type.
    pub ty: ParamType,
    /// Whether the param must be supplied at expansion time.
    pub required: bool,
    /// The default value, if any, kept as a raw TOML value. Validated against
    /// [`ty`](ParamSpec::ty) at parse time.
    pub default: Option<toml::Value>,
    /// The closed member set for an `enum` param; empty for other types.
    pub values: Vec<String>,
    /// Human-readable description.
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Fleet, formula refs, seal, astra
// ---------------------------------------------------------------------------

/// The fleet the mission plan lays over (`[spore.fleet]`).
///
/// `concurrency_cap` and `isolation` are the bounds the seal's
/// `NoResourceCollision` invariant quantifies over (ADR-140 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetConfig {
    /// The referenced fleet name.
    pub name: String,
    /// Max concurrent emergent children of a given output type in flight.
    pub concurrency_cap: Option<u32>,
    /// Isolation mode (e.g. `"worktree"`) guaranteeing non-aliasing writes.
    pub isolation: Option<String>,
}

/// A per-node recipe alias (`[spore.formulas.<alias>]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaRef {
    /// Path to the `.formula.toml`, relative to the manifest.
    pub path: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// ADR-140 D5: declares the formula a pure function of its inputs
    /// (content-cachable). Defaults to `false` (agentic LLM session).
    pub deterministic: bool,
}

/// The optional TLA+ seal (`[spore.seal]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seal {
    /// The `.tla` module path, relative to the manifest.
    pub module: String,
    /// The `.cfg` config path, relative to the manifest.
    pub config: Option<String>,
    /// The properties the seal claims to establish (ADR-140 D2 names four).
    pub properties: Vec<String>,
}

/// The descriptive-emission stanza (`[spore.astra]`, ADR-140 D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstraConfig {
    /// Whether to emit an ASTRA descriptive layer at share time.
    pub emit: bool,
    /// The ASTRA profile (e.g. `"ro-crate"`).
    pub profile: Option<String>,
    /// Output path for the descriptive artifact.
    pub output: Option<String>,
    /// Whether to attach the D4 seal verdict as the proof artifact.
    pub attach_seal_verdict: bool,
}

// ---------------------------------------------------------------------------
// Nodes and edges
// ---------------------------------------------------------------------------

/// The kind of a DAG node (ADR-140 D1). Explicit, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeKind {
    /// Germinates exactly one molecule. `for_each` absent; count is 1.
    Fixed,
    /// Germinates one molecule per entry of a **parameter** list. The param
    /// list is the bound, so no `[bounds]` block is needed.
    Fanout,
    /// Germinates one molecule per item an **upstream node** produces at run
    /// time. Count unknown at germination, so it MUST declare a `[bounds]`
    /// block or the spore fails to load.
    Emergent,
}

impl NodeKind {
    /// Parse a node `kind` string. Absent (`None`) defaults to [`Fixed`].
    ///
    /// [`Fixed`]: NodeKind::Fixed
    fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim) {
            None | Some("fixed") => Some(Self::Fixed),
            Some("fanout") => Some(Self::Fanout),
            Some("emergent") => Some(Self::Emergent),
            Some(_) => None,
        }
    }

    /// The wire string for this kind (used in error messages).
    fn label(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Fanout => "fanout",
            Self::Emergent => "emergent",
        }
    }
}

/// The declared bounds of an emergent zone (`[spore.node.bounds]`, ADR-140 D2).
///
/// The three fields map one-to-one onto three seal properties: `max_instances`
/// feeds bounded termination, `stop_condition` feeds the fail-closed gate, and
/// `output_type` feeds deterministic accounting / no resource collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounds {
    /// The type of item the upstream node emits.
    pub output_type: String,
    /// Hard ceiling on the run-time fan-out.
    pub max_instances: u64,
    /// The condition under which the downstream gate opens.
    pub stop_condition: String,
}

/// One DAG node (`[[spore.node]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The node id, unique within the spore.
    pub id: String,
    /// The explicit kind (ADR-140 D1).
    pub kind: NodeKind,
    /// The formula alias this node nucleates (references `[spore.formulas.*]`).
    pub formula: String,
    /// The fan-out directive for `fanout` / `emergent` nodes (e.g.
    /// `"${params.axes}"` or `"${nodes.analyse-axis.findings}"`).
    pub for_each: Option<String>,
    /// The refinable "what to produce" recipe path, if any.
    pub mission: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// The `${...}`-templated variable bindings passed to the nucleated molecule.
    pub vars: BTreeMap<String, String>,
    /// The bounds block; present iff the node is emergent.
    pub bounds: Option<Bounds>,
}

/// The type of a typed edge (`[[spore.edge]]`).
///
/// All three become `Blocks`/`BlockedBy` links at expansion time; the
/// distinction is descriptive provenance, not a different control semantic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EdgeType {
    /// Output of `from` feeds the work of `to`.
    Feeds,
    /// `from` produces the items `to` fans out over.
    Produces,
    /// `to` verifies the output of `from`.
    Verifies,
}

impl EdgeType {
    /// Parse an edge `type` string. Returns `None` for anything outside the set.
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "feeds" => Some(Self::Feeds),
            "produces" => Some(Self::Produces),
            "verifies" => Some(Self::Verifies),
            _ => None,
        }
    }
}

/// One typed edge between two node ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The blocker node id.
    pub from: String,
    /// The dependent node id.
    pub to: String,
    /// The edge type.
    pub ty: EdgeType,
}

// ---------------------------------------------------------------------------
// Spore
// ---------------------------------------------------------------------------

/// A parsed, validated spore manifest.
///
/// Construct with [`Spore::parse`]. The value is acyclic by construction
/// (cycles are rejected at parse time) and every emergent node carries its
/// bounds, so downstream `expand()` (N3) can assume a well-formed DAG.
#[derive(Debug, Clone, PartialEq)]
pub struct Spore {
    /// The spore name (`[spore].name`).
    pub name: String,
    /// The schema version (`[spore].version`; defaults to 1).
    pub version: u32,
    /// Human-readable description.
    pub description: Option<String>,
    /// The native verb (`"germinate"`).
    pub verb: Option<String>,
    /// The optional TLA+ seal.
    pub seal: Option<Seal>,
    /// The `ParamSchema`, keyed by param name (sorted for determinism).
    pub params: BTreeMap<String, ParamSpec>,
    /// The fleet the mission plan lays over.
    pub fleet: Option<FleetConfig>,
    /// Opt-in review policy for every molecule germinated from this spore.
    pub review: CrossProviderReview,
    /// The per-node recipe aliases, keyed by alias (sorted for determinism).
    pub formulas: BTreeMap<String, FormulaRef>,
    /// The DAG nodes, in declaration order.
    pub nodes: Vec<Node>,
    /// The typed edges, in declaration order.
    pub edges: Vec<Edge>,
    /// The descriptive-emission stanza, if present.
    pub astra: Option<AstraConfig>,
}

impl Spore {
    /// Parse and validate a `spore.toml` manifest.
    ///
    /// # Errors
    ///
    /// Returns a [`SporeError`] on syntactically invalid TOML or any of the
    /// validation failures documented at the [module level](crate::spore):
    /// unknown param type, param/default mismatch, duplicate node id, unknown
    /// node kind, emergent-without-bounds, dangling formula/edge reference,
    /// unknown edge type, or an edge cycle.
    pub fn parse(toml_text: &str) -> Result<Self, SporeError> {
        let raw: RawFile =
            toml::from_str(toml_text).map_err(|e| SporeError::Toml(e.to_string()))?;
        let raw = raw.spore;

        let params = build_params(raw.params)?;
        let formulas = build_formulas(raw.formulas);
        let nodes = build_nodes(raw.nodes, &formulas)?;
        let edges = build_edges(raw.edges, &nodes)?;
        check_acyclic(&edges)?;

        Ok(Self {
            name: raw.name,
            version: raw.version.unwrap_or(1),
            description: raw.description,
            verb: raw.verb,
            seal: raw.seal.map(|s| Seal {
                module: s.module,
                config: s.config,
                properties: s.properties,
            }),
            params,
            fleet: raw.fleet.map(|f| FleetConfig {
                name: f.name,
                concurrency_cap: f.concurrency_cap,
                isolation: f.isolation,
            }),
            review: raw.review,
            formulas,
            nodes,
            edges,
            astra: raw.astra.map(|a| AstraConfig {
                emit: a.emit,
                profile: a.profile,
                output: a.output,
                attach_seal_verdict: a.attach_seal_verdict,
            }),
        })
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Build and validate the param schema, checking each default against its type.
fn build_params(
    raw: BTreeMap<String, RawParam>,
) -> Result<BTreeMap<String, ParamSpec>, SporeError> {
    let mut out = BTreeMap::new();
    for (name, p) in raw {
        let ty = ParamType::parse(&p.ty).ok_or_else(|| SporeError::UnknownParamType {
            param: name.clone(),
            ty: p.ty.clone(),
        })?;
        let values = p.values.unwrap_or_default();

        if ty == ParamType::Enum && values.is_empty() {
            return Err(SporeError::EnumWithoutValues(name));
        }
        if let Some(default) = &p.default {
            validate_default(&name, ty, default, &values)?;
        }

        out.insert(
            name,
            ParamSpec {
                ty,
                required: p.required,
                default: p.default,
                values,
                description: p.description,
            },
        );
    }
    Ok(out)
}

/// Validate one `default` TOML value against the param's declared type.
fn validate_default(
    param: &str,
    ty: ParamType,
    default: &toml::Value,
    values: &[String],
) -> Result<(), SporeError> {
    let mismatch = |detail: String| SporeError::ParamTypeMismatch {
        param: param.to_string(),
        detail,
    };
    match ty {
        ParamType::String => {
            if !default.is_str() {
                return Err(mismatch(format!(
                    "type is string but default is {}",
                    value_kind(default)
                )));
            }
        }
        ParamType::Int => {
            if !default.is_integer() {
                return Err(mismatch(format!(
                    "type is int but default is {}",
                    value_kind(default)
                )));
            }
        }
        ParamType::Bool => {
            if !default.is_bool() {
                return Err(mismatch(format!(
                    "type is bool but default is {}",
                    value_kind(default)
                )));
            }
        }
        ParamType::ListString => match default.as_array() {
            Some(items) if items.iter().all(toml::Value::is_str) => {}
            Some(_) => {
                return Err(mismatch(
                    "type is list<string> but default contains a non-string element".to_string(),
                ));
            }
            None => {
                return Err(mismatch(format!(
                    "type is list<string> but default is {}",
                    value_kind(default)
                )));
            }
        },
        ParamType::Enum => match default.as_str() {
            Some(s) if values.iter().any(|v| v == s) => {}
            Some(s) => {
                return Err(mismatch(format!(
                    "default \"{s}\" is not a member of the enum values {values:?}"
                )));
            }
            None => {
                return Err(mismatch(format!(
                    "type is enum but default is {}",
                    value_kind(default)
                )));
            }
        },
    }
    Ok(())
}

/// A short human label for a TOML value's kind, for error messages.
fn value_kind(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a datetime",
        toml::Value::Array(_) => "an array",
        toml::Value::Table(_) => "a table",
    }
}

/// Build the formula-alias map. No validation needed beyond the move.
fn build_formulas(raw: BTreeMap<String, RawFormulaRef>) -> BTreeMap<String, FormulaRef> {
    raw.into_iter()
        .map(|(alias, f)| {
            (
                alias,
                FormulaRef {
                    path: f.path,
                    description: f.description,
                    deterministic: f.deterministic,
                },
            )
        })
        .collect()
}

/// Build and validate nodes: safe-slug ids, unique ids, known kinds, emergent
/// bounds, fan-out `for_each`, and resolvable formula aliases.
fn build_nodes(
    raw: Vec<RawNode>,
    formulas: &BTreeMap<String, FormulaRef>,
) -> Result<Vec<Node>, SporeError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(raw.len());

    for n in raw {
        // The id becomes the germination alias, which becomes a directory name
        // under the ADR-161 run home. Refuse a hostile slug here, before any
        // path is composed from it.
        validate_node_id(&n.id)?;

        if !seen.insert(n.id.clone()) {
            return Err(SporeError::DuplicateNodeId(n.id));
        }

        let kind =
            NodeKind::parse(n.kind.as_deref()).ok_or_else(|| SporeError::UnknownNodeKind {
                node: n.id.clone(),
                kind: n.kind.clone().unwrap_or_default(),
            })?;

        let bounds = n.bounds.map(|b| Bounds {
            output_type: b.output_type,
            max_instances: b.max_instances,
            stop_condition: b.stop_condition,
        });

        // Emergent zones MUST declare bounds (ADR-140 D2) and a for_each.
        if kind == NodeKind::Emergent {
            if bounds.is_none() {
                return Err(SporeError::EmergentWithoutBounds(n.id));
            }
            if n.for_each.is_none() {
                return Err(SporeError::MissingForEach {
                    node: n.id,
                    kind: kind.label(),
                });
            }
        }
        // Pre-determined fan-out needs a for_each over a param list.
        if kind == NodeKind::Fanout && n.for_each.is_none() {
            return Err(SporeError::MissingForEach {
                node: n.id,
                kind: kind.label(),
            });
        }

        if !formulas.contains_key(&n.formula) {
            return Err(SporeError::UnknownFormula {
                node: n.id,
                formula: n.formula,
            });
        }

        out.push(Node {
            id: n.id,
            kind,
            formula: n.formula,
            for_each: n.for_each,
            mission: n.mission,
            description: n.description,
            vars: n.vars.into_iter().collect(),
            bounds,
        });
    }
    Ok(out)
}

/// Build and validate edges: known types and resolvable endpoints.
fn build_edges(raw: Vec<RawEdge>, nodes: &[Node]) -> Result<Vec<Edge>, SporeError> {
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut out = Vec::with_capacity(raw.len());

    for e in raw {
        let ty = EdgeType::parse(&e.ty).ok_or_else(|| SporeError::UnknownEdgeType {
            from: e.from.clone(),
            to: e.to.clone(),
            ty: e.ty.clone(),
        })?;
        for endpoint in [&e.from, &e.to] {
            if !ids.contains(endpoint.as_str()) {
                return Err(SporeError::EdgeReferencesUnknownNode {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    node: endpoint.clone(),
                });
            }
        }
        out.push(Edge {
            from: e.from,
            to: e.to,
            ty,
        });
    }
    Ok(out)
}

/// Reject an edge set that contains a cycle, reusing the graph toposort.
fn check_acyclic(edges: &[Edge]) -> Result<(), SporeError> {
    let pairs: Vec<(String, String)> = edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    cosmon_graph::toposort(&pairs)
        .map(|_| ())
        .map_err(|cycle| SporeError::EdgeCycle(cycle.0))
}

// ---------------------------------------------------------------------------
// Raw deserialization structs (private)
// ---------------------------------------------------------------------------

/// Top-level wrapper: everything lives under the `[spore]` namespace.
#[derive(Deserialize)]
struct RawFile {
    spore: RawSpore,
}

#[derive(Deserialize)]
struct RawSpore {
    name: String,
    version: Option<u32>,
    description: Option<String>,
    verb: Option<String>,
    seal: Option<RawSeal>,
    #[serde(default)]
    params: BTreeMap<String, RawParam>,
    fleet: Option<RawFleet>,
    #[serde(default)]
    review: CrossProviderReview,
    #[serde(default)]
    formulas: BTreeMap<String, RawFormulaRef>,
    #[serde(default, rename = "node")]
    nodes: Vec<RawNode>,
    #[serde(default, rename = "edge")]
    edges: Vec<RawEdge>,
    astra: Option<RawAstra>,
}

#[derive(Deserialize)]
struct RawSeal {
    module: String,
    config: Option<String>,
    #[serde(default)]
    properties: Vec<String>,
}

#[derive(Deserialize)]
struct RawParam {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    required: bool,
    default: Option<toml::Value>,
    values: Option<Vec<String>>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawFleet {
    name: String,
    concurrency_cap: Option<u32>,
    isolation: Option<String>,
}

#[derive(Deserialize)]
struct RawFormulaRef {
    path: String,
    description: Option<String>,
    #[serde(default)]
    deterministic: bool,
}

#[derive(Deserialize)]
struct RawNode {
    id: String,
    kind: Option<String>,
    formula: String,
    for_each: Option<String>,
    mission: Option<String>,
    description: Option<String>,
    #[serde(default)]
    vars: BTreeMap<String, String>,
    bounds: Option<RawBounds>,
}

#[derive(Deserialize)]
struct RawBounds {
    output_type: String,
    max_instances: u64,
    stop_condition: String,
}

#[derive(Deserialize)]
struct RawEdge {
    from: String,
    to: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Deserialize)]
struct RawAstra {
    #[serde(default)]
    emit: bool,
    profile: Option<String>,
    output: Option<String>,
    #[serde(default)]
    attach_seal_verdict: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The workshop `grace-business-analysis` prototype manifest, embedded
    /// verbatim (it is 100% public). This is the N2 TDD fixture: the
    /// prototype must parse. It predates the D1 `kind` taxonomy, so every
    /// node defaults to `fixed`.
    const PROTOTYPE: &str = r#"
[spore]
name        = "grace-business-analysis"
version     = 1
description = "Germinate a polymer that reads a business model through the public Grace frame."
verb = "germinate"

[spore.seal]
module = "spore.tla"
config = "spore.cfg"
properties = ["Termination", "GateFailClosed", "DeterministicParametrization"]

[spore.params.subject]
type        = "string"
required    = true
description = "What to analyse."

[spore.params.frame]
type        = "enum"
values      = ["grace"]
default     = "grace"

[spore.params.axes]
type    = "list<string>"
default = ["value-incarnation", "creation-vs-capture", "social-capital"]

[spore.params.depth]
type    = "enum"
values  = ["quick", "full"]
default = "full"

[spore.fleet]
name            = "default"
concurrency_cap = 4
isolation       = "worktree"

[spore.formulas.editorial-work]
path        = "formulas/editorial-work.formula.toml"
description = "Generic prose/analysis recipe."

[spore.formulas.grace-axis-analysis]
path        = "formulas/grace-axis-analysis.formula.toml"
description = "Dedicated minimal recipe for one Grace axis."

[[spore.node]]
id      = "frame"
formula = "editorial-work"
mission = "mission-template.md"
description = "Note de cadrage."
[spore.node.vars]
subject = "${params.subject}"
frame   = "${params.frame}"

[[spore.node]]
id       = "analyse-axis"
for_each = "${params.axes}"
formula  = "grace-axis-analysis"
[spore.node.vars]
subject = "${params.subject}"
axis    = "${axis}"

[[spore.node]]
id       = "verify-finding"
for_each = "${nodes.analyse-axis.findings}"
formula  = "grace-axis-analysis"
[spore.node.vars]
finding = "${finding}"

[[spore.node]]
id      = "synthesize"
formula = "editorial-work"
[spore.node.vars]
subject = "${params.subject}"

[[spore.node]]
id      = "graded-verdict"
formula = "editorial-work"
[spore.node.vars]
subject = "${params.subject}"

[[spore.edge]]
from = "frame"
to   = "analyse-axis"
type = "feeds"

[[spore.edge]]
from = "analyse-axis"
to   = "verify-finding"
type = "produces"

[[spore.edge]]
from = "verify-finding"
to   = "synthesize"
type = "feeds"

[[spore.edge]]
from = "synthesize"
to   = "graded-verdict"
type = "feeds"
"#;

    /// A D1-era manifest exercising every kind, an emergent bounds block, the
    /// `deterministic` trait, and the `[spore.astra]` stanza.
    const D1_SCHEMA: &str = r#"
[spore]
name = "d1-demo"
version = 1

[spore.fleet]
name = "default"
concurrency_cap = 4
isolation = "worktree"

[spore.formulas.editorial-work]
path = "formulas/editorial-work.formula.toml"
deterministic = false

[spore.formulas.regenerate-index]
path = "formulas/regenerate-index.formula.toml"
deterministic = true

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "editorial-work"

[[spore.node]]
id = "analyse-axis"
kind = "fanout"
for_each = "${params.axes}"
formula = "editorial-work"

[[spore.node]]
id = "verify-finding"
kind = "emergent"
for_each = "${nodes.analyse-axis.findings}"
formula = "editorial-work"
[spore.node.bounds]
output_type = "finding"
max_instances = 64
stop_condition = "every finding consumed exactly once"

[[spore.node]]
id = "index"
kind = "fixed"
formula = "regenerate-index"

[[spore.edge]]
from = "frame"
to = "analyse-axis"
type = "feeds"

[[spore.edge]]
from = "analyse-axis"
to = "verify-finding"
type = "produces"

[[spore.edge]]
from = "verify-finding"
to = "index"
type = "feeds"

[spore.astra]
emit = true
profile = "ro-crate"
output = "astra/ro-crate-metadata.json"
attach_seal_verdict = true
"#;

    fn minimal_with_node(node_block: &str) -> String {
        format!(
            r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

{node_block}
"#
        )
    }

    #[test]
    fn test_prototype_spore_parses() {
        let spore = Spore::parse(PROTOTYPE).expect("prototype must parse");
        assert_eq!(spore.name, "grace-business-analysis");
        assert_eq!(spore.version, 1);
        assert_eq!(spore.nodes.len(), 5);
        assert_eq!(spore.edges.len(), 4);
        assert_eq!(spore.params.len(), 4);
        assert_eq!(spore.formulas.len(), 2);
        // No node declares a kind, so every node defaults to fixed.
        assert!(spore.nodes.iter().all(|n| n.kind == NodeKind::Fixed));
        // The seal and fleet are present.
        assert!(spore.seal.is_some());
        let fleet = spore.fleet.expect("fleet present");
        assert_eq!(fleet.concurrency_cap, Some(4));
        assert_eq!(fleet.isolation.as_deref(), Some("worktree"));
        // Vars on the frame node are captured.
        let frame = spore.nodes.iter().find(|n| n.id == "frame").unwrap();
        assert_eq!(
            frame.vars.get("subject").map(String::as_str),
            Some("${params.subject}")
        );
        assert_eq!(frame.mission.as_deref(), Some("mission-template.md"));
    }

    #[test]
    fn test_d1_schema_parses_all_kinds() {
        let spore = Spore::parse(D1_SCHEMA).expect("D1 schema must parse");
        let kinds: Vec<_> = spore
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.kind))
            .collect();
        assert_eq!(kinds[0], ("frame", NodeKind::Fixed));
        assert_eq!(kinds[1], ("analyse-axis", NodeKind::Fanout));
        assert_eq!(kinds[2], ("verify-finding", NodeKind::Emergent));
        assert_eq!(kinds[3], ("index", NodeKind::Fixed));

        // The emergent node carries its bounds.
        let verify = &spore.nodes[2];
        let bounds = verify.bounds.as_ref().expect("emergent carries bounds");
        assert_eq!(bounds.output_type, "finding");
        assert_eq!(bounds.max_instances, 64);

        // The deterministic trait round-trips on the formula refs.
        assert!(!spore.formulas["editorial-work"].deterministic);
        assert!(spore.formulas["regenerate-index"].deterministic);

        // The ASTRA stanza is captured.
        let astra = spore.astra.expect("astra present");
        assert!(astra.emit);
        assert_eq!(astra.profile.as_deref(), Some("ro-crate"));
        assert!(astra.attach_seal_verdict);
    }

    #[test]
    fn test_emergent_without_bounds_fails() {
        let toml = minimal_with_node(
            r#"
[[spore.node]]
id = "verify"
kind = "emergent"
for_each = "${nodes.x.findings}"
formula = "work"
"#,
        );
        assert_eq!(
            Spore::parse(&toml),
            Err(SporeError::EmergentWithoutBounds("verify".to_string()))
        );
    }

    #[test]
    fn test_cyclic_edges_fail() {
        let toml = r#"
[spore]
name = "cyclic"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"

[[spore.node]]
id = "b"
formula = "work"

[[spore.edge]]
from = "a"
to = "b"
type = "feeds"

[[spore.edge]]
from = "b"
to = "a"
type = "feeds"
"#;
        match Spore::parse(toml) {
            Err(SporeError::EdgeCycle(_)) => {}
            other => panic!("expected EdgeCycle, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_node_kind_fails() {
        let toml = minimal_with_node(
            r#"
[[spore.node]]
id = "weird"
kind = "quantum"
formula = "work"
"#,
        );
        assert_eq!(
            Spore::parse(&toml),
            Err(SporeError::UnknownNodeKind {
                node: "weird".to_string(),
                kind: "quantum".to_string(),
            })
        );
    }

    #[test]
    fn test_param_default_type_mismatch_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.params.count]
type = "string"
default = 7

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"
"#;
        match Spore::parse(toml) {
            Err(SporeError::ParamTypeMismatch { param, .. }) => assert_eq!(param, "count"),
            other => panic!("expected ParamTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_enum_default_not_in_values_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.params.depth]
type = "enum"
values = ["quick", "full"]
default = "deep"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"
"#;
        match Spore::parse(toml) {
            Err(SporeError::ParamTypeMismatch { param, detail }) => {
                assert_eq!(param, "depth");
                assert!(detail.contains("deep"), "detail mentions the bad default");
            }
            other => panic!("expected ParamTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_enum_without_values_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.params.mode]
type = "enum"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"
"#;
        assert_eq!(
            Spore::parse(toml),
            Err(SporeError::EnumWithoutValues("mode".to_string()))
        );
    }

    #[test]
    fn test_unknown_param_type_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.params.x]
type = "float64"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"
"#;
        assert_eq!(
            Spore::parse(toml),
            Err(SporeError::UnknownParamType {
                param: "x".to_string(),
                ty: "float64".to_string(),
            })
        );
    }

    #[test]
    fn test_unknown_edge_type_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"

[[spore.node]]
id = "b"
formula = "work"

[[spore.edge]]
from = "a"
to = "b"
type = "entangles"
"#;
        assert_eq!(
            Spore::parse(toml),
            Err(SporeError::UnknownEdgeType {
                from: "a".to_string(),
                to: "b".to_string(),
                ty: "entangles".to_string(),
            })
        );
    }

    #[test]
    fn test_edge_references_unknown_node_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"

[[spore.edge]]
from = "a"
to = "ghost"
type = "feeds"
"#;
        assert_eq!(
            Spore::parse(toml),
            Err(SporeError::EdgeReferencesUnknownNode {
                from: "a".to_string(),
                to: "ghost".to_string(),
                node: "ghost".to_string(),
            })
        );
    }

    #[test]
    fn test_duplicate_node_id_fails() {
        let toml = r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
formula = "work"

[[spore.node]]
id = "a"
formula = "work"
"#;
        assert_eq!(
            Spore::parse(toml),
            Err(SporeError::DuplicateNodeId("a".to_string()))
        );
    }

    /// Review finding F6, frozen as a red-first regression at the parse seam.
    ///
    /// A node id is not just a label: it becomes the germination alias, and the
    /// alias becomes a directory name under the run home (ADR-161). An id like
    /// `../../tracked-output` or `/tmp/cosmon-output` parsed cleanly before this
    /// fix — uniqueness, kind, bounds and formula ref all passed — and the
    /// composed `output_dir` then pointed a worker outside the run home. The
    /// grammar refuses it here, before any path exists.
    #[test]
    fn hostile_node_ids_are_refused_at_parse_time() {
        let hostile = [
            "../../tracked-output",
            "..",
            "/tmp/cosmon-output",
            "a/b",
            "a\\b",
            "./x",
            ".",
            ".hidden",
            "-leading-dash",
            "",
            "a:b",
        ];
        for id in hostile {
            let toml = format!(
                r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = '{id}'
formula = "work"
"#
            );
            let parsed = Spore::parse(&toml);
            assert!(
                matches!(&parsed, Err(SporeError::InvalidNodeId { node, .. }) if node == id),
                "node id {id:?} must be refused as an unsafe path slug, got {parsed:?}"
            );
        }
    }

    /// The grammar must not break the ids real spores actually use.
    #[test]
    fn safe_node_id_slugs_are_accepted() {
        for id in ["intake", "ci-gate", "green", "a_b-1", "n0", "A", "9"] {
            assert!(
                validate_node_id(id).is_ok(),
                "benign node id {id:?} must be accepted"
            );
        }
        // The bound is real, not decorative.
        assert!(validate_node_id(&"a".repeat(MAX_NODE_ID_LEN)).is_ok());
        assert!(validate_node_id(&"a".repeat(MAX_NODE_ID_LEN + 1)).is_err());
    }

    #[test]
    fn test_unknown_formula_ref_fails() {
        let toml = minimal_with_node(
            r#"
[[spore.node]]
id = "a"
formula = "nonexistent"
"#,
        );
        assert_eq!(
            Spore::parse(&toml),
            Err(SporeError::UnknownFormula {
                node: "a".to_string(),
                formula: "nonexistent".to_string(),
            })
        );
    }

    #[test]
    fn test_fanout_without_for_each_fails() {
        let toml = minimal_with_node(
            r#"
[[spore.node]]
id = "a"
kind = "fanout"
formula = "work"
"#,
        );
        assert_eq!(
            Spore::parse(&toml),
            Err(SporeError::MissingForEach {
                node: "a".to_string(),
                kind: "fanout",
            })
        );
    }

    #[test]
    fn test_parse_is_deterministic() {
        // Same bytes in => structurally equal value out (no clock, no random).
        let a = Spore::parse(PROTOTYPE).unwrap();
        let b = Spore::parse(PROTOTYPE).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn cross_provider_review_is_an_explicit_spore_opt_in() {
        let spore = Spore::parse(
            r#"
[spore]
name = "reviewed"

[spore.review]
cross_provider = true
reviewer_adapter = "anthropic"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "work"
formula = "work"
"#,
        )
        .unwrap();
        assert!(spore.review.cross_provider);
        assert_eq!(spore.review.reviewer_adapter.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_invalid_toml_fails() {
        match Spore::parse("this is not = valid = toml") {
            Err(SporeError::Toml(_)) => {}
            other => panic!("expected Toml error, got {other:?}"),
        }
    }
}
