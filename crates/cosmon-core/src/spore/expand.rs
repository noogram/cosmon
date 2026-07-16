// SPDX-License-Identifier: AGPL-3.0-only

//! `expand(spore, params)` — the pure ADR-140 D3 expansion (N3).
//!
//! [`expand`] is the moral twin of `cs fleet resolve`: a declarative front end
//! over the existing `cs nucleate` verb, not a scheduler and not a new molecule
//! type. It takes a parsed [`Spore`] and a parameter binding and returns an
//! **ordered list** of [`NucleateCall`]s — *"a Makefile that replays as
//! `cs nucleate`"*. The shell (`cs spore run`, N5) executes the returned list
//! against the live state store; this module touches no I/O, no clock, and no
//! randomness, so it lives in `cosmon-core` with the rest of the zero-I/O
//! domain (INV-DOMAIN-PURE-NO-IO, ADR-082).
//!
//! # The algorithm (ADR-140 D3)
//!
//! 1. **Validate** the param binding against the [`ParamSchema`](super::ParamSpec)
//!    — types, required-ness, enum membership. A missing required param or a
//!    type mismatch is a hard error; expansion does not partially proceed.
//! 2. **Resolve fixed nodes** to one [`NucleateCall`] each, substituting
//!    `${params.*}` in `[spore.node.vars]`.
//! 3. **Resolve pre-determined fan-out nodes**: for each entry of the referenced
//!    param list, emit one call, binding the loop variables `${item}` and
//!    `${index}`.
//! 4. **Emit emergent-zone controllers**: an emergent zone cannot be expanded at
//!    germination (its `for_each` ranges over a *runtime* value), so it expands
//!    to a single controller call carrying its `[bounds]` block and runtime
//!    `for_each`. The controller is the static handle the seal quantifies over.
//! 5. **Topologically order** by the typed edges and set each call's
//!    `blocked_by` to its predecessors' aliases. A cycle is a hard error.
//!
//! # Ordering guarantee
//!
//! The returned list is ordered so a caller can replay it top to bottom and
//! every `blocked_by` alias is **already defined** by an earlier call. This is
//! what makes a spore a Makefile: reading the list top to bottom IS the build
//! plan. Tie-breaks follow node declaration order, so the same spore plus the
//! same params yields a byte-identical list every time.
//!
//! # Loop-variable convention
//!
//! A fan-out node binds two loop variables, available only inside that node's
//! `vars`: `${item}` (the current list entry) and `${index}` (its 0-based
//! position). Any `${...}` token that is neither a declared `params.*` reference
//! nor a loop variable is left **verbatim** — it is presumed a runtime reference
//! (e.g. `${nodes.analyse-axis.findings}`) that a worker resolves later. A
//! `${params.X}` token naming an *undeclared* param is, by contrast, a hard
//! error: that is a typo in the spore, not a runtime handle.
//!
//! # Param binding type
//!
//! The binding is a `BTreeMap<String, toml::Value>`: the shell is responsible
//! for coercing `--var k=v` strings into the declared TOML types before calling
//! `expand`. Keeping the core typed (rather than re-parsing strings here) keeps
//! the validation honest and the function pure.

use std::collections::BTreeMap;

use super::{Bounds, NodeKind, ParamSpec, ParamType, Spore};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors raised while expanding a spore into nucleate calls.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set may grow; external callers must keep a `_ =>` arm
pub enum ExpandError {
    /// A required param was neither supplied in the binding nor defaulted.
    #[error("missing required param: {0}")]
    MissingRequiredParam(String),

    /// The binding supplies a param the spore's `ParamSchema` does not declare.
    #[error("unknown param in binding: {0}")]
    UnknownParam(String),

    /// A supplied (or defaulted) param value does not match its declared type,
    /// or an `enum` param's value is not a member of its `values` list.
    #[error("param \"{param}\": {detail}")]
    ParamTypeMismatch {
        /// The offending param name.
        param: String,
        /// Human-readable description of the mismatch.
        detail: String,
    },

    /// A `${params.X}` token in a node's vars (or `for_each`) names a param that
    /// the schema does not declare.
    #[error("node \"{node}\": reference \"${{{reference}}}\" names an undeclared param")]
    UnknownParamReference {
        /// The node whose template carried the bad reference.
        node: String,
        /// The dangling reference text (without the `${` / `}` delimiters).
        reference: String,
    },

    /// A fan-out node's `for_each` does not have the shape `${params.<list>}`
    /// referencing a `list<string>` param.
    #[error(
        "fanout node \"{node}\": for_each \"{for_each}\" must reference a list<string> param as ${{params.<name>}}"
    )]
    FanoutForEachNotParamList {
        /// The offending node id.
        node: String,
        /// The malformed `for_each` directive.
        for_each: String,
    },

    /// A node references a formula alias that no `[spore.formulas.*]` declares.
    /// (The parser normally catches this; `expand` re-checks defensively so it
    /// is sound on a hand-built [`Spore`].)
    #[error("node \"{node}\" references unknown formula alias \"{formula}\"")]
    UnknownFormula {
        /// The offending node id.
        node: String,
        /// The dangling formula alias.
        formula: String,
    },

    /// The typed edges form a cycle; the DAG cannot be linearised.
    #[error("edge cycle detected involving node \"{0}\"")]
    EdgeCycle(String),
}

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// One resolved `cs nucleate ... --blocked-by ...` invocation.
///
/// The list [`expand`] returns is ordered so that, for every call, each
/// [`blocked_by`](NucleateCall::blocked_by) alias is the [`alias`](NucleateCall::alias)
/// of a call appearing **earlier** in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NucleateCall {
    /// The unique, stable handle for this call. `--blocked-by` references point
    /// at it. For a fixed or emergent node it is the node id; for a fan-out
    /// instance it is `"<node-id>__<index>"`.
    pub alias: String,
    /// The resolved formula path (the `path` of the referenced
    /// `[spore.formulas.*]` alias) the shell passes to `cs nucleate`.
    pub formula: String,
    /// The resolved variable bindings, with `${params.*}` and the loop variables
    /// substituted. Sorted (it is a `BTreeMap`) for byte-stable output.
    pub vars: BTreeMap<String, String>,
    /// The aliases this call must wait on, in deterministic order. Every entry
    /// is the alias of a call earlier in the returned list.
    pub blocked_by: Vec<String>,
    /// The node kind this call came from, so the shell can distinguish an
    /// emergent controller from an ordinary leaf.
    pub kind: NodeKind,
    /// For an emergent controller only: the runtime `for_each` expression the
    /// controller fans out over once its upstream completes. `None` otherwise.
    pub for_each: Option<String>,
    /// For an emergent controller only: the declared bounds the run-time fan-out
    /// stays within (ADR-140 D2). `None` otherwise.
    pub bounds: Option<Bounds>,
}

// ---------------------------------------------------------------------------
// expand
// ---------------------------------------------------------------------------

/// Expand a spore and a parameter binding into the ordered nucleate-call list.
///
/// See the [module docs](self) for the algorithm and the ordering guarantee.
///
/// # Errors
///
/// Returns an [`ExpandError`] on a missing required param, an unknown param,
/// a param/value type mismatch, a dangling `${params.X}` reference, a fan-out
/// whose `for_each` is not a list param, an unknown formula alias, or an edge
/// cycle.
///
/// # Example
///
/// ```
/// use std::collections::BTreeMap;
/// use cosmon_core::spore::{Spore, expand};
///
/// let toml = r#"
/// [spore]
/// name = "demo"
///
/// [spore.formulas.work]
/// path = "formulas/work.formula.toml"
///
/// [[spore.node]]
/// id = "frame"
/// kind = "fixed"
/// formula = "work"
///
/// [[spore.node]]
/// id = "synth"
/// kind = "fixed"
/// formula = "work"
///
/// [[spore.edge]]
/// from = "frame"
/// to = "synth"
/// type = "feeds"
/// "#;
///
/// let spore = Spore::parse(toml).unwrap();
/// let calls = expand(&spore, &BTreeMap::new()).unwrap();
///
/// assert_eq!(calls.len(), 2);
/// assert_eq!(calls[0].alias, "frame");
/// assert!(calls[0].blocked_by.is_empty());
/// assert_eq!(calls[1].alias, "synth");
/// assert_eq!(calls[1].blocked_by, vec!["frame".to_string()]);
/// ```
pub fn expand(
    spore: &Spore,
    params: &BTreeMap<String, toml::Value>,
) -> Result<Vec<NucleateCall>, ExpandError> {
    // 1. Validate + resolve the param binding against the schema.
    let resolved = resolve_params(spore, params)?;

    // 2. Topologically order the nodes (declaration order breaks ties).
    let order = topo_order(spore)?;

    // Predecessor node ids per node, in edge-declaration order (deduped).
    let preds = predecessors(spore);

    // 3-5. Expand each node in topo order, recording the aliases it produced so
    // later nodes can wire their `blocked_by` to already-emitted calls.
    let mut aliases_of: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut out: Vec<NucleateCall> = Vec::new();

    for &ni in &order {
        let node = &spore.nodes[ni];
        let formula = spore
            .formulas
            .get(&node.formula)
            .map(|f| f.path.clone())
            .ok_or_else(|| ExpandError::UnknownFormula {
                node: node.id.clone(),
                formula: node.formula.clone(),
            })?;

        let blocked_by = collect_blocked_by(&node.id, &preds, &aliases_of);

        let mut produced: Vec<String> = Vec::new();
        match node.kind {
            NodeKind::Fixed => {
                let vars = substitute_vars(node, &resolved, None)?;
                produced.push(node.id.clone());
                out.push(NucleateCall {
                    alias: node.id.clone(),
                    formula,
                    vars,
                    blocked_by,
                    kind: NodeKind::Fixed,
                    for_each: None,
                    bounds: None,
                });
            }
            NodeKind::Fanout => {
                let items = fanout_items(node, &resolved)?;
                for (index, item) in items.iter().enumerate() {
                    let vars = substitute_vars(node, &resolved, Some((item, index)))?;
                    let alias = format!("{}__{}", node.id, index);
                    produced.push(alias.clone());
                    out.push(NucleateCall {
                        alias,
                        formula: formula.clone(),
                        vars,
                        blocked_by: blocked_by.clone(),
                        kind: NodeKind::Fanout,
                        for_each: None,
                        bounds: None,
                    });
                }
            }
            NodeKind::Emergent => {
                // A controller node: it cannot fan out at germination (its
                // for_each ranges over a runtime value), so it expands to one
                // call carrying the runtime for_each and the declared bounds.
                let vars = substitute_vars(node, &resolved, None)?;
                produced.push(node.id.clone());
                out.push(NucleateCall {
                    alias: node.id.clone(),
                    formula,
                    vars,
                    blocked_by,
                    kind: NodeKind::Emergent,
                    for_each: node.for_each.clone(),
                    bounds: node.bounds.clone(),
                });
            }
        }
        aliases_of.insert(node.id.as_str(), produced);
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Param resolution + validation
// ---------------------------------------------------------------------------

/// Validate the binding against the schema and resolve every declared param to a
/// concrete value (supplied value, else default; required-with-neither errors).
fn resolve_params(
    spore: &Spore,
    params: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, toml::Value>, ExpandError> {
    // Reject typos: a binding key the schema never declared is a hard error.
    for key in params.keys() {
        if !spore.params.contains_key(key) {
            return Err(ExpandError::UnknownParam(key.clone()));
        }
    }

    let mut out = BTreeMap::new();
    for (name, spec) in &spore.params {
        let value = match params.get(name) {
            Some(v) => v.clone(),
            None => match &spec.default {
                Some(d) => d.clone(),
                None if spec.required => {
                    return Err(ExpandError::MissingRequiredParam(name.clone()));
                }
                None => continue,
            },
        };
        validate_value(name, spec, &value)?;
        out.insert(name.clone(), value);
    }
    Ok(out)
}

/// Validate one resolved value against its declared type and enum membership.
fn validate_value(param: &str, spec: &ParamSpec, val: &toml::Value) -> Result<(), ExpandError> {
    let mismatch = |detail: String| ExpandError::ParamTypeMismatch {
        param: param.to_string(),
        detail,
    };
    match spec.ty {
        ParamType::String => {
            if !val.is_str() {
                return Err(mismatch(format!(
                    "type is string but value is {}",
                    kind(val)
                )));
            }
        }
        ParamType::Int => {
            if !val.is_integer() {
                return Err(mismatch(format!("type is int but value is {}", kind(val))));
            }
        }
        ParamType::Bool => {
            if !val.is_bool() {
                return Err(mismatch(format!("type is bool but value is {}", kind(val))));
            }
        }
        ParamType::ListString => match val.as_array() {
            Some(items) if items.iter().all(toml::Value::is_str) => {}
            Some(_) => {
                return Err(mismatch(
                    "type is list<string> but value contains a non-string element".to_string(),
                ));
            }
            None => {
                return Err(mismatch(format!(
                    "type is list<string> but value is {}",
                    kind(val)
                )));
            }
        },
        ParamType::Enum => match val.as_str() {
            Some(s) if spec.values.iter().any(|v| v == s) => {}
            Some(s) => {
                return Err(mismatch(format!(
                    "value \"{s}\" is not a member of the enum values {:?}",
                    spec.values
                )));
            }
            None => {
                return Err(mismatch(format!("type is enum but value is {}", kind(val))));
            }
        },
    }
    Ok(())
}

/// A short human label for a TOML value's kind, for error messages.
fn kind(v: &toml::Value) -> &'static str {
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

// ---------------------------------------------------------------------------
// Topological ordering
// ---------------------------------------------------------------------------

/// Order node indices so every edge `from -> to` places `from` before `to`,
/// breaking ties by declaration order. Isolated nodes (no edges) keep their
/// declaration position. Returns [`ExpandError::EdgeCycle`] on a cycle.
fn topo_order(spore: &Spore) -> Result<Vec<usize>, ExpandError> {
    let n = spore.nodes.len();
    let idx: BTreeMap<&str, usize> = spore
        .nodes
        .iter()
        .enumerate()
        .map(|(i, nd)| (nd.id.as_str(), i))
        .collect();

    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in &spore.edges {
        // Parser guarantees endpoints resolve; skip defensively if not.
        if let (Some(&f), Some(&t)) = (idx.get(e.from.as_str()), idx.get(e.to.as_str())) {
            adj[f].push(t);
            indeg[t] += 1;
        }
    }

    let mut emitted = vec![false; n];
    let mut order = Vec::with_capacity(n);
    // Kahn with lowest-index (declaration-order) tie-break. O(n^2) is ample for
    // the handful of nodes a spore declares and keeps the order deterministic.
    while let Some(i) = (0..n).find(|&i| !emitted[i] && indeg[i] == 0) {
        emitted[i] = true;
        order.push(i);
        for &t in &adj[i] {
            indeg[t] -= 1;
        }
    }

    if order.len() != n {
        let cyc = spore
            .nodes
            .iter()
            .enumerate()
            .find(|(i, _)| !emitted[*i])
            .map(|(_, nd)| nd.id.clone())
            .unwrap_or_default();
        return Err(ExpandError::EdgeCycle(cyc));
    }
    Ok(order)
}

/// Map each node id to its predecessor node ids, in edge-declaration order,
/// deduped (a node may carry several typed edges to the same predecessor).
fn predecessors(spore: &Spore) -> BTreeMap<&str, Vec<&str>> {
    let mut preds: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &spore.edges {
        let entry = preds.entry(e.to.as_str()).or_default();
        if !entry.contains(&e.from.as_str()) {
            entry.push(e.from.as_str());
        }
    }
    preds
}

/// Collect the `blocked_by` aliases for a node: every alias produced by every
/// predecessor node, in predecessor order then instance order.
fn collect_blocked_by(
    node_id: &str,
    preds: &BTreeMap<&str, Vec<&str>>,
    aliases_of: &BTreeMap<&str, Vec<String>>,
) -> Vec<String> {
    let mut bb = Vec::new();
    if let Some(predecessors) = preds.get(node_id) {
        for pred in predecessors {
            if let Some(aliases) = aliases_of.get(pred) {
                bb.extend(aliases.iter().cloned());
            }
        }
    }
    bb
}

// ---------------------------------------------------------------------------
// Fan-out + substitution
// ---------------------------------------------------------------------------

/// Resolve a fan-out node's `for_each` to the list of items it ranges over.
///
/// The `for_each` must have the shape `${params.<name>}` and reference a
/// `list<string>` param that resolved to a value.
fn fanout_items(
    node: &super::Node,
    resolved: &BTreeMap<String, toml::Value>,
) -> Result<Vec<String>, ExpandError> {
    let for_each = node.for_each.as_deref().ok_or_else(|| {
        // Parser guarantees a fanout has for_each; treat absence as a malformed
        // directive rather than panicking.
        ExpandError::FanoutForEachNotParamList {
            node: node.id.clone(),
            for_each: String::new(),
        }
    })?;

    let param_name = for_each
        .strip_prefix("${")
        .and_then(|s| s.strip_suffix('}'))
        .and_then(|s| s.strip_prefix("params."))
        .ok_or_else(|| ExpandError::FanoutForEachNotParamList {
            node: node.id.clone(),
            for_each: for_each.to_string(),
        })?;

    let value = resolved
        .get(param_name)
        .ok_or_else(|| ExpandError::UnknownParamReference {
            node: node.id.clone(),
            reference: format!("params.{param_name}"),
        })?;

    match value.as_array() {
        Some(items) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .ok_or_else(|| ExpandError::FanoutForEachNotParamList {
                        node: node.id.clone(),
                        for_each: for_each.to_string(),
                    })
            })
            .collect(),
        None => Err(ExpandError::FanoutForEachNotParamList {
            node: node.id.clone(),
            for_each: for_each.to_string(),
        }),
    }
}

/// Substitute `${params.*}` and (when present) the loop variables in every var
/// value of a node, returning the resolved, sorted var map.
fn substitute_vars(
    node: &super::Node,
    resolved: &BTreeMap<String, toml::Value>,
    loop_ctx: Option<(&str, usize)>,
) -> Result<BTreeMap<String, String>, ExpandError> {
    let mut out = BTreeMap::new();
    for (key, template) in &node.vars {
        out.insert(
            key.clone(),
            substitute(&node.id, template, resolved, loop_ctx)?,
        );
    }
    Ok(out)
}

/// Substitute `${...}` tokens in one template string.
///
/// - `${params.X}` for a declared param `X` -> its scalar rendering.
/// - `${item}` / `${index}` inside a fan-out -> the loop entry / position.
/// - any other `${...}` -> left verbatim (a presumed runtime reference).
/// - `${params.X}` for an undeclared `X` -> [`ExpandError::UnknownParamReference`].
fn substitute(
    node_id: &str,
    template: &str,
    resolved: &BTreeMap<String, toml::Value>,
    loop_ctx: Option<(&str, usize)>,
) -> Result<String, ExpandError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = template[i + 2..].find('}') {
                let token = &template[i + 2..i + 2 + end];
                out.push_str(&resolve_token(node_id, token, resolved, loop_ctx)?);
                i = i + 2 + end + 1;
                continue;
            }
        }
        // Not a complete `${...}` token; copy the byte through. Safe because we
        // only ever split on ASCII `$`, `{`, `}` boundaries.
        let ch = template[i..].chars().next().expect("non-empty remainder");
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

/// Resolve a single `${...}` token body to its replacement text.
fn resolve_token(
    node_id: &str,
    token: &str,
    resolved: &BTreeMap<String, toml::Value>,
    loop_ctx: Option<(&str, usize)>,
) -> Result<String, ExpandError> {
    if let Some((item, index)) = loop_ctx {
        match token {
            "item" => return Ok(item.to_string()),
            "index" => return Ok(index.to_string()),
            _ => {}
        }
    }
    if let Some(param) = token.strip_prefix("params.") {
        return match resolved.get(param) {
            Some(value) => Ok(scalar(value)),
            None => Err(ExpandError::UnknownParamReference {
                node: node_id.to_string(),
                reference: token.to_string(),
            }),
        };
    }
    // Unknown token: a presumed runtime reference (e.g. nodes.x.findings). Leave
    // it verbatim so the worker can resolve it downstream.
    Ok(format!("${{{token}}}"))
}

/// Render a resolved param value as the scalar string used in var substitution.
/// Lists join with `,`; scalars use their natural text.
fn scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(items) => items.iter().map(scalar).collect::<Vec<_>>().join(","),
        toml::Value::Table(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::Spore;
    use super::*;

    /// Build a param binding from `(name, toml::Value)` pairs.
    fn binding(pairs: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// A linear fixed chain: frame -> synth -> verdict.
    const LINEAR: &str = r#"
[spore]
name = "linear"

[spore.params.subject]
type = "string"
required = true

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
[spore.node.vars]
subject = "${params.subject}"

[[spore.node]]
id = "synth"
kind = "fixed"
formula = "work"

[[spore.node]]
id = "verdict"
kind = "fixed"
formula = "work"

[[spore.edge]]
from = "frame"
to = "synth"
type = "feeds"

[[spore.edge]]
from = "synth"
to = "verdict"
type = "feeds"
"#;

    #[test]
    fn test_linear_expands_in_order_with_blocked_by_chain() {
        let spore = Spore::parse(LINEAR).unwrap();
        let calls = expand(&spore, &binding(&[("subject", "octopus".into())])).unwrap();

        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].alias, "frame");
        assert!(calls[0].blocked_by.is_empty());
        // ${params.subject} substituted.
        assert_eq!(
            calls[0].vars.get("subject").map(String::as_str),
            Some("octopus")
        );

        assert_eq!(calls[1].alias, "synth");
        assert_eq!(calls[1].blocked_by, vec!["frame".to_string()]);

        assert_eq!(calls[2].alias, "verdict");
        assert_eq!(calls[2].blocked_by, vec!["synth".to_string()]);
    }

    #[test]
    fn test_replays_top_to_bottom_every_alias_predefined() {
        // The ordering guarantee: each blocked_by alias is defined by an
        // earlier call. This is what makes the list a replayable Makefile.
        let spore = Spore::parse(FANOUT_AND_EMERGENT).unwrap();
        let calls = expand(
            &spore,
            &binding(&[(
                "axes",
                toml::Value::Array(vec!["a".into(), "b".into(), "c".into()]),
            )]),
        )
        .unwrap();

        let mut defined: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for call in &calls {
            for dep in &call.blocked_by {
                assert!(
                    defined.contains(dep.as_str()),
                    "alias {dep} used before it is defined"
                );
            }
            defined.insert(call.alias.as_str());
        }
    }

    /// frame (fixed) -> analyse (fanout over axes) -> verify (emergent) -> index (fixed).
    const FANOUT_AND_EMERGENT: &str = r#"
[spore]
name = "fanout-emergent"

[spore.params.axes]
type = "list<string>"
default = ["x", "y"]

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"

[[spore.node]]
id = "analyse"
kind = "fanout"
for_each = "${params.axes}"
formula = "work"
[spore.node.vars]
axis = "${item}"
pos = "${index}"

[[spore.node]]
id = "verify"
kind = "emergent"
for_each = "${nodes.analyse.findings}"
formula = "work"
[spore.node.bounds]
output_type = "finding"
max_instances = 32
stop_condition = "every finding consumed once"

[[spore.node]]
id = "index"
kind = "fixed"
formula = "work"

[[spore.edge]]
from = "frame"
to = "analyse"
type = "feeds"

[[spore.edge]]
from = "analyse"
to = "verify"
type = "produces"

[[spore.edge]]
from = "verify"
to = "index"
type = "feeds"
"#;

    #[test]
    fn test_fanout_emits_one_call_per_item_with_loop_vars() {
        let spore = Spore::parse(FANOUT_AND_EMERGENT).unwrap();
        let calls = expand(
            &spore,
            &binding(&[(
                "axes",
                toml::Value::Array(vec!["alpha".into(), "beta".into(), "gamma".into()]),
            )]),
        )
        .unwrap();

        let analyse: Vec<_> = calls
            .iter()
            .filter(|c| c.alias.starts_with("analyse__"))
            .collect();
        assert_eq!(analyse.len(), 3);
        assert_eq!(analyse[0].alias, "analyse__0");
        assert_eq!(
            analyse[0].vars.get("axis").map(String::as_str),
            Some("alpha")
        );
        assert_eq!(analyse[0].vars.get("pos").map(String::as_str), Some("0"));
        assert_eq!(analyse[2].alias, "analyse__2");
        assert_eq!(
            analyse[2].vars.get("axis").map(String::as_str),
            Some("gamma")
        );
        assert_eq!(analyse[2].vars.get("pos").map(String::as_str), Some("2"));
        // Each fanout instance waits on the single fixed predecessor.
        for c in &analyse {
            assert_eq!(c.blocked_by, vec!["frame".to_string()]);
            assert_eq!(c.kind, NodeKind::Fanout);
        }
    }

    #[test]
    fn test_emergent_controller_carries_for_each_and_bounds() {
        let spore = Spore::parse(FANOUT_AND_EMERGENT).unwrap();
        let calls = expand(&spore, &BTreeMap::new()).unwrap();

        let verify = calls.iter().find(|c| c.alias == "verify").unwrap();
        assert_eq!(verify.kind, NodeKind::Emergent);
        assert_eq!(
            verify.for_each.as_deref(),
            Some("${nodes.analyse.findings}")
        );
        let bounds = verify.bounds.as_ref().expect("controller carries bounds");
        assert_eq!(bounds.max_instances, 32);
        assert_eq!(bounds.output_type, "finding");
        // The default axes = ["x","y"] => two analyse instances feed the controller.
        assert_eq!(
            verify.blocked_by,
            vec!["analyse__0".to_string(), "analyse__1".to_string()]
        );
    }

    #[test]
    fn test_downstream_of_fanout_waits_on_all_instances() {
        let spore = Spore::parse(FANOUT_AND_EMERGENT).unwrap();
        let calls = expand(
            &spore,
            &binding(&[(
                "axes",
                toml::Value::Array(vec!["a".into(), "b".into(), "c".into()]),
            )]),
        )
        .unwrap();
        // index <- verify (single emergent controller).
        let index = calls.iter().find(|c| c.alias == "index").unwrap();
        assert_eq!(index.blocked_by, vec!["verify".to_string()]);
    }

    #[test]
    fn test_same_spore_same_params_is_byte_identical() {
        let spore = Spore::parse(FANOUT_AND_EMERGENT).unwrap();
        let params = binding(&[("axes", toml::Value::Array(vec!["a".into(), "b".into()]))]);
        let a = expand(&spore, &params).unwrap();
        let b = expand(&spore, &params).unwrap();
        // Structural equality and Debug-byte equality both hold (pure fn).
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn test_missing_required_param_refuses() {
        let spore = Spore::parse(LINEAR).unwrap();
        assert_eq!(
            expand(&spore, &BTreeMap::new()),
            Err(ExpandError::MissingRequiredParam("subject".to_string()))
        );
    }

    #[test]
    fn test_unknown_param_in_binding_refuses() {
        let spore = Spore::parse(LINEAR).unwrap();
        let params = binding(&[("subject", "x".into()), ("bogus", "y".into())]);
        assert_eq!(
            expand(&spore, &params),
            Err(ExpandError::UnknownParam("bogus".to_string()))
        );
    }

    #[test]
    fn test_param_type_mismatch_refuses() {
        let spore = Spore::parse(LINEAR).unwrap();
        // subject is declared string; pass an integer.
        let params = binding(&[("subject", toml::Value::Integer(7))]);
        match expand(&spore, &params) {
            Err(ExpandError::ParamTypeMismatch { param, .. }) => assert_eq!(param, "subject"),
            other => panic!("expected ParamTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_param_reference_in_vars_refuses() {
        let toml = r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
kind = "fixed"
formula = "work"
[spore.node.vars]
x = "${params.nope}"
"#;
        let spore = Spore::parse(toml).unwrap();
        assert_eq!(
            expand(&spore, &BTreeMap::new()),
            Err(ExpandError::UnknownParamReference {
                node: "a".to_string(),
                reference: "params.nope".to_string(),
            })
        );
    }

    #[test]
    fn test_runtime_reference_left_verbatim() {
        let toml = r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "a"
kind = "fixed"
formula = "work"
[spore.node.vars]
finding = "${nodes.x.findings}"
"#;
        let spore = Spore::parse(toml).unwrap();
        let calls = expand(&spore, &BTreeMap::new()).unwrap();
        // Unknown (runtime) token survives untouched.
        assert_eq!(
            calls[0].vars.get("finding").map(String::as_str),
            Some("${nodes.x.findings}")
        );
    }

    #[test]
    fn test_isolated_nodes_keep_declaration_order() {
        let toml = r#"
[spore]
name = "t"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "one"
kind = "fixed"
formula = "work"

[[spore.node]]
id = "two"
kind = "fixed"
formula = "work"
"#;
        let spore = Spore::parse(toml).unwrap();
        let calls = expand(&spore, &BTreeMap::new()).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].alias, "one");
        assert_eq!(calls[1].alias, "two");
        assert!(calls.iter().all(|c| c.blocked_by.is_empty()));
    }

    #[test]
    fn test_formula_path_resolved_from_alias() {
        let spore = Spore::parse(LINEAR).unwrap();
        let calls = expand(&spore, &binding(&[("subject", "z".into())])).unwrap();
        assert!(calls
            .iter()
            .all(|c| c.formula == "formulas/work.formula.toml"));
    }
}
