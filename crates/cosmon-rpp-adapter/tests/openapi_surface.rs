// SPDX-License-Identifier: AGPL-3.0-only

//! Keep the hand-curated REST contract exhaustive without publishing internal
//! doors. Axum exposes no public route iterator, so inspect the router's Rust
//! syntax tree, not its comments or the independently maintained event canon.
//! Computed paths must have an explicit resolver; unknown expressions fail closed.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;
use syn::visit::{self, Visit};
use syn::{Expr, ExprMethodCall, Item, Lit};

type CheckResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
struct Registrations {
    paths: BTreeSet<String>,
    errors: Vec<String>,
}

fn literal(expr: &Expr) -> Option<String> {
    if let Expr::Lit(value) = expr {
        if let Lit::Str(value) = &value.lit {
            return Some(value.value());
        }
    }
    None
}

fn route_path(expr: &Expr) -> CheckResult<String> {
    if let Some(path) = literal(expr) {
        return Ok(path);
    }
    // Evaluate the actual production helper, never copy its URL template.
    if let Expr::Reference(reference) = expr {
        if let Expr::Call(call) = reference.expr.as_ref() {
            if let Expr::Path(function) = call.func.as_ref() {
                let segments: Vec<_> = function
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();
                if segments == ["routes", "dist", "binary_url_path"] && call.args.len() == 1 {
                    if let Some(platform) = call.args.first().and_then(literal) {
                        return Ok(cosmon_rpp_adapter::routes::dist::binary_url_path(&platform));
                    }
                }
            }
        }
    }
    Err(
        "unresolved registered path expression; extend the AST resolver before changing routing"
            .into(),
    )
}

impl<'ast> Visit<'ast> for Registrations {
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        match call.method.to_string().as_str() {
            "route" | "route_service" | "nest" | "nest_service" => {
                let result = call
                    .args
                    .first()
                    .ok_or_else(|| "registration has no path".into())
                    .and_then(route_path);
                match result {
                    Ok(path) => {
                        self.paths.insert(path);
                    }
                    Err(error) => self.errors.push(error.to_string()),
                }
            }
            "merge" => self
                .errors
                .push("router merge needs explicit AST traversal of the merged router".into()),
            _ => {}
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_macro(&mut self, _node: &'ast syn::ExprMacro) {
        self.errors.push(
            "router macro needs explicit AST expansion before coverage can be checked".into(),
        );
    }
}

fn registered_paths(source: &str) -> CheckResult<BTreeSet<String>> {
    let file = syn::parse_file(source)?;
    let router = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "router" => Some(function),
            _ => None,
        })
        .ok_or("router function not found")?;
    let mut registered = Registrations::default();
    registered.visit_block(&router.block);
    if !registered.errors.is_empty() {
        return Err(registered.errors.join("; ").into());
    }
    if registered.paths.is_empty() {
        return Err("router contains no registrations".into());
    }
    Ok(registered.paths)
}

// This is deliberately a path-key reader, not a general YAML/schema validator.
// Require the existing block-map style (plain or quoted path keys at two spaces).
// Refuse aliases, flow maps, merge keys and unsupported indentation at that level
// rather than silently obtaining an empty/partial catalogue. Nested descriptions
// and examples are never interpreted as declarations. No new YAML dependency is
// needed for this small, explicitly bounded formatting contract.
fn declared_paths(yaml: &str) -> CheckResult<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    let mut in_paths = false;
    let mut found_paths = false;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') {
            in_paths = false;
            if line.starts_with("paths:") {
                if found_paths || trimmed != "paths:" {
                    return Err("paths must be a single block mapping".into());
                }
                found_paths = true;
                in_paths = true;
            }
            continue;
        }
        if !in_paths {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent > 2 {
            if paths.is_empty() {
                return Err("path keys must have exactly two spaces".into());
            }
            continue;
        }
        if indent != 2 {
            return Err("path keys must have exactly two spaces".into());
        }
        let key = trimmed
            .strip_suffix(':')
            .ok_or("path keys must introduce block mappings")?;
        let key = if key.starts_with('"') {
            serde_json::from_str::<String>(key)?
        } else if let Some(key) = key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')) {
            key.replace("''", "'")
        } else {
            key.to_owned()
        };
        if !key.starts_with('/') || key.contains(char::is_whitespace) {
            return Err(format!("unsupported OpenAPI path key: {key}").into());
        }
        if !paths.insert(key.clone()) {
            return Err(format!("duplicate declared path: {key}").into());
        }
    }
    if !found_paths || paths.is_empty() {
        return Err("OpenAPI paths mapping missing or empty".into());
    }
    Ok(paths)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exclusion {
    path: String,
    reason: String,
}

fn excluded_paths(json: &str) -> CheckResult<BTreeSet<String>> {
    let entries: Vec<Exclusion> = serde_json::from_str(json)?;
    let mut paths = BTreeSet::new();
    for entry in entries {
        if entry.reason.trim().is_empty() {
            return Err(format!("empty exclusion reason for {}", entry.path).into());
        }
        if !entry.path.starts_with('/')
            || entry.path.contains('*')
            || entry.path.contains(char::is_whitespace)
        {
            return Err(format!(
                "exclusion must be an exact path, without wildcards: {}",
                entry.path
            )
            .into());
        }
        if !paths.insert(entry.path.clone()) {
            return Err(format!("duplicate exclusion: {}", entry.path).into());
        }
    }
    Ok(paths)
}

fn check_partition(
    registered: &BTreeSet<String>,
    declared: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
) -> CheckResult<()> {
    let accounted: BTreeSet<_> = declared.union(excluded).cloned().collect();
    let missing: Vec<_> = registered.difference(&accounted).collect();
    let unserved: Vec<_> = declared.difference(registered).collect();
    let stale: Vec<_> = excluded.difference(registered).collect();
    let overlap: Vec<_> = declared.intersection(excluded).collect();
    if !missing.is_empty() || !unserved.is_empty() || !stale.is_empty() || !overlap.is_empty() {
        return Err(format!("OpenAPI partition drift:\nregistered but undocumented/unexcluded: {missing:?}\ndeclared but unregistered: {unserved:?}\nstale exclusions: {stale:?}\nboth declared and excluded: {overlap:?}").into());
    }
    Ok(())
}

#[test]
fn router_paths_are_documented_or_explicitly_excluded() -> CheckResult<()> {
    // Read at runtime so the falsifiers exercise the files even when cargo
    // reuses this test executable. No copied route/path snapshot lives here.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let registered = registered_paths(&std::fs::read_to_string(root.join("src/lib.rs"))?)?;
    let declared = declared_paths(&std::fs::read_to_string(root.join("openapi/v1.yaml"))?)?;
    let excluded = excluded_paths(&std::fs::read_to_string(
        root.join("openapi/exclusions.json"),
    )?)?;
    check_partition(&registered, &declared, &excluded)?;
    println!(
        "OpenAPI partition: {} registered paths/mounts = {} declared + {} excluded",
        registered.len(),
        declared.len(),
        excluded.len()
    );
    Ok(())
}

#[test]
fn source_reader_ignores_comments_and_handles_multiline_raw_literals() -> CheckResult<()> {
    let source = r##"fn router() { Router::new()
        // .route("/fiction", get(handler))
        .route(
            r#"/v1/example/{id}"#,
            get(handler))
        .route("/v1/example/{id}", post(handler)); }"##;
    assert_eq!(
        registered_paths(source)?,
        BTreeSet::from(["/v1/example/{id}".into()])
    );
    assert!(registered_paths("fn router() { Router::new().route(path(), get(h)); }").is_err());
    assert!(registered_paths("fn router() { Router::new().merge(other()); }").is_err());
    Ok(())
}

#[test]
fn yaml_reader_reads_only_path_keys_and_refuses_unsupported_forms() -> CheckResult<()> {
    assert_eq!(declared_paths("paths:\n  '/v1/a':\n    get:\n      description: |\n        /example:\ncomponents:\n  /not-a-path:\n")?, BTreeSet::from(["/v1/a".into()]));
    assert!(declared_paths("paths: *elsewhere\n").is_err());
    assert!(declared_paths("paths:\n  <<: *elsewhere\n").is_err());
    assert!(declared_paths("paths:\n    /wrong-indent:\n").is_err());
    Ok(())
}

#[test]
fn exclusions_require_nonempty_reasons_and_exact_unique_paths() -> CheckResult<()> {
    for json in [
        r#"[{"path":"/v1/a","reason":" "}]"#,
        r#"[{"path":"/v1/*","reason":"too broad"}]"#,
        r#"[{"path":"/a","reason":"one"},{"path":"/a","reason":"two"}]"#,
    ] {
        assert!(
            excluded_paths(json).is_err(),
            "accepted invalid exclusion {json}"
        );
    }
    assert_eq!(
        excluded_paths(r#"[{"path":"/a","reason":"Bootstrap asset."}]"#)?,
        BTreeSet::from(["/a".into()])
    );
    Ok(())
}

#[test]
fn partition_reports_both_directions_stale_entries_and_overlap() {
    let registered = BTreeSet::from(["/served".into(), "/missing".into()]);
    let declared = BTreeSet::from(["/served".into(), "/fiction".into()]);
    let excluded = BTreeSet::from(["/served".into(), "/stale".into()]);
    let Err(error) = check_partition(&registered, &declared, &excluded) else {
        panic!("drift accepted");
    };
    for path in ["/missing", "/fiction", "/stale", "/served"] {
        assert!(
            error.to_string().contains(path),
            "diagnostic omitted {path}"
        );
    }
}
