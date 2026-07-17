// SPDX-License-Identifier: AGPL-3.0-only

//! `cs spore` — the verb family that germinates a whole polymer from a
//! shareable `spore.toml` template (ADR-140 N5).
//!
//! A **spore** is a parameterizable mission plan: a fleet + per-node
//! formulas + a `ParamSchema` + a DAG of typed edges + an optional `.tla`
//! seal. This module is the **shell** over the pure core: it loads the
//! manifest, coerces `--var k=v` strings into the declared TOML types,
//! gates on the seal (D4), and replays the
//! [`expand`](fn@cosmon_core::spore::expand)ed call list against the live
//! state store via the canonical `cs nucleate` persistence path. It is a
//! declarative front end over an existing verb, not a new scheduler and
//! not a new molecule type.
//!
//! Three sibling verbs, one role each:
//!
//! - **`cs spore validate`** — parse (N2) + expand (N3) as a dry run. Prints
//!   the ordered `cs nucleate ... --blocked-by ...` list; germinates nothing.
//! - **`cs spore run`** — parse + expand + seal gate (N4) then germinate the
//!   real polymer against the state store. `--json` emits NDJSON, one object
//!   per germinated molecule (agent-first invariant).
//! - **`cs spore export`** — the share-time emit verb: always writes a
//!   content-addressed bundle id over the manifest + its referenced recipes,
//!   plus an ASTRA-compatible `ro-crate-metadata.json` descriptive layer
//!   (ADR-140 D6). The `[spore.astra]` stanza only customizes the output
//!   path; its absence is not an opt-out.
//!
//! ## The seal gate (ADR-140 D4), stated honestly
//!
//! `cs spore run` never claims a seal is verified when it is not. A spore
//! with no `[spore.seal]` germinates freely (`seal: none`). A *sealed*
//! spore cannot be proven on a machine without the TLC verifier wired in,
//! so the default is **fail-closed**: it refuses unless the operator opts
//! into the risk with `--allow-unchecked-seal`, in which case the status
//! line reads `seal: present, NOT verified` — never `verified`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cosmon_core::id::{FleetId, MoleculeId};
use cosmon_core::nucleate::NucleateResult;
use cosmon_core::spore::{expand, NodeKind, NucleateCall, ParamType, Spore};
use cosmon_core::tag::Tag;

use super::nucleate::{load_formula_at_path, nucleate_for_spore, SporeNucleation};
use super::Context;

/// Arguments for the `spore` subcommand family.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    cmd: Sub,
}

/// The three `cs spore` verbs.
#[derive(clap::Subcommand)]
enum Sub {
    /// Parse + expand a spore as a dry run; print the ordered nucleate
    /// call list without germinating anything.
    #[command(after_help = super::examples::SPORE_VALIDATE)]
    Validate(ValidateArgs),

    /// Germinate the polymer: parse + expand + seal gate, then replay the
    /// call list against the live state store.
    #[command(after_help = super::examples::SPORE_RUN)]
    Run(RunArgs),

    /// Emit a content-addressed bundle id and an ASTRA descriptive layer
    /// (ADR-140 D6) for sharing the spore.
    #[command(after_help = super::examples::SPORE_EXPORT)]
    Export(ExportArgs),
}

/// `cs spore validate <ref> --var k=v` arguments.
#[derive(clap::Args)]
pub struct ValidateArgs {
    /// Path to a `spore.toml` manifest (or a directory containing one).
    #[arg(value_name = "REF")]
    reference: PathBuf,

    /// Bind a parameter (repeatable: `--var key=value`). Values are coerced
    /// into the declared `ParamSchema` type before expansion.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,
}

/// `cs spore run <ref> --var k=v` arguments.
#[derive(clap::Args)]
pub struct RunArgs {
    /// Path to a `spore.toml` manifest (or a directory containing one).
    #[arg(value_name = "REF")]
    reference: PathBuf,

    /// Bind a parameter (repeatable: `--var key=value`).
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,

    /// Germinate a *sealed* spore even though its `.tla` proof was not
    /// verified this run (TLC unavailable). The status line stays honest:
    /// `seal: present, NOT verified` (ADR-140 D4). Without this flag a
    /// sealed spore fails closed.
    #[arg(long = "allow-unchecked-seal")]
    allow_unchecked_seal: bool,

    /// Fleet to germinate the polymer into.
    #[arg(long, default_value = "default")]
    fleet: String,

    /// State store root (default: walk-up `.cosmon`).
    #[arg(long, value_name = "DIR")]
    store_dir: Option<PathBuf>,
}

/// `cs spore export <ref>` arguments.
#[derive(clap::Args)]
pub struct ExportArgs {
    /// Path to a `spore.toml` manifest (or a directory containing one).
    #[arg(value_name = "REF")]
    reference: PathBuf,

    /// Output directory for the ASTRA descriptive layer. Defaults to the
    /// manifest directory. The crate is always written here as
    /// `ro-crate-metadata.json` unless `[spore.astra].output` names a
    /// different (manifest-relative) path.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
}

/// Dispatch a `cs spore` invocation to its verb handler.
///
/// # Errors
/// Propagates manifest-load, expansion, seal-gate, and persistence errors.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.cmd {
        Sub::Validate(a) => run_validate(ctx, a),
        Sub::Run(a) => run_run(ctx, a),
        Sub::Export(a) => run_export(ctx, a),
    }
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

/// `cs spore validate` — parse + expand, print the call list, germinate nothing.
fn run_validate(ctx: &Context, args: &ValidateArgs) -> anyhow::Result<()> {
    let (spore, _dir) = load_spore(&args.reference)?;
    let params = coerce_vars(&spore, &args.vars)?;
    let calls = expand(&spore, &params).map_err(|e| anyhow::anyhow!("expand failed: {e}"))?;

    if ctx.json {
        for call in &calls {
            println!("{}", call_to_json(call));
        }
    } else {
        println!(
            "spore: {} (v{}) - {} call(s)",
            spore.name,
            spore.version,
            calls.len()
        );
        println!("seal: {}", seal_label(&spore));
        for call in &calls {
            print_call_human(call);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// `cs spore run` — parse + expand + seal gate, then germinate the polymer.
fn run_run(ctx: &Context, args: &RunArgs) -> anyhow::Result<()> {
    let (spore, manifest_dir) = load_spore(&args.reference)?;
    let params = coerce_vars(&spore, &args.vars)?;
    let calls = expand(&spore, &params).map_err(|e| anyhow::anyhow!("expand failed: {e}"))?;

    // Seal gate (ADR-140 D4) — fail closed before any state is written.
    let seal_status = gate_seal(&spore, args.allow_unchecked_seal)?;
    // The status note goes to stderr so `--json` stdout stays clean NDJSON.
    eprintln!("seal: {seal_status}");

    let store_dir = cosmon_filestore::resolve_state_dir(args.store_dir.as_deref());
    let fleet_id =
        FleetId::new(&args.fleet).map_err(|e| anyhow::anyhow!("invalid fleet id: {e}"))?;
    let (project_id, energy_default) = resolve_project_context(ctx);

    let results = germinate(
        &calls,
        &spore.review,
        &manifest_dir,
        &store_dir,
        &fleet_id,
        project_id.as_ref(),
        energy_default,
    )?;

    if ctx.json {
        for (call, result) in calls.iter().zip(&results) {
            println!("{}", germinated_to_json(call, result));
        }
    } else {
        println!(
            "Germinated spore {} into {} molecule(s):",
            spore.name,
            results.len()
        );
        for result in &results {
            println!("  {} ({})", result.id, result.formula_id);
        }
    }
    Ok(())
}

/// Replay the expanded call list against the live state store, mapping each
/// node alias to its freshly-minted [`MoleculeId`] so later calls can wire
/// `blocked_by` to the real molecules. The list is ordered so every
/// `blocked_by` alias is already germinated when its dependent is reached.
fn germinate(
    calls: &[NucleateCall],
    review: &cosmon_core::fleet::CrossProviderReview,
    manifest_dir: &Path,
    store_dir: &Path,
    fleet_id: &FleetId,
    project_id: Option<&cosmon_core::id::ProjectId>,
    energy_budget_cap: u32,
) -> anyhow::Result<Vec<NucleateResult>> {
    let mut warm =
        vec![Tag::new("temp:warm").map_err(|e| anyhow::anyhow!("internal tag error: {e}"))?];
    if review.cross_provider {
        warm.push(Tag::new("needs-review").expect("static review tag is valid"));
        warm.push(
            Tag::new("needs-review-cross-provider")
                .expect("static cross-provider review tag is valid"),
        );
        if let Some(adapter) = &review.reviewer_adapter {
            warm.push(
                Tag::new(format!("reviewer-adapter:{adapter}")).map_err(|e| {
                    anyhow::anyhow!("invalid spore reviewer_adapter `{adapter}`: {e}")
                })?,
            );
        }
    }
    let mut alias_to_id: BTreeMap<String, MoleculeId> = BTreeMap::new();
    let mut results = Vec::with_capacity(calls.len());

    for call in calls {
        // Resolve this call's blocked_by aliases to already-germinated IDs.
        // The expand() ordering guarantee makes every lookup succeed.
        let mut blocked_by = Vec::with_capacity(call.blocked_by.len());
        for alias in &call.blocked_by {
            let id = alias_to_id.get(alias).ok_or_else(|| {
                anyhow::anyhow!(
                    "internal: blocked_by alias `{alias}` of call `{}` not yet germinated",
                    call.alias
                )
            })?;
            blocked_by.push(id.clone());
        }

        let formula_path = manifest_dir.join(&call.formula);
        let formula = load_formula_at_path(&formula_path)?;
        let result = nucleate_for_spore(SporeNucleation {
            formula: &formula,
            variables: call.vars.clone().into_iter().collect(),
            kind: None,
            blocked_by: &blocked_by,
            fleet_id,
            store_dir,
            project_id,
            tags: &warm,
            energy_budget_cap,
        })?;
        alias_to_id.insert(call.alias.clone(), result.id.clone());
        results.push(result);
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// `cs spore export` — content-addressed bundle id + ASTRA descriptive layer.
fn run_export(ctx: &Context, args: &ExportArgs) -> anyhow::Result<()> {
    let (spore, manifest_dir) = load_spore(&args.reference)?;
    let bundle = bundle_hash(&spore, &manifest_dir);

    let out_dir = args.out.clone().unwrap_or_else(|| manifest_dir.clone());
    let astra = build_astra(&spore, &bundle);

    // `cs spore export` is the explicit *share-time* emit verb (ADR-140 D6):
    // invoking it always writes the RO-Crate. The `[spore.astra]` stanza only
    // customizes the output path; its absence is not an opt-out (that would
    // make an `export` that exports nothing). When no `output` is declared we
    // default to the RO-Crate standard filename in the out dir. The `emit`
    // flag governs *automatic* emission on run/mission-completion, not this
    // hand-invoked verb.
    let astra_rel = spore
        .astra
        .as_ref()
        .and_then(|a| a.output.clone())
        .unwrap_or_else(|| "ro-crate-metadata.json".to_string());
    let astra_path = out_dir.join(astra_rel);

    if let Some(parent) = astra_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&astra)
        .map_err(|e| anyhow::anyhow!("serialize ASTRA: {e}"))?;
    std::fs::write(&astra_path, body)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", astra_path.display()))?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "spore": spore.name,
                "bundle_hash": bundle,
                "astra": astra_path.display().to_string(),
            })
        );
    } else {
        println!("spore: {}", spore.name);
        println!("bundle: {bundle}");
        println!("astra: {}", astra_path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Resolve a `<ref>` to a parsed [`Spore`] and its manifest directory (the
/// base for relative formula and seal paths). `<ref>` may be a `spore.toml`
/// file or a directory containing one.
fn load_spore(reference: &Path) -> anyhow::Result<(Spore, PathBuf)> {
    let manifest = if reference.is_dir() {
        reference.join("spore.toml")
    } else {
        reference.to_path_buf()
    };
    let text = std::fs::read_to_string(&manifest).map_err(|e| {
        anyhow::anyhow!("failed to read spore manifest {}: {e}", manifest.display())
    })?;
    let spore = Spore::parse(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse spore {}: {e}", manifest.display()))?;
    let dir = manifest
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Ok((spore, dir))
}

/// Coerce raw `--var key=value` strings into the declared `ParamSchema`
/// TOML types so the pure [`expand`](fn@expand) sees properly-typed values. An
/// undeclared key is passed through as a string and rejected by `expand`
/// (which owns the unknown-param error), keeping a single source of truth.
fn coerce_vars(spore: &Spore, raw: &[String]) -> anyhow::Result<BTreeMap<String, toml::Value>> {
    let mut out = BTreeMap::new();
    for kv in raw {
        let (key, value) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --var (expected key=value): {kv}"))?;
        let coerced = coerce_one(spore, key, value)?;
        out.insert(key.to_string(), coerced);
    }
    Ok(out)
}

/// Coerce one value against its declared type. List values split on `,`.
fn coerce_one(spore: &Spore, key: &str, value: &str) -> anyhow::Result<toml::Value> {
    let ty = spore.params.get(key).map(|s| s.ty);
    Ok(match ty {
        Some(ParamType::Int) => toml::Value::Integer(
            value
                .trim()
                .parse::<i64>()
                .map_err(|e| anyhow::anyhow!("--var {key}: expected int, got `{value}`: {e}"))?,
        ),
        Some(ParamType::Bool) => toml::Value::Boolean(
            value
                .trim()
                .parse::<bool>()
                .map_err(|e| anyhow::anyhow!("--var {key}: expected bool, got `{value}`: {e}"))?,
        ),
        Some(ParamType::ListString) => toml::Value::Array(
            value
                .split(',')
                .map(|s| toml::Value::String(s.trim().to_string()))
                .collect(),
        ),
        // String, Enum, or an undeclared key: keep the raw string and let
        // `expand` enforce schema membership / unknown-param rejection.
        _ => toml::Value::String(value.to_string()),
    })
}

/// The seal gate (ADR-140 D4). Returns the honest status line or fails
/// closed. Never returns a "verified" status — TLC verification is N4's
/// contract and is not wired here, so a present seal is at best
/// "present, NOT verified".
fn gate_seal(spore: &Spore, allow_unchecked: bool) -> anyhow::Result<&'static str> {
    match &spore.seal {
        None => Ok("none"),
        Some(_) if allow_unchecked => Ok("present, NOT verified"),
        Some(_) => anyhow::bail!(
            "spore carries a [spore.seal] but its .tla proof was not verified \
             (TLC unavailable); refusing to germinate (fail-closed, ADR-140 D4). \
             Re-run with --allow-unchecked-seal to opt into the risk."
        ),
    }
}

/// The non-gating seal label used by `validate` (read-only, no refusal).
fn seal_label(spore: &Spore) -> &'static str {
    if spore.seal.is_some() {
        "present, NOT verified"
    } else {
        "none"
    }
}

/// Resolve project id and the default step budget from `.cosmon/config.toml`,
/// falling back to defaults for legacy projects.
fn resolve_project_context(ctx: &Context) -> (Option<cosmon_core::id::ProjectId>, u32) {
    let config_path = super::resolve_config_from_context(ctx);
    let loaded = cosmon_filestore::load_project_config(&config_path).ok();
    let project_id = loaded.as_ref().and_then(|c| c.project.project_id.clone());
    let energy = loaded
        .as_ref()
        .map_or(100, |c| c.energy.default_step_budget);
    (project_id, energy)
}

/// Compute a content-addressed bundle hash over the manifest and every
/// recipe / seal file it references. The hash binds each file's relative
/// path and bytes in sorted order, so the same bundle content always
/// yields the same id (content-addressing is the registry, ADR-039).
fn bundle_hash(spore: &Spore, manifest_dir: &Path) -> String {
    let mut files: Vec<String> = vec!["spore.toml".to_string()];
    for f in spore.formulas.values() {
        files.push(f.path.clone());
    }
    if let Some(seal) = &spore.seal {
        files.push(seal.module.clone());
        if let Some(cfg) = &seal.config {
            files.push(cfg.clone());
        }
    }
    files.sort();
    files.dedup();

    let mut buf: Vec<u8> = Vec::new();
    for rel in &files {
        let path = manifest_dir.join(rel);
        let bytes = std::fs::read(&path).unwrap_or_default();
        buf.extend_from_slice(rel.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&bytes);
    }
    format!("blake3:{}", cosmon_hash::Hash::of_bytes(&buf).to_hex())
}

/// Build a minimal RO-Crate / ASTRA-compatible descriptive layer (ADR-140
/// D6). The spore header, params, and DAG map onto ASTRA's method /
/// provenance fields; the seal verdict is attached as the proof artifact —
/// honestly marked unverified, never claiming a proof that did not run.
fn build_astra(spore: &Spore, bundle: &str) -> serde_json::Value {
    let params: Vec<_> = spore
        .params
        .iter()
        .map(|(name, spec)| {
            serde_json::json!({
                "name": name,
                "required": spec.required,
            })
        })
        .collect();
    let nodes: Vec<_> = spore
        .nodes
        .iter()
        .map(|n| serde_json::json!({ "id": n.id, "formula": n.formula }))
        .collect();
    let edges: Vec<_> = spore
        .edges
        .iter()
        .map(|e| serde_json::json!({ "from": e.from, "to": e.to }))
        .collect();

    serde_json::json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {
                "@type": "CreativeWork",
                "@id": "ro-crate-metadata.json",
                "conformsTo": { "@id": "https://w3id.org/ro/crate/1.1" },
                "about": { "@id": "./" }
            },
            {
                "@id": "./",
                "@type": "Dataset",
                "name": spore.name,
                "version": spore.version,
                "description": spore.description,
                "spore:bundleHash": bundle,
                "spore:params": params,
                "spore:nodes": nodes,
                "spore:edges": edges,
                "spore:seal": {
                    "present": spore.seal.is_some(),
                    "verified": false
                }
            }
        ]
    })
}

/// Render one expanded call as a JSON line (`validate --json`).
fn call_to_json(call: &NucleateCall) -> serde_json::Value {
    serde_json::json!({
        "alias": call.alias,
        "formula": call.formula,
        "kind": node_kind_label(call.kind),
        "blocked_by": call.blocked_by,
        "vars": call.vars,
        "for_each": call.for_each,
        "bounds": call.bounds.as_ref().map(|b| serde_json::json!({
            "output_type": b.output_type,
            "max_instances": b.max_instances,
            "stop_condition": b.stop_condition,
        })),
    })
}

/// Render one germinated molecule as a JSON line (`run --json`).
fn germinated_to_json(call: &NucleateCall, result: &NucleateResult) -> serde_json::Value {
    serde_json::json!({
        "alias": call.alias,
        "id": result.id.as_str(),
        "formula": result.formula_id.as_str(),
        "blocked_by": call.blocked_by,
        "status": "active",
    })
}

/// Human-readable one-block rendering of a call for `validate`.
fn print_call_human(call: &NucleateCall) {
    println!("  • {} [{}]", call.alias, node_kind_label(call.kind));
    println!("      formula: {}", call.formula);
    if !call.blocked_by.is_empty() {
        println!("      blocked-by: {}", call.blocked_by.join(", "));
    }
    for (k, v) in &call.vars {
        println!("      var {k} = {v}");
    }
}

/// The wire label for a [`NodeKind`].
fn node_kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Fixed => "fixed",
        NodeKind::Fanout => "fanout",
        NodeKind::Emergent => "emergent",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPORE: &str = r#"
[spore]
name = "demo"
version = 1

[spore.params.subject]
type = "string"
required = true

[spore.params.axes]
type = "list<string>"
default = ["a", "b"]

[spore.formulas.work]
path = "work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
[spore.node.vars]
subject = "${params.subject}"

[[spore.node]]
id = "analyse"
kind = "fanout"
for_each = "${params.axes}"
formula = "work"
[spore.node.vars]
axis = "${item}"

[[spore.edge]]
from = "frame"
to = "analyse"
type = "feeds"
"#;

    const SEALED: &str = r#"
[spore]
name = "sealed"

[spore.seal]
module = "spore.tla"
config = "spore.cfg"

[spore.formulas.work]
path = "work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
"#;

    const FORMULA: &str = r#"
formula = "work"
version = 1
id_prefix = "task"

[[steps]]
id = "do"
title = "Do the work"
description = "the body"
acceptance = "any evidence"
"#;

    /// Write a spore + its referenced formula into a temp dir, return the dir.
    fn fixture(spore: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("spore.toml"), spore).unwrap();
        std::fs::write(dir.path().join("work.formula.toml"), FORMULA).unwrap();
        dir
    }

    #[test]
    fn coerce_vars_respects_declared_types() {
        let spore = Spore::parse(SPORE).unwrap();
        let got = coerce_vars(
            &spore,
            &["subject=octopus".to_string(), "axes=x,y,z".to_string()],
        )
        .unwrap();
        assert_eq!(got["subject"], toml::Value::String("octopus".into()));
        assert_eq!(
            got["axes"],
            toml::Value::Array(vec!["x".into(), "y".into(), "z".into()])
        );
    }

    #[test]
    fn coerce_vars_rejects_missing_equals() {
        let spore = Spore::parse(SPORE).unwrap();
        let err = coerce_vars(&spore, &["bogus".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("expected key=value"));
    }

    #[test]
    fn seal_gate_absent_proceeds() {
        let spore = Spore::parse(SPORE).unwrap();
        assert_eq!(gate_seal(&spore, false).unwrap(), "none");
    }

    #[test]
    fn seal_gate_present_fails_closed_by_default() {
        let spore = Spore::parse(SEALED).unwrap();
        let err = gate_seal(&spore, false).unwrap_err();
        assert!(format!("{err}").contains("fail-closed"));
    }

    #[test]
    fn seal_gate_present_proceeds_under_flag_but_never_claims_verified() {
        let spore = Spore::parse(SEALED).unwrap();
        let status = gate_seal(&spore, true).unwrap();
        assert_eq!(status, "present, NOT verified");
        assert!(!status.contains("verified ") && !status.starts_with("verified"));
    }

    #[test]
    fn load_spore_accepts_directory() {
        let dir = fixture(SPORE);
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        assert_eq!(spore.name, "demo");
        assert_eq!(manifest_dir, dir.path());
    }

    #[test]
    fn germinate_replays_calls_into_the_store_with_blocked_by() {
        let dir = fixture(SPORE);
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        let params = coerce_vars(&spore, &["subject=octopus".to_string()]).unwrap();
        let calls = expand(&spore, &params).unwrap();

        let store = tempfile::tempdir().unwrap();
        let fleet = FleetId::new("default").unwrap();
        let results = germinate(
            &calls,
            &cosmon_core::fleet::CrossProviderReview::default(),
            &manifest_dir,
            store.path(),
            &fleet,
            None,
            100,
        )
        .unwrap();

        // frame + two fanout instances (default axes = ["a","b"]).
        assert_eq!(results.len(), 3);

        // The two analyse molecules must each carry a BlockedBy edge to the
        // frame molecule — proof the alias→id wiring landed on disk.
        use cosmon_state::StateStore;
        let fs = cosmon_filestore::FileStore::new(store.path());
        let frame_id = &results[0].id;
        let downstream = &results[1];
        let mol = fs.load_molecule(&downstream.id).unwrap();
        let blocks_on_frame = mol.typed_links.iter().any(|l| {
            matches!(
                l,
                cosmon_core::interaction::MoleculeLink::BlockedBy { source } if source == frame_id
            )
        });
        assert!(blocks_on_frame, "fanout instance must be blocked by frame");
    }

    #[test]
    fn bundle_hash_is_stable_and_prefixed() {
        let dir = fixture(SPORE);
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        let a = bundle_hash(&spore, &manifest_dir);
        let b = bundle_hash(&spore, &manifest_dir);
        assert_eq!(a, b);
        assert!(a.starts_with("blake3:"));
    }

    #[test]
    fn astra_marks_seal_unverified() {
        let spore = Spore::parse(SEALED).unwrap();
        let astra = build_astra(&spore, "blake3:abc");
        let dataset = &astra["@graph"][1];
        assert_eq!(dataset["spore:seal"]["present"], serde_json::json!(true));
        assert_eq!(dataset["spore:seal"]["verified"], serde_json::json!(false));
    }
}
